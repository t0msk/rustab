//! The elevated operations: `pkexec` re-exec on the unprivileged side, hidden
//! internal subcommand on the elevated side - see `docs/SPEC.md` §6 step 6
//! and `docs/API_CONTRACT.md` §4.
//!
//! ## The three operations, and why there is still only one flag
//!
//! Three things in Rustab need root, and only when the active target is the
//! real `/etc/fstab`: **writing the target** (a save or a restore),
//! **deleting one of the target's own backups**, and **creating a manual
//! backup** of it. The latter two are about files in `/etc/`, next to the
//! target, which an unprivileged process can neither unlink nor create.
//!
//! They share one dispatch point. The request on stdin is a union tagged by
//! `op`, so [`run_internal_if_requested`] still checks exactly one hidden
//! argument, still reads stdin to EOF exactly once, still writes exactly one
//! JSON object to stdout and still returns one exit code - for any of the
//! three. That matters because the security property this module claims is
//! "when the flag is present, no Tauri runtime, webview, plugin or logger is
//! ever initialized", and that claim is only as auditable as the number of
//! places that can turn this process into a root process. There is one. A
//! second flag would have made it two, and a third would have made it three.
//!
//! Everything below about the write operation is unchanged; the other two are
//! deliberately tiny by comparison (resolve then `remove_file`; resolve then
//! copy), because neither writes the target and none of
//! [`writer::write_document`]'s guarantees apply to them or are needed for
//! them. In particular [`writer::create_backup`] never opens the target for
//! writing at all - it only reads it - so the elevated backup operation
//! cannot damage `/etc/fstab` even in principle.
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
//! [`fstab_core::writer::write_document`] - the whole §6 algorithm
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
//!   runtime, the webview, the logger and the plugins are never initialized -
//!   the elevated process reads stdin, calls one function, prints JSON, and
//!   exits. The flag is matched as `argv[1]` exactly, and extra arguments are
//!   refused.
//! - **The helper would need the same authorization anyway.** `pkexec` prompts
//!   per invocation for either design, so nothing about the user experience
//!   or the trust model improves.
//!
//! ## Where that re-exec happens, inside a Flatpak sandbox (T40)
//!
//! One thing about the mechanism - and *only* this one thing - differs when
//! Rustab runs as a Flatpak: the `pkexec` invocation has to be made on the
//! **host**, via `flatpak-spawn --host`, and it has to name this binary by its
//! host-side path rather than by `std::env::current_exe()`. The reason is
//! worth stating precisely, because the obvious reading of `pkexec` is wrong:
//! `pkexec` changes a process's **UID**, not its **mount namespace**, so a
//! `pkexec` child started from inside the sandbox is root *in the sandbox* -
//! and the `/etc` a sandboxed process sees is the runtime's own, with no path
//! at all to the host's `/etc/fstab` (Flatpak reserves `/etc` and refuses even
//! an explicit `--filesystem=/etc/fstab:rw` grant). Such a process would be
//! authorized to write a file that is not the one the user is editing.
//!
//! `flatpak-spawn --host pkexec <host path> --internal-privileged-write`
//! produces what is actually needed: a genuine host process, outside every
//! sandbox, running as root. All of it lives in [`crate::host`]; the only line
//! in this module that knows about it is the one in [`run_elevated`] that asks
//! for the plan. Nothing else here changes - not this module's protocol, not
//! its dispatch, not one of its `handle_*` or `interpret_*` functions - and
//! the single-flag property below is untouched: there is still exactly one
//! argument, in one binary, that can turn a process into a root process.
//!
//! ## The target path, and why the child re-checks it
//!
//! `pkexec` deliberately does not forward the caller's environment, so the
//! child cannot re-resolve the target itself - `RUSTAB_TARGET_PATH` does not
//! survive the transition (that is a feature: it means an env var cannot
//! influence the elevated process at all). The target therefore travels
//! inside the request.
//!
//! Because the path arrives as data, the child refuses to act on anything
//! except the real `/etc/fstab` (literally, or via a symlink that resolves to
//! it). Escalation only ever happens when the unprivileged side is acting on
//! the real file - which since T34/T35 means `is_real || source ==
//! SandboxDefault`, not `is_real` alone (a default launch writes `/etc/fstab`
//! and keeps its backups beside it, while resolving to a draft of its own;
//! see [`crate::commands`]'s `save_document_with` and `backup_target_path`) -
//! and in every one of those cases the path handed to this helper is the
//! hardcoded [`target::REAL_FSTAB_PATH`], never a draft path. So this costs
//! nothing in normal operation, and it means
//! a bug on the unprivileged side cannot turn the elevated write into "write
//! arbitrary content to an arbitrary root-owned path". The allowed target is a
//! parameter of [`handle_privileged_request`] purely so tests exercise the
//! real code path against a temp file instead of needing a bypass switch.
//!
//! The delete operation's `backup_path` arrives as data for the same reason,
//! and is re-checked the same way, with one extra step: the child resolves it
//! through [`crate::backups::resolve_requested`] against the real backup list
//! of the target it has *already* verified, so the only files it can ever
//! unlink are that one target's own backups. A bug (or a lie) on the
//! unprivileged side cannot turn the elevated delete into "unlink an arbitrary
//! root-owned path" - not `/etc/shadow`, not a different target's backup, not
//! the target itself.
//!
//! ## The external-change check lives in the child too (T36)
//!
//! For the same structural reason, and it is worth stating plainly: a write
//! request carries what the unprivileged side believed `/etc/fstab` held (its
//! last-known-real marker's content - see [`target::ExternalChangeGuard`]),
//! and the elevated side re-reads the file and compares, immediately before
//! [`writer::write_document`]. The reason it cannot be checked only on the
//! unprivileged side is this module's own mechanism: `pkexec` blocks on a
//! human typing a password, so the interval between "the parent looked" and
//! "the write happens" contains an arbitrarily long, human-paced wait during
//! which anything else can write the file. The parent's check (in
//! `crate::commands::save_document_with`) is a courtesy that saves a pointless
//! password prompt; the child's is the one that decides, exactly as it is for
//! validation.
//!
//! The create-backup operation carries only a `target`, and that single
//! identity check is the whole of its authority: the path it writes is chosen
//! by [`writer::create_backup`] from the verified target's own name, never by
//! anything in the request, so there is no second string for the unprivileged
//! side to get wrong or to lie about.

use crate::backups;
use crate::dto::{
    CreateBackupErrorDto, CreateBackupErrorKindDto, CreateBackupOutcomeDto, DeleteBackupErrorDto,
    DeleteBackupErrorKindDto, DocumentDto, WriteErrorDto, WriteErrorKindDto, WriteOutcomeDto,
};
use crate::target;
use fstab_core::model::Document;
use fstab_core::{validate, writer};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

/// The hidden first argument that turns this binary into the elevated helper.
///
/// Not a public CLI: it is an implementation detail of the escalation
/// mechanism, its stdin/stdout protocol is unstable, and nothing but
/// [`save_elevated`], [`delete_backup_elevated`] and [`create_backup_elevated`]
/// is expected to pass it.
///
/// The name and the spelling are historical - the flag predates the two
/// backup operations and now carries any [`PrivilegedRequest`] variant. It is
/// kept as-is on purpose: changing it buys nothing and the whole point of this
/// module is that there is exactly *one* string that can turn this process
/// into a root process.
pub const INTERNAL_WRITE_FLAG: &str = "--internal-privileged-write";

/// Exit code of the elevated helper when the requested operation succeeded.
pub const EXIT_OK: i32 = 0;

/// Exit code when the requested operation was attempted and failed; stdout
/// still holds an error response explaining what happened.
pub const EXIT_OPERATION_ERROR: i32 = 1;

/// Exit code when the request itself was unusable (unreadable or malformed
/// stdin, stray arguments). Nothing was written and nothing was deleted - the
/// helper never got as far as knowing which operation it was asked for.
pub const EXIT_PROTOCOL_ERROR: i32 = 2;

/// How much of the child's stderr is quoted back in an error message.
const STDERR_EXCERPT_LIMIT: usize = 1000;

/// `pkexec`'s exit code when the user dismissed the authentication dialog.
/// The program was never executed.
const PKEXEC_DISMISSED: i32 = 126;

/// `pkexec`'s exit code when authorization could not be obtained or the
/// program could not be executed. Again, the program never ran.
const PKEXEC_NOT_AUTHORIZED: i32 = 127;

/// The request the unprivileged side writes to the elevated helper's stdin:
/// one JSON object, then EOF.
///
/// A union tagged by `op`, so the single hidden flag can carry either
/// operation. Every variant names its `target` explicitly rather than relying
/// on the elevated process to re-resolve it (it cannot - see the module docs),
/// and the elevated side re-checks that name against the one target it is
/// allowed to touch before doing anything at all.
///
/// This protocol is internal, undocumented as a public interface and unstable
/// by design (`docs/API_CONTRACT.md` §11): nothing but this binary ever speaks
/// it, to itself, in a single build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PrivilegedRequest {
    /// Perform the whole [`writer::write_document`] operation on `target`.
    Write {
        /// The file to write. Must be the real `/etc/fstab`.
        target: String,
        /// The document to write, in the same shape the frontend sent it.
        candidate: DocumentDto,
        /// What the unprivileged side believes `target` currently holds - the
        /// content its last-known-real marker records (T36). The elevated side
        /// re-reads `target` itself and refuses the write if the two differ,
        /// *immediately* before [`writer::write_document`] runs; see
        /// [`handle_write`].
        ///
        /// `None` means the unprivileged side has no usable record and no
        /// comparison is possible, which is the pre-T36 behaviour (see
        /// [`target::ExternalChangeGuard`]).
        ///
        /// Deliberately **not** `#[serde(default)]`, unlike the additive
        /// fields on the DTOs that cross the Tauri boundary: this protocol is
        /// internal to one build speaking to itself, and a missing field
        /// defaulting to "no record" would be a silently fail-*open* default
        /// on the one check that exists to fail closed. A request that does
        /// not carry the field is a protocol error, as it should be.
        last_known_real_content: Option<String>,
        /// `true` only when the user has already been shown what changed
        /// externally and asked for the write anyway, which makes the elevated
        /// side skip the comparison for this one request (T36).
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
/// One type for both operations rather than one per operation, because the
/// framing errors (stdin unreadable, request unparsable) happen *before* the
/// helper knows which operation it was asked for, and so cannot be reported in
/// an operation-specific shape. Those are reported as [`Self::Error`], which
/// is why the delete path also has to be able to read that variant.
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

// ---------------------------------------------------------------------------
// The elevated side
// ---------------------------------------------------------------------------

/// Runs the elevated helper if this process was started with
/// [`INTERNAL_WRITE_FLAG`] as its first argument, returning the exit code to
/// terminate with. Returns `None` for a normal GUI launch.
///
/// Called as the very first statement of `main` so that, in elevated mode,
/// no Tauri runtime, webview, plugin or logger is ever initialized. This is
/// the *only* branch in the whole program that can do that, for either
/// operation - see the module docs.
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
    Some(handle_privileged_request(
        &mut stdin,
        &mut stdout,
        Path::new(target::REAL_FSTAB_PATH),
    ))
}

/// The whole of the elevated helper: read one JSON [`PrivilegedRequest`] from
/// `input`, perform it, write one JSON [`PrivilegedResponse`] to `output`, and
/// return the exit code.
///
/// `allowed_target` is the only target this is permitted to act on (matched
/// literally or by canonicalized identity) - the only file it may write, and
/// the only file whose backups it may delete. Production passes `/etc/fstab`;
/// tests pass a temp file so that this exact function - not a stand-in - is
/// what they exercise.
///
/// Never panics and never returns without having written a response: a caller
/// (or a test) can rely on stdout holding exactly one JSON object.
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
    // read that too (see `interpret_delete_output`). Nothing has touched disk
    // at this point, in either operation.
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
            &target::ExternalChangeGuard {
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

/// The elevated write: refuse any target but `allowed_target`, refuse a target
/// that changed outside Rustab since the unprivileged side last looked, then
/// perform the complete [`writer::write_document`] operation in this one
/// process under this one authorization.
///
/// ## Why the external-change check is *here* (T36)
///
/// This is the authoritative one, and it is placed as the last thing before
/// `write_document` on purpose. [`save_elevated`] spawns `pkexec`, which
/// blocks on a human reading a dialog and typing a password - an arbitrarily
/// long wait, during which another tool, another Rustab instance or a text
/// editor can write `/etc/fstab`. A check made only in the unprivileged parent
/// before that prompt would therefore leave the largest part of the window
/// uncovered. So the request carries what the parent believed the file held
/// ([`target::ExternalChangeGuard`]), this process re-reads the file itself and
/// compares, and only then writes.
///
/// It is exactly the relationship this module already has for *validation*:
/// [`save_elevated`] validates before prompting as a courtesy (so nobody types
/// a password for a save that cannot succeed), and `write_document` validates
/// again in here, where the answer is binding. Both unprivileged pre-checks
/// could be deleted without weakening any guarantee this function makes;
/// neither of the checks in here can.
fn handle_write<W: Write>(
    output: &mut W,
    allowed_target: &Path,
    target: &str,
    candidate: DocumentDto,
    guard: &target::ExternalChangeGuard,
) -> i32 {
    let target_path = PathBuf::from(target);
    if !target::is_same_file(&target_path, allowed_target) {
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
/// `allowed_target`, resolve `backup_path` against *that* target's real backup
/// list, and unlink exactly the path the list yielded.
///
/// Two checks, then one syscall. Neither check trusts anything the
/// unprivileged side said:
///
/// 1. `target` must be the one target this process may act on - the identical
///    [`target::is_same_file`] test [`handle_write`] applies, so "which file
///    is the real `/etc/fstab`" is decided in one place for both operations.
/// 2. `backup_path` must resolve, through
///    [`backups::resolve_requested`] *here*, to one of that target's actual
///    backups. The path that is then unlinked is the one the backup list
///    yielded, never the string that arrived in the request.
///
/// If either check fails, nothing is unlinked - the response is written before
/// anything touches the disk, which is the same shape of guarantee
/// [`handle_write`] gives for an invalid target.
///
/// There is deliberately no backup-of-the-backup, no temp file and no rename:
/// this removes a *backup*, not the target, so none of
/// [`writer::write_document`]'s atomicity guarantees apply to it or are needed
/// for it. The target file itself is never opened by this function.
fn handle_delete_backup<W: Write>(
    output: &mut W,
    allowed_target: &Path,
    target: &str,
    backup_path: &str,
) -> i32 {
    let target_path = PathBuf::from(target);
    if !target::is_same_file(&target_path, allowed_target) {
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
/// One check, then one function call - the same shape, and the same "nothing
/// happens before the check passes" guarantee, as [`handle_write`] and
/// [`handle_delete_backup`]. The identity test is the identical
/// [`target::is_same_file`] call those two use, so "which file is the real
/// `/etc/fstab`" is still decided in exactly one place for every operation.
///
/// There is nothing else to check, because there is nothing else in the
/// request: [`writer::create_backup`] derives the backup's path from the
/// verified target's own name, in the target's own directory, so this
/// operation cannot be pointed at another file the way an operation taking a
/// destination path could be. And it never opens the target for writing -
/// only for reading - so unlike [`handle_write`] it has no atomicity
/// ceremony to perform and no way to leave `/etc/fstab` in a state nobody
/// planned.
fn handle_create_backup<W: Write>(output: &mut W, allowed_target: &Path, target: &str) -> i32 {
    let target_path = PathBuf::from(target);
    if !target::is_same_file(&target_path, allowed_target) {
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

// ---------------------------------------------------------------------------
// The unprivileged side
// ---------------------------------------------------------------------------

/// Why the elevated half could not be run to completion, before it had a
/// chance to report anything of its own.
///
/// Carries the underlying error but **no message**: each operation words its
/// own, because the useful wording names what was not done ("nothing was
/// written" / "nothing was deleted") and the right error `kind` differs
/// between the two shapes. The *mechanics* - the order of the steps, killing a
/// child whose stdin could not be delivered, and which of these failures mean
/// "the helper provably never acted" - are shared, which is the part that is
/// safety-relevant.
#[derive(Debug)]
enum LaunchFailure {
    /// There was no path to re-exec: [`std::env::current_exe`] failed, or -
    /// inside a Flatpak sandbox, where `current_exe` yields the meaningless
    /// in-sandbox `/app/bin/rustab` - this binary could not be found at any of
    /// its host-side paths (see [`crate::host::elevated_self_plan`]). The
    /// helper never ran.
    NoExePath(io::Error),
    /// The request could not be serialized - a bug between the two halves.
    /// The helper never ran.
    Serialize(serde_json::Error),
    /// `pkexec` itself could not be spawned (most often: polkit is not
    /// installed). The helper never ran.
    Spawn(io::Error),
    /// The request could not be handed to the helper's stdin. The child is
    /// killed before its stdin is closed, so it never reached EOF and cannot
    /// have parsed a request; and a truncated prefix of one JSON object can
    /// never parse, so even a child that did reach EOF would have answered
    /// with a protocol error instead of acting. The helper never acted.
    SendRequest(io::Error),
    /// The helper was started but could not be waited for. This is the one
    /// launch failure where it may already have acted, and neither operation
    /// may claim otherwise.
    Wait(io::Error),
}

impl LaunchFailure {
    /// `true` when the elevated half provably did not perform its operation.
    ///
    /// Only [`LaunchFailure::Wait`] is uncertain; every other variant happens
    /// before the request could possibly have been acted on.
    fn nothing_happened(&self) -> bool {
        !matches!(self, LaunchFailure::Wait(_))
    }
}

/// Re-executes this binary as root with [`INTERNAL_WRITE_FLAG`], hands
/// `request` to it on stdin, and returns what it printed and exited with.
///
/// The whole `pkexec` plumbing for **both** operations, deliberately in one
/// function: the spawn, the single stdin write, the pipe close, and the wait
/// are identical for a write and for a delete, and two copies of them could
/// drift apart in ways that matter (a forgotten `child.kill()` on a failed
/// stdin write leaves a root process blocked on a pipe that will never close).
///
/// Performs **no filesystem writes of its own** - it serializes JSON to a pipe
/// and reads bytes back.
///
/// ## The one thing T40 changed here
///
/// *Which command* is spawned, and nothing else. On an ordinary install it is
/// still `pkexec <this binary> <flag>`. Inside a Flatpak sandbox it is
/// `flatpak-spawn --host pkexec <this binary's host path> <flag>`, because
/// `pkexec` on its own would produce a root process that is **still inside the
/// sandbox** - and the `/etc` a sandboxed process sees is the runtime's, not
/// the host's, so such a process cannot write the real `/etc/fstab` no matter
/// what it is authorized to do (`pkexec` changes the UID, not the mount
/// namespace; confirmed empirically - see [`crate::host`]). The decision and
/// the host-path resolution both live in [`crate::host::elevated_self_plan`];
/// this function only asks for the plan and runs it.
///
/// Everything after the spawn is untouched and deliberately so: writing the
/// request to stdin, closing it, waiting, and handing the exit code, stdout
/// and stderr to the `interpret_*_output` functions all behave identically
/// through `flatpak-spawn --host pkexec`, which passes stdio through and exits
/// with the child's status (confirmed by the T40 diagnostics). The helper on
/// the other end is the same binary running the same [`INTERNAL_WRITE_FLAG`]
/// branch; it simply runs on the host, where - having no `/.flatpak-info` of
/// its own - it reads and writes the real filesystem directly, exactly as the
/// non-Flatpak case always did.
fn run_elevated(request: &PrivilegedRequest) -> Result<Output, LaunchFailure> {
    let plan =
        crate::host::elevated_self_plan(INTERNAL_WRITE_FLAG).map_err(LaunchFailure::NoExePath)?;
    let payload = serde_json::to_vec(request).map_err(LaunchFailure::Serialize)?;

    let mut child = plan
        .command()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(LaunchFailure::Spawn)?;

    // The child reads stdin to EOF before writing anything, so handing it the
    // whole request and then closing the pipe cannot deadlock regardless of
    // the request's size.
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(err) = stdin.write_all(&payload) {
            // Kill the child rather than leaving it blocked on a stdin that
            // will never be complete.
            let _ = child.kill();
            let _ = child.wait();
            return Err(LaunchFailure::SendRequest(err));
        }
        drop(stdin);
    }

    child.wait_with_output().map_err(LaunchFailure::Wait)
}

/// What `pkexec`'s own exit code means when the helper left no usable response
/// on stdout.
///
/// Both operations classify through this one enum so they can never disagree
/// about whether a dismissed prompt means "nothing happened" - that
/// interpretation is itself safety-relevant, and `pkexec`'s codes cannot be
/// provoked reliably in a test, so it is worth having exactly one copy of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoResponseReason {
    /// Exit 126: the user dismissed the authentication dialog. `pkexec`
    /// documents that the program was never executed.
    Dismissed,
    /// Exit 127: authorization could not be obtained, or the program could not
    /// be executed. Again it never ran.
    NotAuthorized,
    /// Anything else, including death by signal: the helper may have been
    /// started and killed part-way through. Unknown territory.
    Unexplained(Option<i32>),
}

impl NoResponseReason {
    /// Classifies the child's exit code.
    fn classify(code: Option<i32>) -> Self {
        match code {
            Some(PKEXEC_DISMISSED) => NoResponseReason::Dismissed,
            Some(PKEXEC_NOT_AUTHORIZED) => NoResponseReason::NotAuthorized,
            other => NoResponseReason::Unexplained(other),
        }
    }

    /// `true` only for the codes that mean the helper never ran at all.
    ///
    /// Conservative by construction: [`NoResponseReason::Unexplained`] is
    /// never claimed to have left things untouched.
    fn nothing_happened(self) -> bool {
        !matches!(self, NoResponseReason::Unexplained(_))
    }

    /// How the exit is described in an error message ("with status 42" / "on a
    /// signal").
    fn how_it_exited(self) -> String {
        match self {
            NoResponseReason::Dismissed => format!("with status {PKEXEC_DISMISSED}"),
            NoResponseReason::NotAuthorized => format!("with status {PKEXEC_NOT_AUTHORIZED}"),
            NoResponseReason::Unexplained(Some(code)) => format!("with status {code}"),
            NoResponseReason::Unexplained(None) => "on a signal".to_string(),
        }
    }
}

// -- the save operation -----------------------------------------------------

/// Performs a save of `candidate` to `target` as root, by re-executing this
/// binary under `pkexec` with [`INTERNAL_WRITE_FLAG`].
///
/// This function performs **no filesystem writes of its own**. Everything
/// that touches disk happens inside the elevated
/// [`writer::write_document`] call (see the module docs).
///
/// `guard` is what the caller believes `target` currently holds, plus the
/// user's "write anyway" answer (T36). It is pure transport here - this
/// function neither reads `target` nor decides anything from it; the elevated
/// side re-reads the file and makes the call ([`handle_write`]), which is why
/// the password wait cannot hide a change from the check.
///
/// # Errors
///
/// Returns a [`WriteErrorDto`] whose `kind` is
/// [`WriteErrorKindDto::PrivilegeEscalation`] if the helper could not be
/// launched, the authorization was refused or dismissed, or the helper
/// produced no usable response; otherwise the error the helper itself
/// reported, forwarded unchanged - including
/// [`WriteErrorKindDto::ExternalChange`], which is how the elevated side
/// reports a target that changed under it.
pub fn save_elevated(
    target: &Path,
    candidate: &Document,
    guard: &target::ExternalChangeGuard,
) -> Result<WriteOutcomeDto, WriteErrorDto> {
    // Validate before prompting. This is a courtesy, not the safety net: a
    // document with blocking findings is rejected without making the user
    // authenticate for a save that cannot succeed. `write_document` validates
    // again inside the elevated process, and that call is the authoritative
    // one - this check could be deleted without weakening any guarantee.
    let report = validate::validate(candidate);
    if report.has_errors() {
        return Err(WriteErrorDto::from(&writer::WriteError::Validation(report)));
    }

    let request = PrivilegedRequest::Write {
        target: target.to_string_lossy().into_owned(),
        candidate: DocumentDto::from(candidate),
        last_known_real_content: guard.last_known_real_content.clone(),
        allow_external_change: guard.allow_external_change,
    };
    let output = run_elevated(&request).map_err(|failure| write_launch_error(target, &failure))?;

    interpret_write_output(target, output.status.code(), &output.stdout, &output.stderr)
}

/// Words a [`LaunchFailure`] as a save failure.
fn write_launch_error(target: &Path, failure: &LaunchFailure) -> WriteErrorDto {
    let unchanged = failure.nothing_happened();
    match failure {
        LaunchFailure::NoExePath(err) => WriteErrorDto::new(
            WriteErrorKindDto::PrivilegeEscalation,
            unchanged,
            format!(
                "could not determine which Rustab executable to run as root, so the elevated \
                 writer could not be started ({err}); nothing was written"
            ),
        ),
        LaunchFailure::Serialize(err) => WriteErrorDto::new(
            WriteErrorKindDto::Internal,
            unchanged,
            format!("could not serialize the save request ({err}); nothing was written"),
        ),
        LaunchFailure::Spawn(err) => WriteErrorDto::new(
            WriteErrorKindDto::PrivilegeEscalation,
            unchanged,
            format!(
                "could not start the elevated writer needed to write {} ({err}); nothing was \
                 written. On an ordinary install that means pkexec (polkit) could not be run; in \
                 a Flatpak it means flatpak-spawn could not be run.",
                target.display()
            ),
        ),
        LaunchFailure::SendRequest(err) => WriteErrorDto::new(
            WriteErrorKindDto::PrivilegeEscalation,
            unchanged,
            format!(
                "could not hand the save request to the elevated writer ({err}); nothing was \
                 written"
            ),
        ),
        LaunchFailure::Wait(err) => WriteErrorDto::new(
            WriteErrorKindDto::PrivilegeEscalation,
            // The helper may have run; without its response this side cannot
            // claim the target is untouched.
            unchanged,
            format!("the elevated writer could not be waited for ({err})"),
        ),
    }
}

/// Maps the elevated writer's exit status, stdout and stderr onto a result.
///
/// Pure: no process spawning, so the interpretation of every outcome
/// (including the `pkexec`-specific exit codes, which cannot be provoked
/// reliably in a test) is unit-testable on its own.
///
/// A parsable JSON response always wins over the exit code - it is the
/// helper's own account of what it did. Only when there is no usable response
/// does the exit code decide, and then the rule is conservative: claim
/// `target_unchanged` only for the codes that mean the helper never ran.
fn interpret_write_output(
    target: &Path,
    code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<WriteOutcomeDto, WriteErrorDto> {
    let text = String::from_utf8_lossy(stdout);
    match serde_json::from_str::<PrivilegedResponse>(text.trim()) {
        Ok(PrivilegedResponse::Ok { outcome }) => {
            return Ok(WriteOutcomeDto {
                elevated: true,
                ..outcome
            })
        }
        Ok(PrivilegedResponse::Error { error }) => return Err(error),
        // Only a backup request can provoke these, and this function is only
        // ever reached for a `write` one. Treated as "no usable response"
        // rather than trusted, because a response that does not answer the
        // question asked is not an account of this operation.
        Ok(
            PrivilegedResponse::BackupDeleted
            | PrivilegedResponse::BackupDeleteFailed { .. }
            | PrivilegedResponse::BackupCreated { .. }
            | PrivilegedResponse::BackupCreateFailed { .. },
        )
        | Err(_) => {}
    }

    let detail = stderr_excerpt(stderr);
    let reason = NoResponseReason::classify(code);
    let error = match reason {
        NoResponseReason::Dismissed => WriteErrorDto::new(
            WriteErrorKindDto::PrivilegeEscalation,
            reason.nothing_happened(),
            format!(
                "the authorization request was dismissed, so {} was not written and is unchanged",
                target.display()
            ),
        ),
        NoResponseReason::NotAuthorized => WriteErrorDto::new(
            WriteErrorKindDto::PrivilegeEscalation,
            reason.nothing_happened(),
            format!(
                "authorization to write {} was not granted (or the elevated writer could not be \
                 executed), so nothing was written{detail}",
                target.display()
            ),
        ),
        NoResponseReason::Unexplained(_) => WriteErrorDto::new(
            WriteErrorKindDto::PrivilegeEscalation,
            // Unknown territory: the helper may have been killed mid-write.
            // `write_document`'s own guarantees still hold (the target holds
            // either its old or its new content), but this side cannot say
            // which, and must not claim "unchanged".
            reason.nothing_happened(),
            format!(
                "the elevated writer exited {} without a usable result, so the state of {} could \
                 not be confirmed; re-open the file to see which content it holds{detail}",
                reason.how_it_exited(),
                target.display()
            ),
        ),
    };
    Err(error.with_path(target))
}

// -- the backup-deletion operation ------------------------------------------

/// Deletes `backup_path` - one of `target`'s own backups - as root, by
/// re-executing this binary under `pkexec` with [`INTERNAL_WRITE_FLAG`].
///
/// Needed at all because the backups of the real `/etc/fstab` live beside it
/// in `/etc/`, which an unprivileged process cannot unlink from (see
/// `crates/fstab-core/src/writer.rs` on backup naming). A non-real target's
/// backups live in a user-owned directory and never come through here.
///
/// This function performs **no filesystem changes of its own**, and the
/// elevated half it starts re-resolves `backup_path` against `target`'s real
/// backup list before unlinking anything, so neither half can be talked into
/// deleting a file that is not one of this target's backups.
///
/// # Errors
///
/// Returns a [`DeleteBackupErrorDto`] whose `kind` is
/// [`DeleteBackupErrorKindDto::PrivilegeEscalation`] if the helper could not
/// be launched, the authorization was refused or dismissed, or the helper
/// produced no usable response; otherwise the error the helper itself
/// reported, forwarded unchanged.
pub fn delete_backup_elevated(
    target: &Path,
    backup_path: &str,
) -> Result<(), DeleteBackupErrorDto> {
    let request = PrivilegedRequest::DeleteBackup {
        target: target.to_string_lossy().into_owned(),
        backup_path: backup_path.to_string(),
    };
    let output =
        run_elevated(&request).map_err(|failure| delete_launch_error(backup_path, &failure))?;

    interpret_delete_output(
        backup_path,
        output.status.code(),
        &output.stdout,
        &output.stderr,
    )
}

/// Words a [`LaunchFailure`] as a backup-deletion failure.
///
/// Every variant lands on [`DeleteBackupErrorKindDto::PrivilegeEscalation`]:
/// from the frontend's point of view these are one situation - the elevated
/// helper never got to do the job - and the message says which it was.
/// (A serialization failure is a bug rather than an escalation problem, but
/// `DeleteBackupErrorKindDto` deliberately has no `internal` kind: a request of
/// two `String`s cannot fail to serialize, and inventing a variant the
/// frontend must handle for an unreachable case is not worth it.)
fn delete_launch_error(backup_path: &str, failure: &LaunchFailure) -> DeleteBackupErrorDto {
    let message = match failure {
        LaunchFailure::NoExePath(err) => format!(
            "could not determine which Rustab executable to run as root, so the elevated helper \
             could not be started ({err}); {backup_path} was not deleted"
        ),
        LaunchFailure::Serialize(err) => {
            format!("could not serialize the delete request ({err}); {backup_path} was not deleted")
        }
        LaunchFailure::Spawn(err) => format!(
            "could not start the elevated helper needed to delete {backup_path} ({err}); it \
             was not deleted. On an ordinary install that means pkexec (polkit) could not be \
             run; in a Flatpak it means flatpak-spawn could not be run."
        ),
        LaunchFailure::SendRequest(err) => format!(
            "could not hand the delete request to the elevated helper ({err}); {backup_path} was \
             not deleted"
        ),
        LaunchFailure::Wait(err) => format!(
            "the elevated helper could not be waited for ({err}), so it is not known whether \
             {backup_path} was deleted; refresh the backup list to see"
        ),
    };
    DeleteBackupErrorDto::new(DeleteBackupErrorKindDto::PrivilegeEscalation, message)
}

/// Maps the elevated helper's exit status, stdout and stderr onto a delete
/// result.
///
/// Pure, for the same reason [`interpret_write_output`] is, and it follows the
/// same rule: a parsable JSON response wins over the exit code, and only when
/// there is none does the exit code decide - through the same
/// [`NoResponseReason`], so the two operations cannot disagree about what
/// `pkexec`'s codes mean.
fn interpret_delete_output(
    backup_path: &str,
    code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<(), DeleteBackupErrorDto> {
    let text = String::from_utf8_lossy(stdout);
    match serde_json::from_str::<PrivilegedResponse>(text.trim()) {
        Ok(PrivilegedResponse::BackupDeleted) => return Ok(()),
        Ok(PrivilegedResponse::BackupDeleteFailed { error }) => return Err(error),
        // The helper answers in this shape when it could not read or parse the
        // request at all - at that point it does not yet know which operation
        // was asked for, so it cannot answer in the delete-specific shape.
        // Nothing was deleted: it never got as far as a path.
        Ok(PrivilegedResponse::Error { error }) => {
            return Err(DeleteBackupErrorDto::new(
                DeleteBackupErrorKindDto::PrivilegeEscalation,
                format!(
                    "the elevated helper rejected the delete request ({}), so {backup_path} was \
                     not deleted",
                    error.message
                ),
            ))
        }
        // Only a `write` or a `create_backup` request can provoke these; a
        // response that does not answer the question asked is not an account
        // of this operation.
        Ok(
            PrivilegedResponse::Ok { .. }
            | PrivilegedResponse::BackupCreated { .. }
            | PrivilegedResponse::BackupCreateFailed { .. },
        )
        | Err(_) => {}
    }

    let detail = stderr_excerpt(stderr);
    let reason = NoResponseReason::classify(code);
    let message = match reason {
        NoResponseReason::Dismissed => format!(
            "the authorization request was dismissed, so {backup_path} was not deleted and is \
             still there"
        ),
        NoResponseReason::NotAuthorized => format!(
            "authorization to delete {backup_path} was not granted (or the elevated helper could \
             not be executed), so nothing was deleted{detail}"
        ),
        NoResponseReason::Unexplained(_) => format!(
            "the elevated helper exited {} without a usable result, so it is not known whether \
             {backup_path} was deleted; refresh the backup list to see{detail}",
            reason.how_it_exited()
        ),
    };
    Err(DeleteBackupErrorDto::new(
        DeleteBackupErrorKindDto::PrivilegeEscalation,
        message,
    ))
}

// -- the manual backup-creation operation ------------------------------------

/// Creates a manual backup of `target` as root, by re-executing this binary
/// under `pkexec` with [`INTERNAL_WRITE_FLAG`].
///
/// Needed at all because a backup of the real `/etc/fstab` has to be created
/// *in* `/etc/`, which an unprivileged process cannot write to - the mirror
/// image of why [`delete_backup_elevated`] exists. A non-real target's backups
/// live in a user-owned directory and never come through here.
///
/// This function performs **no filesystem changes of its own**, and the
/// elevated half it starts chooses the backup's path itself from the target it
/// has verified, so there is no path in this request that could point the
/// elevated copy at the wrong file.
///
/// # Errors
///
/// Returns a [`CreateBackupErrorDto`] whose `kind` is
/// [`CreateBackupErrorKindDto::PrivilegeEscalation`] if the helper could not be
/// launched, the authorization was refused or dismissed, or the helper produced
/// no usable response; otherwise the error the helper itself reported,
/// forwarded unchanged.
pub fn create_backup_elevated(
    target: &Path,
) -> Result<CreateBackupOutcomeDto, CreateBackupErrorDto> {
    let request = PrivilegedRequest::CreateBackup {
        target: target.to_string_lossy().into_owned(),
    };
    let output = run_elevated(&request).map_err(|failure| create_launch_error(target, &failure))?;

    interpret_create_backup_output(target, output.status.code(), &output.stdout, &output.stderr)
}

/// Words a [`LaunchFailure`] as a backup-creation failure.
///
/// Every variant lands on [`CreateBackupErrorKindDto::PrivilegeEscalation`],
/// for the same reason [`delete_launch_error`]'s do: from the frontend's point
/// of view these are one situation - the elevated helper never got to do the
/// job - and the message says which it was. (A serialization failure is a bug
/// rather than an escalation problem, but a request of one `String` cannot fail
/// to serialize, and inventing a kind the frontend must handle for an
/// unreachable case is not worth it.)
fn create_launch_error(target: &Path, failure: &LaunchFailure) -> CreateBackupErrorDto {
    let target = target.display();
    let message = match failure {
        LaunchFailure::NoExePath(err) => format!(
            "could not determine which Rustab executable to run as root, so the elevated helper \
             could not be started ({err}); no backup of {target} was created"
        ),
        LaunchFailure::Serialize(err) => format!(
            "could not serialize the backup request ({err}); no backup of {target} was created"
        ),
        LaunchFailure::Spawn(err) => format!(
            "could not start the elevated helper needed to back up {target} ({err}); no backup \
             was created. On an ordinary install that means pkexec (polkit) could not be run; in \
             a Flatpak it means flatpak-spawn could not be run."
        ),
        LaunchFailure::SendRequest(err) => format!(
            "could not hand the backup request to the elevated helper ({err}); no backup of \
             {target} was created"
        ),
        LaunchFailure::Wait(err) => format!(
            "the elevated helper could not be waited for ({err}), so it is not known whether a \
             backup of {target} was created; refresh the backup list to see"
        ),
    };
    CreateBackupErrorDto::new(CreateBackupErrorKindDto::PrivilegeEscalation, message)
}

/// Maps the elevated helper's exit status, stdout and stderr onto a
/// backup-creation result.
///
/// Pure, for the same reason [`interpret_write_output`] is, and it follows the
/// same rule: a parsable JSON response wins over the exit code, and only when
/// there is none does the exit code decide - through the same
/// [`NoResponseReason`], so no operation can disagree with another about what
/// `pkexec`'s codes mean.
fn interpret_create_backup_output(
    target: &Path,
    code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<CreateBackupOutcomeDto, CreateBackupErrorDto> {
    let text = String::from_utf8_lossy(stdout);
    match serde_json::from_str::<PrivilegedResponse>(text.trim()) {
        Ok(PrivilegedResponse::BackupCreated { outcome }) => return Ok(outcome),
        Ok(PrivilegedResponse::BackupCreateFailed { error }) => return Err(error),
        // The helper answers in this shape when it could not read or parse the
        // request at all - at that point it does not yet know which operation
        // was asked for, so it cannot answer in the create-specific shape.
        // Nothing was created: it never got as far as a target.
        Ok(PrivilegedResponse::Error { error }) => {
            return Err(CreateBackupErrorDto::new(
                CreateBackupErrorKindDto::PrivilegeEscalation,
                format!(
                    "the elevated helper rejected the backup request ({}), so no backup of {} \
                     was created",
                    error.message,
                    target.display()
                ),
            ))
        }
        // Only another operation can provoke these; a response that does not
        // answer the question asked is not an account of this one.
        Ok(
            PrivilegedResponse::Ok { .. }
            | PrivilegedResponse::BackupDeleted
            | PrivilegedResponse::BackupDeleteFailed { .. },
        )
        | Err(_) => {}
    }

    let detail = stderr_excerpt(stderr);
    let reason = NoResponseReason::classify(code);
    let target = target.display();
    let message = match reason {
        NoResponseReason::Dismissed => {
            format!("the authorization request was dismissed, so no backup of {target} was created")
        }
        NoResponseReason::NotAuthorized => format!(
            "authorization to back up {target} was not granted (or the elevated helper could not \
             be executed), so no backup was created{detail}"
        ),
        NoResponseReason::Unexplained(_) => format!(
            "the elevated helper exited {} without a usable result, so it is not known whether a \
             backup of {target} was created; refresh the backup list to see{detail}",
            reason.how_it_exited()
        ),
    };
    Err(CreateBackupErrorDto::new(
        CreateBackupErrorKindDto::PrivilegeEscalation,
        message,
    ))
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
    ) -> (i32, PrivilegedResponse) {
        // No record of the file's previous content, which is the one guard
        // that disables T36's check entirely - so these tests exercise exactly
        // the pre-T36 behaviour of the helper. The guarded variants have their
        // own helper below.
        run_helper_guarded(
            target,
            allowed_target,
            candidate,
            &target::ExternalChangeGuard::unchecked(),
        )
    }

    /// [`run_helper`] with an explicit [`target::ExternalChangeGuard`], for the
    /// T36 tests: the request carries what the unprivileged side claims the
    /// target held, and the helper re-reads the file itself.
    fn run_helper_guarded(
        target: &Path,
        allowed_target: &Path,
        candidate: &Document,
        guard: &target::ExternalChangeGuard,
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

    // -- T36: the authoritative external-change check ------------------------
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
            &target::ExternalChangeGuard {
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
    /// the write goes through, exactly as it did before T36.
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
            &target::ExternalChangeGuard {
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
            &target::ExternalChangeGuard {
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
            &target::ExternalChangeGuard {
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
    /// comments, which a text-based protocol could silently reinterpret.
    /// Since T36 it also carries what the parent last knew the target to
    /// hold, which the elevated side needs in order to re-check it.
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
    /// hidden flag can carry any of them and none can be mistaken for another.
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
            draft_sync_warning: None,
        };
        let stdout = serde_json::to_vec(&PrivilegedResponse::Ok { outcome }).expect("json");

        let result = interpret_write_output(Path::new("/etc/fstab"), Some(EXIT_OK), &stdout, b"");

        let outcome = result.expect("the response says it succeeded");
        // The unprivileged side knows it used pkexec and says so.
        assert!(outcome.elevated);
    }

    #[test]
    fn a_dismissed_authorization_is_reported_as_target_unchanged() {
        let result = interpret_write_output(
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
        let result = interpret_write_output(
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
        let killed = interpret_write_output(Path::new("/etc/fstab"), None, b"", b"")
            .expect_err("no response is a failure");
        assert!(!killed.target_unchanged);
        assert!(killed.message.contains("could not be confirmed"));

        let odd = interpret_write_output(Path::new("/etc/fstab"), Some(42), b"partial out", b"")
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
        let stdout = serde_json::to_vec(&PrivilegedResponse::Error {
            error: error.clone(),
        })
        .expect("json");

        let result = interpret_write_output(
            Path::new("/etc/fstab"),
            Some(EXIT_OPERATION_ERROR),
            &stdout,
            b"",
        );

        assert_eq!(result.expect_err("an error response is an error"), error);
    }

    /// `save_elevated` must not prompt for a password to perform a save that
    /// validation already rejects.
    ///
    /// Safe to call with the real path because the rejection happens before
    /// anything is read, spawned or written - `save_elevated` never touches
    /// the target itself (the T36 guard is pure transport here; it is the
    /// elevated side that re-reads the file).
    #[test]
    fn save_elevated_rejects_an_invalid_candidate_before_escalating() {
        let error = save_elevated(
            Path::new("/etc/fstab"),
            &invalid_candidate(),
            &target::ExternalChangeGuard::unchecked(),
        )
        .expect_err("an invalid candidate must be rejected");
        assert_eq!(error.kind, WriteErrorKindDto::Validation);
        assert!(error.target_unchanged);
        assert!(error.report.is_some());
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

    /// Runs the elevated *deleter* exactly as `main` does, but with an
    /// in-memory stdin/stdout and a temp file standing in for `/etc/fstab`.
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

    // -- interpretation of the child's output, delete path -------------------

    #[test]
    fn a_json_delete_response_is_trusted_over_the_exit_code() {
        let deleted = serde_json::to_vec(&PrivilegedResponse::BackupDeleted).expect("json");
        // Even with a nonsense exit code, the helper's own account wins.
        interpret_delete_output("/etc/fstab.rustab-backup.1.2", Some(42), &deleted, b"")
            .expect("the response says the backup is gone");

        let reported = DeleteBackupErrorDto::new(
            DeleteBackupErrorKindDto::Delete,
            "could not delete the backup /etc/fstab.rustab-backup.1.2: Read-only file system",
        );
        let failed = serde_json::to_vec(&PrivilegedResponse::BackupDeleteFailed {
            error: reported.clone(),
        })
        .expect("json");
        let error =
            interpret_delete_output("/etc/fstab.rustab-backup.1.2", Some(EXIT_OK), &failed, b"")
                .expect_err("an error response is an error");
        assert_eq!(error, reported, "a reported error is forwarded verbatim");
    }

    #[test]
    fn a_dismissed_authorization_for_a_delete_says_the_backup_is_still_there() {
        let error = interpret_delete_output(
            "/etc/fstab.rustab-backup.1.2",
            Some(PKEXEC_DISMISSED),
            b"",
            b"Error executing command as another user: Not authorized",
        )
        .expect_err("a dismissed prompt is a failure");

        assert_eq!(error.kind, DeleteBackupErrorKindDto::PrivilegeEscalation);
        assert!(error.message.contains("was not deleted"));
        assert!(error.message.contains("/etc/fstab.rustab-backup.1.2"));
    }

    #[test]
    fn a_refused_authorization_for_a_delete_includes_the_stderr_detail() {
        let error = interpret_delete_output(
            "/etc/fstab.rustab-backup.1.2",
            Some(PKEXEC_NOT_AUTHORIZED),
            b"",
            b"polkit-agent-helper-1: authentication failure",
        )
        .expect_err("a refused prompt is a failure");

        assert_eq!(error.kind, DeleteBackupErrorKindDto::PrivilegeEscalation);
        assert!(error.message.contains("nothing was deleted"));
        assert!(error.message.contains("authentication failure"));
    }

    /// The honest case: the helper died without reporting, so this side does
    /// not know whether the backup is gone and must not pretend either way.
    #[test]
    fn an_unexplained_exit_for_a_delete_does_not_claim_an_outcome() {
        for code in [None, Some(42)] {
            let error = interpret_delete_output("/etc/fstab.rustab-backup.1.2", code, b"", b"")
                .expect_err("no response is a failure");
            assert_eq!(error.kind, DeleteBackupErrorKindDto::PrivilegeEscalation);
            assert!(
                error.message.contains("not known whether"),
                "unexpected message: {}",
                error.message
            );
        }
    }

    /// A framing error (the helper could not even parse the request) is
    /// answered in the write-shaped `Error` variant, because at that point the
    /// helper does not yet know which operation it was asked for. The delete
    /// side must read that rather than fall through to "no usable response",
    /// and must report it as "nothing was deleted".
    #[test]
    fn a_protocol_error_reaches_the_delete_side_as_an_escalation_failure() {
        let mut stdin = &b"not json at all"[..];
        let mut stdout: Vec<u8> = Vec::new();
        let code = handle_privileged_request(&mut stdin, &mut stdout, Path::new("/etc/fstab"));
        assert_eq!(code, EXIT_PROTOCOL_ERROR);

        let error =
            interpret_delete_output("/etc/fstab.rustab-backup.1.2", Some(code), &stdout, b"")
                .expect_err("a protocol error is a failure");
        assert_eq!(error.kind, DeleteBackupErrorKindDto::PrivilegeEscalation);
        assert!(error.message.contains("was not deleted"));
        assert!(
            error.message.contains("could not parse its request"),
            "the helper's own account must be carried through: {}",
            error.message
        );
    }

    // -- the elevated manual backup -----------------------------------------

    /// Runs the elevated *backup creator* exactly as `main` does, but with an
    /// in-memory stdin/stdout and a temp file standing in for `/etc/fstab`.
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

    // -- interpretation of the child's output, create-backup path ------------

    #[test]
    fn a_json_create_backup_response_is_trusted_over_the_exit_code() {
        let reported = CreateBackupOutcomeDto {
            target: "/etc/fstab".to_string(),
            backup: "/etc/fstab.rustab-backup.0000000001.000000002.manual".to_string(),
            pruned_backups: Vec::new(),
            notes: Vec::new(),
        };
        let created = serde_json::to_vec(&PrivilegedResponse::BackupCreated {
            outcome: reported.clone(),
        })
        .expect("json");
        // Even with a nonsense exit code, the helper's own account wins.
        let outcome =
            interpret_create_backup_output(Path::new("/etc/fstab"), Some(42), &created, b"")
                .expect("the response says the backup exists");
        assert_eq!(outcome, reported);

        let failure = CreateBackupErrorDto::new(
            CreateBackupErrorKindDto::Backup,
            "could not create a backup at /etc/fstab.rustab-backup.x: No space left on device",
        );
        let failed = serde_json::to_vec(&PrivilegedResponse::BackupCreateFailed {
            error: failure.clone(),
        })
        .expect("json");
        let error =
            interpret_create_backup_output(Path::new("/etc/fstab"), Some(EXIT_OK), &failed, b"")
                .expect_err("an error response is an error");
        assert_eq!(error, failure, "a reported error is forwarded verbatim");
    }

    #[test]
    fn a_dismissed_authorization_for_a_create_backup_says_nothing_was_created() {
        let error = interpret_create_backup_output(
            Path::new("/etc/fstab"),
            Some(PKEXEC_DISMISSED),
            b"",
            b"Error executing command as another user: Not authorized",
        )
        .expect_err("a dismissed prompt is a failure");

        assert_eq!(error.kind, CreateBackupErrorKindDto::PrivilegeEscalation);
        assert!(error.message.contains("no backup"));
        assert!(error.message.contains("/etc/fstab"));
    }

    #[test]
    fn a_refused_authorization_for_a_create_backup_includes_the_stderr_detail() {
        let error = interpret_create_backup_output(
            Path::new("/etc/fstab"),
            Some(PKEXEC_NOT_AUTHORIZED),
            b"",
            b"polkit-agent-helper-1: authentication failure",
        )
        .expect_err("a refused prompt is a failure");

        assert_eq!(error.kind, CreateBackupErrorKindDto::PrivilegeEscalation);
        assert!(error.message.contains("no backup was created"));
        assert!(error.message.contains("authentication failure"));
    }

    /// The honest case: the helper died without reporting, so this side does
    /// not know whether a backup was created and must not pretend either way.
    #[test]
    fn an_unexplained_exit_for_a_create_backup_does_not_claim_an_outcome() {
        for code in [None, Some(42)] {
            let error = interpret_create_backup_output(Path::new("/etc/fstab"), code, b"", b"")
                .expect_err("no response is a failure");
            assert_eq!(error.kind, CreateBackupErrorKindDto::PrivilegeEscalation);
            assert!(
                error.message.contains("not known whether"),
                "unexpected message: {}",
                error.message
            );
        }
    }

    /// A framing error (the helper could not even parse the request) is
    /// answered in the write-shaped `Error` variant, because at that point the
    /// helper does not yet know which operation it was asked for. The create
    /// side must read that rather than fall through to "no usable response",
    /// and must report it as "no backup was created".
    #[test]
    fn a_protocol_error_reaches_the_create_side_as_an_escalation_failure() {
        let mut stdin = &b"not json at all"[..];
        let mut stdout: Vec<u8> = Vec::new();
        let code = handle_privileged_request(&mut stdin, &mut stdout, Path::new("/etc/fstab"));
        assert_eq!(code, EXIT_PROTOCOL_ERROR);

        let error =
            interpret_create_backup_output(Path::new("/etc/fstab"), Some(code), &stdout, b"")
                .expect_err("a protocol error is a failure");
        assert_eq!(error.kind, CreateBackupErrorKindDto::PrivilegeEscalation);
        assert!(error.message.contains("no backup"));
        assert!(
            error.message.contains("could not parse its request"),
            "the helper's own account must be carried through: {}",
            error.message
        );
    }

    /// A response that answers the *other* operation is not an account of this
    /// one, so it is treated as "no usable response" rather than trusted.
    #[test]
    fn a_response_for_the_other_operation_is_not_trusted() {
        let deleted = serde_json::to_vec(&PrivilegedResponse::BackupDeleted).expect("json");
        let error = interpret_write_output(Path::new("/etc/fstab"), Some(EXIT_OK), &deleted, b"")
            .expect_err("a delete response cannot substantiate a write");
        assert_eq!(error.kind, WriteErrorKindDto::PrivilegeEscalation);
        assert!(
            !error.target_unchanged,
            "exit 0 with no account is unknown territory"
        );

        let wrote = serde_json::to_vec(&PrivilegedResponse::Ok {
            outcome: WriteOutcomeDto {
                target: "/etc/fstab".to_string(),
                backup: None,
                pruned_backups: Vec::new(),
                validation_warnings: Vec::new(),
                notes: Vec::new(),
                elevated: false,
                draft_sync_warning: None,
            },
        })
        .expect("json");
        let error =
            interpret_delete_output("/etc/fstab.rustab-backup.1.2", Some(EXIT_OK), &wrote, b"")
                .expect_err("a write response cannot substantiate a delete");
        assert_eq!(error.kind, DeleteBackupErrorKindDto::PrivilegeEscalation);

        // ...and the third operation is no different in either direction.
        let created = serde_json::to_vec(&PrivilegedResponse::BackupCreated {
            outcome: CreateBackupOutcomeDto {
                target: "/etc/fstab".to_string(),
                backup: "/etc/fstab.rustab-backup.0000000001.000000002.manual".to_string(),
                pruned_backups: Vec::new(),
                notes: Vec::new(),
            },
        })
        .expect("json");
        let error =
            interpret_delete_output("/etc/fstab.rustab-backup.1.2", Some(EXIT_OK), &created, b"")
                .expect_err("a create response cannot substantiate a delete");
        assert_eq!(error.kind, DeleteBackupErrorKindDto::PrivilegeEscalation);

        let error =
            interpret_create_backup_output(Path::new("/etc/fstab"), Some(EXIT_OK), &deleted, b"")
                .expect_err("a delete response cannot substantiate a create");
        assert_eq!(error.kind, CreateBackupErrorKindDto::PrivilegeEscalation);

        let error = interpret_write_output(Path::new("/etc/fstab"), Some(EXIT_OK), &created, b"")
            .expect_err("a create response cannot substantiate a write");
        assert_eq!(error.kind, WriteErrorKindDto::PrivilegeEscalation);
        assert!(!error.target_unchanged);
    }

    /// The operations must never disagree about what `pkexec`'s own exit
    /// codes mean. They cannot, because they classify through one shared
    /// [`NoResponseReason`] - asserted here directly, and cross-checked
    /// against the `target_unchanged` the write path derives from it.
    #[test]
    fn all_operations_agree_on_what_pkexecs_exit_codes_mean() {
        let cases = [
            (Some(PKEXEC_DISMISSED), NoResponseReason::Dismissed, true),
            (
                Some(PKEXEC_NOT_AUTHORIZED),
                NoResponseReason::NotAuthorized,
                true,
            ),
            (Some(42), NoResponseReason::Unexplained(Some(42)), false),
            (None, NoResponseReason::Unexplained(None), false),
            (
                Some(EXIT_OK),
                NoResponseReason::Unexplained(Some(EXIT_OK)),
                false,
            ),
        ];

        for (code, expected, nothing_happened) in cases {
            let reason = NoResponseReason::classify(code);
            assert_eq!(reason, expected, "classification of {code:?}");
            assert_eq!(reason.nothing_happened(), nothing_happened);

            // The write path's user-visible guarantee is derived from exactly
            // this, with no usable response on stdout.
            let error = interpret_write_output(Path::new("/etc/fstab"), code, b"", b"")
                .expect_err("no response is a failure");
            assert_eq!(
                error.target_unchanged, nothing_happened,
                "the write path must follow the shared classification for {code:?}"
            );
            assert_eq!(error.kind, WriteErrorKindDto::PrivilegeEscalation);

            // And so is the delete path's wording.
            let error = interpret_delete_output("/b", code, b"", b"")
                .expect_err("no response is a failure");
            assert_eq!(error.kind, DeleteBackupErrorKindDto::PrivilegeEscalation);
            assert_eq!(
                error.message.contains("not known whether"),
                !nothing_happened,
                "the delete path must only hedge where the shared classification does: {}",
                error.message
            );

            // ...and the create path's.
            let error = interpret_create_backup_output(Path::new("/etc/fstab"), code, b"", b"")
                .expect_err("no response is a failure");
            assert_eq!(error.kind, CreateBackupErrorKindDto::PrivilegeEscalation);
            assert_eq!(
                error.message.contains("not known whether"),
                !nothing_happened,
                "the create path must only hedge where the shared classification does: {}",
                error.message
            );
        }
    }

    /// Every [`LaunchFailure`] variant except `Wait` provably happens before
    /// the helper could act, and every operation must say so.
    #[test]
    fn only_a_failed_wait_leaves_the_outcome_unknown() {
        let io_error = || io::Error::from(io::ErrorKind::PermissionDenied);
        let certain = [
            LaunchFailure::NoExePath(io_error()),
            LaunchFailure::Serialize(serde_json::from_str::<u8>("x").expect_err("a parse error")),
            LaunchFailure::Spawn(io_error()),
            LaunchFailure::SendRequest(io_error()),
        ];
        for failure in &certain {
            assert!(failure.nothing_happened(), "{failure:?}");
            assert!(
                write_launch_error(Path::new("/etc/fstab"), failure).target_unchanged,
                "{failure:?}"
            );
            let error = delete_launch_error("/etc/fstab.rustab-backup.1.2", failure);
            assert_eq!(error.kind, DeleteBackupErrorKindDto::PrivilegeEscalation);
            assert!(
                error.message.contains("not deleted"),
                "{failure:?}: {}",
                error.message
            );
            let error = create_launch_error(Path::new("/etc/fstab"), failure);
            assert_eq!(error.kind, CreateBackupErrorKindDto::PrivilegeEscalation);
            assert!(
                error.message.contains("no backup"),
                "{failure:?}: {}",
                error.message
            );
        }

        let unknown = LaunchFailure::Wait(io_error());
        assert!(!unknown.nothing_happened());
        assert!(!write_launch_error(Path::new("/etc/fstab"), &unknown).target_unchanged);
        assert!(delete_launch_error("/b", &unknown)
            .message
            .contains("not known whether"));
        assert!(create_launch_error(Path::new("/etc/fstab"), &unknown)
            .message
            .contains("not known whether"));
    }
}
