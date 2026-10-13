//! The elevated write: `pkexec` re-exec on the unprivileged side, hidden
//! internal subcommand on the elevated side — see `docs/SPEC.md` §6 step 6
//! and `docs/API_CONTRACT.md`.
//!
//! ## The requirement, restated
//!
//! `docs/SPEC.md` §6 step 6 forbids the obvious-but-wrong design: an
//! unprivileged temp-file write followed by a separate privileged `mv`. Two
//! privileged steps mean a window in which a crash, a failed second step, or
//! a race leaves the real `/etc/fstab` in a state nobody planned. The
//! requirement is that the elevated process "either does nothing or completes
//! the full atomic swap".
//!
//! ## How that is guaranteed here, by construction
//!
//! The elevated process runs **the same binary** with a hidden first
//! argument, and in that mode it does exactly one thing:
//! [`fstab_core::writer::write_document`] — the whole §6 algorithm
//! (validate → back up → prune → temp file in the target's own directory →
//! `fsync` → `fsync` dir → `rename` → `fsync` dir → read back and verify) in
//! a single function call, inside a single process, under a single
//! authorization.
//!
//! The unprivileged side does **not** write anything, anywhere, ever: it
//! serializes the candidate document to JSON, hands it to the child on
//! stdin, and reads one JSON object back from stdout. There is no temp file
//! on this side to be moved, no partial state to resume, and no second
//! privileged call to make. Delete the elevated process at any moment and the
//! only possible outcomes are the ones `write_document` itself guarantees,
//! because `write_document` is the only thing that ever touched the disk.
//!
//! That is also why the candidate travels as a `DocumentDto`, not as rendered
//! text: the elevated side reconstructs the exact same
//! [`fstab_core::model::Document`] the user was shown and lets
//! `write_document` render it, so there is no second serializer anywhere in
//! the chain whose output could differ from the first one's.
//!
//! ## Why re-exec this binary instead of shipping a helper binary
//!
//! A separate `rustab-helper` executable would be the textbook answer. It was
//! rejected on purpose:
//!
//! - **One artifact to build, package, and sign.** A second binary has to be
//!   added to the Tauri bundle, installed to a known absolute path, kept
//!   version-locked with the GUI (a stale helper writing a format the GUI no
//!   longer speaks is a real hazard), and signed separately for distribution
//!   formats that sign executables. `docs/SPEC.md` §13/§14 describe a
//!   single-binary bundle; a helper would complicate every one of those paths.
//! - **It cannot drift from the audited code.** The elevated path calls the
//!   identical `write_document` compiled from the identical source in the
//!   same build. There is no way for "the helper's copy" of the write logic
//!   to fall behind, because there is no copy.
//! - **No security benefit is given up.** A helper binary's advantage would
//!   be a smaller elevated code surface. Here the elevated surface is
//!   *already* minimal and provably so: [`run_internal_if_requested`] runs
//!   before anything else in `main`, so when the flag is present the Tauri
//!   runtime, the webview, the logger and the plugins are never initialized —
//!   the elevated process reads stdin, calls one function, prints JSON, and
//!   exits. The flag is matched as `argv[1]` exactly, and extra arguments are
//!   refused.
//! - **The helper would need the same authorization anyway.** `pkexec` prompts
//!   per invocation for either design, so nothing about the user experience
//!   or the trust model improves.
//!
//! ## The target path, and why the child re-checks it
//!
//! `pkexec` deliberately does not forward the caller's environment, so the
//! child cannot re-resolve the target itself — `RUSTAB_TARGET_PATH` does not
//! survive the transition (that is a feature: it means an env var cannot
//! influence the elevated process at all). The target therefore travels
//! inside the request.
//!
//! Because the path arrives as data, the child refuses to write anywhere
//! except the real `/etc/fstab` (literally, or via a symlink that resolves to
//! it). Escalation only ever happens when the unprivileged side resolved
//! `is_real == true`, so this costs nothing in normal operation, and it means
//! a bug on the unprivileged side cannot turn the elevated write into "write
//! arbitrary content to an arbitrary root-owned path". The allowed target is a
//! parameter of [`handle_privileged_write`] purely so tests exercise the real
//! code path against a temp file instead of needing a bypass switch.

use crate::dto::{DocumentDto, WriteErrorDto, WriteErrorKindDto, WriteOutcomeDto};
use crate::target;
use fstab_core::model::Document;
use fstab_core::{validate, writer};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The hidden first argument that turns this binary into the elevated writer.
///
/// Not a public CLI: it is an implementation detail of the escalation
/// mechanism, its stdin/stdout protocol is unstable, and nothing but
/// [`save_elevated`] is expected to pass it.
pub const INTERNAL_WRITE_FLAG: &str = "--internal-privileged-write";

/// Exit code of the elevated writer when the write succeeded.
pub const EXIT_OK: i32 = 0;

/// Exit code when the write was attempted and failed; stdout still holds a
/// [`PrivilegedWriteResponse::Error`] explaining what happened.
pub const EXIT_WRITE_ERROR: i32 = 1;

/// Exit code when the request itself was unusable (unreadable or malformed
/// stdin, stray arguments). Nothing was written.
pub const EXIT_PROTOCOL_ERROR: i32 = 2;

/// How much of the child's stderr is quoted back in an error message.
const STDERR_EXCERPT_LIMIT: usize = 1000;

/// `pkexec`'s exit code when the user dismissed the authentication dialog.
/// The program was never executed.
const PKEXEC_DISMISSED: i32 = 126;

/// `pkexec`'s exit code when authorization could not be obtained or the
/// program could not be executed. Again, the program never ran.
const PKEXEC_NOT_AUTHORIZED: i32 = 127;

/// The request the unprivileged side writes to the elevated writer's stdin:
/// one JSON object, then EOF.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivilegedWriteRequest {
    /// The file to write. Must be the real `/etc/fstab` (see the module docs).
    pub target: String,
    /// The document to write, in the same shape the frontend sent it.
    pub candidate: DocumentDto,
}

/// The single JSON object the elevated writer prints to stdout before
/// exiting. Nothing else is ever written to stdout in that mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PrivilegedWriteResponse {
    /// The atomic write completed and was verified.
    Ok {
        /// What it did.
        outcome: WriteOutcomeDto,
    },
    /// The write did not complete. `error.target_unchanged` says whether the
    /// target still holds its previous content.
    Error {
        /// Why.
        error: WriteErrorDto,
    },
}

// ---------------------------------------------------------------------------
// The elevated side
// ---------------------------------------------------------------------------

/// Runs the elevated writer if this process was started with
/// [`INTERNAL_WRITE_FLAG`] as its first argument, returning the exit code to
/// terminate with. Returns `None` for a normal GUI launch.
///
/// Called as the very first statement of `main` so that, in elevated mode,
/// no Tauri runtime, webview, plugin or logger is ever initialized.
pub fn run_internal_if_requested() -> Option<i32> {
    let mut args = std::env::args_os();
    args.next()?; // argv[0]
    if args.next()? != INTERNAL_WRITE_FLAG {
        return None;
    }
    if args.next().is_some() {
        eprintln!("{INTERNAL_WRITE_FLAG} takes no further arguments");
        return Some(EXIT_PROTOCOL_ERROR);
    }

    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    Some(handle_privileged_write(
        &mut stdin,
        &mut stdout,
        Path::new(target::REAL_FSTAB_PATH),
    ))
}

/// The whole of the elevated writer: read one JSON request from `input`,
/// perform the complete [`writer::write_document`] operation, write one JSON
/// [`PrivilegedWriteResponse`] to `output`, and return the exit code.
///
/// `allowed_target` is the only path this is permitted to write (matched
/// literally or by canonicalized identity). Production passes
/// `/etc/fstab`; tests pass a temp file so that this exact function — not a
/// stand-in — is what they exercise.
///
/// Never panics and never returns without having written a response: a caller
/// (or a test) can rely on stdout holding exactly one JSON object.
pub fn handle_privileged_write<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    allowed_target: &Path,
) -> i32 {
    let mut raw = String::new();
    if let Err(err) = input.read_to_string(&mut raw) {
        return respond(
            output,
            PrivilegedWriteResponse::Error {
                error: WriteErrorDto::new(
                    WriteErrorKindDto::Internal,
                    true,
                    format!("the elevated writer could not read its request: {err}"),
                ),
            },
            EXIT_PROTOCOL_ERROR,
        );
    }

    let request: PrivilegedWriteRequest = match serde_json::from_str(&raw) {
        Ok(request) => request,
        Err(err) => {
            return respond(
                output,
                PrivilegedWriteResponse::Error {
                    error: WriteErrorDto::new(
                        WriteErrorKindDto::Internal,
                        true,
                        format!("the elevated writer could not parse its request: {err}"),
                    ),
                },
                EXIT_PROTOCOL_ERROR,
            );
        }
    };

    let target_path = PathBuf::from(&request.target);
    if !target::is_same_file(&target_path, allowed_target) {
        return respond(
            output,
            PrivilegedWriteResponse::Error {
                error: WriteErrorDto::new(
                    WriteErrorKindDto::InvalidTarget,
                    true,
                    format!(
                        "the elevated writer only writes {}; it refused to write {}",
                        allowed_target.display(),
                        target_path.display()
                    ),
                )
                .with_path(&target_path),
            },
            EXIT_WRITE_ERROR,
        );
    }

    // One call, one process, one authorization: validation, backup, temp
    // file, fsyncs, rename and read-back verification all happen inside here.
    let candidate = Document::from(request.candidate);
    match writer::write_document(&target_path, &candidate) {
        Ok(outcome) => respond(
            output,
            PrivilegedWriteResponse::Ok {
                // `false`: this process cannot know whether it was launched
                // through `pkexec` or directly. The unprivileged side, which
                // does know, overwrites this.
                outcome: WriteOutcomeDto::from_outcome(&outcome, false),
            },
            EXIT_OK,
        ),
        Err(err) => respond(
            output,
            PrivilegedWriteResponse::Error {
                error: WriteErrorDto::from(&err),
            },
            EXIT_WRITE_ERROR,
        ),
    }
}

/// Serializes `response` to `output` and returns `code`.
///
/// A failure to even print the response is reported on stderr and turned into
/// [`EXIT_PROTOCOL_ERROR`], so the unprivileged side sees "the helper
/// produced no usable result" rather than a success it cannot substantiate.
fn respond<W: Write>(output: &mut W, response: PrivilegedWriteResponse, code: i32) -> i32 {
    match serde_json::to_string(&response) {
        Ok(json) => match writeln!(output, "{json}").and_then(|()| output.flush()) {
            Ok(()) => code,
            Err(err) => {
                eprintln!("could not write the elevated writer's response: {err}");
                EXIT_PROTOCOL_ERROR
            }
        },
        Err(err) => {
            eprintln!("could not serialize the elevated writer's response: {err}");
            EXIT_PROTOCOL_ERROR
        }
    }
}

// ---------------------------------------------------------------------------
// The unprivileged side
// ---------------------------------------------------------------------------

/// Performs a save of `candidate` to `target` as root, by re-executing this
/// binary under `pkexec` with [`INTERNAL_WRITE_FLAG`].
///
/// This function performs **no filesystem writes of its own**. Everything
/// that touches disk happens inside the elevated
/// [`writer::write_document`] call (see the module docs).
///
/// # Errors
///
/// Returns a [`WriteErrorDto`] whose `kind` is
/// [`WriteErrorKindDto::PrivilegeEscalation`] if the helper could not be
/// launched, the authorization was refused or dismissed, or the helper
/// produced no usable response; otherwise the error the helper itself
/// reported, forwarded unchanged.
pub fn save_elevated(
    target: &Path,
    candidate: &Document,
) -> Result<WriteOutcomeDto, WriteErrorDto> {
    // Validate before prompting. This is a courtesy, not the safety net: a
    // document with blocking findings is rejected without making the user
    // authenticate for a save that cannot succeed. `write_document` validates
    // again inside the elevated process, and that call is the authoritative
    // one — this check could be deleted without weakening any guarantee.
    let report = validate::validate(candidate);
    if report.has_errors() {
        return Err(WriteErrorDto::from(&writer::WriteError::Validation(report)));
    }

    let exe = std::env::current_exe().map_err(|err| {
        WriteErrorDto::new(
            WriteErrorKindDto::PrivilegeEscalation,
            true,
            format!(
                "could not determine Rustab's own executable path, so the elevated writer could \
                 not be started ({err}); nothing was written"
            ),
        )
    })?;

    let request = PrivilegedWriteRequest {
        target: target.to_string_lossy().into_owned(),
        candidate: DocumentDto::from(candidate),
    };
    let payload = serde_json::to_vec(&request).map_err(|err| {
        WriteErrorDto::new(
            WriteErrorKindDto::Internal,
            true,
            format!("could not serialize the save request ({err}); nothing was written"),
        )
    })?;

    let mut child = Command::new("pkexec")
        .arg(&exe)
        .arg(INTERNAL_WRITE_FLAG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| {
            WriteErrorDto::new(
                WriteErrorKindDto::PrivilegeEscalation,
                true,
                format!(
                    "could not run pkexec to obtain the privileges needed to write {} ({err}); \
                     nothing was written. Is polkit installed?",
                    target.display()
                ),
            )
        })?;

    // The child reads stdin to EOF before writing anything, so handing it the
    // whole request and then closing the pipe cannot deadlock regardless of
    // the request's size.
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(err) = stdin.write_all(&payload) {
            // Kill the child rather than leaving it blocked on a stdin that
            // will never be complete. Claiming the target is unchanged is
            // sound here for two independent reasons: the child is killed
            // before its stdin is closed, so it has not even reached EOF and
            // cannot have parsed anything; and a truncated prefix of one JSON
            // object can never parse, so even a child that did reach EOF
            // would have answered with a protocol error instead of writing.
            let _ = child.kill();
            let _ = child.wait();
            return Err(WriteErrorDto::new(
                WriteErrorKindDto::PrivilegeEscalation,
                true,
                format!(
                    "could not hand the save request to the elevated writer ({err}); nothing was \
                     written"
                ),
            ));
        }
        drop(stdin);
    }

    let output = child.wait_with_output().map_err(|err| {
        WriteErrorDto::new(
            WriteErrorKindDto::PrivilegeEscalation,
            // The helper may have run; without its response this side cannot
            // claim the target is untouched.
            false,
            format!("the elevated writer could not be waited for ({err})"),
        )
    })?;

    interpret_child_output(target, output.status.code(), &output.stdout, &output.stderr)
}

/// Maps the elevated writer's exit status, stdout and stderr onto a result.
///
/// Pure: no process spawning, so the interpretation of every outcome
/// (including the `pkexec`-specific exit codes, which cannot be provoked
/// reliably in a test) is unit-testable on its own.
///
/// A parsable JSON response always wins over the exit code — it is the
/// helper's own account of what it did. Only when there is no usable response
/// does the exit code decide, and then the rule is conservative: claim
/// `target_unchanged` only for the codes that mean the helper never ran.
fn interpret_child_output(
    target: &Path,
    code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<WriteOutcomeDto, WriteErrorDto> {
    let text = String::from_utf8_lossy(stdout);
    if let Ok(response) = serde_json::from_str::<PrivilegedWriteResponse>(text.trim()) {
        return match response {
            PrivilegedWriteResponse::Ok { outcome } => Ok(WriteOutcomeDto {
                elevated: true,
                ..outcome
            }),
            PrivilegedWriteResponse::Error { error } => Err(error),
        };
    }

    let detail = stderr_excerpt(stderr);
    let error = match code {
        Some(PKEXEC_DISMISSED) => WriteErrorDto::new(
            WriteErrorKindDto::PrivilegeEscalation,
            true,
            format!(
                "the authorization request was dismissed, so {} was not written and is unchanged",
                target.display()
            ),
        ),
        Some(PKEXEC_NOT_AUTHORIZED) => WriteErrorDto::new(
            WriteErrorKindDto::PrivilegeEscalation,
            true,
            format!(
                "authorization to write {} was not granted (or the elevated writer could not be \
                 executed), so nothing was written{detail}",
                target.display()
            ),
        ),
        other => WriteErrorDto::new(
            WriteErrorKindDto::PrivilegeEscalation,
            // Unknown territory: the helper may have been killed mid-write.
            // `write_document`'s own guarantees still hold (the target holds
            // either its old or its new content), but this side cannot say
            // which, and must not claim "unchanged".
            false,
            format!(
                "the elevated writer exited {} without a usable result, so the state of {} could \
                 not be confirmed; re-open the file to see which content it holds{detail}",
                match other {
                    Some(code) => format!("with status {code}"),
                    None => "on a signal".to_string(),
                },
                target.display()
            ),
        ),
    };
    Err(error.with_path(target))
}

/// Renders a short, prefixed excerpt of the child's stderr for inclusion in
/// an error message, or an empty string if there was nothing on it.
fn stderr_excerpt(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let excerpt: String = trimmed.chars().take(STDERR_EXCERPT_LIMIT).collect();
    format!(" (pkexec said: {excerpt})")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::WriteErrorKindDto;
    use crate::testutil::TempDir;
    use fstab_core::model::{FstabEntry, FstabOption, Line, Options, SourceId};
    use fstab_core::parser;
    use std::fs;

    const ORIGINAL: &str = "# the content the elevated writer must back up\n\
                            UUID=11111111-2222-3333-4444-555555555555 /mnt/original ext4 noauto 0 2\n";

    /// A document that passes validation on any machine: `noauto`, a
    /// well-formed UUID, and a mount point that does not need to exist.
    fn valid_candidate(mount_point: &str) -> Document {
        Document::from_lines(vec![
            Line::Comment(" written by the elevated writer test".to_string()),
            Line::Entry(FstabEntry::new(
                SourceId::Uuid("11111111-2222-3333-4444-555555555555".into()),
                mount_point,
                "ext4",
                Options::from_vec(vec![FstabOption::Flag("noauto".into())]),
                0,
                2,
            )),
        ])
    }

    /// Two entries claiming the same mount point: fails structural validation.
    fn invalid_candidate() -> Document {
        parser::parse(
            "UUID=11111111-2222-3333-4444-555555555555 /mnt/clash ext4 noauto 0 2\n\
             UUID=22222222-3333-4444-5555-666666666666 /mnt/clash ext4 noauto 0 2\n",
        )
    }

    /// Runs the elevated writer exactly as `main` does, but with an in-memory
    /// stdin/stdout and a temp file standing in for `/etc/fstab`.
    fn run_helper(
        target: &Path,
        allowed_target: &Path,
        candidate: &Document,
    ) -> (i32, PrivilegedWriteResponse) {
        let request = PrivilegedWriteRequest {
            target: target.to_string_lossy().into_owned(),
            candidate: DocumentDto::from(candidate),
        };
        let payload = serde_json::to_vec(&request).expect("the request must serialize");
        let mut stdin = payload.as_slice();
        let mut stdout: Vec<u8> = Vec::new();
        let code = handle_privileged_write(&mut stdin, &mut stdout, allowed_target);
        let text = String::from_utf8(stdout).expect("the response must be UTF-8");
        let response =
            serde_json::from_str(text.trim()).expect("the response must be a single JSON object");
        (code, response)
    }

    /// The core claim of this module: given a request on stdin, the helper
    /// performs the *entire* `write_document` operation — backup included —
    /// and reports it as JSON.
    #[test]
    fn helper_performs_the_whole_write_document_operation_from_stdin() {
        let dir = TempDir::new("helper-ok");
        let target = dir.join("fstab");
        fs::write(&target, ORIGINAL).expect("seed the target");

        let candidate = valid_candidate("/mnt/elevated");
        let (code, response) = run_helper(&target, &target, &candidate);

        assert_eq!(code, EXIT_OK);
        let PrivilegedWriteResponse::Ok { outcome } = response else {
            panic!("the write should have succeeded: {response:?}");
        };

        // The target holds exactly the candidate's serialization...
        assert_eq!(
            fs::read_to_string(&target).expect("read the target"),
            candidate.to_string()
        );
        // ...a backup of the previous content exists, which is proof the
        // helper went through `write_document` rather than just writing the
        // file...
        let backup = outcome.backup.expect("a backup must have been created");
        assert_eq!(
            fs::read_to_string(&backup).expect("read the backup"),
            ORIGINAL
        );
        assert_eq!(outcome.target, target.to_string_lossy());
        // ...and the helper never claims to have been elevated, since it
        // cannot know.
        assert!(!outcome.elevated);
    }

    /// A first-ever save: no backup, file created.
    #[test]
    fn helper_creates_a_missing_target_without_a_backup() {
        let dir = TempDir::new("helper-create");
        let target = dir.join("fstab");

        let candidate = valid_candidate("/mnt/first");
        let (code, response) = run_helper(&target, &target, &candidate);

        assert_eq!(code, EXIT_OK);
        let PrivilegedWriteResponse::Ok { outcome } = response else {
            panic!("the write should have succeeded: {response:?}");
        };
        assert!(outcome.backup.is_none());
        assert_eq!(
            fs::read_to_string(&target).expect("read the target"),
            candidate.to_string()
        );
    }

    /// Validation happens *inside* the helper, before anything is written —
    /// the unprivileged side's pre-check is not what protects the file.
    #[test]
    fn helper_refuses_an_invalid_candidate_and_leaves_the_target_untouched() {
        let dir = TempDir::new("helper-invalid");
        let target = dir.join("fstab");
        fs::write(&target, ORIGINAL).expect("seed the target");

        let (code, response) = run_helper(&target, &target, &invalid_candidate());

        assert_eq!(code, EXIT_WRITE_ERROR);
        let PrivilegedWriteResponse::Error { error } = response else {
            panic!("the write should have failed: {response:?}");
        };
        assert_eq!(error.kind, WriteErrorKindDto::Validation);
        assert!(error.target_unchanged);
        assert!(error.report.expect("a report must be attached").has_errors);
        assert_eq!(
            fs::read_to_string(&target).expect("read the target"),
            ORIGINAL
        );
        // Not even a backup or a temp file was created.
        let leftovers: Vec<String> = fs::read_dir(dir.path())
            .expect("read the temp dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(leftovers, vec!["fstab".to_string()]);
    }

    /// The path arrives as data, so the helper re-checks it: anything but its
    /// one allowed target is refused without a write.
    #[test]
    fn helper_refuses_a_target_other_than_the_allowed_one() {
        let dir = TempDir::new("helper-wrong-target");
        let decoy = dir.join("somewhere-else");
        fs::write(&decoy, "# must not be touched\n").expect("seed the decoy");
        let allowed = dir.join("fstab");

        let (code, response) = run_helper(&decoy, &allowed, &valid_candidate("/mnt/nope"));

        assert_eq!(code, EXIT_WRITE_ERROR);
        let PrivilegedWriteResponse::Error { error } = response else {
            panic!("the write should have been refused: {response:?}");
        };
        assert_eq!(error.kind, WriteErrorKindDto::InvalidTarget);
        assert!(error.target_unchanged);
        assert_eq!(
            fs::read_to_string(&decoy).expect("read the decoy"),
            "# must not be touched\n"
        );
        assert!(!allowed.exists());
    }

    /// Garbage on stdin is a protocol error with a JSON body, not a panic and
    /// not a write.
    #[test]
    fn helper_reports_a_protocol_error_for_malformed_stdin() {
        let dir = TempDir::new("helper-garbage");
        let target = dir.join("fstab");
        fs::write(&target, ORIGINAL).expect("seed the target");

        let mut stdin = &b"not json at all"[..];
        let mut stdout: Vec<u8> = Vec::new();
        let code = handle_privileged_write(&mut stdin, &mut stdout, &target);

        assert_eq!(code, EXIT_PROTOCOL_ERROR);
        let text = String::from_utf8(stdout).expect("UTF-8");
        let response: PrivilegedWriteResponse =
            serde_json::from_str(text.trim()).expect("a JSON response is still produced");
        let PrivilegedWriteResponse::Error { error } = response else {
            panic!("a protocol error must not be reported as success");
        };
        assert_eq!(error.kind, WriteErrorKindDto::Internal);
        assert!(error.target_unchanged);
        assert_eq!(
            fs::read_to_string(&target).expect("read the target"),
            ORIGINAL
        );
    }

    /// The request shape is the DTO shape, so a candidate survives the trip
    /// through JSON exactly — including malformed lines and trailing
    /// comments, which a text-based protocol could silently reinterpret.
    #[test]
    fn request_round_trips_the_candidate_exactly() {
        let candidate = parser::parse(
            "# header\n\
             \n\
             UUID=11111111-2222-3333-4444-555555555555 /mnt/data ext4 noauto,x-systemd.automount= 0 2 # note\n\
             /dev/sdb1\n",
        );
        let request = PrivilegedWriteRequest {
            target: "/etc/fstab".to_string(),
            candidate: DocumentDto::from(&candidate),
        };
        let json = serde_json::to_string(&request).expect("serialize");
        let back: PrivilegedWriteRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(Document::from(back.candidate), candidate);
    }

    // -- interpretation of the child's output -------------------------------

    #[test]
    fn a_json_response_is_trusted_over_the_exit_code() {
        let outcome = WriteOutcomeDto {
            target: "/etc/fstab".to_string(),
            backup: Some("/etc/fstab.rustab-backup.0000000001.000000002".to_string()),
            pruned_backups: Vec::new(),
            validation_warnings: Vec::new(),
            notes: Vec::new(),
            elevated: false,
        };
        let stdout = serde_json::to_vec(&PrivilegedWriteResponse::Ok { outcome }).expect("json");

        let result = interpret_child_output(Path::new("/etc/fstab"), Some(EXIT_OK), &stdout, b"");

        let outcome = result.expect("the response says it succeeded");
        // The unprivileged side knows it used pkexec and says so.
        assert!(outcome.elevated);
    }

    #[test]
    fn a_dismissed_authorization_is_reported_as_target_unchanged() {
        let result = interpret_child_output(
            Path::new("/etc/fstab"),
            Some(PKEXEC_DISMISSED),
            b"",
            b"Error executing command as another user: Not authorized",
        );

        let error = result.expect_err("a dismissed prompt is a failure");
        assert_eq!(error.kind, WriteErrorKindDto::PrivilegeEscalation);
        assert!(error.target_unchanged);
        assert_eq!(error.path.as_deref(), Some("/etc/fstab"));
    }

    #[test]
    fn a_refused_authorization_is_reported_as_target_unchanged_with_detail() {
        let result = interpret_child_output(
            Path::new("/etc/fstab"),
            Some(PKEXEC_NOT_AUTHORIZED),
            b"",
            b"polkit-agent-helper-1: authentication failure",
        );

        let error = result.expect_err("a refused prompt is a failure");
        assert!(error.target_unchanged);
        assert!(error.message.contains("authentication failure"));
    }

    /// The honest case: the helper died without reporting. The target may
    /// hold either content, and this must not be dressed up as "unchanged".
    #[test]
    fn an_unexplained_exit_does_not_claim_the_target_is_unchanged() {
        let killed = interpret_child_output(Path::new("/etc/fstab"), None, b"", b"")
            .expect_err("no response is a failure");
        assert!(!killed.target_unchanged);
        assert!(killed.message.contains("could not be confirmed"));

        let odd = interpret_child_output(Path::new("/etc/fstab"), Some(42), b"partial out", b"")
            .expect_err("no response is a failure");
        assert!(!odd.target_unchanged);
    }

    /// An error the helper itself reported is forwarded verbatim, not
    /// re-labelled as an escalation problem.
    #[test]
    fn a_reported_write_error_is_forwarded_unchanged() {
        let error = WriteErrorDto::new(
            WriteErrorKindDto::Backup,
            true,
            "could not create a backup at /etc/fstab.rustab-backup.x: No space left on device",
        );
        let stdout = serde_json::to_vec(&PrivilegedWriteResponse::Error {
            error: error.clone(),
        })
        .expect("json");

        let result = interpret_child_output(
            Path::new("/etc/fstab"),
            Some(EXIT_WRITE_ERROR),
            &stdout,
            b"",
        );

        assert_eq!(result.expect_err("an error response is an error"), error);
    }

    /// `save_elevated` must not prompt for a password to perform a save that
    /// validation already rejects.
    #[test]
    fn save_elevated_rejects_an_invalid_candidate_before_escalating() {
        let error = save_elevated(Path::new("/etc/fstab"), &invalid_candidate())
            .expect_err("an invalid candidate must be rejected");
        assert_eq!(error.kind, WriteErrorKindDto::Validation);
        assert!(error.target_unchanged);
        assert!(error.report.is_some());
    }
}
