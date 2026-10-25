//! Structural and semantic validation of candidate fstab documents - see `docs/SPEC.md` §5, §6.
//!
//! Implements the two validation steps from `docs/SPEC.md` §6 ("Atomicity"):
//!
//! 1. **Structural validation** (`structural_findings`): cheap, in-memory
//!    checks of a [`Document`]'s entries - non-empty `fs_type`, duplicate
//!    mount points, malformed-looking `UUID=`/`PARTUUID=` values, plus
//!    advisory checks (mount point missing on disk, visible
//!    [`Line::Malformed`] lines).
//! 2. **Semantic validation** (`semantic_findings`): equivalent to
//!    `findmnt --verify --tab-file <path>`, run against a temporary
//!    rendering of the candidate - never the real target file.
//!
//! [`validate`] runs both and returns a [`ValidationReport`], separating
//! blocking [`Severity::Error`] findings from advisory [`Severity::Warning`]
//! ones. Callers should check [`ValidationReport::has_errors`] (or
//! [`ValidationReport::is_ok`]) before writing to disk.
//!
//! `validate` takes no `target_path`: every check is self-contained within
//! the candidate [`Document`] or a throwaway temp file - none need to know
//! the eventual destination.
//!
//! ## `findmnt` I/O-vs-pure-logic separation
//!
//! Like `discovery.rs`'s `lsblk` split, the semantic check is divided into
//! `run_findmnt_verify` (the actual I/O: render the candidate and run
//! `findmnt --verify --tab-file` against those bytes through a
//! [`crate::exec::CommandRunner`]) and `interpret_findmnt_result` (a pure
//! function from `io::Result<FindmntRunResult>` to `Finding`s, which unit
//! tests exercise directly, including the "findmnt unavailable" case via a
//! synthetic `io::Error`).
//!
//! ## Which process answers the system's questions
//!
//! Two checks here are about the machine being edited rather than about the
//! document: the `findmnt --verify` one, and the advisory "mount point
//! exists on disk" one. Both go through the same
//! [`crate::exec::CommandRunner`] seam, so a process containerized away
//! from that machine (a Flatpak sandbox, whose `/` is the runtime's) can
//! route them to it. The existence check asks about every mount point in
//! one call, not one call per entry.
//!
//! [`validate`] runs `findmnt` in *this* process, via
//! [`crate::exec::DirectRunner`], which stages the rendered candidate in a
//! throwaway file under [`std::env::temp_dir`] and points `findmnt` at
//! that. That is the right answer for a CLI, for the elevated helper, and
//! for the tests - but not for a process that is containerized away from
//! the system it is editing, where `findmnt` has to run elsewhere to see
//! the real `/dev` and mount table. [`validate_with`] therefore takes the
//! runner, and [`validate`] is just that with [`crate::exec::DirectRunner`]
//! supplied; see [`crate::exec`] for why the seam is shaped the way it is.
//!
//! Per SPEC §6 step 2, a failure to even run `findmnt` (missing binary or
//! other I/O error) is surfaced as a non-blocking [`Severity::Warning`]
//! saying verification ran in degraded mode - never silent, never a hard
//! error, since an environment without `findmnt` should still be able to
//! save a structurally valid fstab. An unanswerable *existence* question is
//! treated differently - it emits nothing at all, see
//! [`mount_point_missing_findings`].

use crate::exec::{CommandRunner, DirectRunner};
use crate::model::{Document, Line, SourceId};
use std::collections::HashMap;
use std::io;

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

/// Which of an entry's fields a [`Finding`] is about, when it's about
/// exactly one. A domain concept, not a UI one - a GUI maps each variant to
/// whichever widget shows that field.
///
/// [`Finding::field`] being `None` doesn't mean "unknown"; it means the
/// finding isn't about exactly one field - it spans multiple lines (e.g. a
/// shared mount point), isn't about a parsed entry (e.g.
/// [`Line::Malformed`]), or names no field of an otherwise identifiable
/// line (a `findmnt --verify` diagnostic - see [`findmnt_detail_line_findings`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingField {
    /// The entry's mount point (second fstab field).
    MountPoint,
    /// The entry's source device/identifier (first fstab field).
    Source,
    /// The entry's filesystem type (third fstab field).
    FsType,
    /// The entry's human-readable description (see
    /// [`crate::model::FstabEntry::description`]), not one of the six fstab
    /// fields.
    ///
    /// No current check produces this variant, but it's kept because it's
    /// part of a wire-locked enum (spelled `"description"`, see
    /// `docs/API_CONTRACT.md` §7) - a future description-specific check
    /// should reuse it rather than add a new one.
    Description,
    /// The entry's trailing inline comment, appended after the six fields
    /// on the same line (see [`crate::model::FstabEntry::trailing_comment`]).
    /// Distinct from [`Self::Description`], a comment block placed *above*
    /// the entry. Produced by [`trailing_comment_contains_newline_findings`].
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
    /// Which single field of an entry this finding is about, if any - see
    /// [`FindingField`] for what `None` means here.
    pub field: Option<FindingField>,
    /// The 0-based index into the validated [`Document`]'s `lines`, if this
    /// finding is about a single line. Matches `LineDto`'s identity
    /// convention in the DTO crates, so a caller can look the line back up.
    ///
    /// Usually set together with [`Self::field`] (both or neither), with
    /// two exceptions that set `line_index` alone: a [`Line::Malformed`]
    /// line (identifiable, but never had fields to localize to), and a
    /// `findmnt --verify` diagnostic attributed to one entry (identifies
    /// the line via its mount point, but not which field is at fault). So
    /// `line_index: Some, field: None` means "about this whole line", not
    /// an invariant violation.
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

    /// Constructs an advisory [`Severity::Warning`] localized to one field
    /// of one line.
    fn warning_at(message: impl Into<String>, field: FindingField, line_index: usize) -> Self {
        Finding {
            severity: Severity::Warning,
            message: message.into(),
            field: Some(field),
            line_index: Some(line_index),
        }
    }

    /// Constructs an advisory [`Severity::Warning`] localized to a line but
    /// no specific field (see [`Self::line_index`]).
    fn warning_at_line(message: impl Into<String>, line_index: usize) -> Self {
        Finding {
            severity: Severity::Warning,
            message: message.into(),
            field: None,
            line_index: Some(line_index),
        }
    }

    /// The [`Severity::Error`] counterpart of [`Self::warning_at_line`],
    /// used by [`findmnt_detail_line_findings`] for `findmnt --verify`
    /// `[E]` diagnostics attributed to one entry but no specific field.
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
/// from structural and semantic validation (structural first, then semantic).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationReport {
    /// All findings from this validation run, both blocking and advisory.
    pub findings: Vec<Finding>,
}

impl ValidationReport {
    /// `true` if any finding is a blocking [`Severity::Error`] - callers
    /// must abort the save before touching disk when this is `true`.
    pub fn has_errors(&self) -> bool {
        self.findings.iter().any(|f| f.severity == Severity::Error)
    }

    /// `true` if there are no blocking errors (there may still be warnings).
    pub fn is_ok(&self) -> bool {
        !self.has_errors()
    }

    /// Iterator over just the blocking [`Severity::Error`] findings.
    pub fn errors(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Error)
    }

    /// Iterator over just the advisory [`Severity::Warning`] findings.
    pub fn warnings(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Warning)
    }
}

/// Validates a candidate fstab [`Document`]: structural checks (see module
/// docs) plus a semantic check equivalent to `findmnt --verify --tab-file
/// <path>` against a temporary rendering of `doc`.
///
/// Never touches the real target file - `doc` is staged in a throwaway
/// temp file under [`std::env::temp_dir`] and `findmnt` points at that.
/// Never panics; if `findmnt` can't be run at all, that's an advisory
/// [`Severity::Warning`], not a silent pass.
///
/// Runs `findmnt` in this process. A caller that must run it somewhere
/// else (see the module docs) calls [`validate_with`] instead.
pub fn validate(doc: &Document) -> ValidationReport {
    validate_with(doc, &DirectRunner)
}

/// [`validate`], with the process that runs `findmnt --verify` chosen by
/// the caller.
///
/// Identical in every other respect, including which findings block a
/// save: `runner` decides only *where* `findmnt` runs and where the
/// rendered candidate is staged for it, never what its output means.
pub fn validate_with(doc: &Document, runner: &dyn CommandRunner) -> ValidationReport {
    let mut findings = structural_findings(doc, runner);
    findings.extend(semantic_findings(doc, runner));
    ValidationReport { findings }
}

// ---------------------------------------------------------------------------
// Structural validation
// ---------------------------------------------------------------------------

/// Runs all structural checks against `doc` and returns every [`Finding`]
/// produced. Pure and I/O-free except for the "mount point exists on disk"
/// advisory check, which asks `runner` about every mount point at once.
fn structural_findings(doc: &Document, runner: &dyn CommandRunner) -> Vec<Finding> {
    let mut findings = Vec::new();

    findings.extend(malformed_line_findings(doc));
    findings.extend(empty_fs_type_findings(doc));
    findings.extend(duplicate_mount_point_findings(doc));
    findings.extend(malformed_uuid_findings(doc));
    findings.extend(mount_point_missing_findings(doc, runner));
    findings.extend(trailing_comment_contains_newline_findings(doc));

    findings
}

/// Surfaces any [`Line::Malformed`] lines in `doc` as advisory warnings.
/// Non-blocking: a malformed line the parser couldn't understand isn't by
/// itself a reason to refuse saving the rest of the file (it round-trips
/// verbatim either way).
///
/// Tagged with [`Finding::line_index`] so the GUI's per-entry drawer can
/// scope the finding to the one line it's about, rather than surfacing it
/// in every entry's drawer regardless of which is open (see
/// `docs/UI_DESIGN.md` §5). No [`FindingField`] applies, since a malformed
/// line was never a parsed entry.
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
/// Unreachable via normal parsing (`parser.rs` requires 4+ fields, and
/// `str::split_whitespace` can't yield an empty token), but checked anyway
/// as a second line of defense against entries built directly via the
/// `fstab-core` API, bypassing the parser.
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
/// Can't happen from real parsing (same reasoning as
/// [`empty_fs_type_findings`]); this guards against an `EntryDto` built
/// directly via the Tauri IPC boundary. Such a value reaching the writer
/// would split one physical line into two, corrupting the file.
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
/// except the literal `none` mount point (see [`SWAP_NONE_MOUNT_POINT`]),
/// which swap entries legitimately share.
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

/// Flags [`SourceId::Uuid`] and [`SourceId::PartUuid`] values that don't
/// look like a canonical UUID (8-4-4-4-12 hex-with-hyphens, e.g.
/// `11111111-2222-3333-4444-555555555555`) as a blocking error.
///
/// Deliberately lenient: only checks shape (hyphen positions, hex digits,
/// case-insensitive), not that the UUID identifies a real device or has
/// valid version/variant bits (RFC 9562 §4-5) - fstab itself doesn't
/// require that, and being stricter risks rejecting valid-but-uncommon
/// real values.
fn malformed_uuid_findings(doc: &Document) -> Vec<Finding> {
    doc.lines
        .iter()
        .enumerate()
        .filter_map(|(idx, line)| match line {
            Line::Entry(entry) => Some((idx, entry)),
            _ => None,
        })
        .filter_map(|(idx, entry)| match &entry.source {
            // `UUID=` also accepts the shorter FAT/exFAT volume-serial shape
            // (e.g. `CAF7-75BB`), common for EFI system partitions.
            // `PARTUUID=` (a GPT/MBR GUID, never a volume ID) gets no such
            // allowance.
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

/// Returns `true` if `value` has the canonical UUID shape: 32 hex digits
/// grouped `8-4-4-4-12` with hyphens at exactly those positions, case-insensitively.
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

/// Returns `true` if `value` is a FAT/exFAT volume serial number: 4 hex
/// digits, a hyphen, 4 more hex digits (e.g. `CAF7-75BB`),
/// case-insensitively. FAT-family filesystems have no 128-bit UUID, only
/// this 32-bit volume ID, so it's a valid alternate `UUID=` shape.
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
/// A [`Severity::Warning`] per `docs/SPEC.md` §9, since a mount point may
/// legitimately not exist yet (e.g. provisioning before first boot).
///
/// Skipped for the literal `none` mount point. Decoded via
/// [`crate::model::decode_escaped_whitespace`] before the existence check,
/// since the raw (possibly `\040`-escaped) text would never match a real
/// on-disk path - but the finding itself reports the raw text, as stored.
///
/// ## Why this goes through the runner
///
/// "Does this path exist?" has to be asked of the machine whose fstab is
/// being edited, and a process inside a Flatpak sandbox is not it: the
/// `/` it sees is the runtime's, so `/boot`, `/opt` and every
/// `/run/media/…` would be reported missing even though the host has them
/// all. So the question goes to `runner`, which is the host's when there is
/// one - in a single batched call, since an out-of-process runner pays per
/// call and a document has one mount point per entry (see
/// [`crate::exec::CommandRunner::paths_exist`]).
fn mount_point_missing_findings(doc: &Document, runner: &dyn CommandRunner) -> Vec<Finding> {
    let candidates: Vec<(usize, &str, String)> = doc
        .lines
        .iter()
        .enumerate()
        .filter_map(|(idx, line)| match line {
            Line::Entry(entry) => Some((idx, entry)),
            _ => None,
        })
        .filter(|(_, entry)| entry.mount_point != SWAP_NONE_MOUNT_POINT)
        .map(|(idx, entry)| {
            (
                idx,
                entry.mount_point.as_str(),
                crate::model::decode_escaped_whitespace(&entry.mount_point),
            )
        })
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }

    // Asked once each, however many entries share a mount point (a
    // duplicate is its own blocking error, but this check still runs).
    let mut queried: Vec<&str> = Vec::with_capacity(candidates.len());
    for (_, _, decoded) in &candidates {
        if !queried.contains(&decoded.as_str()) {
            queried.push(decoded.as_str());
        }
    }

    // An advisory check that could not be answered emits nothing: inventing
    // "does not exist" warnings because the host was unreachable would
    // flood the UI with claims about a filesystem nobody looked at. A reply
    // of the wrong length is treated the same way - it is not an answer.
    let Ok(answers) = runner.paths_exist(&queried) else {
        return Vec::new();
    };
    if answers.len() != queried.len() {
        return Vec::new();
    }

    candidates
        .iter()
        .filter(|(_, _, decoded)| {
            queried
                .iter()
                .position(|path| *path == decoded.as_str())
                .is_some_and(|at| !answers[at])
        })
        .map(|(idx, raw, _)| {
            Finding::warning_at(
                format!("mount point {raw:?} does not currently exist on disk"),
                FindingField::MountPoint,
                *idx,
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
/// synthesizable struct (rather than raw [`std::process::Output`]) so
/// [`interpret_findmnt_result`] can be unit-tested with hand-built values.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FindmntRunResult {
    /// `true` if `findmnt --verify` exited with status 0.
    success: bool,
    /// `findmnt`'s stdout followed by stderr, concatenated so nothing is
    /// dropped even though `--verify` documents stdout as the usual place
    /// for problems.
    combined_output: String,
}

/// Runs only the semantic (`findmnt --verify`) half of validation -
/// structural checks happen separately - and returns its [`Finding`]s.
fn semantic_findings(doc: &Document, runner: &dyn CommandRunner) -> Vec<Finding> {
    let rendered = doc.to_string();
    let result = run_findmnt_verify(&rendered, runner);
    interpret_findmnt_result(doc, &result)
}

/// Pure decision logic over the result of [`run_findmnt_verify`] - no
/// process invocation or I/O, so it's directly unit-testable (including
/// the "findmnt unavailable" path, via a synthetic `io::Error`).
///
/// - `Err(_)` (binary couldn't be launched) → a [`Severity::Warning`]
///   saying verification ran in degraded mode, never silently a pass.
/// - `Ok(run)` with `run.success == true` → no findings.
/// - `Ok(run)` with `run.success == false` → preferred path first:
///   [`parse_findmnt_detail`] decomposes the output into one diagnostic
///   per mount point, and [`findmnt_detail_line_findings`] tries to
///   attribute each to exactly one entry. When both succeed, the result is
///   one per-line [`Finding`] per diagnostic (so a GUI can put each
///   complaint on its own row/drawer), and no whole-output dump.
///
///   If anything is unrecognized, or a mount point is ambiguous, the whole
///   result falls back - all-or-nothing - to a summary-line check:
///   `findmnt --verify` prints `"<N> parse error(s), <M> error(s), <K>
///   warning(s)"` and exits non-zero whenever `N > 0` *or* `M > 0`,
///   conflating "a line couldn't be parsed" (`N`, non-fatal to findmnt
///   itself) with "a real mount/boot problem" (`M`).
///   [`parse_findmnt_summary`] separates them:
///     - Summary parses, `M == 0` → non-blocking [`Severity::Warning`]
///       regardless of `N` or the non-zero exit code.
///     - Summary parses, `M > 0` → blocking [`Severity::Error`], with
///       findmnt's own output included.
///     - Summary can't be found/parsed → blocking [`Severity::Error`] as a
///       conservative fallback, never silently a pass.
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
            // Preferred path: decompose into per-entry findings. Both halves
            // must agree, or the whole result falls through to the summary
            // check below rather than reporting a partial attribution.
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
/// with the mount point of the block it appeared under (see
/// [`parse_findmnt_detail`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct FindmntDetailLine {
    /// The mount-point text from the block header, verbatim as `findmnt`
    /// printed it (which matches the candidate document's own text).
    mount_point: String,
    /// [`Severity::Error`] for `[E]`, [`Severity::Warning`] for `[W]` -
    /// taken from this line's own marker, not the summary's counts.
    severity: Severity,
    /// The diagnostic text after the marker.
    message: String,
}

/// The exact indentation `findmnt --verify` puts before each `[E]`/`[W]`
/// line (confirmed against util-linux 2.42.2). Matched exactly - a
/// different indentation means an output shape this parser hasn't seen.
const FINDMNT_DETAIL_INDENT: &str = "   ";

/// The indentation a long `[E]`/`[W]` diagnostic wraps its continuation
/// line to - aligned under the message text, i.e.
/// [`FINDMNT_DETAIL_INDENT`] plus `"[W] "`'s width (`"[E] "` is the same
/// width, so one constant covers both). Confirmed against real output: a
/// system where `/etc/fstab` differs from what systemd has loaded gets an
/// extra warning long enough to wrap, `"your fstab has been modified, but
/// systemd still uses the old version;\n       use 'systemctl
/// daemon-reload' to reload"`.
const FINDMNT_DETAIL_CONTINUATION_INDENT: &str = "       ";

/// Decomposes `findmnt --verify`'s combined stdout+stderr into one
/// [`FindmntDetailLine`] per `[E]`/`[W]` diagnostic, plus a
/// `had_unrecognized_content` flag for anything that didn't fit the
/// expected shape.
///
/// Diagnostics are grouped by mount point: an unindented header line, then
/// one or more [`FINDMNT_DETAIL_INDENT`] + `[E] `/`[W] ` + message lines.
/// Real output (util-linux 2.42.2):
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
/// Discarded rather than surfaced: blank lines (which also close the
/// current block); the summary line (its counts just aggregate the
/// per-line diagnostics); and `findmnt: <path>: parse error at line N --
/// ignored` (duplicates [`malformed_line_findings`], and its path is the
/// throwaway temp file anyway).
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

        // A wrapped continuation of the diagnostic line just above, not a
        // new one of its own - folded onto it rather than treated as
        // unrecognized content (see FINDMNT_DETAIL_CONTINUATION_INDENT).
        if let Some((_, count)) = open_block.as_ref() {
            if *count > 0 {
                if let Some(rest) = line.strip_prefix(FINDMNT_DETAIL_CONTINUATION_INDENT) {
                    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
                        if let Some(last) = detail_lines.last_mut() {
                            last.message.push(' ');
                            last.message.push_str(rest);
                            continue;
                        }
                    }
                }
            }
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
/// its severity and message. Requires exactly [`FINDMNT_DETAIL_INDENT`]
/// followed by `"[E] "` or `"[W] "`; anything else is unrecognized.
fn parse_findmnt_detail_message_line(line: &str) -> Option<(Severity, &str)> {
    let rest = line.strip_prefix(FINDMNT_DETAIL_INDENT)?;
    if let Some(message) = rest.strip_prefix("[E] ") {
        return Some((Severity::Error, message));
    }
    rest.strip_prefix("[W] ")
        .map(|message| (Severity::Warning, message))
}

/// Returns `true` for `findmnt`'s own `findmnt: <path>: parse error at line
/// N -- ignored` stderr line, recognized only to discard it (see
/// [`parse_findmnt_detail`]).
fn is_findmnt_parse_error_line(line: &str) -> bool {
    let line = line.trim_end();
    line.starts_with("findmnt: ")
        && line.contains(": parse error at line ")
        && line.ends_with(" -- ignored")
}

/// Returns `true` if `line` could be a `findmnt --verify` block header -
/// an fstab mount-point text on its own line.
///
/// A mount point is always one whitespace-free token (a space is carried
/// as `\040`, never a literal space), so "no whitespace at all" matches
/// every real header while rejecting findmnt's prose lines.
fn looks_like_findmnt_mount_point_header(line: &str) -> bool {
    !line.is_empty() && !line.chars().any(char::is_whitespace)
}

/// Resolves [`FindmntDetailLine`]s against `doc`, returning one per-line
/// [`Finding`] per diagnostic, or `None` if any one can't be attributed to
/// exactly one entry.
///
/// Matching is plain string equality against `mount_point` as stored
/// (raw) - safe because `findmnt` was given this same document rendered
/// by its own serializer.
///
/// `None` is all-or-nothing: a mount point matching zero entries means
/// this function and `findmnt` disagree, and one matching two or more
/// (e.g. shared `none`) has no single line to blame. Guessing either way
/// risks a wrong attribution, worse than the unscoped fallback.
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

        // Prefixed so the reader knows this is findmnt's verdict, not rustab's.
        let message = format!("findmnt --verify: {}", detail.message);
        findings.push(match detail.severity {
            Severity::Error => Finding::error_at_line(message, line_index),
            Severity::Warning => Finding::warning_at_line(message, line_index),
        });
    }

    Some(findings)
}

/// The three counts `findmnt --verify` reports in its summary line (see
/// [`parse_findmnt_summary`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FindmntSummary {
    /// `N`: unparseable lines (non-fatal to findmnt itself).
    parse_errors: u32,
    /// `M`: real semantic/mount/boot-reachability problems.
    errors: u32,
    /// `K`: advisory warnings.
    #[allow(dead_code)] // part of the parsed shape; not currently read by any caller.
    warnings: u32,
}

/// Parses `findmnt --verify`'s summary line from combined stdout+stderr,
/// tolerating singular/plural wording for each count (`"1 error"` vs.
/// `"2 errors"`) and "parse error(s)" vs. plain "error(s)".
///
/// Confirmed against real output (util-linux 2.42.2), e.g.
/// `"1 parse error, 2 errors, 0 warnings"`.
///
/// Returns `None` if no line matches the expected shape (e.g. a future
/// findmnt changes the wording) - callers must treat `None` conservatively
/// (today's "non-zero exit = Error"), never as a free pass.
fn parse_findmnt_summary(text: &str) -> Option<FindmntSummary> {
    text.lines().find_map(parse_findmnt_summary_line)
}

/// Parses a single line as `findmnt --verify`'s summary line, or `None`.
/// Factored out of [`parse_findmnt_summary`] so [`parse_findmnt_detail`]
/// can reuse the identical test rather than keep a second copy.
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

/// Runs `findmnt --verify --tab-file <staged path>` against a throwaway
/// file holding `rendered`, through `runner`.
///
/// `runner` stages the bytes and appends the resulting path as the final
/// argument (see [`CommandRunner::run`]), because `findmnt --verify`
/// accepts only a seekable `--tab-file` - there is no stdin mode to use
/// instead. Verification never touches the real target file.
///
/// Returns `Err` if the candidate couldn't be staged or `findmnt` couldn't
/// be launched - both treated identically by [`interpret_findmnt_result`]
/// as "verification unavailable". A `findmnt` that ran and rejected the
/// file is `Ok` with `success: false`.
fn run_findmnt_verify(rendered: &str, runner: &dyn CommandRunner) -> io::Result<FindmntRunResult> {
    let output = runner.run(
        "findmnt",
        &["--verify", "--tab-file"],
        Some(rendered.as_bytes()),
    )?;

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
    use std::fs;
    use std::process::Command;

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
        let findings = mount_point_missing_findings(&doc, &DirectRunner);
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
        assert!(mount_point_missing_findings(&doc, &DirectRunner).is_empty());
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
        let findings = mount_point_missing_findings(&doc, &DirectRunner);

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
        assert!(mount_point_missing_findings(&doc, &DirectRunner).is_empty());
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
        let findings = mount_point_missing_findings(&doc, &DirectRunner);
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
        let findings = mount_point_missing_findings(&doc, &DirectRunner);
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
            structural_findings(&doc, &DirectRunner)
                .iter()
                .all(|finding| finding.field != Some(FindingField::Description)),
            "no structural check may be about a description any more: {:?}",
            structural_findings(&doc, &DirectRunner)
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

    // --- Semantic: parse-error (N) vs real-error (M) summary-line split ---

    /// Real `findmnt --verify` output (util-linux 2.42.2) against a two-line
    /// file (one unparseable line, one auto-mounted entry with a dead
    /// `UUID=`, a real error):
    /// ```text
    /// findmnt: t.fstab: parse error at line 1 -- ignored
    ///
    /// 1 parse error, 2 errors, 0 warnings
    /// /mnt/unreachable
    ///    [E] unreachable on boot required target: No such file or directory
    ///    [E] unreachable on boot required source: UUID=deadbeef-0000-0000-0000-000000000000
    /// ```
    /// With `noauto` instead (merely a warning), the summary line is
    /// `"1 parse error, 0 errors, 2 warnings"` instead.
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

    /// Real `findmnt --verify` output (util-linux 2.42.2) for a document
    /// with one unparseable line, a `noauto` entry with a dead UUID
    /// (`[W]`), and an auto-mounted entry with a dead UUID (`[E]`), in
    /// [`run_findmnt_verify`]'s assembly order: stdout, blank separator,
    /// then stderr.
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

    /// A document matching [`real_two_block_findmnt_output`], with the two
    /// entries deliberately not at indices 0/1 so a correct `line_index`
    /// can't pass by coincidence.
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
    fn findmnt_detail_parser_folds_a_wrapped_diagnostic_onto_the_line_above() {
        // T41: real findmnt output - its systemd-reload notice is long
        // enough to wrap onto a continuation line.
        let (detail, unrecognized) = parse_findmnt_detail(
            "/mnt/data\n   [W] your fstab has been modified, but systemd still uses the old \
             version;\n       use 'systemctl daemon-reload' to reload\n",
        );
        assert!(!unrecognized, "got: {detail:?}");
        assert_eq!(detail.len(), 1);
        assert_eq!(
            detail[0].message,
            "your fstab has been modified, but systemd still uses the old version; \
             use 'systemctl daemon-reload' to reload"
        );

        // A continuation-shaped line with no diagnostic above it to fold
        // onto is still unrecognized, not silently dropped.
        let (detail, unrecognized) =
            parse_findmnt_detail("/mnt/data\n       stray continuation, nothing above it\n");
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
        // `noauto` keeps findmnt from treating these fake UUIDs as errors
        // (downgraded to warnings). Correct either way findmnt is installed:
        // unavailable findmnt degrades to a warning, not an error.
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

    /// `true` if `findmnt` is actually runnable on this machine. Mirrors
    /// `writer.rs`'s identically-named test helper (not shared, since both
    /// are private `#[cfg(test)]` items).
    fn is_findmnt_available() -> bool {
        Command::new("findmnt")
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success())
    }

    /// A [`Line::Malformed`] line alongside an otherwise valid entry
    /// (`noauto` on a nonexistent-but-well-formed UUID) must not be a
    /// blocking error - the malformed line's unparseable `N > 0` must not
    /// get conflated with a real `M > 0` semantic error.
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
            // Real findmnt reports this as `1 parse error, 0 errors, 2
            // warnings`; per-line decomposition turns that into a per-entry
            // warning tagged to the entry's line, with no whole-output dump.
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

    /// A malformed line must not mask a real semantic error (`M > 0`)
    /// elsewhere in the document - that's still a blocking error. Skipped
    /// without `findmnt` (degrades to a warning, covered elsewhere).
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

    // --- The command-runner seam (see `crate::exec`) ---

    /// The exact command the semantic check is built from, and the exact
    /// bytes it verifies: the document's own rendering, handed over as
    /// input so the runner can stage it wherever `findmnt` will run.
    #[test]
    fn the_semantic_check_runs_findmnt_verify_against_the_rendered_document() {
        let doc = doc_with_entry(FstabEntry::new(
            SourceId::Uuid("11111111-2222-3333-4444-555555555555".into()),
            "/mnt/data",
            "ext4",
            Options::new(),
            0,
            2,
        ));
        let runner = crate::exec::RecordingRunner::answering(0, "");

        let report = validate_with(&doc, &runner);

        let runs = runner.runs();
        assert_eq!(runs.len(), 1, "exactly one command per validation");
        assert_eq!(runs[0].program, "findmnt");
        assert_eq!(runs[0].args, vec!["--verify", "--tab-file"]);
        assert_eq!(runs[0].input.as_deref(), Some(doc.to_string().as_bytes()));
        // A `findmnt` that exited 0 contributes no findings of its own.
        assert!(
            !report.has_errors(),
            "report: {:?}",
            report.errors().collect::<Vec<_>>()
        );
    }

    /// `findmnt`'s verdict is read off the runner's output, not off
    /// whatever the real `findmnt` on this machine would have said.
    #[test]
    fn a_failing_findmnt_from_the_runner_blocks_the_save() {
        let doc = doc_with_entry(FstabEntry::new(
            SourceId::Uuid("11111111-2222-3333-4444-555555555555".into()),
            "/mnt/data",
            "ext4",
            Options::new(),
            0,
            2,
        ));
        let runner = crate::exec::RecordingRunner::answering(
            1,
            "/mnt/data\n   [E] unreachable on boot required source: UUID=…\n\n\
             0 parse errors, 1 error, 0 warnings\n",
        );

        let report = validate_with(&doc, &runner);

        assert!(report.has_errors());
        assert!(report
            .errors()
            .any(|finding| finding.message.contains("findmnt --verify: unreachable")));
    }

    /// A three-entry document with `none` among the mount points, so the
    /// batching and the exemption are proven together: one question, asked
    /// about exactly the decoded mount points that are not exempt.
    #[test]
    fn the_existence_check_asks_one_batched_question_for_the_whole_document() {
        let doc = Document::from_lines(vec![
            Line::Entry(FstabEntry::new(
                SourceId::Uuid("11111111-2222-3333-4444-555555555555".into()),
                "/boot",
                "vfat",
                Options::new(),
                0,
                2,
            )),
            Line::Comment(" a comment between entries".to_string()),
            Line::Entry(FstabEntry::new(
                SourceId::Path("/dev/sda5".into()),
                "none",
                "swap",
                Options::new(),
                0,
                0,
            )),
            Line::Entry(FstabEntry::new(
                SourceId::Path("/dev/sdb1".into()),
                "/mnt/My\\040Disk",
                "ext4",
                Options::new(),
                0,
                2,
            )),
            Line::Entry(FstabEntry::new(
                SourceId::Path("/dev/sdc1".into()),
                "/opt",
                "ext4",
                Options::new(),
                0,
                2,
            )),
        ]);
        let runner = crate::exec::RecordingRunner::answering(0, "");

        validate_with(&doc, &runner);

        let queries = runner.exists_queries();
        assert_eq!(
            queries.len(),
            1,
            "exactly one existence question per validation"
        );
        assert_eq!(queries[0], vec!["/boot", "/mnt/My Disk", "/opt"]);
    }

    /// The runner's verdict is what decides, not this machine's filesystem:
    /// `/boot` is reported missing even where it exists, and the path that
    /// exists nowhere real produces no finding because the runner says it
    /// is there.
    #[test]
    fn the_existence_findings_follow_the_runners_answers() {
        let doc = Document::from_lines(vec![
            Line::Entry(FstabEntry::new(
                SourceId::Path("/dev/sda1".into()),
                "/boot",
                "vfat",
                Options::new(),
                0,
                2,
            )),
            Line::Entry(FstabEntry::new(
                SourceId::Path("/dev/sdb1".into()),
                "/this/path/almost-certainly/does/not/exist/on/any/machine",
                "ext4",
                Options::new(),
                0,
                2,
            )),
        ]);
        let runner = crate::exec::RecordingRunner::answering(0, "")
            .with_existing_paths(&["/this/path/almost-certainly/does/not/exist/on/any/machine"]);

        let report = validate_with(&doc, &runner);

        let missing: Vec<_> = report
            .warnings()
            .filter(|finding| finding.message.contains("does not currently exist on disk"))
            .collect();
        assert_eq!(missing.len(), 1, "{:?}", report.findings);
        assert!(missing[0].message.contains("/boot"));
        assert_eq!(missing[0].line_index, Some(0));
    }

    /// An advisory check nobody could answer must stay silent: a sandbox
    /// that cannot reach the host must not claim every mount point is gone.
    #[test]
    fn an_unanswerable_existence_question_produces_no_findings() {
        let doc = doc_with_entry(FstabEntry::new(
            SourceId::Path("/dev/sda1".into()),
            "/this/path/almost-certainly/does/not/exist/on/any/machine",
            "ext4",
            Options::new(),
            0,
            2,
        ));
        let runner = crate::exec::RecordingRunner::answering(0, "")
            .with_unanswerable_existence("Portal call failed: ServiceUnknown");

        let report = validate_with(&doc, &runner);

        assert_eq!(runner.exists_queries().len(), 1, "it must still be asked");
        assert!(
            !report
                .findings
                .iter()
                .any(|finding| finding.message.contains("does not currently exist on disk")),
            "{:?}",
            report.findings
        );
    }

    /// No non-exempt mount point means nothing to ask about - and an
    /// out-of-process runner must not be made to spawn anything for an
    /// empty question.
    #[test]
    fn a_document_with_nothing_to_check_asks_no_existence_question() {
        let doc = doc_with_entry(FstabEntry::new(
            SourceId::Path("/dev/sda5".into()),
            "none",
            "swap",
            Options::new(),
            0,
            0,
        ));
        let runner = crate::exec::RecordingRunner::answering(0, "");

        validate_with(&doc, &runner);

        assert!(runner.exists_queries().is_empty());
    }
}
