//! Structural and semantic validation of candidate fstab documents — see `docs/SPEC.md` §5, §6.
//!
//! This module implements the two validation steps described in
//! `docs/SPEC.md` §6 ("Atomicity") step 1 and step 2:
//!
//! 1. **Structural validation** (`structural_findings`): cheap, in-memory
//!    checks of a [`Document`]'s [`crate::model::FstabEntry`] lines —
//!    non-empty `fs_type`, duplicate mount points, malformed-looking
//!    `UUID=`/`PARTUUID=` values, and a couple of advisory checks (mount
//!    point missing on disk, visible [`Line::Malformed`] lines).
//! 2. **Semantic validation** (`semantic_findings`): an external check
//!    equivalent to `findmnt --verify --tab-file <path>`, run against a
//!    temporary rendering of the candidate document — never against the
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
//! check) — none of them need to know what file the candidate would
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
//! - `run_findmnt_verify`: the actual I/O — renders the candidate
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
//! verification ran in degraded mode — it is deliberately not silent and
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
}

impl Finding {
    /// Constructs a blocking [`Severity::Error`] finding with the given
    /// message.
    fn error(message: impl Into<String>) -> Self {
        Finding {
            severity: Severity::Error,
            message: message.into(),
        }
    }

    /// Constructs an advisory [`Severity::Warning`] finding with the given
    /// message.
    fn warning(message: impl Into<String>) -> Self {
        Finding {
            severity: Severity::Warning,
            message: message.into(),
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

    findings
}

/// Surfaces any [`Line::Malformed`] lines in `doc` as advisory warnings.
/// Per `docs/SPEC.md` §5, a malformed line must be preserved (the parser
/// already guarantees that) and surfaced to the user rather than silently
/// skipped — but it is non-blocking here: a document containing a malformed
/// line the parser could not understand is not, by itself, a reason to
/// refuse to save the *rest* of the file (the parser's `Display` impl
/// re-renders malformed lines verbatim, so nothing is lost either way).
fn malformed_line_findings(doc: &Document) -> Vec<Finding> {
    doc.lines
        .iter()
        .filter_map(|line| match line {
            Line::Malformed(raw) => Some(Finding::warning(format!(
                "line could not be understood as a valid entry and was preserved as-is: {raw:?}"
            ))),
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
    doc.entries()
        .filter(|entry| entry.fs_type.is_empty())
        .map(|entry| {
            Finding::error(format!(
                "entry for mount point {:?} has an empty filesystem type",
                entry.mount_point
            ))
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
/// bits (RFC 9562 §4-5) — `/etc/fstab` has never required a *version-correct*
/// UUID, only a filesystem UUID in the conventional textual shape, so being
/// stricter than that would risk rejecting valid-but-uncommon real-world
/// values this crate has no way to double-check against actual hardware.
fn malformed_uuid_findings(doc: &Document) -> Vec<Finding> {
    doc.entries()
        .filter_map(|entry| match &entry.source {
            SourceId::Uuid(value) if !looks_like_uuid(value) => {
                Some(("UUID", value, &entry.mount_point))
            }
            SourceId::PartUuid(value) if !looks_like_uuid(value) => {
                Some(("PARTUUID", value, &entry.mount_point))
            }
            _ => None,
        })
        .map(|(prefix, value, mount_point)| {
            Finding::error(format!(
                "entry for mount point {mount_point:?} has a malformed {prefix}= value \
                 {value:?}; expected the standard 8-4-4-4-12 hex UUID shape \
                 (e.g. 11111111-2222-3333-4444-555555555555)"
            ))
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

/// Advisory check: flags a mount point that doesn't currently exist on disk.
/// Non-blocking per `docs/SPEC.md` §9 — a mount point legitimately may not
/// exist yet at validation time (e.g. provisioning a fresh `/mnt/newdrive`
/// directory before first boot), so this is a [`Severity::Warning`], not a
/// [`Severity::Error`].
///
/// Skipped entirely for the literal `none` mount point (swap entries have no
/// real mount point to check). Checks `mount_point` exactly as stored by the
/// model, i.e. without decoding any octal-escaped whitespace (`\040` etc.)
/// it may contain — per `docs/SPEC.md`'s escaped-whitespace policy, this
/// crate never decodes that text, so a mount point containing an escaped
/// space will not match the real on-disk path and will be (harmlessly, since
/// this is advisory-only) reported as "missing" even if it exists. Decoding
/// before the existence check would need a decode/encode helper pair this
/// crate does not currently expose (see `model.rs`'s module docs); not worth
/// adding for an advisory-only check unless a concrete need arises.
fn mount_point_missing_findings(doc: &Document) -> Vec<Finding> {
    doc.entries()
        .filter(|entry| entry.mount_point != SWAP_NONE_MOUNT_POINT)
        .filter(|entry| !Path::new(&entry.mount_point).exists())
        .map(|entry| {
            Finding::warning(format!(
                "mount point {:?} does not currently exist on disk",
                entry.mount_point
            ))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Semantic validation (`findmnt --verify --tab-file`)
// ---------------------------------------------------------------------------

/// The result of successfully running `findmnt --verify --tab-file`
/// (regardless of whether `findmnt` itself reported the file as valid) —
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
    interpret_findmnt_result(&result)
}

/// Pure decision logic: given the result of attempting to run
/// `findmnt --verify --tab-file` (see [`run_findmnt_verify`]), decides what
/// [`Finding`]s to report. Contains no process invocation or file I/O of its
/// own, which is what makes it directly unit-testable (including the
/// "findmnt unavailable" degraded-mode path, via a synthetic
/// `Err(io::Error::from(io::ErrorKind::NotFound))`) without needing to
/// actually uninstall `findmnt`.
///
/// - `Err(_)` (the binary could not be launched at all, for any reason —
///   not on `$PATH`, or some other I/O error) → one [`Severity::Warning`]
///   finding stating that semantic verification ran in degraded mode
///   (structural checks only), never silently treated as success, per
///   `docs/SPEC.md` §6.
/// - `Ok(run)` with `run.success == true` → no findings.
/// - `Ok(run)` with `run.success == false` → `findmnt --verify` prints a
///   final summary line shaped like
///   `"<N> parse error(s), <M> error(s), <K> warning(s)"` and exits
///   non-zero whenever `N > 0` *or* `M > 0` — i.e. it conflates "a line
///   could not be parsed" (`N`; `findmnt` itself logs `parse error at line
///   X -- ignored` and keeps analyzing the rest of the file, so this is not
///   fatal) with "the file parsed fine but has a real mount/boot-reachability
///   problem" (`M`). [`parse_findmnt_summary`] extracts `N`/`M`/`K` from that
///   line so the two cases are not conflated here:
///     - If the summary line parses and `M == 0` (regardless of `N`, and
///       regardless of the non-zero exit code that `N > 0` alone causes),
///       this is a non-blocking [`Severity::Warning`] — findmnt did not
///       find any real semantic problem.
///     - If the summary line parses and `M > 0`, this is still a blocking
///       [`Severity::Error`], and the finding still includes `findmnt`'s own
///       output text.
///     - If the summary line cannot be found/parsed at all (output shape
///       `findmnt` produced does not match what this function expects),
///       this falls back to the original conservative behavior — a
///       blocking [`Severity::Error`] — rather than ever silently treating
///       an unrecognized failure as a pass.
fn interpret_findmnt_result(result: &io::Result<FindmntRunResult>) -> Vec<Finding> {
    match result {
        Err(err) => vec![Finding::warning(format!(
            "semantic verification (equivalent to `findmnt --verify`) was skipped because \
             findmnt could not be run ({err}); this is a degraded-mode result covering \
             structural checks only, not a confirmation that the file is fully valid. \
             Install `findmnt` (part of util-linux) to enable full verification."
        ))],
        Ok(run) if run.success => Vec::new(),
        Ok(run) => {
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
/// future `findmnt` version changes the wording) — callers must treat `None`
/// conservatively (today's "non-zero exit = Error" behavior), never as a
/// free pass, since this function has no way to tell the two cases apart
/// once the shape is unrecognized.
fn parse_findmnt_summary(text: &str) -> Option<FindmntSummary> {
    text.lines().find_map(|line| {
        let fields: Vec<&str> = line.trim().split(", ").collect();
        let [parse_field, error_field, warning_field] = fields.as_slice() else {
            return None;
        };
        Some(FindmntSummary {
            parse_errors: parse_count_field(parse_field, "parse error", "parse errors")?,
            errors: parse_count_field(error_field, "error", "errors")?,
            warnings: parse_count_field(warning_field, "warning", "warnings")?,
        })
    })
}

/// Parses one comma-separated field of a `findmnt --verify` summary line
/// (e.g. `"1 parse error"` or `"2 errors"`) into its leading count, provided
/// the remainder of the field matches `singular` or `plural` exactly
/// (whichever one `findmnt` chose does not need to agree with the count
/// itself — e.g. real `findmnt` prints `"0 parse errors"`, plural despite
/// the zero count — this function only checks the wording is one of the two
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
/// not need same-filesystem rename atomicity — unlike the final write in a
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
/// any other `std::process::Command::output` failure) — both cases are
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

    // --- Structural: malformed lines surfaced as warnings ---

    #[test]
    fn malformed_line_is_a_non_blocking_warning() {
        let doc = Document::from_lines(vec![Line::Malformed("/dev/sdb1".to_string())]);
        let findings = malformed_line_findings(&doc);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
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

    // --- Semantic: pure decision logic (findmnt I/O-vs-logic split) ---

    #[test]
    fn findmnt_not_found_is_degraded_mode_warning_not_error() {
        let result: io::Result<FindmntRunResult> = Err(io::Error::from(io::ErrorKind::NotFound));
        let findings = interpret_findmnt_result(&result);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert!(findings[0].message.contains("degraded"));
    }

    #[test]
    fn findmnt_other_launch_failure_is_also_degraded_mode_warning() {
        let result: io::Result<FindmntRunResult> =
            Err(io::Error::other("simulated launch failure"));
        let findings = interpret_findmnt_result(&result);
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
        assert!(interpret_findmnt_result(&result).is_empty());
    }

    #[test]
    fn findmnt_failure_is_an_error_including_its_output_text() {
        let result: io::Result<FindmntRunResult> = Ok(FindmntRunResult {
            success: false,
            combined_output: "/data\n   [E] unreachable on boot required source: UUID=dead"
                .to_string(),
        });
        let findings = interpret_findmnt_result(&result);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0]
            .message
            .contains("unreachable on boot required source"));
    }

    // --- Semantic: parse-error (N) vs real-error (M) summary-line split (T5b) ---

    /// Real `findmnt` (util-linux 2.42.2) output, captured by hand to confirm
    /// the exact summary-line shape before writing the parser against it —
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
        let findings = interpret_findmnt_result(&result);
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
        let findings = interpret_findmnt_result(&result);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0]
            .message
            .contains("unreachable on boot required source"));
    }

    // --- Top-level `validate()` ---

    #[test]
    fn validate_reports_no_errors_for_a_valid_document() {
        // `noauto` keeps `findmnt --verify` from treating these fake,
        // nonexistent UUIDs as boot-required-but-unreachable *errors* (it
        // downgrades that to warnings for non-auto-mounted entries) — see
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
    /// would, at most, warn about (not error on) — `noauto` on a
    /// nonexistent-but-well-formed UUID, the same pattern `writer.rs`'s own
    /// tests use to avoid tripping a real `findmnt` error. Before the T5b
    /// fix, `findmnt --verify`'s non-zero exit (caused solely by the
    /// malformed line being an unparseable `N > 0`, not a real `M > 0`
    /// error) was mapped unconditionally to a blocking [`Severity::Error`],
    /// making this document permanently unsavable — contradicting
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
            assert!(
                report.warnings().any(|f| f.message.contains("parse error")),
                "expected findmnt's own parse-error warning when findmnt is available, got: {:?}",
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
    /// (`M > 0`) — that must still be a blocking [`Severity::Error`],
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
