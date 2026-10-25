//! The elevated side of Rustab's three privileged operations, in full -
//! see `docs/SPEC.md` §6 step 6 and `docs/API_CONTRACT.md` §4.
//!
//! Everything a root process ever runs is here, and nothing else is: one
//! JSON request in on stdin, one operation, one JSON response out on
//! stdout, one exit code. No process is spawned, no environment variable
//! read, no log emitted, no Tauri runtime, webview or plugin so much as
//! linked (see [`crate`]). The unprivileged `pkexec`/`flatpak-spawn`
//! plumbing and the interpretation of what came back live in the `rustab`
//! crate's `privileged` module.
//!
//! ## The three operations, and why there is still only one entry point
//!
//! Three things need root, only when the active target is the real
//! `/etc/fstab`: **writing the target** (save/restore), **deleting one of
//! its backups**, and **creating a manual backup** - the latter two
//! because an unprivileged process can't unlink or create files in `/etc/`.
//!
//! All three arrive through [`handle_privileged_request`]: the request is a
//! union tagged by `op`, so each operation is one `match` arm rather than a
//! second way into a root process. Whoever calls this function - the
//! `rustab-helper` binary, or the GUI binary's hidden
//! `--internal-privileged-write` branch - checks exactly one argument and
//! turns the process into root through exactly one path, auditable as
//! such. The backup operations are deliberately tiny (resolve then
//! `remove_file`; resolve then copy) - neither writes the target, and
//! [`writer::create_backup`] never even opens it for writing.
//!
//! ## The requirement
//!
//! `docs/SPEC.md` §6 step 6 forbids an unprivileged temp-file write
//! followed by a separate privileged `mv`: two privileged steps leave a
//! window where a crash or race can leave `/etc/fstab` in an unplanned
//! state. The elevated process must either do nothing or complete the
//! full atomic swap.
//!
//! ## How that's guaranteed here
//!
//! A write does exactly one thing: [`writer::write_document`] - the whole
//! §6 algorithm in one function call, inside one process, under one
//! authorization. The unprivileged side never writes anything: it
//! serializes the candidate to JSON, hands it over on stdin, and reads one
//! JSON object back. No temp file to move, no partial state to resume, no
//! second privileged call. The candidate travels as a
//! [`DocumentDto`], not rendered text, so this side reconstructs the exact
//! `Document` the user saw and lets `write_document` render it - no second
//! serializer whose output could differ from the first.
//!
//! ## The target path, and why it is re-checked here
//!
//! `pkexec` doesn't forward the caller's environment, so this process
//! can't re-resolve the target itself - the target travels inside the
//! request. Because it arrives as data, every operation refuses to act on
//! anything except the real `/etc/fstab` (literally, or via a resolving
//! symlink), through [`target_identity::is_same_file`]. Escalation only
//! happens when the unprivileged side is acting on the real file, and in
//! every such case the path handed over is the hardcoded
//! [`target_identity::REAL_FSTAB_PATH`], never a draft path - so a bug on
//! the unprivileged side can't turn an elevated write into "write
//! arbitrary content to an arbitrary root-owned path". The allowed target
//! is a parameter of [`handle_privileged_request`] purely so tests can
//! exercise the real code path against a temp file.
//!
//! The delete operation's `backup_path` arrives as data for the same
//! reason, with one extra step: it is resolved through
//! [`backups::resolve_requested`] against the real backup list of the
//! target already verified above, so the only files this process can ever
//! unlink are that target's own backups.
//!
//! ## The external-change check lives here too
//!
//! For the same structural reason: a write request carries what the
//! unprivileged side believed `/etc/fstab` held, and this side re-reads and
//! compares immediately before `write_document`. It can't be checked only
//! on the unprivileged side, since `pkexec` blocks on a human typing a
//! password - an arbitrarily long window during which anything else can
//! write the file. The parent's check is a courtesy that avoids a
//! pointless password prompt; this one is the one that decides.
//!
//! The create-backup operation carries only a `target`; that single
//! identity check is its whole authority, since the path it writes is
//! chosen by [`writer::create_backup`] from the verified target's own
//! name, never from anything else in the request.

use crate::backups;
use crate::dto::{
    CreateBackupErrorDto, CreateBackupErrorKindDto, CreateBackupOutcomeDto, DeleteBackupErrorDto,
    DeleteBackupErrorKindDto, DocumentDto, WriteErrorDto, WriteErrorKindDto, WriteOutcomeDto,
};
use crate::target_identity;
use fstab_core::model::Document;
use fstab_core::writer;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Exit code of the elevated helper when the requested operation succeeded.
pub const EXIT_OK: i32 = 0;

/// Exit code when the requested operation was attempted and failed; stdout
/// still holds an error response explaining what happened.
pub const EXIT_OPERATION_ERROR: i32 = 1;

/// Exit code when the request itself was unusable (unreadable or malformed
/// stdin, stray arguments). Nothing was written and nothing was deleted - the
/// helper never got as far as knowing which operation it was asked for.
pub const EXIT_PROTOCOL_ERROR: i32 = 2;

/// The request the unprivileged side writes to the elevated helper's stdin:
/// one JSON object, then EOF.
///
/// A union tagged by `op`, so the single entry point can carry any
/// operation. Every variant names its `target` explicitly, since the
/// elevated process can't re-resolve it (see module docs), and re-checks
/// that name against the one target it's allowed to touch before doing
/// anything.
///
/// Internal, undocumented, unstable by design (`docs/API_CONTRACT.md`
/// §11): nothing but Rustab's own two binaries ever speak it, to each
/// other, in one build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PrivilegedRequest {
    /// Perform the whole [`writer::write_document`] operation on `target`.
    Write {
        /// The file to write. Must be the real `/etc/fstab`.
        target: String,
        /// The document to write, in the same shape the frontend sent it.
        candidate: DocumentDto,
        /// What the unprivileged side believes `target` currently holds (its
        /// last-known-real marker's content). The elevated side re-reads
        /// `target` and refuses the write if the two differ, immediately
        /// before [`writer::write_document`] runs; see [`handle_write`].
        ///
        /// `None` means no usable record, so no comparison is possible
        /// (see [`target_identity::ExternalChangeGuard`]).
        ///
        /// Deliberately not `#[serde(default)]`, unlike the DTOs crossing
        /// the Tauri boundary: this protocol is internal to one build
        /// speaking to itself, and defaulting a missing field to "no
        /// record" would silently fail *open* on the one check that
        /// exists to fail closed.
        last_known_real_content: Option<String>,
        /// `true` only when the user has already been shown the external
        /// change and asked for the write anyway, skipping the comparison
        /// for this one request.
        allow_external_change: bool,
    },
    /// Unlink one of `target`'s own backups. Never `target` itself.
    DeleteBackup {
        /// The target whose backups may be deleted. Must be the real
        /// `/etc/fstab`.
        target: String,
        /// The backup to delete. Honoured only if it resolves, *inside the
        /// elevated process*, to one of `target`'s real backups.
        backup_path: String,
    },
    /// Copy `target` to a fresh *manual* backup beside it, leaving `target`
    /// itself untouched.
    ///
    /// Carries no path but the target on purpose: the backup's name is chosen
    /// by [`writer::create_backup`] inside the elevated process, so there is
    /// nothing here for a caller to point somewhere else.
    CreateBackup {
        /// The target to back up. Must be the real `/etc/fstab`.
        target: String,
    },
}

/// The single JSON object the elevated helper prints to stdout before
/// exiting. Nothing else is ever written to stdout in that mode.
///
/// One type for all operations, not one per operation, since framing
/// errors (unreadable stdin, unparsable request) happen before the helper
/// knows which operation was asked for, and are reported as
/// [`Self::Error`] regardless.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PrivilegedResponse {
    /// A `write` completed and was verified.
    Ok {
        /// What it did.
        outcome: WriteOutcomeDto,
    },
    /// A `write` did not complete, or the request itself was unusable.
    /// `error.target_unchanged` says whether the target still holds its
    /// previous content.
    Error {
        /// Why.
        error: WriteErrorDto,
    },
    /// A `delete_backup` completed: the backup is gone, and nothing else was
    /// touched. Carries no payload - there is nothing to report.
    BackupDeleted,
    /// A `delete_backup` did not complete. The backup may or may not still
    /// exist (`kind` says which); the target itself was never opened.
    BackupDeleteFailed {
        /// Why.
        error: DeleteBackupErrorDto,
    },
    /// A `create_backup` completed: a new manual backup exists beside the
    /// target, which was only read.
    BackupCreated {
        /// What it did, including which backup it created.
        outcome: CreateBackupOutcomeDto,
    },
    /// A `create_backup` did not complete. No backup was created; the target
    /// was never opened for writing in any case.
    BackupCreateFailed {
        /// Why.
        error: CreateBackupErrorDto,
    },
}

/// The whole of the elevated helper: read one JSON [`PrivilegedRequest`]
/// from `input`, perform it, write one JSON [`PrivilegedResponse`] to
/// `output`, and return the exit code.
///
/// `allowed_target` is the only target this may act on (matched literally
/// or by canonicalized identity). Production passes `/etc/fstab`; tests
/// pass a temp file, so this exact function is what they exercise.
///
/// Never panics, never returns without writing a response.
pub fn handle_privileged_request<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    allowed_target: &Path,
) -> i32 {
    let mut raw = String::new();
    if let Err(err) = input.read_to_string(&mut raw) {
        return respond(
            output,
            PrivilegedResponse::Error {
                error: WriteErrorDto::new(
                    WriteErrorKindDto::Internal,
                    true,
                    format!("the elevated helper could not read its request: {err}"),
                ),
            },
            EXIT_PROTOCOL_ERROR,
        );
    }

    // A framing failure is reported before the operation is known, so it is
    // reported in the write-shaped `Error` variant; the delete path knows to
    // read that too (see `interpret_delete_output` on the unprivileged side).
    // Nothing has touched disk at this point, in any operation.
    let request: PrivilegedRequest = match serde_json::from_str(&raw) {
        Ok(request) => request,
        Err(err) => {
            return respond(
                output,
                PrivilegedResponse::Error {
                    error: WriteErrorDto::new(
                        WriteErrorKindDto::Internal,
                        true,
                        format!("the elevated helper could not parse its request: {err}"),
                    ),
                },
                EXIT_PROTOCOL_ERROR,
            );
        }
    };

    match request {
        PrivilegedRequest::Write {
            target,
            candidate,
            last_known_real_content,
            allow_external_change,
        } => handle_write(
            output,
            allowed_target,
            &target,
            candidate,
            &target_identity::ExternalChangeGuard {
                last_known_real_content,
                allow_external_change,
            },
        ),
        PrivilegedRequest::DeleteBackup {
            target,
            backup_path,
        } => handle_delete_backup(output, allowed_target, &target, &backup_path),
        PrivilegedRequest::CreateBackup { target } => {
            handle_create_backup(output, allowed_target, &target)
        }
    }
}

/// The elevated write: refuse any target but `allowed_target`, refuse a
/// target that changed outside Rustab since the unprivileged side last
/// looked, then perform the complete [`writer::write_document`] operation
/// in this one process under this one authorization.
///
/// ## Why the external-change check is *here*
///
/// Placed as the last thing before `write_document` on purpose: the
/// unprivileged side spawns `pkexec`, which blocks on a human typing a
/// password for an arbitrarily long time, during which anything else
/// could write `/etc/fstab`. A parent-side-only check would leave that
/// whole window uncovered. So the request carries what the parent
/// believed the file held ([`target_identity::ExternalChangeGuard`]), and
/// this process re-reads and compares before writing - the same
/// relationship this module has for validation, where the parent's
/// pre-check is a courtesy and this one is binding.
fn handle_write<W: Write>(
    output: &mut W,
    allowed_target: &Path,
    target: &str,
    candidate: DocumentDto,
    guard: &target_identity::ExternalChangeGuard,
) -> i32 {
    let target_path = PathBuf::from(target);
    if !target_identity::is_same_file(&target_path, allowed_target) {
        return respond(
            output,
            PrivilegedResponse::Error {
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
            EXIT_OPERATION_ERROR,
        );
    }

    // The authoritative external-change check: re-read the verified target
    // *in this process*, now that the authentication wait is over, and refuse
    // rather than overwrite content that appeared while it was on screen.
    // Nothing has touched the disk at this point, which is why the error can
    // promise `target_unchanged`.
    if let Some(change) = guard.detect(&target_path) {
        return respond(
            output,
            PrivilegedResponse::Error {
                error: WriteErrorDto::external_change(
                    &target_path,
                    &change.last_known,
                    &change.current,
                ),
            },
            EXIT_OPERATION_ERROR,
        );
    }

    // One call, one process, one authorization: validation, backup, temp
    // file, fsyncs, rename and read-back verification all happen inside here.
    // The plain entry point on purpose: whichever way this process was
    // launched - an ordinary install's `pkexec` child, or `rustab-helper`
    // started by `flatpak-spawn --host pkexec` - it is already on the host
    // (no `/.flatpak-info`), so its own `findmnt --verify` and existence
    // checks see the real system.
    let candidate = Document::from(candidate);
    match writer::write_document(&target_path, &candidate) {
        Ok(outcome) => respond(
            output,
            PrivilegedResponse::Ok {
                // `false`: this process cannot know whether it was launched
                // through `pkexec` or directly. The unprivileged side, which
                // does know, overwrites this.
                outcome: WriteOutcomeDto::from_outcome(&outcome, false),
            },
            EXIT_OK,
        ),
        Err(err) => respond(
            output,
            PrivilegedResponse::Error {
                error: WriteErrorDto::from(&err),
            },
            EXIT_OPERATION_ERROR,
        ),
    }
}

/// The elevated backup deletion, in full: refuse any target but
/// `allowed_target`, resolve `backup_path` against *that* target's real
/// backup list, and unlink exactly the path the list yielded.
///
/// Two checks, then one syscall, neither trusting the unprivileged side:
///
/// 1. `target` must pass the same [`target_identity::is_same_file`] test
///    [`handle_write`] applies.
/// 2. `backup_path` must resolve, through [`backups::resolve_requested`]
///    *here*, to one of that target's actual backups - the path unlinked
///    is the one the backup list yielded, never the request's raw string.
///
/// No backup-of-the-backup, no temp file, no rename: this removes a
/// backup, not the target, so none of `write_document`'s atomicity
/// guarantees apply. The target file itself is never opened.
fn handle_delete_backup<W: Write>(
    output: &mut W,
    allowed_target: &Path,
    target: &str,
    backup_path: &str,
) -> i32 {
    let target_path = PathBuf::from(target);
    if !target_identity::is_same_file(&target_path, allowed_target) {
        return respond(
            output,
            PrivilegedResponse::BackupDeleteFailed {
                error: DeleteBackupErrorDto::new(
                    DeleteBackupErrorKindDto::Delete,
                    format!(
                        "the elevated helper only deletes backups of {}; it refused to delete \
                         anything belonging to {}",
                        allowed_target.display(),
                        target_path.display()
                    ),
                ),
            },
            EXIT_OPERATION_ERROR,
        );
    }

    let backup = match backups::resolve_requested(&target_path, backup_path) {
        Ok(backup) => backup,
        Err(err) => {
            return respond(
                output,
                PrivilegedResponse::BackupDeleteFailed {
                    error: DeleteBackupErrorDto::new(
                        DeleteBackupErrorKindDto::InvalidBackup,
                        format!("{err}; nothing was deleted"),
                    ),
                },
                EXIT_OPERATION_ERROR,
            );
        }
    };

    match fs::remove_file(&backup) {
        Ok(()) => respond(output, PrivilegedResponse::BackupDeleted, EXIT_OK),
        Err(err) => respond(
            output,
            PrivilegedResponse::BackupDeleteFailed {
                error: DeleteBackupErrorDto::new(
                    DeleteBackupErrorKindDto::Delete,
                    format!("could not delete the backup {}: {err}", backup.display()),
                ),
            },
            EXIT_OPERATION_ERROR,
        ),
    }
}

/// The elevated manual backup, in full: refuse any target but
/// `allowed_target`, then call [`writer::create_backup`] on it.
///
/// One check, then one call - the same "nothing happens before the check
/// passes" shape, and the same [`target_identity::is_same_file`] identity
/// test, as [`handle_write`] and [`handle_delete_backup`].
///
/// Nothing else to check: [`writer::create_backup`] derives the backup's
/// path from the verified target's own name, so there's no destination to
/// get pointed elsewhere, and it only reads the target, so unlike
/// [`handle_write`] there's no atomicity ceremony needed.
fn handle_create_backup<W: Write>(output: &mut W, allowed_target: &Path, target: &str) -> i32 {
    let target_path = PathBuf::from(target);
    if !target_identity::is_same_file(&target_path, allowed_target) {
        return respond(
            output,
            PrivilegedResponse::BackupCreateFailed {
                error: CreateBackupErrorDto::new(
                    CreateBackupErrorKindDto::InvalidTarget,
                    format!(
                        "the elevated helper only backs up {}; it refused to back up {}",
                        allowed_target.display(),
                        target_path.display()
                    ),
                ),
            },
            EXIT_OPERATION_ERROR,
        );
    }

    match writer::create_backup(&target_path) {
        Ok(outcome) => respond(
            output,
            PrivilegedResponse::BackupCreated {
                outcome: CreateBackupOutcomeDto::from_outcome(&outcome),
            },
            EXIT_OK,
        ),
        Err(err) => respond(
            output,
            PrivilegedResponse::BackupCreateFailed {
                error: CreateBackupErrorDto::from(&err),
            },
            EXIT_OPERATION_ERROR,
        ),
    }
}

/// The protocol's error body for a request that was never read at all -
/// stray arguments, and nothing else.
///
/// A plain `String` rather than a [`PrivilegedResponse`] so the caller needs
/// no serializer of its own: `rustab-helper` depends on this crate and
/// nothing else, deliberately. The body is the same write-shaped
/// [`PrivilegedResponse::Error`] a framing failure produces, with
/// `target_unchanged` set - a process that refused its own argv has not
/// touched a file.
///
/// Serialization of this value cannot fail (three owned fields, no maps
/// with non-string keys), so an unreachable fallback is spelled out inline
/// rather than surfaced as a `Result` every caller would have to handle in
/// a branch that can never run.
pub fn protocol_error_body(message: &str) -> String {
    let response = PrivilegedResponse::Error {
        error: WriteErrorDto::new(WriteErrorKindDto::Internal, true, message),
    };
    serde_json::to_string(&response).unwrap_or_else(|_| {
        r#"{"status":"error","error":{"kind":"internal","target_unchanged":true,"message":"the elevated helper could not serialize its own error"}}"#
            .to_string()
    })
}

/// Serializes `response` to `output` and returns `code`.
///
/// A failure to even print the response is reported on stderr and turned into
/// [`EXIT_PROTOCOL_ERROR`], so the unprivileged side sees "the helper
/// produced no usable result" rather than a success it cannot substantiate.
fn respond<W: Write>(output: &mut W, response: PrivilegedResponse, code: i32) -> i32 {
    match serde_json::to_string(&response) {
        Ok(json) => match writeln!(output, "{json}").and_then(|()| output.flush()) {
            Ok(()) => code,
            Err(err) => {
                eprintln!("could not write the elevated helper's response: {err}");
                EXIT_PROTOCOL_ERROR
            }
        },
        Err(err) => {
            eprintln!("could not serialize the elevated helper's response: {err}");
            EXIT_PROTOCOL_ERROR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    /// Runs the elevated writer exactly as the helper binary does, but with
    /// an in-memory stdin/stdout and a temp file standing in for
    /// `/etc/fstab`.
    fn run_helper(
        target: &Path,
        allowed_target: &Path,
        candidate: &Document,
    ) -> (i32, PrivilegedResponse) {
        // No record of the file's previous content, which is the one guard
        // that disables the external-change check entirely - so these tests
        // exercise exactly the pre-check behaviour of the helper. The guarded
        // variants have their own helper below.
        run_helper_guarded(
            target,
            allowed_target,
            candidate,
            &target_identity::ExternalChangeGuard::unchecked(),
        )
    }

    /// [`run_helper`] with an explicit
    /// [`target_identity::ExternalChangeGuard`], for the external-change
    /// tests: the request carries what the unprivileged side claims the
    /// target held, and the helper re-reads the file itself.
    fn run_helper_guarded(
        target: &Path,
        allowed_target: &Path,
        candidate: &Document,
        guard: &target_identity::ExternalChangeGuard,
    ) -> (i32, PrivilegedResponse) {
        let request = PrivilegedRequest::Write {
            target: target.to_string_lossy().into_owned(),
            candidate: DocumentDto::from(candidate),
            last_known_real_content: guard.last_known_real_content.clone(),
            allow_external_change: guard.allow_external_change,
        };
        let payload = serde_json::to_vec(&request).expect("the request must serialize");
        let mut stdin = payload.as_slice();
        let mut stdout: Vec<u8> = Vec::new();
        let code = handle_privileged_request(&mut stdin, &mut stdout, allowed_target);
        let text = String::from_utf8(stdout).expect("the response must be UTF-8");
        let response =
            serde_json::from_str(text.trim()).expect("the response must be a single JSON object");
        (code, response)
    }

    /// The core claim of this module: given a request on stdin, the helper
    /// performs the *entire* `write_document` operation - backup included -
    /// and reports it as JSON.
    #[test]
    fn helper_performs_the_whole_write_document_operation_from_stdin() {
        let dir = TempDir::new("helper-ok");
        let target = dir.join("fstab");
        fs::write(&target, ORIGINAL).expect("seed the target");

        let candidate = valid_candidate("/mnt/elevated");
        let (code, response) = run_helper(&target, &target, &candidate);

        assert_eq!(code, EXIT_OK);
        let PrivilegedResponse::Ok { outcome } = response else {
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
        let PrivilegedResponse::Ok { outcome } = response else {
            panic!("the write should have succeeded: {response:?}");
        };
        assert!(outcome.backup.is_none());
        assert_eq!(
            fs::read_to_string(&target).expect("read the target"),
            candidate.to_string()
        );
    }

    /// Validation happens *inside* the helper, before anything is written -
    /// the unprivileged side's pre-check is not what protects the file.
    #[test]
    fn helper_refuses_an_invalid_candidate_and_leaves_the_target_untouched() {
        let dir = TempDir::new("helper-invalid");
        let target = dir.join("fstab");
        fs::write(&target, ORIGINAL).expect("seed the target");

        let (code, response) = run_helper(&target, &target, &invalid_candidate());

        assert_eq!(code, EXIT_OPERATION_ERROR);
        let PrivilegedResponse::Error { error } = response else {
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

        assert_eq!(code, EXIT_OPERATION_ERROR);
        let PrivilegedResponse::Error { error } = response else {
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
        let code = handle_privileged_request(&mut stdin, &mut stdout, &target);

        assert_eq!(code, EXIT_PROTOCOL_ERROR);
        let text = String::from_utf8(stdout).expect("UTF-8");
        let response: PrivilegedResponse =
            serde_json::from_str(text.trim()).expect("a JSON response is still produced");
        let PrivilegedResponse::Error { error } = response else {
            panic!("a protocol error must not be reported as success");
        };
        assert_eq!(error.kind, WriteErrorKindDto::Internal);
        assert!(error.target_unchanged);
        assert_eq!(
            fs::read_to_string(&target).expect("read the target"),
            ORIGINAL
        );
    }

    // -- the authoritative external-change check -----------------------------
    //
    // These exercise the real `handle_privileged_request` against a temp file
    // standing in for `/etc/fstab`, which is the point of its `allowed_target`
    // parameter: the check under test genuinely *reads* the target, and
    // `cargo test` must never read the machine's own file.

    /// The check that actually protects the file: the request says what the
    /// unprivileged side last knew, the helper re-reads the target *itself*,
    /// finds something else, and refuses - leaving the file byte-for-byte as
    /// it was, with no backup and no temp file beside it.
    ///
    /// This is the case a check in the unprivileged parent alone cannot cover:
    /// the content below appeared while `pkexec` was waiting for a password.
    #[test]
    fn helper_refuses_a_write_when_the_target_changed_since_the_request_was_built() {
        let dir = TempDir::new("helper-external-change");
        let target = dir.join("fstab");
        fs::write(&target, "# written by someone else, during the prompt\n")
            .expect("seed the target");

        let (code, response) = run_helper_guarded(
            &target,
            &target,
            &valid_candidate("/mnt/blocked"),
            &target_identity::ExternalChangeGuard {
                last_known_real_content: Some(ORIGINAL.to_string()),
                allow_external_change: false,
            },
        );

        assert_eq!(code, EXIT_OPERATION_ERROR);
        let PrivilegedResponse::Error { error } = response else {
            panic!("the write should have been refused: {response:?}");
        };
        assert_eq!(error.kind, WriteErrorKindDto::ExternalChange);
        assert!(
            error.target_unchanged,
            "nothing was opened for writing, so this is a guarantee"
        );
        let change = error
            .external_change
            .as_deref()
            .expect("the diff payload must be attached");
        assert_eq!(change.last_known_content, ORIGINAL);
        assert_eq!(
            change.current_content, "# written by someone else, during the prompt\n",
            "the frontend builds its diff out of these two"
        );
        assert_eq!(
            fs::read_to_string(&target).expect("read the target"),
            "# written by someone else, during the prompt\n",
            "the external content must survive untouched"
        );
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
        assert_eq!(
            leftovers,
            vec!["fstab".to_string()],
            "not even a backup or a temp file may be created by a refused write"
        );
    }

    /// The same request against a target that still holds what it claimed:
    /// the write goes through, exactly as it did before the check existed.
    #[test]
    fn helper_writes_when_the_target_still_holds_what_the_request_expected() {
        let dir = TempDir::new("helper-no-external-change");
        let target = dir.join("fstab");
        fs::write(&target, ORIGINAL).expect("seed the target");

        let candidate = valid_candidate("/mnt/allowed");
        let (code, response) = run_helper_guarded(
            &target,
            &target,
            &candidate,
            &target_identity::ExternalChangeGuard {
                last_known_real_content: Some(ORIGINAL.to_string()),
                allow_external_change: false,
            },
        );

        assert_eq!(code, EXIT_OK);
        assert!(
            matches!(response, PrivilegedResponse::Ok { .. }),
            "{response:?}"
        );
        assert_eq!(
            fs::read_to_string(&target).expect("read the target"),
            candidate.to_string()
        );
    }

    /// "Write anyway": the user has seen the diff and asked for it, so the
    /// helper performs the full ordinary write over the changed content -
    /// whose previous bytes `write_document` backs up on the way, as it does
    /// for any other save.
    #[test]
    fn helper_writes_over_an_external_change_when_the_user_allowed_it() {
        let dir = TempDir::new("helper-write-anyway");
        let target = dir.join("fstab");
        fs::write(&target, "# someone else's content\n").expect("seed the target");

        let candidate = valid_candidate("/mnt/anyway");
        let (code, response) = run_helper_guarded(
            &target,
            &target,
            &candidate,
            &target_identity::ExternalChangeGuard {
                last_known_real_content: Some(ORIGINAL.to_string()),
                allow_external_change: true,
            },
        );

        assert_eq!(code, EXIT_OK);
        let PrivilegedResponse::Ok { outcome } = response else {
            panic!("an allowed write must succeed: {response:?}");
        };
        assert_eq!(
            fs::read_to_string(&target).expect("read the target"),
            candidate.to_string()
        );
        assert_eq!(
            fs::read_to_string(outcome.backup.expect("a backup must still be taken"))
                .expect("read the backup"),
            "# someone else's content\n",
            "the overwritten external content stays recoverable, as for any save"
        );
    }

    /// Ordering: the identity check comes first and is unconditional. A
    /// request for the wrong target is refused as such even when it also
    /// carries a stale expectation, so a drifted guard can never be used to
    /// get a different error (or a read) for a file this process may not
    /// touch.
    #[test]
    fn the_identity_check_still_comes_first_for_a_drifted_request() {
        let dir = TempDir::new("helper-wrong-target-drifted");
        let decoy = dir.join("somewhere-else");
        fs::write(&decoy, "# must not be touched\n").expect("seed the decoy");
        let allowed = dir.join("fstab");

        let (code, response) = run_helper_guarded(
            &decoy,
            &allowed,
            &valid_candidate("/mnt/nope"),
            &target_identity::ExternalChangeGuard {
                last_known_real_content: Some(ORIGINAL.to_string()),
                allow_external_change: false,
            },
        );

        assert_eq!(code, EXIT_OPERATION_ERROR);
        let PrivilegedResponse::Error { error } = response else {
            panic!("the write should have been refused: {response:?}");
        };
        assert_eq!(error.kind, WriteErrorKindDto::InvalidTarget);
        assert_eq!(
            fs::read_to_string(&decoy).expect("read the decoy"),
            "# must not be touched\n"
        );
    }

    /// The request shape is the DTO shape, so a candidate survives the trip
    /// through JSON exactly - including malformed lines and trailing
    /// comments, which a text-based protocol could silently reinterpret. It
    /// also carries what the parent last knew the target to hold, which the
    /// elevated side needs in order to re-check it.
    #[test]
    fn request_round_trips_the_candidate_exactly() {
        let candidate = parser::parse(
            "# header\n\
             \n\
             UUID=11111111-2222-3333-4444-555555555555 /mnt/data ext4 noauto,x-systemd.automount= 0 2 # note\n\
             /dev/sdb1\n",
        );
        let request = PrivilegedRequest::Write {
            target: "/etc/fstab".to_string(),
            candidate: DocumentDto::from(&candidate),
            last_known_real_content: Some("# what the parent last knew\n".to_string()),
            allow_external_change: false,
        };
        let json = serde_json::to_string(&request).expect("serialize");
        let back: PrivilegedRequest = serde_json::from_str(&json).expect("deserialize");
        let PrivilegedRequest::Write {
            candidate: back,
            last_known_real_content,
            allow_external_change,
            ..
        } = back
        else {
            panic!("a write request must deserialize as a write request: {back:?}");
        };
        assert_eq!(Document::from(back), candidate);
        assert_eq!(
            last_known_real_content.as_deref(),
            Some("# what the parent last knew\n"),
            "the expectation the elevated side re-checks must survive the trip verbatim"
        );
        assert!(!allow_external_change);
    }

    /// The tagged union: each operation is discriminated by `op`, so the one
    /// entry point can carry any of them and none can be mistaken for another.
    #[test]
    fn all_operations_are_discriminated_by_the_op_tag() {
        let write = PrivilegedRequest::Write {
            target: "/etc/fstab".to_string(),
            candidate: DocumentDto::from(&valid_candidate("/mnt/d")),
            last_known_real_content: None,
            allow_external_change: false,
        };
        let delete = PrivilegedRequest::DeleteBackup {
            target: "/etc/fstab".to_string(),
            backup_path: "/etc/fstab.rustab-backup.0000000001.000000002".to_string(),
        };
        let create = PrivilegedRequest::CreateBackup {
            target: "/etc/fstab".to_string(),
        };

        let write_json = serde_json::to_string(&write).expect("serialize");
        assert!(write_json.starts_with(r#"{"op":"write","#), "{write_json}");
        assert_eq!(
            serde_json::to_string(&delete).expect("serialize"),
            r#"{"op":"delete_backup","target":"/etc/fstab","backup_path":"/etc/fstab.rustab-backup.0000000001.000000002"}"#
        );
        assert_eq!(
            serde_json::to_string(&create).expect("serialize"),
            r#"{"op":"create_backup","target":"/etc/fstab"}"#
        );

        for request in [write, delete, create] {
            let json = serde_json::to_string(&request).expect("serialize");
            let back: PrivilegedRequest = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, request);
        }
    }

    // -- the elevated backup deletion ---------------------------------------

    /// Creates `count` real backups of `target` by performing `count` real
    /// saves, so the names under test are the ones `fstab-core` actually
    /// produces rather than ones this test invented.
    fn seed_backups(target: &Path, count: usize) {
        fs::write(target, ORIGINAL).expect("seed the target");
        for i in 0..count {
            writer::write_document(target, &valid_candidate(&format!("/mnt/save-{i}")))
                .expect("the save should succeed");
        }
    }

    /// The paths `writer::list_backups` reports for `target`, newest last.
    fn backup_paths(target: &Path) -> Vec<PathBuf> {
        writer::list_backups(target)
            .expect("listing the backups must succeed")
            .into_iter()
            .map(|entry| entry.path)
            .collect()
    }

    /// Runs the elevated *deleter* exactly as the helper binary does, but
    /// with an in-memory stdin/stdout and a temp file standing in for
    /// `/etc/fstab`.
    fn run_delete_helper(
        target: &Path,
        allowed_target: &Path,
        backup_path: &str,
    ) -> (i32, PrivilegedResponse) {
        let request = PrivilegedRequest::DeleteBackup {
            target: target.to_string_lossy().into_owned(),
            backup_path: backup_path.to_string(),
        };
        let payload = serde_json::to_vec(&request).expect("the request must serialize");
        let mut stdin = payload.as_slice();
        let mut stdout: Vec<u8> = Vec::new();
        let code = handle_privileged_request(&mut stdin, &mut stdout, allowed_target);
        let text = String::from_utf8(stdout).expect("the response must be UTF-8");
        let response =
            serde_json::from_str(text.trim()).expect("the response must be a single JSON object");
        (code, response)
    }

    /// The core claim of the delete operation: the named backup is gone, and
    /// *nothing else* is - most importantly not the target file itself, which
    /// this operation never opens.
    #[test]
    fn helper_deletes_exactly_the_named_backup_and_leaves_the_target_untouched() {
        let dir = TempDir::new("helper-delete-ok");
        let target = dir.join("fstab");
        seed_backups(&target, 3);
        let before = backup_paths(&target);
        assert_eq!(before.len(), 3);
        let target_content = fs::read_to_string(&target).expect("read the target");

        let doomed = &before[1];
        let (code, response) = run_delete_helper(&target, &target, &doomed.to_string_lossy());

        assert_eq!(code, EXIT_OK);
        assert_eq!(
            response,
            PrivilegedResponse::BackupDeleted,
            "the delete should have succeeded: {response:?}"
        );

        assert!(!doomed.exists(), "the named backup must be gone");
        let after = backup_paths(&target);
        assert_eq!(after, vec![before[0].clone(), before[2].clone()]);
        // The target file is provably untouched: same bytes, and still there.
        assert_eq!(
            fs::read_to_string(&target).expect("read the target"),
            target_content,
            "deleting a backup must not touch the target file"
        );
        // Nothing else appeared either - no temp file, no backup of a backup.
        let mut leftovers: Vec<String> = fs::read_dir(dir.path())
            .expect("read the temp dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        leftovers.sort();
        let mut expected: Vec<String> = vec!["fstab".to_string()];
        expected.extend(
            after
                .iter()
                .map(|path| path.file_name().unwrap().to_string_lossy().into_owned()),
        );
        expected.sort();
        assert_eq!(leftovers, expected);
    }

    /// **The single most important test in the delete path.**
    ///
    /// The elevated process runs as root, so the only thing standing between a
    /// `backup_path` that arrived as *data* and an arbitrary `unlink()` as root
    /// is the re-resolution inside `handle_delete_backup`. This asserts that
    /// guarantee against the elevated handler itself - not against
    /// `backups::resolve_requested` in isolation, which is separately tested -
    /// using the same near-miss list `backups.rs` uses: a different target's
    /// backup in the same directory, names that *almost* match the scheme, a
    /// path outside the directory entirely, path traversal, the empty string,
    /// and the target file itself.
    #[test]
    fn helper_refuses_to_delete_anything_that_is_not_a_backup_of_the_allowed_target() {
        let dir = TempDir::new("helper-delete-refuse");
        let target = dir.join("fstab");
        seed_backups(&target, 1);
        let real_backup = backup_paths(&target)[0].clone();

        // A different target's backup, sitting in the same directory.
        let other_target = dir.join("fstab.other");
        seed_backups(&other_target, 1);
        let other_backup = backup_paths(&other_target)[0].clone();

        // Files in the right directory whose names are *almost* a backup of
        // this target: a wrong timestamp width, a non-numeric timestamp, an
        // extra suffix, and a plain hand-made name.
        let near_misses = [
            "fstab.rustab-backup.1.2",
            "fstab.rustab-backup.0000000001.000000002.old",
            "fstab.rustab-backup.aaaaaaaaaa.bbbbbbbbb",
            "fstab.bak",
        ];
        for name in near_misses {
            fs::write(dir.join(name), "# not a Rustab backup\n").expect("write the near miss");
        }

        let mut refused: Vec<String> = vec![
            other_backup.to_string_lossy().into_owned(),
            "/etc/shadow".to_string(),
            "../../etc/fstab".to_string(),
            String::new(),
            // The target itself is never a backup of itself: the delete
            // operation must not be a way to unlink /etc/fstab.
            target.to_string_lossy().into_owned(),
        ];
        refused.extend(
            near_misses
                .iter()
                .map(|name| dir.join(name).to_string_lossy().into_owned()),
        );
        // Path traversal that lands back inside the directory but names the
        // target rather than a backup.
        refused.push(dir.join("sub/../fstab").to_string_lossy().into_owned());

        for requested in &refused {
            let (code, response) = run_delete_helper(&target, &target, requested);
            assert_eq!(
                code, EXIT_OPERATION_ERROR,
                "{requested} must be refused, not deleted"
            );
            let PrivilegedResponse::BackupDeleteFailed { error } = response else {
                panic!("{requested} must be refused: {response:?}");
            };
            assert_eq!(
                error.kind,
                DeleteBackupErrorKindDto::InvalidBackup,
                "unexpected kind for {requested}: {error:?}"
            );
            assert!(error.message.contains("nothing was deleted"));
        }

        // Nothing on disk moved: every file the test created is still there.
        assert!(target.exists(), "the target must still exist");
        assert!(other_target.exists());
        assert!(other_backup.exists(), "another target's backup is not ours");
        assert!(
            real_backup.exists(),
            "the one real backup is still untouched"
        );
        for name in near_misses {
            assert!(dir.join(name).exists(), "{name} must still exist");
        }
        assert!(
            Path::new("/etc/shadow").exists() || !Path::new("/etc").exists(),
            "sanity: the refusal must not have been a successful unlink of /etc/shadow"
        );
    }

    /// The target arrives as data for the delete operation too, so the helper
    /// re-checks it: backups of anything but its one allowed target are
    /// refused, and nothing is unlinked.
    #[test]
    fn helper_refuses_a_delete_for_a_target_other_than_the_allowed_one() {
        let dir = TempDir::new("helper-delete-wrong-target");
        let decoy = dir.join("somewhere-else");
        seed_backups(&decoy, 1);
        let decoy_backup = backup_paths(&decoy)[0].clone();
        let allowed = dir.join("fstab");
        seed_backups(&allowed, 1);

        let (code, response) = run_delete_helper(&decoy, &allowed, &decoy_backup.to_string_lossy());

        assert_eq!(code, EXIT_OPERATION_ERROR);
        let PrivilegedResponse::BackupDeleteFailed { error } = response else {
            panic!("the delete should have been refused: {response:?}");
        };
        assert_eq!(error.kind, DeleteBackupErrorKindDto::Delete);
        assert!(
            error
                .message
                .contains(&allowed.to_string_lossy().to_string()),
            "the message must name the one target it does manage: {error:?}"
        );
        assert!(
            decoy_backup.exists(),
            "a backup of a target the helper may not manage must survive"
        );
        assert!(decoy.exists());
        assert_eq!(
            backup_paths(&allowed).len(),
            1,
            "and so must the allowed target's"
        );
    }

    /// A removal that fails for an ordinary reason (here: a directory with no
    /// write permission) is reported as `delete`, with the backup still there.
    ///
    /// Returns early when running as root, where `unlink` ignores directory
    /// permissions and the failure cannot be provoked. The repo's testing
    /// rules say never to run these as root anyway (`CONTRIBUTING.md`), so in
    /// practice this always exercises the branch.
    #[test]
    fn helper_reports_a_removal_that_fails_for_another_reason() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new("helper-delete-ro");
        let holder = dir.join("ro");
        fs::create_dir(&holder).expect("create the holder directory");
        let target = holder.join("fstab");
        seed_backups(&target, 1);
        let backup = backup_paths(&target)[0].clone();

        // r-x: the backup list can still be read, but nothing can be unlinked.
        fs::set_permissions(&holder, fs::Permissions::from_mode(0o500))
            .expect("drop write permission");
        let (code, response) = run_delete_helper(&target, &target, &backup.to_string_lossy());
        fs::set_permissions(&holder, fs::Permissions::from_mode(0o700))
            .expect("restore write permission so the temp dir can be cleaned up");

        if code == EXIT_OK {
            return; // running as root: directory permissions do not apply
        }
        assert_eq!(code, EXIT_OPERATION_ERROR);
        let PrivilegedResponse::BackupDeleteFailed { error } = response else {
            panic!("the delete should have failed: {response:?}");
        };
        assert_eq!(error.kind, DeleteBackupErrorKindDto::Delete);
        assert!(
            error
                .message
                .contains(&backup.to_string_lossy().to_string()),
            "the message must name the backup: {error:?}"
        );
        assert!(backup.exists(), "a failed delete must leave the backup");
    }

    // -- the elevated manual backup -----------------------------------------

    /// Runs the elevated *backup creator* exactly as the helper binary does,
    /// but with an in-memory stdin/stdout and a temp file standing in for
    /// `/etc/fstab`.
    fn run_create_helper(target: &Path, allowed_target: &Path) -> (i32, PrivilegedResponse) {
        let request = PrivilegedRequest::CreateBackup {
            target: target.to_string_lossy().into_owned(),
        };
        let payload = serde_json::to_vec(&request).expect("the request must serialize");
        let mut stdin = payload.as_slice();
        let mut stdout: Vec<u8> = Vec::new();
        let code = handle_privileged_request(&mut stdin, &mut stdout, allowed_target);
        let text = String::from_utf8(stdout).expect("the response must be UTF-8");
        let response =
            serde_json::from_str(text.trim()).expect("the response must be a single JSON object");
        (code, response)
    }

    /// The core claim of the create operation: a manual backup of the target's
    /// current content appears beside it, and the target itself is not written
    /// to at all - no new content, no temp file, no rename.
    #[test]
    fn helper_creates_a_manual_backup_and_never_writes_the_target() {
        let dir = TempDir::new("helper-create-backup");
        let target = dir.join("fstab");
        fs::write(&target, ORIGINAL).expect("seed the target");

        let (code, response) = run_create_helper(&target, &target);

        assert_eq!(code, EXIT_OK);
        let PrivilegedResponse::BackupCreated { outcome } = response else {
            panic!("the backup should have been created: {response:?}");
        };
        assert_eq!(outcome.target, target.to_string_lossy());
        assert!(
            outcome.backup.ends_with(".manual"),
            "an explicitly requested backup is a manual one: {outcome:?}"
        );
        assert_eq!(
            fs::read_to_string(&outcome.backup).expect("read the backup"),
            ORIGINAL
        );
        assert!(outcome.pruned_backups.is_empty());

        // The target is byte-for-byte what it was.
        assert_eq!(
            fs::read_to_string(&target).expect("read the target"),
            ORIGINAL
        );
        // And nothing else was created - in particular no temp file.
        let mut leftovers: Vec<String> = fs::read_dir(dir.path())
            .expect("read the temp dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        leftovers.sort();
        let mut expected = vec![
            "fstab".to_string(),
            Path::new(&outcome.backup)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        ];
        expected.sort();
        assert_eq!(leftovers, expected);
    }

    /// The target arrives as data for this operation too, so the helper
    /// re-checks it: a request naming anything but its one allowed target is
    /// refused, and nothing is created anywhere.
    #[test]
    fn helper_refuses_a_create_backup_for_a_target_other_than_the_allowed_one() {
        let dir = TempDir::new("helper-create-wrong-target");
        let decoy = dir.join("somewhere-else");
        fs::write(&decoy, "# must not be copied\n").expect("seed the decoy");
        let allowed = dir.join("fstab");
        fs::write(&allowed, ORIGINAL).expect("seed the allowed target");

        let (code, response) = run_create_helper(&decoy, &allowed);

        assert_eq!(code, EXIT_OPERATION_ERROR);
        let PrivilegedResponse::BackupCreateFailed { error } = response else {
            panic!("the backup should have been refused: {response:?}");
        };
        assert_eq!(error.kind, CreateBackupErrorKindDto::InvalidTarget);
        assert!(
            error
                .message
                .contains(&allowed.to_string_lossy().to_string()),
            "the message must name the one target it does manage: {error:?}"
        );
        // Neither file gained a backup, and the decoy is untouched.
        assert!(backup_paths(&decoy).is_empty());
        assert!(backup_paths(&allowed).is_empty());
        assert_eq!(
            fs::read_to_string(&decoy).expect("read the decoy"),
            "# must not be copied\n"
        );
    }

    /// "There is nothing to back up yet" survives the protocol as its own
    /// kind, rather than arriving at the UI as a generic I/O failure.
    #[test]
    fn helper_reports_a_missing_target_as_target_missing() {
        let dir = TempDir::new("helper-create-missing");
        let target = dir.join("fstab");

        let (code, response) = run_create_helper(&target, &target);

        assert_eq!(code, EXIT_OPERATION_ERROR);
        let PrivilegedResponse::BackupCreateFailed { error } = response else {
            panic!("a missing target must be an error: {response:?}");
        };
        assert_eq!(error.kind, CreateBackupErrorKindDto::TargetMissing);
        assert!(
            fs::read_dir(dir.path())
                .expect("read the temp dir")
                .next()
                .is_none(),
            "nothing may have been created"
        );
    }
}
