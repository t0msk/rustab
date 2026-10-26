//! The unprivileged half of the elevated operations: the `pkexec` re-exec
//! and the interpretation of what came back - see `docs/SPEC.md` §6 step 6
//! and `docs/API_CONTRACT.md` §4.
//!
//! The elevated half itself lives in [`rustab_privileged::protocol`], in a
//! crate with no Tauri, GUI or logging dependency, and is re-exported below
//! so this module still presents the whole mechanism in one place.
//!
//! ## The three operations, and why there is still only one flag
//!
//! Three things need root, only when the active target is the real
//! `/etc/fstab`: **writing the target** (save/restore), **deleting one of
//! its backups**, and **creating a manual backup** - the latter two
//! because an unprivileged process can't unlink or create files in `/etc/`.
//!
//! All three share one dispatch point: the stdin request is a union tagged
//! by `op`, so [`run_internal_if_requested`] still checks exactly one
//! hidden argument and turns *this* process into root through exactly one
//! path, auditable as such. The `rustab-helper` binary has the same
//! property with no argument at all: its whole `main` is that one
//! dispatch.
//!
//! ## The requirement
//!
//! `docs/SPEC.md` §6 step 6 forbids an unprivileged temp-file write
//! followed by a separate privileged `mv`: two privileged steps leave a
//! window where a crash or race can leave `/etc/fstab` in an unplanned
//! state. The elevated process must either do nothing or complete the
//! full atomic swap - which is why this side writes **nothing**: it
//! serializes the candidate to JSON, hands it to the child on stdin, and
//! reads one JSON object back. No temp file to move, no partial state to
//! resume, no second privileged call. The candidate travels as a
//! `DocumentDto`, not rendered text, so the elevated side reconstructs the
//! exact `Document` the user saw and lets `write_document` render it - no
//! second serializer whose output could differ from the first.
//!
//! ## Why an ordinary install re-execs this binary, and why a separate helper exists anyway
//!
//! On an ordinary install the elevated process *is* this binary, started
//! again as `pkexec <this binary> --internal-privileged-write`: one
//! artifact to build, package and sign, nothing that can drift from the
//! audited `write_document` code, and no second copy of the protocol to
//! keep in step. [`run_internal_if_requested`] runs before Tauri, the
//! webview, the plugins and the logger initialize, so the elevated surface
//! is the same minimal one either way, and `pkexec` prompts per invocation
//! regardless.
//!
//! That argument stops holding inside a Flatpak sandbox. The GUI binary is
//! dynamically linked against the **runtime's** GTK, WebKitGTK and glibc,
//! and the `pkexec` has to happen on the **host** (see below), which may
//! satisfy none of those libraries - a re-exec there can fail before it
//! reads a single byte of its request. So Rustab also ships
//! `rustab-helper`: the same elevated code
//! ([`rustab_privileged::protocol::handle_privileged_request`], not a copy
//! of it) in a binary with no GUI dependency at all, which can be linked
//! statically and therefore runs on a host whose libraries are unknown.
//! Two artifacts, still one implementation - the drift risk a helper
//! normally brings is avoided by sharing the crate rather than by avoiding
//! the binary.
//!
//! ## What happens instead, inside a Flatpak sandbox
//!
//! The `pkexec` invocation must run on the **host**, via `flatpak-spawn
//! --host`, naming the elevated binary by its host-side path rather than
//! `std::env::current_exe()`. `pkexec` changes a process's UID, not its
//! mount namespace, so a `pkexec` child started inside the sandbox is root
//! *in the sandbox*, with no path to the real `/etc/fstab` at all.
//! `flatpak-spawn --host pkexec <host path>` gives a genuine host root
//! process instead.
//!
//! The binary named there is **the helper, with no flag**:
//! `flatpak-spawn --host pkexec <app-path>/bin/rustab-helper`.
//! `rustab-helper` refuses any argument, so [`INTERNAL_WRITE_FLAG`] is not
//! passed in that shape and is a GUI-only entry point. All of that lives
//! in [`crate::host`]; the only line here that knows about it is in
//! [`run_elevated`]. Nothing in this module's protocol, request shape or
//! response interpretation changes, and each binary still has exactly one
//! way in.
//!
//! ## What this side deliberately does *not* decide
//!
//! The target path, the backup path and the expected previous content all
//! travel inside the request, and every one of them is re-checked inside
//! the elevated process, which trusts nothing this side claims - see
//! [`rustab_privileged::protocol`] for each check and why it lives there.
//! The pre-checks on this side (validating a candidate, resolving a backup
//! path, comparing the real file against the marker) exist only to avoid
//! asking for a password for an operation that cannot succeed; each could
//! be deleted without weakening a guarantee.

use crate::dto::{
    CreateBackupErrorDto, CreateBackupErrorKindDto, CreateBackupOutcomeDto, DeleteBackupErrorDto,
    DeleteBackupErrorKindDto, DocumentDto, WriteErrorDto, WriteErrorKindDto, WriteOutcomeDto,
};
use crate::target;
use fstab_core::model::Document;
use fstab_core::{validate, writer};
use std::io::{self, Write};
use std::path::Path;
use std::process::{Output, Stdio};

// Re-exported so `rustab_lib::privileged` still names the whole mechanism,
// as it did before the elevated half moved to its own crate (module docs).
pub use rustab_privileged::protocol::{
    handle_privileged_request, protocol_error_body, PrivilegedRequest, PrivilegedResponse, EXIT_OK,
    EXIT_OPERATION_ERROR, EXIT_PROTOCOL_ERROR,
};

/// The hidden first argument that turns **this** binary into the elevated
/// helper.
///
/// Not a public CLI: an implementation detail of the escalation mechanism,
/// with an unstable stdin/stdout protocol. The only thing that ever passes
/// it is [`crate::host::elevated_plan`]'s ordinary-install shape - inside a
/// Flatpak the elevated process is `rustab-helper`, which takes no argument
/// at all, so this flag never travels there.
///
/// The name predates the two backup operations and now carries any
/// [`PrivilegedRequest`] variant; kept as-is since changing it buys
/// nothing, and the whole point is that there's exactly *one* string that
/// can turn this process into a root process.
pub const INTERNAL_WRITE_FLAG: &str = "--internal-privileged-write";

/// How much of the child's stderr is quoted back in an error message.
const STDERR_EXCERPT_LIMIT: usize = 1000;

/// `pkexec`'s exit code when the user dismissed the authentication dialog.
/// The program was never executed.
const PKEXEC_DISMISSED: i32 = 126;

/// `pkexec`'s exit code when authorization could not be obtained or the
/// program could not be executed. Again, the program never ran.
const PKEXEC_NOT_AUTHORIZED: i32 = 127;

// ---------------------------------------------------------------------------
// The elevated side's one entry point in this binary
// ---------------------------------------------------------------------------

/// Runs the elevated helper if this process was started with
/// [`INTERNAL_WRITE_FLAG`] as its first argument, returning the exit code to
/// terminate with. Returns `None` for a normal GUI launch.
///
/// Called as the very first statement of `main` so that, in elevated mode,
/// no Tauri runtime, webview, plugin or logger is ever initialized. This is
/// the *only* branch in this binary that can do that, for any of the three
/// operations - see the module docs.
///
/// The work itself is [`handle_privileged_request`], the same function
/// `rustab-helper` calls: this branch is the argv check and the stdio
/// handles, nothing more.
pub fn run_internal_if_requested() -> Option<i32> {
    let mut args = std::env::args_os();
    args.next()?; // argv[0]
    if args.next()? != INTERNAL_WRITE_FLAG {
        return None;
    }
    if args.next().is_some() {
        // Same shape as `rustab-helper`'s own argv refusal: exit code 2
        // always carries a JSON body on stdout, from either binary.
        let message = stray_argument_message();
        println!("{}", protocol_error_body(&message));
        eprintln!("{message}");
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

/// What [`run_internal_if_requested`] says when it is handed an argument it
/// does not know, on both stdout (wrapped by [`protocol_error_body`]) and
/// stderr.
///
/// A function of its own only so a test can pin the shape of that refusal
/// without driving a second process: the branch itself reads the real
/// `argv` and writes the real stdout.
fn stray_argument_message() -> String {
    format!("{INTERNAL_WRITE_FLAG} takes no further arguments; nothing was done")
}

// ---------------------------------------------------------------------------
// The unprivileged side
// ---------------------------------------------------------------------------

/// Why the elevated half could not be run to completion, before it had a
/// chance to report anything of its own.
///
/// Carries the underlying error but no message: each operation words its
/// own ("nothing was written" vs. "nothing was deleted"), while the
/// shared, safety-relevant part is the mechanics - step order, killing a
/// child whose stdin failed, and which failures mean "provably never acted".
#[derive(Debug)]
enum LaunchFailure {
    /// No path to start: [`std::env::current_exe`] failed, or - inside a
    /// Flatpak sandbox - `rustab-helper` wasn't found at any host-side path
    /// (see [`crate::host::elevated_plan`]). The helper never ran.
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

/// Starts the elevated half as root, hands `request` to it on stdin, and
/// returns what it printed and exited with.
///
/// The whole `pkexec` plumbing for **both** operations, in one function,
/// since the spawn/stdin-write/pipe-close/wait sequence is identical for
/// both and two copies could drift apart in safety-relevant ways (e.g. a
/// forgotten `child.kill()` on a failed stdin write leaving a root process
/// blocked on a pipe forever).
///
/// Performs no filesystem writes of its own - just JSON over a pipe.
///
/// Under Flatpak, *which command* is spawned changes: not `pkexec <this
/// binary> <flag>` but `flatpak-spawn --host pkexec
/// <host path>/bin/rustab-helper`, with no flag - `pkexec` alone would
/// produce a root process still inside the sandbox, unable to see the real
/// `/etc/`, and the GUI binary cannot be trusted to exec on an unknown
/// host. That decision, the choice of binary and the host-path resolution
/// all live in [`crate::host::elevated_plan`]; this function only asks for
/// the plan and runs it.
///
/// Everything after the spawn is untouched and deliberately so: writing the
/// request to stdin, closing it, waiting, and handing the exit code, stdout
/// and stderr to the `interpret_*_output` functions all behave identically
/// through `flatpak-spawn --host pkexec`, which passes stdio through and
/// exits with the child's status (confirmed by the sandbox diagnostics
/// recorded in `docs/FLATPAK.md` §3.2). The process on the other end runs
/// the same [`handle_privileged_request`] compiled from the same source -
/// in the Flatpak case from the helper's `main` instead of this binary's
/// flag branch - and it runs on the host, where, having no
/// `/.flatpak-info` of its own, it reads and writes the real filesystem
/// directly, exactly as the non-Flatpak case always did.
fn run_elevated(request: &PrivilegedRequest) -> Result<Output, LaunchFailure> {
    let plan = crate::host::elevated_plan(INTERNAL_WRITE_FLAG).map_err(LaunchFailure::NoExePath)?;
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
/// No filesystem writes of its own - everything that touches disk happens
/// inside the elevated [`writer::write_document`] call.
///
/// `guard` is what the caller believes `target` currently holds, plus the
/// user's "write anyway" answer. Pure transport here: this function
/// neither reads `target` nor decides anything from it; the elevated side
/// re-reads the file and makes the call (`rustab_privileged::protocol`'s
/// `handle_write`), which is why the password wait can't hide a change from
/// the check.
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
    // This half runs in the GUI process, which may be sandboxed, so its
    // `findmnt --verify` goes through the host runner; otherwise it would
    // reject every document a sandboxed save could ever submit.
    let report = validate::validate_with(candidate, crate::host::command_runner());
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
/// Pure, so every outcome (including `pkexec`-specific exit codes that
/// can't be reliably provoked in a test) is unit-testable on its own.
///
/// A parsable JSON response always wins over the exit code - the helper's
/// own account of what it did. Only without one does the exit code decide,
/// conservatively: `target_unchanged` only for codes that mean the helper
/// never ran.
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
/// Needed because the real `/etc/fstab`'s backups live beside it in
/// `/etc/`, which an unprivileged process can't unlink from. A non-real
/// target's backups live in a user-owned directory and never come through here.
///
/// No filesystem changes of its own - the elevated half re-resolves
/// `backup_path` against `target`'s real backup list before unlinking
/// anything, so neither half can be talked into deleting a file that
/// isn't one of this target's backups.
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

/// Creates a manual backup of `target` as root, by re-executing this
/// binary under `pkexec` with [`INTERNAL_WRITE_FLAG`].
///
/// Needed because a backup of the real `/etc/fstab` must be created *in*
/// `/etc/`, which an unprivileged process can't write to - the mirror
/// image of [`delete_backup_elevated`]. A non-real target's backups never
/// come through here.
///
/// No filesystem changes of its own - the elevated half chooses the
/// backup's path itself from the verified target, so there's no path in
/// this request that could point it at the wrong file.
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
    use fstab_core::parser;

    /// Two entries claiming the same mount point: fails structural validation.
    ///
    /// The one candidate this half still needs - everything else it asserts
    /// is about interpreting the child's output, not about documents.
    fn invalid_candidate() -> Document {
        parser::parse(
            "UUID=11111111-2222-3333-4444-555555555555 /mnt/clash ext4 noauto 0 2\n\
             UUID=22222222-3333-4444-5555-666666666666 /mnt/clash ext4 noauto 0 2\n",
        )
    }

    // -- this binary's elevated entry point ---------------------------------

    /// A stray argument is refused with a parsable protocol body on stdout,
    /// not only a line on stderr, so "exit code 2 always comes with a JSON
    /// body" holds for this binary exactly as it does for `rustab-helper` -
    /// and the write side reads that body as an operation that provably
    /// touched nothing.
    #[test]
    fn a_stray_argument_is_refused_with_a_parsable_protocol_error() {
        let body = protocol_error_body(&stray_argument_message());

        let error = interpret_write_output(
            Path::new("/etc/fstab"),
            Some(EXIT_PROTOCOL_ERROR),
            body.as_bytes(),
            b"",
        )
        .expect_err("a refused argv is not a completed write");

        assert_eq!(error.kind, WriteErrorKindDto::Internal);
        assert!(error.target_unchanged);
        assert!(
            error.message.contains(INTERNAL_WRITE_FLAG) && error.message.contains("nothing"),
            "the refusal must say what was refused and that nothing happened: {}",
            error.message
        );
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
    /// the target itself (the external-change guard is pure transport here;
    /// it is the elevated side that re-reads the file).
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
