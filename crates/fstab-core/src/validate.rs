//! Structural and semantic validation of candidate fstab documents - see `docs/SPEC.md` §5, §6.
//!
//! This module implements the two validation steps described in
//! `docs/SPEC.md` §6 ("Atomicity") step 1 and step 2:
//!
//! 1. **Structural validation** (`structural_findings`): cheap, in-memory
//!    checks of a [`Document`]'s [`crate::model::FstabEntry`] lines -
//!    non-empty `fs_type`, duplicate mount points, malformed-looking
//!    `UUID=`/`PARTUUID=` values, and a couple of advisory checks (mount
//!    point missing on disk, visible [`Line::Malformed`] lines).
//! 2. **Semantic validation** (`semantic_findings`): an external check
//!    equivalent to `findmnt --verify --tab-file <path>`, run against a
//!    temporary rendering of the candidate document - never against the
//!    real target file.
//!
//! [`validate`] runs both and returns a [`ValidationReport`] distinguishing
//! blocking [`Severity::Error`] findings from advisory [`Severity::Warning`]
//! findings. A caller (e.g. the future atomic writer, T6) should check
//! [`ValidationReport::has_errors`] (or the equivalent [`ValidationReport::is_ok`])
//! before writing anything to disk, and may still want to surface warnings
//! to the user even when there are no errors.
//!
//! ## Design note: no `target_path` parameter
//!
//! An earlier sketch of this module's API included a `target_path: &Path`
//! parameter on [`validate`]. It was dropped: every check this module
//! performs is self-contained within the candidate [`Document`] (structural
//! checks) or operates against a throwaway temporary file (the semantic
//! check) - none of them need to know what file the candidate would
//! eventually be written to. Keeping that parameter would mean either
//! leaving it unused (which `-D warnings` would reject) or inventing a use
//! for it that doesn't serve any check actually required by
//! `docs/SPEC.md` §5/§6. If a future check genuinely needs the target path
//! (e.g. comparing against the file currently on disk), add the parameter
//! then.
//!
//! ## `findmnt` I/O-vs-pure-logic separation
//!
//! Following the same pattern `discovery.rs` uses for `lsblk`
//! (`run_lsblk` vs. `parse_lsblk_json`), the `findmnt` semantic check is
//! split into:
//!
//! - `run_findmnt_verify`: the actual I/O - renders the candidate
//!   document, writes it to a fresh temp file (via [`std::env::temp_dir`]),
//!   and shells out to `findmnt --verify --tab-file <temp path>`.
//! - `interpret_findmnt_result`: a pure function that maps an
//!   `io::Result<FindmntRunResult>` to zero or more [`Finding`]s, with no
//!   process invocation of its own. This is what unit tests exercise
//!   directly (including the "findmnt unavailable" degraded-mode case, via
//!   a synthetic `Err(io::Error::from(ErrorKind::NotFound))`) without
//!   needing to actually uninstall `findmnt` from the test machine.
//!
//! Per `docs/SPEC.md` §6 step 2 and the explicit instruction to never
//! silently skip-and-claim-success, any failure to even run `findmnt`
//! (missing binary, or any other I/O error attempting to launch it) is
//! surfaced as a non-blocking [`Severity::Warning`] finding that says
//! verification ran in degraded mode - it is deliberately not silent and
//! deliberately not a hard error (an environment without `findmnt`
//! installed should still be able to save a structurally valid fstab).

use crate::model::{Document, Line, SourceId};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// The special `mount_point` value conventionally used by swap entries,
/// which have no real mount point to collide on. Per fstab convention (e.g.
/// `UUID=... none swap sw 0 0`), multiple swap entries legitimately share
/// this value, so it is exempt from duplicate-mount-point detection and from
/// the "mount point exists on disk" advisory check.
const SWAP_NONE_MOUNT_POINT: &str = "none";

/// The severity of a single [`Finding`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// A blocking problem: the candidate document must not be written to
    /// disk while this finding is present.
    Error,
    /// An advisory problem: worth surfacing to the user, but does not block
    /// a save (e.g. a mount point that doesn't exist yet, or a degraded
    /// semantic check due to `findmnt` being unavailable).
    Warning,
}

/// Which of an entry's fields a [`Finding`] is about, when it is about
/// exactly one.
///
/// This is a domain concept - one of the fields of a
/// [`crate::model::FstabEntry`] line - not a UI concept; a GUI consuming
/// this is free to map each variant onto whichever input widget shows that
/// field. [`Finding::field`] being `None` does not mean "unknown"; it means
/// the finding is not about exactly one field/line at all - either it spans
/// multiple lines (e.g. a mount point shared by two entries), or it isn't
/// about a parsed entry in the first place (e.g. a [`Line::Malformed`]
/// line), or it is about one identifiable line but names no field of it
/// (a `findmnt --verify` diagnostic - see [`findmnt_detail_line_findings`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingField {
    /// The entry's mount point (second fstab field).
    MountPoint,
    /// The entry's source device/identifier (first fstab field).
    Source,
    /// The entry's filesystem type (third fstab field).
    FsType,
    /// The entry's human-readable description - not one of the six fstab
    /// fields itself, but the comment line (or lines) rustab attaches to the
    /// entry (see [`crate::model::FstabEntry::description`]).
    ///
    /// No check in this module currently produces this variant: T43 deleted
    /// the only one that did (a description had to be a single line back when
    /// it was written as exactly one comment line; it no longer does). The
    /// variant itself is deliberately kept - it is part of a wire-locked
    /// enum, spelled `"description"`, documented in `docs/API_CONTRACT.md`
    /// §7, and a future description-specific check should reuse it rather
    /// than re-adding it. `fstab-core` is a library crate, so a `pub` variant
    /// nothing constructs is not a `dead_code` hazard.
    Description,
    /// The entry's trailing inline comment - the free-form text appended
    /// after the entry's six fstab fields on the same physical line,
    /// following a `#` (see [`crate::model::FstabEntry::trailing_comment`]).
    /// Distinct from [`Self::Description`], which is a separate comment
    /// line (or lines) placed *above* the entry rather than appended after
    /// it. Produced by [`trailing_comment_contains_newline_findings`].
    TrailingComment,
}

/// A single structural or semantic validation finding, with a severity and a
/// human-readable message describing the problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Whether this finding blocks a save ([`Severity::Error`]) or is merely
    /// advisory ([`Severity::Warning`]).
    pub severity: Severity,
    /// A human-readable description of the problem, suitable for showing
    /// directly to a user (e.g. in the GUI's inline validation feedback, per
    /// `docs/SPEC.md` §9).
    pub message: String,
    /// Which single field of an entry this finding is about, if any. See
    /// [`FindingField`]'s doc comment for exactly what `None` means here -
    /// it is not "unknown", it is "not applicable to a single field".
    pub field: Option<FindingField>,
    /// The 0-based index, into the validated [`Document`]'s `lines`, of the
    /// single line this finding is about, if any. This reuses the project's
    /// existing convention that a line's identity is simply its index in
    /// the lines array (already documented on `LineDto` in both DTO
    /// crates) rather than inventing a new identity scheme - a caller that
    /// knows which `Document`/`DocumentDto` was validated can use this
    /// index to look the exact line back up.
    ///
    /// Usually set exactly when [`Self::field`] is set (both or neither) -
    /// with two exceptions, both of which set `line_index` alone. A
    /// [`Line::Malformed`] line is one: it is a single, identifiable line,
    /// so `line_index` is set for it, while `field` stays `None` because it
    /// was never a parsed entry with fields to localize to in the first
    /// place. A `findmnt --verify` diagnostic attributed to one entry (see
    /// [`findmnt_detail_line_findings`]) is the other: `findmnt` names the
    /// mount point its complaint is about, which is enough to identify the
    /// line, but not which of that entry's fields is at fault. A consumer
    /// must therefore treat `line_index: Some, field: None` as "about this
    /// whole line, not one field of it" - not assume the pairing always
    /// holds.
    pub line_index: Option<usize>,
}

impl Finding {
    /// Constructs a blocking [`Severity::Error`] finding with the given
    /// message, not localized to any one field/line.
    fn error(message: impl Into<String>) -> Self {
        Finding {
            severity: Severity::Error,
            message: message.into(),
            field: None,
            line_index: None,
        }
    }

    /// Constructs an advisory [`Severity::Warning`] finding with the given
    /// message, not localized to any one field/line.
    fn warning(message: impl Into<String>) -> Self {
        Finding {
            severity: Severity::Warning,
            message: message.into(),
            field: None,
            line_index: None,
        }
    }

    /// Constructs a blocking [`Severity::Error`] finding localized to a
    /// single field of a single line - see [`FindingField`] and
    /// [`Self::line_index`] for what the two extra parameters mean.
    fn error_at(message: impl Into<String>, field: FindingField, line_index: usize) -> Self {
        Finding {
            severity: Severity::Error,
            message: message.into(),
            field: Some(field),
            line_index: Some(line_index),
        }
    }

    /// Constructs an advisory [`Severity::Warning`] finding localized to a
    /// single field of a single line - see [`FindingField`] and
    /// [`Self::line_index`] for what the two extra parameters mean.
    fn warning_at(message: impl Into<String>, field: FindingField, line_index: usize) -> Self {
        Finding {
            severity: Severity::Warning,
            message: message.into(),
            field: Some(field),
            line_index: Some(line_index),
        }
    }

    /// Constructs an advisory [`Severity::Warning`] finding localized to a
    /// single *line* without naming one of its [`FindingField`]s - see
    /// [`Self::line_index`]'s doc comment for why `line_index` is set here
    /// while `field` stays `None`.
    fn warning_at_line(message: impl Into<String>, line_index: usize) -> Self {
        Finding {
            severity: Severity::Warning,
            message: message.into(),
            field: None,
            line_index: Some(line_index),
        }
    }

    /// Constructs a blocking [`Severity::Error`] finding localized to a
    /// single *line* without naming one of its [`FindingField`]s - the
    /// [`Severity::Error`] counterpart of [`Self::warning_at_line`], used by
    /// [`findmnt_detail_line_findings`] for a `findmnt --verify` `[E]`
    /// diagnostic that could be attributed to exactly one entry but names no
    /// specific fstab field.
    fn error_at_line(message: impl Into<String>, line_index: usize) -> Self {
        Finding {
            severity: Severity::Error,
            message: message.into(),
            field: None,
            line_index: Some(line_index),
        }
    }
}

/// The outcome of validating a candidate [`Document`]: every [`Finding`]
/// produced by structural and semantic validation, in no particular order
/// across the two categories (structural findings are pushed first, then
/// semantic ones).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationReport {
    /// All findings from this validation run, both blocking and advisory.
    pub findings: Vec<Finding>,
}

impl ValidationReport {
    /// Returns `true` if any finding in this report is a blocking
    /// [`Severity::Error`]. A caller performing a save (e.g. the atomic
    /// writer in T6) must treat `true` here as "abort before touching
    /// disk", per `docs/SPEC.md` §6 step 1-2.
    pub fn has_errors(&self) -> bool {
        self.findings.iter().any(|f| f.severity == Severity::Error)
    }

    /// Returns `true` if this report contains no blocking errors (it may
    /// still contain advisory warnings). The logical complement of
    /// [`Self::has_errors`], provided as the trivially-checkable "is it safe
    /// to proceed" predicate a caller wants.
    pub fn is_ok(&self) -> bool {
        !self.has_errors()
    }

    /// Returns an iterator over just the blocking [`Severity::Error`]
    /// findings.
    pub fn errors(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Error)
    }

    /// Returns an iterator over just the advisory [`Severity::Warning`]
    /// findings.
    pub fn warnings(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Warning)
    }
}

/// Validates a candidate fstab [`Document`], running both structural checks
/// (see the module-level docs) and a semantic check equivalent to
/// `findmnt --verify --tab-file <path>` against a temporary rendering of
/// `doc`.
///
/// This never touches any real target file: the semantic check writes `doc`
/// (via its [`std::fmt::Display`] / canonical serializer) to a throwaway
/// temp file under [`std::env::temp_dir`] and points `findmnt` at that temp
/// path only. Never panics; if `findmnt` itself cannot be run at all (not on
/// `$PATH`, or any other I/O error attempting to launch it), that is
/// reported as an advisory [`Severity::Warning`] finding rather than
/// silently treated as a pass.
pub fn validate(doc: &Document) -> ValidationReport {
    let mut findings = structural_findings(doc);
    findings.extend(semantic_findings(doc));
    ValidationReport { findings }
}

// ---------------------------------------------------------------------------
// Structural validation
// ---------------------------------------------------------------------------

/// Runs all structural checks against `doc` and returns every [`Finding`]
/// produced. Pure and I/O-free except for the "mount point exists on disk"
/// advisory check, which does a single [`Path::exists`] stat call per entry.
fn structural_findings(doc: &Document) -> Vec<Finding> {
    let mut findings = Vec::new();

    findings.extend(malformed_line_findings(doc));
    findings.extend(empty_fs_type_findings(doc));
    findings.extend(duplicate_mount_point_findings(doc));
    findings.extend(malformed_uuid_findings(doc));
    findings.extend(mount_point_missing_findings(doc));
    findings.extend(trailing_comment_contains_newline_findings(doc));

    findings
}

/// Surfaces any [`Line::Malformed`] lines in `doc` as advisory warnings.
/// Per `docs/SPEC.md` §5, a malformed line must be preserved (the parser
/// already guarantees that) and surfaced to the user rather than silently
/// skipped - but it is non-blocking here: a document containing a malformed
/// line the parser could not understand is not, by itself, a reason to
/// refuse to save the *rest* of the file (the parser's `Display` impl
/// re-renders malformed lines verbatim, so nothing is lost either way).
///
/// Tagged with [`Finding::line_index`] (via [`Finding::warning_at_line`]) so
/// a consumer - the GUI's per-entry drawer, specifically - can scope this
/// finding to the one line it is actually about, same as every other
/// line-specific finding. It was originally left completely untagged
/// (T21.3), which meant it read as "document-level" to that scoping and
/// surfaced inside *every* entry's add/edit drawer regardless of which line
/// was open - confusing in exactly the way a direct user report already
/// got the cross-entry-warning case fixed for (`docs/UI_DESIGN.md` §5).
/// There is still no [`FindingField`] for it (a malformed line was never a
/// parsed entry with fields to localize to), so `field` stays `None`.
fn malformed_line_findings(doc: &Document) -> Vec<Finding> {
    doc.lines
        .iter()
        .enumerate()
        .filter_map(|(idx, line)| match line {
            Line::Malformed(raw) => Some(Finding::warning_at_line(
                format!(
                    "line could not be understood as a valid entry and was preserved as-is: \
                     {raw:?}"
                ),
                idx,
            )),
            _ => None,
        })
        .collect()
}

/// Flags any [`FstabEntry`] whose `fs_type` is empty as a blocking error.
///
/// The parser (`parser.rs`) already requires at least four whitespace fields
/// to produce an [`Line::Entry`] at all, and an empty *token* between two
/// runs of whitespace cannot occur from `str::split_whitespace`, so this
/// should be unreachable via normal parsing. It is checked anyway as a
/// second line of defense against entries constructed directly via the
/// `fstab-core` API (e.g. by a future GUI add/edit form) bypassing the
/// parser entirely.
fn empty_fs_type_findings(doc: &Document) -> Vec<Finding> {
    doc.lines
        .iter()
        .enumerate()
        .filter_map(|(idx, line)| match line {
            Line::Entry(entry) if entry.fs_type.is_empty() => Some((idx, entry)),
            _ => None,
        })
        .map(|(idx, entry)| {
            Finding::error_at(
                format!(
                    "entry for mount point {:?} has an empty filesystem type",
                    entry.mount_point
                ),
                FindingField::FsType,
                idx,
            )
        })
        .collect()
}

/// Flags any [`crate::model::FstabEntry`] whose `trailing_comment` contains
/// an embedded newline (`\n` or `\r`) as a blocking error.
///
/// A trailing comment parsed from a real file can never contain one (the
/// source text was already split on newlines before any field reached this
/// code, and `trailing_comment` is only ever the text after the first `#` on
/// that one physical line) - this exists as a second line of defense against
/// an `EntryDto` constructed directly via the Tauri IPC boundary, bypassing
/// the text parser entirely, the same reasoning [`empty_fs_type_findings`]
/// above documents for its own analogous case. If such a value ever reached
/// the writer, it would split one physical fstab line into two, corrupting
/// the file - the second half would be misparsed as a [`Line::Malformed`]
/// line or a bogus new entry on the next load.
fn trailing_comment_contains_newline_findings(doc: &Document) -> Vec<Finding> {
    doc.lines
        .iter()
        .enumerate()
        .filter_map(|(idx, line)| match line {
            Line::Entry(entry) => Some((idx, entry)),
            _ => None,
        })
        .filter(|(_, entry)| {
            entry
                .trailing_comment
                .as_deref()
                .is_some_and(|c| c.contains('\n') || c.contains('\r'))
        })
        .map(|(idx, entry)| {
            Finding::error_at(
                format!(
                    "entry for mount point {:?} has a trailing comment containing an embedded \
                     newline; a trailing comment is appended after the entry's fields on the \
                     same physical line, so it must be a single line of text",
                    entry.mount_point
                ),
                FindingField::TrailingComment,
                idx,
            )
        })
        .collect()
}

/// Flags mount points shared by two or more entries as a blocking error,
/// except for the literal `none` mount point (see [`SWAP_NONE_MOUNT_POINT`]),
/// which multiple swap entries legitimately share since they have no real
/// mount point to collide on.
fn duplicate_mount_point_findings(doc: &Document) -> Vec<Finding> {
    let mut seen: HashMap<&str, u32> = HashMap::new();
    for entry in doc.entries() {
        if entry.mount_point == SWAP_NONE_MOUNT_POINT {
            continue;
        }
        *seen.entry(entry.mount_point.as_str()).or_insert(0) += 1;
    }

    seen.into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(mount_point, count)| {
            Finding::error(format!(
                "mount point {mount_point:?} is used by {count} entries; each mount point \
                 (other than \"none\", used by swap entries) must be unique"
            ))
        })
        .collect()
}

/// Flags [`SourceId::Uuid`] and [`SourceId::PartUuid`] values that don't look
/// like a canonical UUID (the standard 8-4-4-4-12 hex-with-hyphens textual
/// form, e.g. `11111111-2222-3333-4444-555555555555`) as a blocking error.
///
/// Deliberately lenient: this only checks the overall shape (hyphen
/// positions and hex digits), not that the UUID identifies any real device,
/// and it accepts both upper- and lower-case hex digits (`blkid`/`lsblk`
/// report lower-case, but fstab files edited by hand sometimes contain
/// upper-case). It does not attempt to validate the UUID version/variant
/// bits (RFC 9562 §4-5) - `/etc/fstab` has never required a *version-correct*
/// UUID, only a filesystem UUID in the conventional textual shape, so being
/// stricter than that would risk rejecting valid-but-uncommon real-world
/// values this crate has no way to double-check against actual hardware.
fn malformed_uuid_findings(doc: &Document) -> Vec<Finding> {
    doc.lines
        .iter()
        .enumerate()
        .filter_map(|(idx, line)| match line {
            Line::Entry(entry) => Some((idx, entry)),
            _ => None,
        })
        .filter_map(|(idx, entry)| match &entry.source {
            // `UUID=` accepts a second, shorter shape beyond the canonical
            // 36-char form: `blkid`/the kernel report a FAT/exFAT
            // filesystem's volume serial number as an 8-4 hex pair (e.g.
            // `CAF7-75BB`), and a real `/etc/fstab` referencing a FAT/EFI
            // system partition this way is common and valid - rejecting it
            // was a real false positive, not overcaution. `PARTUUID=`
            // (always a GPT/MBR partition-table GUID, never a filesystem
            // volume ID) is deliberately not given the same allowance here;
            // no report has shown a real short PARTUUID shape in the wild.
            SourceId::Uuid(value)
                if !looks_like_uuid(value) && !looks_like_fat_volume_id(value) =>
            {
                Some((idx, "UUID", value, &entry.mount_point))
            }
            SourceId::PartUuid(value) if !looks_like_uuid(value) => {
                Some((idx, "PARTUUID", value, &entry.mount_point))
            }
            _ => None,
        })
        .map(|(idx, prefix, value, mount_point)| {
            let expected = if prefix == "UUID" {
                "either the standard 8-4-4-4-12 hex UUID shape (e.g. \
                 11111111-2222-3333-4444-555555555555) or a FAT/exFAT volume ID \
                 (e.g. CAF7-75BB)"
            } else {
                "the standard 8-4-4-4-12 hex UUID shape (e.g. \
                 11111111-2222-3333-4444-555555555555)"
            };
            Finding::error_at(
                format!(
                    "entry for mount point {mount_point:?} has a malformed {prefix}= value \
                     {value:?}; expected {expected}"
                ),
                FindingField::Source,
                idx,
            )
        })
        .collect()
}

/// Returns `true` if `value` has the canonical UUID textual shape: 32 hex
/// digits grouped `8-4-4-4-12` and separated by literal hyphens at exactly
/// those positions, case-insensitively. See [`malformed_uuid_findings`] for
/// why this check is intentionally no stricter than that.
fn looks_like_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    const HYPHEN_POSITIONS: [usize; 4] = [8, 13, 18, 23];
    for (i, &b) in bytes.iter().enumerate() {
        if HYPHEN_POSITIONS.contains(&i) {
            if b != b'-' {
                return false;
            }
        } else if !b.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

/// Returns `true` if `value` has the shape `blkid`/the kernel use for a
/// FAT/exFAT filesystem's volume serial number: exactly 4 hex digits, a
/// literal hyphen, then exactly 4 more hex digits (e.g. `CAF7-75BB`),
/// case-insensitively. Unlike [`looks_like_uuid`], this is a real,
/// commonly-seen alternate `UUID=` shape - FAT-family filesystems (vfat,
/// the typical ESP filesystem) have no 128-bit UUID to report, only this
/// 32-bit volume ID, and `/etc/fstab` entries referencing one this way are
/// valid and unremarkable, not a malformed value.
fn looks_like_fat_volume_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 9 {
        return false;
    }
    bytes[..4].iter().all(u8::is_ascii_hexdigit)
        && bytes[4] == b'-'
        && bytes[5..].iter().all(u8::is_ascii_hexdigit)
}

/// Advisory check: flags a mount point that doesn't currently exist on disk.
/// Non-blocking per `docs/SPEC.md` §9 - a mount point legitimately may not
/// exist yet at validation time (e.g. provisioning a fresh `/mnt/newdrive`
/// directory before first boot), so this is a [`Severity::Warning`], not a
/// [`Severity::Error`].
///
/// Skipped entirely for the literal `none` mount point (swap entries have no
/// real mount point to check). `mount_point` is decoded via
/// [`crate::model::decode_escaped_whitespace`] before the existence check -
/// the model stores it raw (octal escapes and all, per `docs/SPEC.md`'s
/// escaped-whitespace policy), but a literal `\040` in that text would never
/// match the real on-disk path, which has an actual space. The *finding*
/// still reports the raw, as-stored text, so it reads like the rest of the
/// document and like something the user can find by eye.
fn mount_point_missing_findings(doc: &Document) -> Vec<Finding> {
    doc.lines
        .iter()
        .enumerate()
        .filter_map(|(idx, line)| match line {
            Line::Entry(entry) => Some((idx, entry)),
            _ => None,
        })
        .filter(|(_, entry)| entry.mount_point != SWAP_NONE_MOUNT_POINT)
        .filter(|(_, entry)| {
            !Path::new(&crate::model::decode_escaped_whitespace(&entry.mount_point)).exists()
        })
        .map(|(idx, entry)| {
            Finding::warning_at(
                format!(
                    "mount point {:?} does not currently exist on disk",
                    entry.mount_point
                ),
                FindingField::MountPoint,
                idx,
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Semantic validation (`findmnt --verify --tab-file`)
// ---------------------------------------------------------------------------

/// The result of successfully running `findmnt --verify --tab-file`
/// (regardless of whether `findmnt` itself reported the file as valid) -
/// i.e. the process launched and ran to completion. Deliberately a small,
/// synthesizable struct (rather than the raw [`std::process::Output`]) so
/// [`interpret_findmnt_result`] can be unit-tested with hand-built values,
/// without needing a real child process.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FindmntRunResult {
    /// `true` if `findmnt --verify` exited with status 0 (no errors).
    success: bool,
    /// `findmnt`'s stdout followed by its stderr (if non-empty), concatenated
    /// so a caller gets whatever diagnostic text `findmnt` produced either
    /// way; `findmnt --verify` is documented to report problems on stdout,
    /// but stderr is included too so nothing is silently dropped.
    combined_output: String,
}

/// Runs structural checks are not repeated here; this performs only the
/// semantic (`findmnt --verify`) half of validation and returns its
/// [`Finding`]s.
fn semantic_findings(doc: &Document) -> Vec<Finding> {
    let rendered = doc.to_string();
    let result = run_findmnt_verify(&rendered);
    interpret_findmnt_result(doc, &result)
}

/// Pure decision logic: given the result of attempting to run
/// `findmnt --verify --tab-file` (see [`run_findmnt_verify`]), decides what
/// [`Finding`]s to report. Contains no process invocation or file I/O of its
/// own, which is what makes it directly unit-testable (including the
/// "findmnt unavailable" degraded-mode path, via a synthetic
/// `Err(io::Error::from(io::ErrorKind::NotFound))`) without needing to
/// actually uninstall `findmnt`.
///
/// - `Err(_)` (the binary could not be launched at all, for any reason -
///   not on `$PATH`, or some other I/O error) → one [`Severity::Warning`]
///   finding stating that semantic verification ran in degraded mode
///   (structural checks only), never silently treated as success, per
///   `docs/SPEC.md` §6.
/// - `Ok(run)` with `run.success == true` → no findings.
/// - `Ok(run)` with `run.success == false` → first, the preferred path:
///   `findmnt`'s output is grouped by mount point, so
///   [`parse_findmnt_detail`] tries to decompose it into one diagnostic per
///   mount point, and [`findmnt_detail_line_findings`] tries to attribute
///   each of those to exactly one [`Line::Entry`] of `doc`. When *both*
///   succeed (every line of `findmnt`'s output was recognized, and every
///   mount point it named resolved to exactly one entry), the result is one
///   per-line [`Finding`] per diagnostic, carrying the real
///   [`Finding::line_index`] of the entry it is about and the severity of
///   its own `[E]`/`[W]` marker - and no whole-output dump at all. That is
///   what lets a GUI put each complaint on the row/drawer it belongs to
///   instead of showing one unscoped blob on every entry.
///
///   If anything about the output is unexpected, or any mount point is
///   ambiguous (matches zero, or two or more entries), the whole result
///   falls back - all-or-nothing, never partially attributed - to the
///   original behavior below, which is the safety net and deliberately
///   unchanged: `findmnt --verify` prints a
///   final summary line shaped like
///   `"<N> parse error(s), <M> error(s), <K> warning(s)"` and exits
///   non-zero whenever `N > 0` *or* `M > 0` - i.e. it conflates "a line
///   could not be parsed" (`N`; `findmnt` itself logs `parse error at line
///   X -- ignored` and keeps analyzing the rest of the file, so this is not
///   fatal) with "the file parsed fine but has a real mount/boot-reachability
///   problem" (`M`). [`parse_findmnt_summary`] extracts `N`/`M`/`K` from that
///   line so the two cases are not conflated here:
///     - If the summary line parses and `M == 0` (regardless of `N`, and
///       regardless of the non-zero exit code that `N > 0` alone causes),
///       this is a non-blocking [`Severity::Warning`] - findmnt did not
///       find any real semantic problem.
///     - If the summary line parses and `M > 0`, this is still a blocking
///       [`Severity::Error`], and the finding still includes `findmnt`'s own
///       output text.
///     - If the summary line cannot be found/parsed at all (output shape
///       `findmnt` produced does not match what this function expects),
///       this falls back to the original conservative behavior - a
///       blocking [`Severity::Error`] - rather than ever silently treating
///       an unrecognized failure as a pass.
fn interpret_findmnt_result(doc: &Document, result: &io::Result<FindmntRunResult>) -> Vec<Finding> {
    match result {
        Err(err) => vec![Finding::warning(format!(
            "semantic verification (equivalent to `findmnt --verify`) was skipped because \
             findmnt could not be run ({err}); this is a degraded-mode result covering \
             structural checks only, not a confirmation that the file is fully valid. \
             Install `findmnt` (part of util-linux) to enable full verification."
        ))],
        Ok(run) if run.success => Vec::new(),
        Ok(run) => {
            // Preferred path: decompose findmnt's output into per-entry
            // findings. Both halves have to agree before anything is
            // reported this way - an unrecognized output shape, or a mount
            // point that cannot be pinned to exactly one entry, drops the
            // whole result through to the unchanged fallback below rather
            // than reporting a partially-attributed (and therefore
            // possibly incomplete) set of findings.
            let (detail_lines, had_unrecognized_content) =
                parse_findmnt_detail(&run.combined_output);
            if !had_unrecognized_content {
                if let Some(findings) = findmnt_detail_line_findings(doc, &detail_lines) {
                    if !findings.is_empty() {
                        return findings;
                    }
                }
            }

            let detail = run.combined_output.trim();
            match parse_findmnt_summary(&run.combined_output) {
                Some(summary) if summary.errors == 0 => {
                    vec![Finding::warning(format!(
                        "findmnt --verify found {} parse error(s) in this fstab, which findmnt \
                         itself treats as non-fatal (it logs \"parse error ... -- ignored\" for \
                         each one and keeps analyzing the rest of the file); no real mount/boot \
                         errors were found, so this does not block saving. findmnt's full \
                         output:\n{detail}",
                        summary.parse_errors
                    ))]
                }
                Some(_) | None => {
                    let message = if detail.is_empty() {
                        "findmnt --verify reported this fstab as invalid, but produced no \
                         further output."
                            .to_string()
                    } else {
                        format!("findmnt --verify reported a problem with this fstab:\n{detail}")
                    };
                    vec![Finding::error(message)]
                }
            }
        }
    }
}

/// One `[E]`/`[W]` diagnostic line from `findmnt --verify`'s output,
/// together with the mount point of the block it appeared under (see
/// [`parse_findmnt_detail`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct FindmntDetailLine {
    /// The mount-point text from the block header this diagnostic appeared
    /// under, verbatim as `findmnt` printed it - which is verbatim as the
    /// candidate document contained it, since the candidate was rendered
    /// via [`Document`]'s `Display` impl before being handed to `findmnt`.
    mount_point: String,
    /// [`Severity::Error`] for a `[E]` marker, [`Severity::Warning`] for a
    /// `[W]` one. Taken from this line's own marker, never re-derived from
    /// the summary line's aggregate counts.
    severity: Severity,
    /// The diagnostic text after the marker, e.g. `"unreachable on boot
    /// required target: No such file or directory"`.
    message: String,
}

/// The exact indentation `findmnt --verify` puts before each `[E]`/`[W]`
/// diagnostic line (confirmed against util-linux 2.42.2). Matched exactly,
/// not as "some whitespace": a different indentation is something this
/// parser has not seen and must not guess about.
const FINDMNT_DETAIL_INDENT: &str = "   ";

/// Decomposes `findmnt --verify`'s combined stdout+stderr text into one
/// [`FindmntDetailLine`] per `[E]`/`[W]` diagnostic, returning them along
/// with a `had_unrecognized_content` flag that is `true` if *anything* in
/// `text` could not be cleanly explained.
///
/// `findmnt --verify` groups its diagnostics by mount point: an unindented
/// line holding the mount-point text verbatim, followed by one or more
/// lines of [`FINDMNT_DETAIL_INDENT`] + `[E] `/`[W] ` + message. Real
/// captured output (util-linux 2.42.2), as
/// [`run_findmnt_verify`] assembles it (stdout, then stderr):
///
/// ```text
/// /mnt/unreachable
///    [E] unreachable on boot required target: No such file or directory
///    [E] unreachable on boot required source: UUID=deadbeef-0000-...
///
/// findmnt: /tmp/x.fstab: parse error at line 1 -- ignored
///
/// 1 parse error, 2 errors, 0 warnings
/// ```
///
/// Everything else this recognizes is deliberately *discarded* rather than
/// surfaced:
///
/// - Blank lines, which carry no information. A blank line also closes any
///   block currently open, since real output never separates a header from
///   its own diagnostics.
/// - The summary line (via [`parse_findmnt_summary_line`]); its counts are
///   aggregates of the per-line diagnostics, so reporting them too would
///   just repeat what the per-line findings already say.
/// - A `findmnt: <path>: parse error at line N -- ignored` stderr line.
///   This one is pure duplication: a line `findmnt` could not parse is
///   already reported by [`malformed_line_findings`], which has that line's
///   own correct `line_index` - and the path in it is the throwaway temp
///   file, which means nothing to a user.
///
/// The flag is set to `true` for anything else at all: an unknown line
/// shape, a `[E]`/`[W]` line with no block header open above it, or a
/// header with no diagnostics under it. The caller must then fall back
/// (see [`interpret_findmnt_result`]) - the point of the flag is that a
/// future `findmnt` whose output this parser only *partly* understands must
/// never result in silently dropped diagnostics.
fn parse_findmnt_detail(text: &str) -> (Vec<FindmntDetailLine>, bool) {
    let mut detail_lines: Vec<FindmntDetailLine> = Vec::new();
    let mut had_unrecognized_content = false;
    // The block currently open: its mount-point header text, and how many
    // diagnostic lines have been attributed to it so far.
    let mut open_block: Option<(String, usize)> = None;

    for line in text.lines() {
        if let Some((severity, message)) = parse_findmnt_detail_message_line(line) {
            match open_block.as_mut() {
                Some((mount_point, count)) => {
                    *count += 1;
                    detail_lines.push(FindmntDetailLine {
                        mount_point: mount_point.clone(),
                        severity,
                        message: message.to_string(),
                    });
                }
                // A diagnostic with nothing to attribute it to.
                None => had_unrecognized_content = true,
            }
            continue;
        }

        // Any non-diagnostic line ends the block above it; a header that
        // collected no diagnostics is not a shape this parser has seen.
        if let Some((_, count)) = open_block.take() {
            if count == 0 {
                had_unrecognized_content = true;
            }
        }

        if line.trim().is_empty()
            || parse_findmnt_summary_line(line).is_some()
            || is_findmnt_parse_error_line(line)
        {
            continue;
        }

        if looks_like_findmnt_mount_point_header(line) {
            open_block = Some((line.to_string(), 0));
            continue;
        }

        had_unrecognized_content = true;
    }

    if let Some((_, count)) = open_block {
        if count == 0 {
            had_unrecognized_content = true;
        }
    }

    (detail_lines, had_unrecognized_content)
}

/// Recognizes one indented `findmnt --verify` diagnostic line, returning
/// its severity and the message after the marker. Requires exactly
/// [`FINDMNT_DETAIL_INDENT`] followed by exactly `"[E] "` or `"[W] "` -
/// anything else (other indentation, an unknown marker letter) is not
/// recognized, so the caller treats it as unexplained content.
fn parse_findmnt_detail_message_line(line: &str) -> Option<(Severity, &str)> {
    let rest = line.strip_prefix(FINDMNT_DETAIL_INDENT)?;
    if let Some(message) = rest.strip_prefix("[E] ") {
        return Some((Severity::Error, message));
    }
    rest.strip_prefix("[W] ")
        .map(|message| (Severity::Warning, message))
}

/// Returns `true` for `findmnt`'s own `findmnt: <path>: parse error at line
/// N -- ignored` stderr line. See [`parse_findmnt_detail`] for why this line
/// is recognized only in order to discard it.
fn is_findmnt_parse_error_line(line: &str) -> bool {
    let line = line.trim_end();
    line.starts_with("findmnt: ")
        && line.contains(": parse error at line ")
        && line.ends_with(" -- ignored")
}

/// Returns `true` if `line` could be a `findmnt --verify` block header, i.e.
/// an fstab mount-point text printed on its own line.
///
/// A mount point is a single whitespace-free token: the fstab format splits
/// its fields on whitespace, and a mount point containing a space carries it
/// as an octal escape (`\040`) which this crate deliberately stores and
/// renders raw (see `docs/SPEC.md`). So "contains no whitespace at all" both
/// matches every real header and rejects `findmnt`'s prose lines (its own
/// `findmnt: ...` messages, the summary line, and any future wording), which
/// is what keeps an unrecognized line from being mistaken for a header.
fn looks_like_findmnt_mount_point_header(line: &str) -> bool {
    !line.is_empty() && !line.chars().any(char::is_whitespace)
}

/// Resolves [`FindmntDetailLine`]s against `doc`, returning one per-line
/// [`Finding`] per diagnostic - or `None` if any single one of them cannot
/// be attributed to exactly one entry.
///
/// Matching is plain string equality against [`crate::model::FstabEntry::mount_point`]
/// as stored (raw, octal escapes and all), the same way
/// [`mount_point_missing_findings`] matches: `findmnt` was given this very
/// document rendered by its own serializer, so the header text it echoes
/// back is byte-identical to what the model holds.
///
/// `None` is all-or-nothing on purpose. A mount point that matches *no*
/// entry means this function and `findmnt` disagree about the document,
/// and one that matches *two or more* (e.g. the `none` several swap entries
/// share) means there is no single right line to blame. Either way, guessing
/// risks pinning a real problem to the wrong row - worse than the honest
/// unscoped fallback - and a *partial* result risks dropping a diagnostic
/// entirely, so neither is attempted.
fn findmnt_detail_line_findings(
    doc: &Document,
    lines: &[FindmntDetailLine],
) -> Option<Vec<Finding>> {
    let mut findings = Vec::with_capacity(lines.len());

    for detail in lines {
        let mut matching = doc
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| match line {
                Line::Entry(entry) => entry.mount_point == detail.mount_point,
                _ => false,
            })
            .map(|(idx, _)| idx);

        let line_index = matching.next()?;
        if matching.next().is_some() {
            return None;
        }

        // The message is prefixed with its source so a user reading it on
        // its own row knows this is `findmnt`'s verdict, not rustab's own
        // wording - the fallback whole-output findings say so explicitly,
        // and these must not lose that attribution by being shorter.
        let message = format!("findmnt --verify: {}", detail.message);
        findings.push(match detail.severity {
            Severity::Error => Finding::error_at_line(message, line_index),
            Severity::Warning => Finding::warning_at_line(message, line_index),
        });
    }

    Some(findings)
}

/// The three counts `findmnt --verify` reports in its final summary line
/// (see [`parse_findmnt_summary`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FindmntSummary {
    /// `N`: lines `findmnt` could not parse at all (logged as `parse error
    /// at line X -- ignored` and otherwise non-fatal).
    parse_errors: u32,
    /// `M`: real semantic/mount/boot-reachability problems found in lines
    /// that did parse successfully.
    errors: u32,
    /// `K`: advisory warnings (e.g. a boot-required source that is merely
    /// unreachable right now, downgraded from an error by `noauto`).
    #[allow(dead_code)] // part of the parsed shape; not currently read by any caller.
    warnings: u32,
}

/// Parses `findmnt --verify`'s final summary line out of its combined
/// stdout+stderr text, tolerating both the singular ("1 error") and plural
/// ("2 errors", and also "0 errors") wording `findmnt` uses for each of the
/// three counts, and either "parse error(s)" or plain "error(s)" for the
/// respective counts.
///
/// Confirmed against real `findmnt` (util-linux 2.42.2) output such as:
/// - `"1 parse error, 2 errors, 0 warnings"`
/// - `"1 parse error, 0 errors, 2 warnings"`
/// - `"0 parse errors, 0 errors, 2 warnings"`
/// - `"0 parse errors, 1 error, 0 warnings"`
///
/// Returns `None` if no line in `text` matches the expected
/// `"<N> parse error(s), <M> error(s), <K> warning(s)"` shape at all (e.g. a
/// future `findmnt` version changes the wording) - callers must treat `None`
/// conservatively (today's "non-zero exit = Error" behavior), never as a
/// free pass, since this function has no way to tell the two cases apart
/// once the shape is unrecognized.
fn parse_findmnt_summary(text: &str) -> Option<FindmntSummary> {
    text.lines().find_map(parse_findmnt_summary_line)
}

/// Parses a single line as `findmnt --verify`'s summary line, returning
/// `None` if it is not one. Factored out of [`parse_findmnt_summary`] (which
/// is just this applied across every line of the output) so
/// [`parse_findmnt_detail`] can use the identical test when classifying a
/// line, rather than keeping a second copy of the summary-line shape that
/// could drift from this one.
fn parse_findmnt_summary_line(line: &str) -> Option<FindmntSummary> {
    let fields: Vec<&str> = line.trim().split(", ").collect();
    let [parse_field, error_field, warning_field] = fields.as_slice() else {
        return None;
    };
    Some(FindmntSummary {
        parse_errors: parse_count_field(parse_field, "parse error", "parse errors")?,
        errors: parse_count_field(error_field, "error", "errors")?,
        warnings: parse_count_field(warning_field, "warning", "warnings")?,
    })
}

/// Parses one comma-separated field of a `findmnt --verify` summary line
/// (e.g. `"1 parse error"` or `"2 errors"`) into its leading count, provided
/// the remainder of the field matches `singular` or `plural` exactly
/// (whichever one `findmnt` chose does not need to agree with the count
/// itself - e.g. real `findmnt` prints `"0 parse errors"`, plural despite
/// the zero count - this function only checks the wording is one of the two
/// known forms).
fn parse_count_field(field: &str, singular: &str, plural: &str) -> Option<u32> {
    let field = field.trim();
    let (count_str, label) = field.split_once(char::is_whitespace)?;
    let count: u32 = count_str.parse().ok()?;
    if label.trim() == singular || label.trim() == plural {
        Some(count)
    } else {
        None
    }
}

/// Monotonically increasing counter used to keep temp-file names produced by
/// concurrent calls (e.g. parallel test threads within the same process)
/// from colliding, since [`std::process::id`] alone is constant for the
/// whole process lifetime.
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes `rendered` (the candidate document's canonical serialized text) to
/// a fresh file under [`std::env::temp_dir`] and returns its path.
///
/// This is only used for the read-only `findmnt --verify` check, which does
/// not need same-filesystem rename atomicity - unlike the final write in a
/// future atomic-writer task (T6), which must use a temp file in the same
/// directory as the real target for `rename(2)` to be atomic. Verification
/// never touches the real target file at all.
fn write_candidate_to_temp_file(rendered: &str) -> io::Result<PathBuf> {
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "rustab-findmnt-verify-{}-{counter}.tmp",
        std::process::id()
    ));
    fs::write(&path, rendered)?;
    Ok(path)
}

/// Runs `findmnt --verify --tab-file <temp path>` against a fresh temp-file
/// rendering of `rendered`, and returns the outcome as a [`FindmntRunResult`].
///
/// Returns `Err` if the temp file could not be written, or if the `findmnt`
/// binary could not be launched at all (not installed / not on `$PATH`, or
/// any other `std::process::Command::output` failure) - both cases are
/// treated identically by [`interpret_findmnt_result`] as "verification
/// unavailable", since this function has no way to run the real check
/// either way. The temp file is removed afterwards on a best-effort basis
/// (its absence or removal failure is not itself reported as an error, since
/// it is a throwaway file under the OS temp directory, not a correctness
/// concern).
fn run_findmnt_verify(rendered: &str) -> io::Result<FindmntRunResult> {
    let temp_path = write_candidate_to_temp_file(rendered)?;

    let output_result = Command::new("findmnt")
        .arg("--verify")
        .arg("--tab-file")
        .arg(&temp_path)
        .output();

    let _ = fs::remove_file(&temp_path);

    let output = output_result?;
    let mut combined_output = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.stderr.is_empty() {
        if !combined_output.is_empty() {
            combined_output.push('\n');
        }
        combined_output.push_str(&String::from_utf8_lossy(&output.stderr));
    }

    Ok(FindmntRunResult {
        success: output.status.success(),
        combined_output,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FstabEntry, Options, SourceId};

    /// Builds a document with a single entry, for tests that only care
    /// about one structural property.
    fn doc_with_entry(entry: FstabEntry) -> Document {
        Document::from_lines(vec![Line::Entry(entry)])
    }

    // --- Structural: fs_type ---

    #[test]
    fn empty_fs_type_is_an_error() {
        let entry = FstabEntry::new(
            SourceId::Path("/dev/sda1".into()),
            "/mnt",
            "",
            Options::new(),
            0,
            0,
        );
        let findings = empty_fs_type_findings(&doc_with_entry(entry));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
    }

    #[test]
    fn non_empty_fs_type_is_fine() {
        let entry = FstabEntry::new(
            SourceId::Path("/dev/sda1".into()),
            "/mnt",
            "ext4",
            Options::new(),
            0,
            0,
        );
        assert!(empty_fs_type_findings(&doc_with_entry(entry)).is_empty());
    }

    #[test]
    fn empty_fs_type_finding_is_tagged_with_its_field_and_real_line_index() {
        // The offending entry is preceded by a comment and a blank line, so
        // a correct `line_index` of 2 (not "the first entry", which would
        // coincidentally also be the case if this test only had one
        // preceding line) proves the index is really `doc.lines`' index,
        // not just "the first/only entry" by accident.
        let doc = Document::from_lines(vec![
            Line::Comment(" a comment".to_string()),
            Line::Blank,
            Line::Entry(FstabEntry::new(
                SourceId::Path("/dev/sda1".into()),
                "/mnt",
                "",
                Options::new(),
                0,
                0,
            )),
        ]);
        let findings = empty_fs_type_findings(&doc);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].field, Some(FindingField::FsType));
        assert_eq!(findings[0].line_index, Some(2));
    }

    // --- Structural: duplicate mount points ---

    #[test]
    fn duplicate_mount_point_is_an_error() {
        let doc = Document::from_lines(vec![
            Line::Entry(FstabEntry::new(
                SourceId::Uuid("11111111-2222-3333-4444-555555555555".into()),
                "/data",
                "ext4",
                Options::new(),
                0,
                2,
            )),
            Line::Entry(FstabEntry::new(
                SourceId::Uuid("22222222-3333-4444-5555-666666666666".into()),
                "/data",
                "ext4",
                Options::new(),
                0,
                2,
            )),
        ]);
        let findings = duplicate_mount_point_findings(&doc);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].message.contains("/data"));
        // Regression guard: this finding spans two lines, so it must never
        // be tagged as if it were about a single field/line.
        assert_eq!(findings[0].field, None);
        assert_eq!(findings[0].line_index, None);
    }

    #[test]
    fn repeated_none_mount_point_for_swap_entries_is_not_a_false_positive() {
        let doc = Document::from_lines(vec![
            Line::Entry(FstabEntry::new(
                SourceId::Path("/dev/sda5".into()),
                "none",
                "swap",
                Options::new(),
                0,
                0,
            )),
            Line::Entry(FstabEntry::new(
                SourceId::Path("/dev/sdb5".into()),
                "none",
                "swap",
                Options::new(),
                0,
                0,
            )),
        ]);
        assert!(duplicate_mount_point_findings(&doc).is_empty());
    }

    #[test]
    fn unique_mount_points_produce_no_findings() {
        let doc = Document::from_lines(vec![
            Line::Entry(FstabEntry::new(
                SourceId::Path("/dev/sda1".into()),
                "/",
                "ext4",
                Options::new(),
                0,
                1,
            )),
            Line::Entry(FstabEntry::new(
                SourceId::Path("/dev/sda2".into()),
                "/home",
                "ext4",
                Options::new(),
                0,
                2,
            )),
        ]);
        assert!(duplicate_mount_point_findings(&doc).is_empty());
    }

    // --- Structural: malformed UUID shape ---

    #[test]
    fn well_formed_uuid_and_partuuid_are_fine() {
        let doc = Document::from_lines(vec![
            Line::Entry(FstabEntry::new(
                SourceId::Uuid("11111111-2222-3333-4444-555555555555".into()),
                "/",
                "ext4",
                Options::new(),
                0,
                1,
            )),
            Line::Entry(FstabEntry::new(
                SourceId::PartUuid("AAAAAAAA-bbbb-CCCC-dddd-EEEEEEEEEEEE".into()),
                "/boot",
                "vfat",
                Options::new(),
                0,
                2,
            )),
        ]);
        assert!(malformed_uuid_findings(&doc).is_empty());
    }

    #[test]
    fn malformed_uuid_shape_is_an_error() {
        let doc = doc_with_entry(FstabEntry::new(
            SourceId::Uuid("not-a-real-uuid".into()),
            "/",
            "ext4",
            Options::new(),
            0,
            1,
        ));
        let findings = malformed_uuid_findings(&doc);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].message.contains("UUID="));
    }

    #[test]
    fn malformed_uuid_finding_is_tagged_with_its_field_and_real_line_index() {
        let doc = Document::from_lines(vec![
            Line::Comment(" a comment".to_string()),
            Line::Blank,
            Line::Entry(FstabEntry::new(
                SourceId::Uuid("not-a-real-uuid".into()),
                "/",
                "ext4",
                Options::new(),
                0,
                1,
            )),
        ]);
        let findings = malformed_uuid_findings(&doc);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].field, Some(FindingField::Source));
        assert_eq!(findings[0].line_index, Some(2));
    }

    #[test]
    fn malformed_partuuid_shape_is_an_error() {
        let doc = doc_with_entry(FstabEntry::new(
            SourceId::PartUuid("1234".into()),
            "/boot",
            "vfat",
            Options::new(),
            0,
            2,
        ));
        let findings = malformed_uuid_findings(&doc);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("PARTUUID="));
    }

    #[test]
    fn looks_like_uuid_rejects_wrong_length_and_bad_hyphens() {
        assert!(looks_like_uuid("11111111-2222-3333-4444-555555555555"));
        assert!(!looks_like_uuid("11111111222233334444555555555555")); // no hyphens
        assert!(!looks_like_uuid("11111111-2222-3333-4444-5555555555")); // too short
        assert!(!looks_like_uuid("gggggggg-2222-3333-4444-555555555555")); // non-hex
    }

    #[test]
    fn fat_volume_id_shaped_uuid_is_not_malformed() {
        // Real `blkid` output for a FAT/exFAT filesystem (e.g. the ESP) -
        // a genuine, common real-world fstab value, not a typo.
        let doc = doc_with_entry(FstabEntry::new(
            SourceId::Uuid("CAF7-75BB".into()),
            "/boot",
            "vfat",
            Options::new(),
            0,
            2,
        ));
        assert!(malformed_uuid_findings(&doc).is_empty());
    }

    #[test]
    fn fat_volume_id_shape_is_case_insensitive_and_rejects_near_misses() {
        assert!(looks_like_fat_volume_id("CAF7-75BB"));
        assert!(looks_like_fat_volume_id("caf7-75bb"));
        assert!(!looks_like_fat_volume_id("CAF7_75BB")); // wrong separator
        assert!(!looks_like_fat_volume_id("CAF75-75BB")); // wrong length (5-4)
        assert!(!looks_like_fat_volume_id("CAF7-75B")); // wrong length (4-3)
        assert!(!looks_like_fat_volume_id("CAFG-75BB")); // non-hex
        assert!(!looks_like_fat_volume_id(
            "11111111-2222-3333-4444-555555555555"
        )); // the other (full) shape, not this one
    }

    #[test]
    fn fat_volume_id_shape_is_not_accepted_for_partuuid() {
        // PARTUUID is always a GPT/MBR partition-table GUID, never a
        // filesystem volume ID - the short FAT shape must still be
        // rejected here even though it is fine for UUID=.
        let doc = doc_with_entry(FstabEntry::new(
            SourceId::PartUuid("CAF7-75BB".into()),
            "/boot",
            "vfat",
            Options::new(),
            0,
            2,
        ));
        let findings = malformed_uuid_findings(&doc);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("PARTUUID="));
    }

    // --- Structural: malformed lines surfaced as warnings ---

    #[test]
    fn malformed_line_is_a_non_blocking_warning() {
        let doc = Document::from_lines(vec![Line::Malformed("/dev/sdb1".to_string())]);
        let findings = malformed_line_findings(&doc);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
        // A malformed line is not a parsed entry, so it must never be
        // tagged with a `FindingField` - but it is still exactly one
        // identifiable line, so `line_index` must point at it (regression
        // guard: leaving this `None` made the finding read as
        // "document-level" to the GUI's per-drawer scoping and surfaced it
        // inside every entry's add/edit drawer, not just this line's own).
        assert_eq!(findings[0].field, None);
        assert_eq!(findings[0].line_index, Some(0));
    }

    // --- Structural: mount point existence (advisory) ---

    #[test]
    fn nonexistent_mount_point_is_a_warning_not_an_error() {
        let doc = doc_with_entry(FstabEntry::new(
            SourceId::Uuid("11111111-2222-3333-4444-555555555555".into()),
            "/this/path/almost-certainly/does/not/exist/on/any/machine",
            "ext4",
            Options::new(),
            0,
            2,
        ));
        let findings = mount_point_missing_findings(&doc);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
    }

    #[test]
    fn existing_mount_point_produces_no_finding() {
        let doc = doc_with_entry(FstabEntry::new(
            SourceId::Path("/dev/sda1".into()),
            "/", // always exists
            "ext4",
            Options::new(),
            0,
            1,
        ));
        assert!(mount_point_missing_findings(&doc).is_empty());
    }

    /// Regression test: a mount point containing an octal-escaped space
    /// (`\040`) must be decoded before the existence check, or a path that
    /// genuinely exists on disk is wrongly reported as missing because the
    /// literal text `\040` never matches a real space in a filesystem path.
    #[test]
    fn escaped_space_in_an_existing_mount_point_produces_no_finding() {
        let dir = std::env::temp_dir().join(format!(
            "rustab-fstab-core-escaped-space-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(dir.join("My Backup")).expect("create the test directory");
        let mount_point = format!("{}/My\\040Backup", dir.display());

        let doc = doc_with_entry(FstabEntry::new(
            SourceId::Path("/dev/sda1".into()),
            &mount_point,
            "ext4",
            Options::new(),
            0,
            2,
        ));
        let findings = mount_point_missing_findings(&doc);

        fs::remove_dir_all(&dir).expect("clean up the test directory");
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn none_mount_point_is_exempt_from_existence_check() {
        let doc = doc_with_entry(FstabEntry::new(
            SourceId::Path("/dev/sda5".into()),
            "none",
            "swap",
            Options::new(),
            0,
            0,
        ));
        assert!(mount_point_missing_findings(&doc).is_empty());
    }

    #[test]
    fn mount_point_missing_finding_is_tagged_with_its_field_and_real_line_index() {
        let doc = Document::from_lines(vec![
            Line::Comment(" a comment".to_string()),
            Line::Blank,
            Line::Entry(FstabEntry::new(
                SourceId::Uuid("11111111-2222-3333-4444-555555555555".into()),
                "/this/path/almost-certainly/does/not/exist/on/any/machine",
                "ext4",
                Options::new(),
                0,
                2,
            )),
        ]);
        let findings = mount_point_missing_findings(&doc);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].field, Some(FindingField::MountPoint));
        assert_eq!(findings[0].line_index, Some(2));
    }

    /// The exact scenario T21.3 of `PROJECT_PLAN.md` exists to get right: a
    /// document with two entries that both have a missing mount point must
    /// produce two independently-tagged `MountPoint` findings, each pointing
    /// at its own entry's real `line_index` - not, say, both pointing at the
    /// same index, or the two findings being indistinguishable from each
    /// other by field alone.
    #[test]
    fn two_missing_mount_points_produce_two_findings_with_distinct_line_indexes() {
        let doc = Document::from_lines(vec![
            Line::Entry(FstabEntry::new(
                SourceId::Uuid("11111111-2222-3333-4444-555555555555".into()),
                "/this/path/almost-certainly/does/not/exist/on/any/machine/one",
                "ext4",
                Options::new(),
                0,
                2,
            )),
            Line::Comment(" separating comment".to_string()),
            Line::Entry(FstabEntry::new(
                SourceId::Uuid("22222222-3333-4444-5555-666666666666".into()),
                "/this/path/almost-certainly/does/not/exist/on/any/machine/two",
                "ext4",
                Options::new(),
                0,
                2,
            )),
        ]);
        let findings = mount_point_missing_findings(&doc);
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].field, Some(FindingField::MountPoint));
        assert_eq!(findings[1].field, Some(FindingField::MountPoint));
        assert_eq!(findings[0].line_index, Some(0));
        assert_eq!(findings[1].line_index, Some(2));
        assert_ne!(findings[0].line_index, findings[1].line_index);
    }

    // --- Structural: a description is free-form text, never a finding ---

    /// T43 retired the "a description must be a single line" check: a
    /// description is now parsed from, and written back as, however many
    /// comment lines it needs, so an embedded `\n` is the expected shape of a
    /// multi-line note rather than an error. Nothing about a description's
    /// *text* is structurally invalid any more, which this pins: a
    /// multi-line description must produce no structural finding at all.
    #[test]
    fn a_multi_line_description_produces_no_structural_finding() {
        let doc = Document::from_lines(vec![Line::Entry(
            FstabEntry::new(
                SourceId::Path("/dev/sda1".into()),
                "/mnt/data",
                "ext4",
                Options::new(),
                0,
                2,
            )
            .with_description("photo archive\nonly mounted for backups"),
        )]);

        assert!(
            structural_findings(&doc)
                .iter()
                .all(|finding| finding.field != Some(FindingField::Description)),
            "no structural check may be about a description any more: {:?}",
            structural_findings(&doc)
        );
    }

    // --- Structural: trailing_comment embedded newline ---

    #[test]
    fn trailing_comment_containing_a_newline_is_an_error() {
        let doc = doc_with_entry(
            FstabEntry::new(
                SourceId::Path("/dev/sda1".into()),
                "/mnt/data",
                "ext4",
                Options::new(),
                0,
                2,
            )
            .with_trailing_comment("first line\nsecond line"),
        );
        let findings = trailing_comment_contains_newline_findings(&doc);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
        assert_eq!(findings[0].field, Some(FindingField::TrailingComment));
        assert_eq!(findings[0].line_index, Some(0));
    }

    #[test]
    fn trailing_comment_containing_a_carriage_return_is_an_error() {
        let doc = doc_with_entry(
            FstabEntry::new(
                SourceId::Path("/dev/sda1".into()),
                "/mnt/data",
                "ext4",
                Options::new(),
                0,
                2,
            )
            .with_trailing_comment("first line\rsecond line"),
        );
        let findings = trailing_comment_contains_newline_findings(&doc);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
        assert_eq!(findings[0].field, Some(FindingField::TrailingComment));
        assert_eq!(findings[0].line_index, Some(0));
    }

    #[test]
    fn single_line_trailing_comment_produces_no_finding() {
        let doc = doc_with_entry(
            FstabEntry::new(
                SourceId::Path("/dev/sda1".into()),
                "/mnt/data",
                "ext4",
                Options::new(),
                0,
                2,
            )
            .with_trailing_comment("encrypted swap, see notes"),
        );
        assert!(trailing_comment_contains_newline_findings(&doc).is_empty());
    }

    #[test]
    fn absent_trailing_comment_produces_no_finding() {
        let doc = doc_with_entry(FstabEntry::new(
            SourceId::Path("/dev/sda1".into()),
            "/mnt/data",
            "ext4",
            Options::new(),
            0,
            2,
        ));
        assert!(trailing_comment_contains_newline_findings(&doc).is_empty());
    }

    // --- Semantic: pure decision logic (findmnt I/O-vs-logic split) ---

    #[test]
    fn findmnt_not_found_is_degraded_mode_warning_not_error() {
        let result: io::Result<FindmntRunResult> = Err(io::Error::from(io::ErrorKind::NotFound));
        let findings = interpret_findmnt_result(&Document::from_lines(vec![]), &result);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert!(findings[0].message.contains("degraded"));
    }

    #[test]
    fn findmnt_other_launch_failure_is_also_degraded_mode_warning() {
        let result: io::Result<FindmntRunResult> =
            Err(io::Error::other("simulated launch failure"));
        let findings = interpret_findmnt_result(&Document::from_lines(vec![]), &result);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert!(findings[0].message.contains("simulated launch failure"));
    }

    #[test]
    fn findmnt_success_produces_no_findings() {
        let result: io::Result<FindmntRunResult> = Ok(FindmntRunResult {
            success: true,
            combined_output: String::new(),
        });
        assert!(interpret_findmnt_result(&Document::from_lines(vec![]), &result).is_empty());
    }

    #[test]
    fn findmnt_failure_is_an_error_including_its_output_text() {
        let result: io::Result<FindmntRunResult> = Ok(FindmntRunResult {
            success: false,
            combined_output: "/data\n   [E] unreachable on boot required source: UUID=dead"
                .to_string(),
        });
        let findings = interpret_findmnt_result(&Document::from_lines(vec![]), &result);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0]
            .message
            .contains("unreachable on boot required source"));
    }

    // --- Semantic: parse-error (N) vs real-error (M) summary-line split (T5b) ---

    /// Real `findmnt` (util-linux 2.42.2) output, captured by hand to confirm
    /// the exact summary-line shape before writing the parser against it -
    /// see this module's "T5b" fix. `findmnt --verify --tab-file t.fstab`
    /// against a two-line file (one unparseable line, one entry with
    /// `UUID=deadbeef-...` and `defaults`, i.e. auto-mounted and
    /// unreachable, a real error):
    /// ```text
    /// findmnt: t.fstab: parse error at line 1 -- ignored
    ///
    /// 1 parse error, 2 errors, 0 warnings
    /// /mnt/unreachable
    ///    [E] unreachable on boot required target: No such file or directory
    ///    [E] unreachable on boot required source: UUID=deadbeef-0000-0000-0000-000000000000
    /// ```
    /// exit code 1. The same file but with `noauto` instead of `defaults`
    /// (so the unreachable source is merely a warning, not a real error)
    /// produces `"1 parse error, 0 errors, 2 warnings"`, still exit code 1.
    #[test]
    fn parse_findmnt_summary_handles_real_findmnt_output_shapes() {
        assert_eq!(
            parse_findmnt_summary("1 parse error, 2 errors, 0 warnings"),
            Some(FindmntSummary {
                parse_errors: 1,
                errors: 2,
                warnings: 0
            })
        );
        assert_eq!(
            parse_findmnt_summary("1 parse error, 0 errors, 2 warnings"),
            Some(FindmntSummary {
                parse_errors: 1,
                errors: 0,
                warnings: 2
            })
        );
        // Real findmnt uses the plural "errors"/"warnings" wording even for
        // a zero count (e.g. "0 parse errors", not "0 parse error").
        assert_eq!(
            parse_findmnt_summary("0 parse errors, 0 errors, 2 warnings"),
            Some(FindmntSummary {
                parse_errors: 0,
                errors: 0,
                warnings: 2
            })
        );
        assert_eq!(
            parse_findmnt_summary("0 parse errors, 1 error, 0 warnings"),
            Some(FindmntSummary {
                parse_errors: 0,
                errors: 1,
                warnings: 0
            })
        );
        // The summary line is typically preceded by `parse error ... --
        // ignored` lines and/or a blank line, and followed by the detail
        // block; the parser must find it regardless of surrounding text.
        let full_output = "findmnt: t.fstab: parse error at line 1 -- ignored\n\n\
                            1 parse error, 2 errors, 0 warnings\n\
                            /mnt/unreachable\n\
                            \u{20}\u{20}\u{20}[E] unreachable on boot required target: \
                            No such file or directory";
        assert_eq!(
            parse_findmnt_summary(full_output),
            Some(FindmntSummary {
                parse_errors: 1,
                errors: 2,
                warnings: 0
            })
        );
    }

    #[test]
    fn parse_findmnt_summary_returns_none_for_unrecognized_shapes() {
        assert_eq!(parse_findmnt_summary(""), None);
        assert_eq!(
            parse_findmnt_summary("/data\n   [E] unreachable on boot required source: UUID=dead"),
            None
        );
        assert_eq!(
            parse_findmnt_summary("findmnt: some future wording we did not anticipate"),
            None
        );
    }

    #[test]
    fn findmnt_failure_with_only_parse_errors_is_a_warning_not_an_error() {
        let result: io::Result<FindmntRunResult> = Ok(FindmntRunResult {
            success: false,
            combined_output: "findmnt: t.fstab: parse error at line 1 -- ignored\n\n\
                              1 parse error, 0 errors, 2 warnings\n\
                              /mnt/unreachable\n\
                              \u{20}\u{20}\u{20}[W] unreachable target: No such file or \
                              directory"
                .to_string(),
        });
        let findings = interpret_findmnt_result(&Document::from_lines(vec![]), &result);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert!(findings[0].message.contains("parse error"));
    }

    #[test]
    fn findmnt_failure_with_real_errors_alongside_parse_errors_is_still_an_error() {
        let result: io::Result<FindmntRunResult> = Ok(FindmntRunResult {
            success: false,
            combined_output: "findmnt: t.fstab: parse error at line 1 -- ignored\n\n\
                              1 parse error, 2 errors, 0 warnings\n\
                              /mnt/unreachable\n\
                              \u{20}\u{20}\u{20}[E] unreachable on boot required source: \
                              UUID=deadbeef-0000-0000-0000-000000000000"
                .to_string(),
        });
        let findings = interpret_findmnt_result(&Document::from_lines(vec![]), &result);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0]
            .message
            .contains("unreachable on boot required source"));
    }

    // --- Semantic: per-line decomposition of findmnt's detail output ---

    /// Real `findmnt --verify` output for a document with one unparseable
    /// line, one `noauto` entry with a nonexistent UUID (two `[W]`s), and
    /// one auto-mounted entry with a nonexistent UUID (two `[E]`s), in the
    /// order [`run_findmnt_verify`] assembles it: stdout (the detail
    /// blocks), a blank separator, then stderr (the parse-error line, a
    /// blank line, and the summary line). Captured by hand against
    /// util-linux 2.42.2, with stdout/stderr redirected to separate files to
    /// confirm which stream each part comes from.
    fn real_two_block_findmnt_output() -> String {
        "/mnt/data\n\
         \u{20}\u{20}\u{20}[W] unreachable target: No such file or directory\n\
         /mnt/unreachable\n\
         \u{20}\u{20}\u{20}[E] unreachable on boot required target: No such file or directory\n\
         \u{20}\u{20}\u{20}[E] unreachable on boot required source: \
         UUID=deadbeef-0000-0000-0000-000000000000\n\
         \n\
         findmnt: /tmp/rustab-findmnt-verify-1-0.tmp: parse error at line 1 -- ignored\n\
         \n\
         1 parse error, 2 errors, 0 warnings\n"
            .to_string()
    }

    /// A document matching [`real_two_block_findmnt_output`]: the two
    /// entries it names, deliberately *not* at indices 0 and 1 (a malformed
    /// line precedes them and a blank line separates them) so a correct
    /// `line_index` cannot pass by coincidence.
    fn doc_for_real_two_block_output() -> Document {
        Document::from_lines(vec![
            Line::Malformed("this line is garbage and cannot be parsed".to_string()),
            Line::Entry(FstabEntry::new(
                SourceId::Uuid("11111111-2222-3333-4444-555555555555".into()),
                "/mnt/data",
                "ext4",
                Options::from_vec(vec![crate::model::FstabOption::Flag("noauto".into())]),
                0,
                2,
            )),
            Line::Blank,
            Line::Entry(FstabEntry::new(
                SourceId::Uuid("deadbeef-0000-0000-0000-000000000000".into()),
                "/mnt/unreachable",
                "ext4",
                Options::from_vec(vec![crate::model::FstabOption::Flag("defaults".into())]),
                0,
                2,
            )),
        ])
    }

    #[test]
    fn findmnt_detail_output_is_parsed_into_per_mount_point_diagnostics() {
        let (detail, had_unrecognized_content) =
            parse_findmnt_detail(&real_two_block_findmnt_output());
        assert!(
            !had_unrecognized_content,
            "every line of real findmnt output must be explained, got: {detail:?}"
        );
        assert_eq!(
            detail,
            vec![
                FindmntDetailLine {
                    mount_point: "/mnt/data".to_string(),
                    severity: Severity::Warning,
                    message: "unreachable target: No such file or directory".to_string(),
                },
                FindmntDetailLine {
                    mount_point: "/mnt/unreachable".to_string(),
                    severity: Severity::Error,
                    message: "unreachable on boot required target: No such file or directory"
                        .to_string(),
                },
                FindmntDetailLine {
                    mount_point: "/mnt/unreachable".to_string(),
                    severity: Severity::Error,
                    message: "unreachable on boot required source: \
                              UUID=deadbeef-0000-0000-0000-000000000000"
                        .to_string(),
                },
            ]
        );
    }

    /// (a) The whole point of this change: each of findmnt's diagnostics
    /// lands on the one entry it is about, with its own `[E]`/`[W]`
    /// severity - and no unscoped whole-output dump is produced alongside
    /// them (which is what made every entry's drawer show the same blob).
    #[test]
    fn findmnt_failure_decomposes_into_per_line_findings_with_no_document_level_dump() {
        let result: io::Result<FindmntRunResult> = Ok(FindmntRunResult {
            success: false,
            combined_output: real_two_block_findmnt_output(),
        });

        let findings = interpret_findmnt_result(&doc_for_real_two_block_output(), &result);

        assert_eq!(findings.len(), 3, "got: {findings:?}");

        assert_eq!(findings[0].severity, Severity::Warning);
        assert_eq!(findings[0].line_index, Some(1));
        assert_eq!(
            findings[0].message,
            "findmnt --verify: unreachable target: No such file or directory"
        );

        assert_eq!(findings[1].severity, Severity::Error);
        assert_eq!(findings[1].line_index, Some(3));
        assert_eq!(
            findings[1].message,
            "findmnt --verify: unreachable on boot required target: No such file or directory"
        );

        assert_eq!(findings[2].severity, Severity::Error);
        assert_eq!(findings[2].line_index, Some(3));
        assert_eq!(
            findings[2].message,
            "findmnt --verify: unreachable on boot required source: \
             UUID=deadbeef-0000-0000-0000-000000000000"
        );

        // Same shape as the malformed-line case: one identifiable line, but
        // no single field of it to blame.
        assert!(findings.iter().all(|f| f.field.is_none()));
        // The regression this change is about: nothing may be left
        // untagged, or the GUI reads it as document-level and shows it in
        // every entry's drawer regardless of which line is open.
        assert!(
            findings.iter().all(|f| f.line_index.is_some()),
            "no leftover document-level finding is allowed on the clean path, got: {findings:?}"
        );
        // And findmnt's raw text must not also be dumped wholesale.
        assert!(findings
            .iter()
            .all(|f| !f.message.contains("parse error") && !f.message.contains("[E]")));
    }

    /// (b) Fallback safety, ambiguous match: several swap entries
    /// legitimately share the mount point `none`, so a diagnostic naming
    /// `none` has no single line to blame. That must produce today's
    /// unchanged single whole-output finding, not a guess.
    #[test]
    fn findmnt_detail_naming_an_ambiguous_mount_point_falls_back_to_the_whole_text_finding() {
        let doc = Document::from_lines(vec![
            Line::Entry(FstabEntry::new(
                SourceId::Path("/dev/sda5".into()),
                "none",
                "swap",
                Options::from_vec(vec![crate::model::FstabOption::Flag("sw".into())]),
                0,
                0,
            )),
            Line::Entry(FstabEntry::new(
                SourceId::Path("/dev/sdb5".into()),
                "none",
                "swap",
                Options::from_vec(vec![crate::model::FstabOption::Flag("sw".into())]),
                0,
                0,
            )),
        ]);
        let combined_output = "none\n\
                               \u{20}\u{20}\u{20}[E] unreachable on boot required source: \
                               /dev/sdb5\n\
                               \n\
                               0 parse errors, 1 error, 0 warnings\n";
        let result: io::Result<FindmntRunResult> = Ok(FindmntRunResult {
            success: false,
            combined_output: combined_output.to_string(),
        });

        // Precondition: the output itself is perfectly recognizable - this
        // test is about the *resolution* step refusing, not the parser.
        let (detail, had_unrecognized_content) = parse_findmnt_detail(combined_output);
        assert!(!had_unrecognized_content);
        assert_eq!(detail.len(), 1);
        assert_eq!(findmnt_detail_line_findings(&doc, &detail), None);

        let findings = interpret_findmnt_result(&doc, &result);
        assert_eq!(findings.len(), 1, "got: {findings:?}");
        assert_eq!(findings[0].severity, Severity::Error);
        assert_eq!(findings[0].field, None);
        assert_eq!(findings[0].line_index, None);
        assert!(findings[0]
            .message
            .contains("findmnt --verify reported a problem with this fstab"));
        assert!(findings[0]
            .message
            .contains("unreachable on boot required source"));
    }

    /// (b, second half) Fallback safety, zero matches: a mount point that
    /// names no entry in the document at all means this code and findmnt
    /// disagree about what was verified, which must also fall back rather
    /// than drop the diagnostic.
    #[test]
    fn findmnt_detail_naming_an_unknown_mount_point_falls_back_to_the_whole_text_finding() {
        let doc = doc_with_entry(FstabEntry::new(
            SourceId::Path("/dev/sda1".into()),
            "/mnt/data",
            "ext4",
            Options::new(),
            0,
            2,
        ));
        let combined_output = "/mnt/somewhere-else\n\
                               \u{20}\u{20}\u{20}[W] unreachable target: No such file or \
                               directory\n\
                               \n\
                               0 parse errors, 0 errors, 1 warning\n";
        let result: io::Result<FindmntRunResult> = Ok(FindmntRunResult {
            success: false,
            combined_output: combined_output.to_string(),
        });

        let (detail, had_unrecognized_content) = parse_findmnt_detail(combined_output);
        assert!(!had_unrecognized_content);
        assert_eq!(findmnt_detail_line_findings(&doc, &detail), None);

        let findings = interpret_findmnt_result(&doc, &result);
        assert_eq!(findings.len(), 1, "got: {findings:?}");
        assert_eq!(findings[0].line_index, None);
        // `errors == 0`, so the unchanged fallback logic still calls this a
        // non-blocking warning rather than inventing a new verdict.
        assert_eq!(findings[0].severity, Severity::Warning);
        assert!(findings[0].message.contains("/mnt/somewhere-else"));
    }

    /// (c) Fallback safety, unrecognized shape: a line this parser cannot
    /// explain (here, a hypothetical future findmnt message) must flip the
    /// whole result back to the full-text fallback, so the unexplained text
    /// still reaches the user instead of being silently dropped - even
    /// though the rest of the output was perfectly attributable.
    #[test]
    fn unrecognized_findmnt_output_falls_back_without_dropping_anything() {
        let combined_output = "/mnt/data\n\
                               \u{20}\u{20}\u{20}[W] unreachable target: No such file or \
                               directory\n\
                               findmnt: some future wording nobody anticipated\n\
                               \n\
                               0 parse errors, 1 error, 0 warnings\n";
        let doc = doc_with_entry(FstabEntry::new(
            SourceId::Path("/dev/sda1".into()),
            "/mnt/data",
            "ext4",
            Options::new(),
            0,
            2,
        ));
        let result: io::Result<FindmntRunResult> = Ok(FindmntRunResult {
            success: false,
            combined_output: combined_output.to_string(),
        });

        let (detail, had_unrecognized_content) = parse_findmnt_detail(combined_output);
        assert!(
            had_unrecognized_content,
            "an unexplained line must flip the flag, got: {detail:?}"
        );

        let findings = interpret_findmnt_result(&doc, &result);
        assert_eq!(findings.len(), 1, "got: {findings:?}");
        assert_eq!(findings[0].severity, Severity::Error);
        assert_eq!(findings[0].line_index, None);
        // Both the attributable diagnostic *and* the unexplained line are
        // still in front of the user.
        assert!(findings[0].message.contains("unreachable target"));
        assert!(findings[0]
            .message
            .contains("some future wording nobody anticipated"));
    }

    #[test]
    fn findmnt_detail_parser_rejects_structurally_surprising_output() {
        // A mount-point header with no diagnostics under it.
        let (detail, unrecognized) = parse_findmnt_detail("/mnt/data\n/mnt/other\n   [E] boom\n");
        assert!(unrecognized, "got: {detail:?}");

        // A diagnostic line with no header above it to attribute it to.
        let (detail, unrecognized) = parse_findmnt_detail("   [E] boom with nothing above it\n");
        assert!(unrecognized, "got: {detail:?}");
        assert!(detail.is_empty());

        // An unknown marker letter where a diagnostic was expected.
        let (detail, unrecognized) = parse_findmnt_detail("/mnt/data\n   [I] some new severity\n");
        assert!(unrecognized, "got: {detail:?}");

        // Indentation other than the confirmed three spaces.
        let (detail, unrecognized) = parse_findmnt_detail("/mnt/data\n\t[E] tab-indented\n");
        assert!(unrecognized, "got: {detail:?}");

        // A trailing header left open at end of input.
        let (detail, unrecognized) = parse_findmnt_detail("/mnt/data\n   [E] boom\n/mnt/other\n");
        assert!(unrecognized, "got: {detail:?}");
    }

    #[test]
    fn findmnt_parse_error_line_is_recognized_only_to_be_discarded() {
        assert!(is_findmnt_parse_error_line(
            "findmnt: /tmp/rustab-findmnt-verify-1-0.tmp: parse error at line 3 -- ignored"
        ));
        assert!(!is_findmnt_parse_error_line(
            "findmnt: some other message entirely"
        ));
        assert!(!is_findmnt_parse_error_line("/mnt/data"));

        // Discarded, not surfaced: its information is already covered by
        // `malformed_line_findings`, which has the real line_index.
        let (detail, unrecognized) = parse_findmnt_detail(
            "findmnt: /tmp/x.tmp: parse error at line 1 -- ignored\n\n\
             1 parse error, 0 errors, 0 warnings\n",
        );
        assert!(!unrecognized);
        assert!(detail.is_empty());
    }

    #[test]
    fn mount_point_header_recognition_rejects_prose_lines() {
        assert!(looks_like_findmnt_mount_point_header("/mnt/data"));
        // An escaped-whitespace mount point is still a single token, since
        // the model stores and renders it raw.
        assert!(looks_like_findmnt_mount_point_header("/mnt/my\\040disk"));
        assert!(looks_like_findmnt_mount_point_header("none"));
        assert!(!looks_like_findmnt_mount_point_header(""));
        assert!(!looks_like_findmnt_mount_point_header("   [E] indented"));
        assert!(!looks_like_findmnt_mount_point_header(
            "1 parse error, 2 errors, 0 warnings"
        ));
        assert!(!looks_like_findmnt_mount_point_header(
            "findmnt: some future wording"
        ));
    }

    // --- Top-level `validate()` ---

    #[test]
    fn validate_reports_no_errors_for_a_valid_document() {
        // `noauto` keeps `findmnt --verify` from treating these fake,
        // nonexistent UUIDs as boot-required-but-unreachable *errors* (it
        // downgrades that to warnings for non-auto-mounted entries) - see
        // the module docs for why the semantic check is exercised against
        // real `findmnt` here rather than only via `interpret_findmnt_result`.
        // This keeps the test meaningful (it exercises the real `findmnt`
        // binary when available) while remaining correct regardless of
        // whether `findmnt` happens to be installed on the machine running
        // the test: unavailable-findmnt degrades to a warning, not an error.
        let doc = Document::from_lines(vec![
            Line::Comment(" synthetic valid fstab for validator tests".to_string()),
            Line::Entry(FstabEntry::new(
                SourceId::Uuid("11111111-2222-3333-4444-555555555555".into()),
                "/mnt/data",
                "ext4",
                Options::from_vec(vec![crate::model::FstabOption::Flag("noauto".into())]),
                0,
                2,
            )),
            Line::Entry(FstabEntry::new(
                SourceId::Path("/dev/sda5".into()),
                "none",
                "swap",
                Options::from_vec(vec![crate::model::FstabOption::Flag("sw".into())]),
                0,
                0,
            )),
        ]);

        let report = validate(&doc);
        assert!(
            report.is_ok(),
            "expected no blocking errors, got: {:?}",
            report.errors().collect::<Vec<_>>()
        );
        assert!(!report.has_errors());
    }

    #[test]
    fn validate_reports_duplicate_mount_point_error() {
        let doc = Document::from_lines(vec![
            Line::Entry(FstabEntry::new(
                SourceId::Uuid("11111111-2222-3333-4444-555555555555".into()),
                "/data",
                "ext4",
                Options::from_vec(vec![crate::model::FstabOption::Flag("noauto".into())]),
                0,
                2,
            )),
            Line::Entry(FstabEntry::new(
                SourceId::Uuid("22222222-3333-4444-5555-666666666666".into()),
                "/data",
                "ext4",
                Options::from_vec(vec![crate::model::FstabOption::Flag("noauto".into())]),
                0,
                2,
            )),
        ]);

        let report = validate(&doc);
        assert!(report.has_errors());
        assert!(report
            .errors()
            .any(|f| f.message.contains("/data") && f.message.contains("unique")));
    }

    #[test]
    fn validate_reports_malformed_uuid_error() {
        let doc = doc_with_entry(FstabEntry::new(
            SourceId::Uuid("totally-not-a-uuid".into()),
            "/mnt/data",
            "ext4",
            Options::from_vec(vec![crate::model::FstabOption::Flag("noauto".into())]),
            0,
            2,
        ));

        let report = validate(&doc);
        assert!(report.has_errors());
        assert!(report.errors().any(|f| f.message.contains("UUID=")));
    }

    #[test]
    fn validate_reports_missing_mount_point_as_warning_only() {
        let doc = doc_with_entry(FstabEntry::new(
            SourceId::Uuid("11111111-2222-3333-4444-555555555555".into()),
            "/this/path/almost-certainly/does/not/exist/on/any/machine",
            "ext4",
            Options::from_vec(vec![crate::model::FstabOption::Flag("noauto".into())]),
            0,
            2,
        ));

        let report = validate(&doc);
        assert!(
            !report.has_errors(),
            "a missing mount point must not block a save"
        );
        assert!(report
            .warnings()
            .any(|f| f.message.contains("does not currently exist on disk")));
    }

    /// `true` if `findmnt` is actually runnable on this machine. Mirrors the
    /// identically-named helper in `writer.rs`'s test module (not reused
    /// across modules since both are private `#[cfg(test)]` items).
    fn is_findmnt_available() -> bool {
        Command::new("findmnt")
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success())
    }

    /// Regression test for T5b: a document with one [`Line::Malformed`] line
    /// *and* an otherwise fully-resolvable entry that `findmnt --verify`
    /// would, at most, warn about (not error on) - `noauto` on a
    /// nonexistent-but-well-formed UUID, the same pattern `writer.rs`'s own
    /// tests use to avoid tripping a real `findmnt` error. Before the T5b
    /// fix, `findmnt --verify`'s non-zero exit (caused solely by the
    /// malformed line being an unparseable `N > 0`, not a real `M > 0`
    /// error) was mapped unconditionally to a blocking [`Severity::Error`],
    /// making this document permanently unsavable - contradicting
    /// `docs/SPEC.md` §5's requirement that a malformed line be tolerated as
    /// a visible warning, not treated as fatal.
    #[test]
    fn malformed_line_alongside_an_otherwise_valid_entry_is_not_a_blocking_error() {
        let doc = Document::from_lines(vec![
            Line::Malformed("this line is garbage and cannot be parsed".to_string()),
            Line::Entry(FstabEntry::new(
                SourceId::Uuid("11111111-2222-3333-4444-555555555555".into()),
                "/mnt/data",
                "ext4",
                Options::from_vec(vec![crate::model::FstabOption::Flag("noauto".into())]),
                0,
                2,
            )),
        ]);

        let report = validate(&doc);
        assert!(
            !report.has_errors(),
            "a malformed line must not permanently block saving the rest of the document, \
             got errors: {:?}",
            report.errors().collect::<Vec<_>>()
        );
        // The malformed line itself is still surfaced, just as a warning.
        assert!(report
            .warnings()
            .any(|f| f.message.contains("could not be understood")));
        if is_findmnt_available() {
            // Confirm the fix is actually being exercised against real
            // findmnt here, not just passing because findmnt happens to be
            // unavailable (which would degrade to a warning regardless).
            //
            // This originally looked for findmnt's own "parse error"
            // wording in a document-level warning. Per-line decomposition
            // deliberately removed that: real findmnt reports this document
            // as `1 parse error, 0 errors, 2 warnings` with both warnings
            // attributable to the one entry, so the result is now two
            // per-line warnings and *no* whole-output dump - and findmnt's
            // parse-error line is dropped as pure duplication of the
            // malformed-line warning asserted just above, which already
            // carries that line's own correct `line_index`. The equivalent
            // (in fact stricter) evidence that real findmnt ran is
            // therefore its per-entry verdict, tagged to the entry's line.
            assert!(
                report.warnings().any(|f| f.line_index == Some(1)
                    && f.field.is_none()
                    && f.message.contains("findmnt --verify: unreachable")),
                "expected findmnt's own per-entry warning, tagged to the entry's line, when \
                 findmnt is available, got: {:?}",
                report.findings
            );
        } else {
            eprintln!(
                "note: findmnt is not available on this machine; this test still passed via \
                 the degraded-mode warning path, not the parse-error/real-error split"
            );
        }
    }

    /// Companion regression test: the malformed line alone must not mask a
    /// *real* semantic error findmnt finds elsewhere in the same document
    /// (`M > 0`) - that must still be a blocking [`Severity::Error`],
    /// confirming T5b only changes the `M == 0` case. Skipped when `findmnt`
    /// is unavailable, since without it this check degrades to a
    /// non-blocking warning regardless (covered by this module's other
    /// degraded-mode tests).
    #[test]
    fn malformed_line_does_not_mask_a_real_semantic_error_elsewhere() {
        if !is_findmnt_available() {
            eprintln!("skipping: findmnt is not available on this machine");
            return;
        }

        let doc = Document::from_lines(vec![
            Line::Malformed("this line is garbage and cannot be parsed".to_string()),
            // Auto-mounted (no `noauto`) with a UUID no device has: `findmnt
            // --verify` reports this as a real error, same pattern as
            // `writer.rs`'s `semantic_validation_failure_leaves_target_untouched`.
            Line::Entry(FstabEntry::new(
                SourceId::Uuid("deadbeef-0000-0000-0000-000000000000".into()),
                "/mnt/unreachable",
                "ext4",
                Options::from_vec(vec![crate::model::FstabOption::Flag("defaults".into())]),
                0,
                2,
            )),
        ]);

        let report = validate(&doc);
        assert!(
            report.has_errors(),
            "a real semantic error must still block saving even alongside a malformed line, \
             report: {:?}",
            report.findings
        );
    }
}
