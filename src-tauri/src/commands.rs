//! The `#[tauri::command]` layer: the complete, frozen set of calls the
//! frontend may make - see `docs/API_CONTRACT.md`.
//!
//! ## Shape of this module
//!
//! Each command is a thin, boring wrapper: it resolves the active target
//! through [`crate::target`], converts DTOs through [`crate::dto`], and
//! delegates the real work to `fstab-core`. No command re-implements
//! validation, serialization, backup naming or writing, and no command takes
//! a *target* path from the frontend. The carefully bounded exceptions -
//! [`restore_backup`], [`read_backup`] and [`delete_backup`], which take a
//! *backup* path - never use it as a path until it has been matched against
//! the active target's real backup list (see [`crate::backups`]).
//!
//! The genuinely interesting logic - "which save path do I take" - is
//! [`save_document_with`], and it is a single `if` on purpose: both branches
//! end in the same [`writer::write_document`] call, one of them simply has a
//! `pkexec` re-exec in front of it (see [`crate::privileged`]).
//! [`delete_backup_with`] is the same shape for the same reason: the real
//! `/etc/fstab`'s backups live in root-owned `/etc/`, so removing one needs
//! the same single escalation, while a sandbox target's backups live in a
//! user-owned directory and need none.
//!
//! Reading a backup, by contrast, needs no escalation at all in either case:
//! `/etc/fstab` and the backups beside it are world-readable, which is the
//! same reason [`list_entries`] and [`list_backups`] already work against the
//! real target unprivileged.
//!
//! ## Why every command is `#[tauri::command(async)]`
//!
//! Several of these do blocking work: `lsblk`, `findmnt`, file I/O, and in
//! the `/etc/fstab` case a `pkexec` call that blocks until the user has typed
//! a password. A plain synchronous Tauri command runs on the main thread,
//! which would freeze the window for the duration. `(async)` makes Tauri run
//! these synchronous functions on its thread pool instead, which keeps them
//! ordinary blocking Rust functions - directly callable from the unit tests
//! below, with no runtime to set up and no `AppHandle` to fake.
//!
//! ## Why the argument-taking commands add `rename_all = "snake_case"`
//!
//! Tauri renames command *arguments* to camelCase on the IPC boundary by
//! default, while serde leaves the DTO *fields* inside them in snake_case.
//! That mix would mean a frontend sending `{"backupPath": ..., }` whose
//! nested objects use `{"mount_point": ...}` - one spelling rule per nesting
//! level, which is exactly the kind of detail an integration task gets wrong
//! once and then debugs for an hour. Pinning the argument case to snake_case
//! makes the whole payload, at every level, snake_case.

use crate::backups;
use crate::dto::{
    BackupInfoDto, CommandErrorDto, CommandErrorKindDto, CreateBackupErrorDto,
    CreateBackupErrorKindDto, CreateBackupOutcomeDto, DeleteBackupErrorDto,
    DeleteBackupErrorKindDto, DeviceDiscoveryDto, DocumentDto, DocumentViewDto, ExternalLinkDto,
    TargetInfoDto, ValidationReportDto, WriteErrorDto, WriteErrorKindDto, WriteOutcomeDto,
};
use crate::privileged;
use crate::target::{self, ActiveTarget};
use fstab_core::model::Document;
use fstab_core::{discovery, parser, validate, writer};
use std::fs;
use std::io;
use std::path::Path;

/// Returns the active target: which file this session reads and writes, and
/// whether it is the real `/etc/fstab`.
///
/// Resolving the target can create the sandbox file, which is why this can
/// fail; see [`target::active_target`].
///
/// # Errors
///
/// [`CommandErrorKindDto::TargetResolution`] if the target could not be
/// determined.
#[tauri::command(async)]
pub fn get_target_info() -> Result<TargetInfoDto, CommandErrorDto> {
    Ok(TargetInfoDto::from(&resolve_target()?))
}

/// Reads the active target and returns its text together with its parsed
/// form.
///
/// A target that does not exist yet is not an error: it reports
/// `exists: false` with empty text and no lines, which is the correct
/// starting state for a first-ever save.
///
/// # Errors
///
/// [`CommandErrorKindDto::TargetResolution`] if the target could not be
/// determined, [`CommandErrorKindDto::ReadTarget`] if it exists but could not
/// be read, [`CommandErrorKindDto::NotUtf8`] if its bytes are not valid
/// UTF-8.
#[tauri::command(async)]
pub fn list_entries() -> Result<DocumentViewDto, CommandErrorDto> {
    let target = resolve_target()?;
    let info = TargetInfoDto::from(&target);

    let (exists, text) = match fs::read(&target.path) {
        Ok(bytes) => {
            // Deliberately *not* `from_utf8_lossy`. A lossy read would show
            // the user a file with U+FFFD where their bytes were, and a save
            // would then write that mangled text back over a file that was
            // fine before. Refusing to load is the only safe answer, and the
            // message says what to do about it.
            let text = String::from_utf8(bytes).map_err(|err| {
                CommandErrorDto::new(
                    CommandErrorKindDto::NotUtf8,
                    format!(
                        "{} is not valid UTF-8 ({err}), so Rustab will not load it - editing it \
                         here could only corrupt it. Fix the encoding with a text editor first.",
                        target.path.display()
                    ),
                )
            })?;
            (true, text)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => (false, String::new()),
        Err(err) => {
            return Err(CommandErrorDto::new(
                CommandErrorKindDto::ReadTarget,
                format!("could not read {}: {err}", target.path.display()),
            ))
        }
    };

    let document = parser::parse(&text);
    Ok(DocumentViewDto {
        target: info,
        exists,
        text,
        has_malformed_lines: document.has_malformed_lines(),
        document: DocumentDto::from(&document),
    })
}

/// Validates a candidate document without writing anything.
///
/// Runs the identical [`validate::validate`] call a save runs, so the
/// findings shown in the form are the findings that will block (or not block)
/// the save. Nothing on disk is touched: the semantic `findmnt --verify`
/// check inside `validate` renders the candidate to its own throwaway temp
/// file, never to the target.
#[tauri::command(async, rename_all = "snake_case")]
pub fn validate_candidate(candidate: DocumentDto) -> ValidationReportDto {
    ValidationReportDto::from(&validate::validate(&Document::from(candidate)))
}

/// Renders a candidate document to the exact text a save would write.
///
/// Exists because the frontend has no dependency on `fstab-core` and so
/// cannot serialize a document itself, while the diff view
/// (`docs/UI_DESIGN.md` §6) has to show the user precisely what would land in
/// the file. Pure: no I/O at all.
#[tauri::command(async, rename_all = "snake_case")]
pub fn render_candidate(candidate: DocumentDto) -> String {
    Document::from(candidate).to_string()
}

/// Saves `candidate` over the active target, atomically.
///
/// Dispatches on [`ActiveTarget::is_real`]: a sandbox (or any other
/// non-`/etc/fstab`) target is written in-process; the real `/etc/fstab` is
/// written by a single elevated re-exec that performs the whole operation as
/// root. Both paths run the same [`writer::write_document`].
///
/// # Errors
///
/// A [`WriteErrorDto`]; check its `target_unchanged` field, which is `true`
/// only when the target is guaranteed to still hold its previous bytes.
#[tauri::command(async, rename_all = "snake_case")]
pub fn save(candidate: DocumentDto) -> Result<WriteOutcomeDto, WriteErrorDto> {
    let target = resolve_target_for_write()?;
    save_document(&target, &Document::from(candidate))
}

/// Lists the active target's auto-backups, newest first.
///
/// # Errors
///
/// [`CommandErrorKindDto::TargetResolution`] if the target could not be
/// determined, [`CommandErrorKindDto::ListBackups`] if its directory could
/// not be read.
#[tauri::command(async)]
pub fn list_backups() -> Result<Vec<BackupInfoDto>, CommandErrorDto> {
    let target = resolve_target()?;
    backups::list(&target.path)
        .map_err(|err| CommandErrorDto::new(CommandErrorKindDto::ListBackups, err.to_string()))
}

/// Restores a previously taken backup onto the active target.
///
/// `backup_path` must be one of the paths [`list_backups`] returned for the
/// *current* target; anything else is refused before being used as a path at
/// all (see [`crate::backups`]).
///
/// A restore is **not** a privileged copy and not a shortcut: the backup's
/// text is parsed into a document and then goes through the very same
/// [`save_document`] a normal save uses, so it is validated, backed up (the
/// content being replaced is itself backed up first), written to a temp file,
/// `fsync`ed, renamed, verified - and, for the real `/etc/fstab`, elevated
/// exactly once. Restoring a broken backup is therefore refused for the same
/// reasons saving broken content is.
///
/// # Errors
///
/// [`WriteErrorKindDto::InvalidBackup`] if `backup_path` is not one of this
/// target's backups, [`WriteErrorKindDto::ReadBackup`] if it cannot be read
/// or is not UTF-8, otherwise whatever the save itself reported.
#[tauri::command(async, rename_all = "snake_case")]
pub fn restore_backup(backup_path: String) -> Result<WriteOutcomeDto, WriteErrorDto> {
    let target = resolve_target_for_write()?;

    // Both failure modes - "the list could not be read" and "the requested
    // path is not in it" - are reported as `InvalidBackup`: from the
    // frontend's point of view they are the same situation (this backup
    // cannot be used, and nothing was written), and the message says which
    // one it was.
    let backup = backups::resolve_requested(&target.path, &backup_path).map_err(|err| {
        WriteErrorDto::new(
            WriteErrorKindDto::InvalidBackup,
            true,
            format!("{err}; nothing was restored"),
        )
    })?;

    let text = backups::read_text(&backup).map_err(|err| {
        WriteErrorDto::new(
            WriteErrorKindDto::ReadBackup,
            true,
            format!(
                "could not read the backup {}: {err}; nothing was restored",
                backup.display()
            ),
        )
        .with_path(&backup)
    })?;

    save_document(&target, &parser::parse(&text))
}

/// Returns a backup's raw text, for the backup-diff view.
///
/// `backup_path` must be one of the paths [`list_backups`] returned for the
/// *current* target, and is resolved exactly the way [`restore_backup`]
/// resolves it - matched against the real backup list before it is ever used
/// as a path (see [`crate::backups`]).
///
/// Writes nothing and needs no privilege escalation for either target kind:
/// `/etc/fstab` and the backups beside it are world-readable, so an
/// unprivileged read suffices - the same reason [`list_entries`] can read the
/// real target without a password prompt. Pair the returned text with
/// [`list_entries`]'s `text` to diff a restore point against what is live now.
///
/// # Errors
///
/// [`CommandErrorKindDto::TargetResolution`] if the target could not be
/// determined, [`CommandErrorKindDto::InvalidBackup`] if `backup_path` is not
/// one of this target's backups, [`CommandErrorKindDto::ReadBackup`] if it is
/// one but could not be read, [`CommandErrorKindDto::NotUtf8`] if its bytes
/// are not valid UTF-8.
#[tauri::command(async, rename_all = "snake_case")]
pub fn read_backup(backup_path: String) -> Result<String, CommandErrorDto> {
    let target = resolve_target()?;
    read_backup_of(&target.path, &backup_path)
}

/// Deletes one of the active target's backups.
///
/// `backup_path` must be one of the paths [`list_backups`] returned for the
/// *current* target. Only ever a backup file: the active target itself is
/// never one of its own backups, so it can never be the file removed here
/// (asserted by [`crate::privileged`]'s and this module's tests).
///
/// Dispatches on [`ActiveTarget::is_real`] exactly like [`save`] and
/// [`restore_backup`] do: a non-real target's backups live in a user-owned
/// directory and are removed in-process, while the real `/etc/fstab`'s
/// backups live in root-owned `/etc/` and are removed by a single elevated
/// re-exec (see [`crate::privileged`]). Unlike a save, this never opens,
/// renames or writes the target file - which is why it rejects with a
/// [`DeleteBackupErrorDto`] rather than a [`WriteErrorDto`]: there is no
/// `target_unchanged` question to answer.
///
/// # Errors
///
/// A [`DeleteBackupErrorDto`]; see [`DeleteBackupErrorKindDto`] for the kinds.
#[tauri::command(async, rename_all = "snake_case")]
pub fn delete_backup(backup_path: String) -> Result<(), DeleteBackupErrorDto> {
    let target = resolve_target_for_delete()?;
    delete_backup_from(&target, &backup_path)
}

/// Creates a backup of the active target right now, without changing it.
///
/// The explicit counterpart of the backup a [`save`] takes on its own: it
/// copies the target's *current* content to a new **manual** backup beside it
/// and returns that backup as a [`BackupInfoDto`] - the same row shape
/// [`list_backups`] produces, so the frontend can prepend it or simply
/// re-list. A manual backup is exempt from the retention bound that prunes
/// automatic ones (`fstab_core::writer::MAX_BACKUPS`); only an explicit
/// [`delete_backup`] ever removes it.
///
/// Takes no arguments: like every other command here, *which* file it acts on
/// is resolved server-side and never supplied by the frontend. Dispatches on
/// [`ActiveTarget::is_real`] exactly like [`save`] and [`delete_backup`] do -
/// a non-real target is copied in-process, the real `/etc/fstab` through a
/// single elevated re-exec, because its backups have to be created in
/// root-owned `/etc/`.
///
/// This never opens, renames or writes the target file; it only reads it.
/// That is why it rejects with a [`CreateBackupErrorDto`] rather than a
/// [`WriteErrorDto`], for the same reason [`delete_backup`] does: there is no
/// `target_unchanged` question to answer.
///
/// # Errors
///
/// A [`CreateBackupErrorDto`]; see [`CreateBackupErrorKindDto`] for the kinds.
/// Notably [`CreateBackupErrorKindDto::TargetMissing`] when the target does
/// not exist yet - an explicit "back this up now" reports that rather than
/// succeeding with nothing to show.
#[tauri::command(async)]
pub fn create_backup() -> Result<BackupInfoDto, CreateBackupErrorDto> {
    let target = resolve_target_for_create_backup()?;
    create_backup_of(&target)
}

/// Lists block devices for the source picker.
///
/// Never fails: a system where `lsblk` is missing or unusable yields an empty
/// list with `lsblk_available: false`, and manual entry still works
/// (`docs/SPEC.md` §8).
#[tauri::command(async)]
pub fn discover_devices() -> DeviceDiscoveryDto {
    DeviceDiscoveryDto::from(&discovery::discover_devices())
}

/// Opens the directory containing the active target in the host file
/// manager, via `xdg-open`.
///
/// The active target's directory is also where its backups live (see
/// [`crate::backups`]'s module docs), so this is the one button the Backups
/// panel needs to let the user browse the backup files directly.
///
/// **No privilege escalation, for either target kind.** `/etc/` is
/// world-readable and world-executable, which is the same reasoning
/// [`read_backup`] already documents for reading a backup's content -
/// listing a directory needs strictly less access than reading a file inside
/// it.
///
/// # Errors
///
/// [`CommandErrorKindDto::TargetResolution`] if the target could not be
/// determined, [`CommandErrorKindDto::OpenLocation`] if the file manager
/// could not be launched.
#[tauri::command(async, rename_all = "snake_case")]
pub fn open_backup_location() -> Result<(), CommandErrorDto> {
    let target = resolve_target()?;
    open_backup_location_of(&target.path)
}

/// `open_external_link(link) -> null` (API_CONTRACT.md §3.13). Opens one
/// of this app's five known external links (used by the About dialog) via
/// `xdg-open` - the GitHub repo and Sponsors links in the host's default web
/// browser, the `Email` link's `mailto:` URI in the host's default mail
/// client (`xdg-open` dispatches by URI scheme, so no separate code path is
/// needed for that case).
///
/// Deliberately takes a closed enum, not a raw URL string: the URLs/URIs
/// this command will ever open are a fixed, hardcoded set baked into
/// [`ExternalLinkDto::url`], never anything the frontend supplies -
/// applying this project's minimal-dispatch-surface philosophy (compare
/// `privileged.rs`'s own module doc) to a non-privileged case: there is no
/// argument surface here for a caller to redirect this command at an
/// arbitrary URL. This is also why the About dialog's email line goes
/// through this command rather than a plain `<a href="mailto:...">` - an
/// in-webview anchor navigation to a non-`http(s)` scheme is not guaranteed
/// to be handled by the host webview at all (WebKitGTK shows "The URL can't
/// be shown" for `mailto:` rather than falling back to the OS's own
/// scheme handler), so this command's `xdg-open` dispatch is the only
/// reliable way to reach the user's mail client from inside the app.
///
/// **No privilege escalation.** Opening a browser or mail client needs no
/// elevated access, same reasoning as [`open_backup_location`].
///
/// # Errors
///
/// [`CommandErrorKindDto::OpenExternalLink`] if the external program could
/// not be launched.
#[tauri::command(async, rename_all = "snake_case")]
pub fn open_external_link(link: ExternalLinkDto) -> Result<(), CommandErrorDto> {
    open_external_link_with(link, open_browser)
}

// ---------------------------------------------------------------------------
// Shared internals
// ---------------------------------------------------------------------------

/// The one dispatch point between the two save paths.
///
/// Both branches end in [`writer::write_document`] - the in-process branch
/// calls it directly, the elevated branch calls it inside a root process that
/// does nothing else (see [`crate::privileged`]). There is deliberately no
/// third branch, and no path that writes anything without going through
/// `write_document`.
///
/// Takes the already-resolved [`ActiveTarget`] rather than resolving it
/// itself so the tests can point a save at a temp file without mutating
/// process-wide environment variables (which would race with every other
/// test in the binary).
fn save_document(
    target: &ActiveTarget,
    candidate: &Document,
) -> Result<WriteOutcomeDto, WriteErrorDto> {
    save_document_with(target, candidate, privileged::save_elevated)
}

/// [`save_document`], with the elevated half injectable.
///
/// The seam exists for one reason: the dispatch decision ("a real target is
/// never written by this process") has to be *asserted*, and asserting it
/// against the production `elevate` would mean a unit test that launches
/// `pkexec` and waits on an authentication prompt. The tests therefore pass a
/// recording stand-in and check that a real target reaches it and never
/// reaches the in-process writer; the production path is a one-line
/// delegation that cannot diverge.
fn save_document_with(
    target: &ActiveTarget,
    candidate: &Document,
    elevate: impl Fn(&Path, &Document) -> Result<WriteOutcomeDto, WriteErrorDto>,
) -> Result<WriteOutcomeDto, WriteErrorDto> {
    if target.is_real {
        elevate(&target.path, candidate)
    } else {
        writer::write_document(&target.path, candidate)
            .map(|outcome| WriteOutcomeDto::from_outcome(&outcome, false))
            .map_err(|err| WriteErrorDto::from(&err))
    }
}

/// The body of [`read_backup`], with the target passed in.
///
/// Split out for the same reason [`save_document`] is: the tests can point it
/// at a temp file without mutating the process-wide environment that
/// [`target::active_target`] reads.
fn read_backup_of(target: &Path, backup_path: &str) -> Result<String, CommandErrorDto> {
    let backup = backups::resolve_requested(target, backup_path)
        .map_err(|err| CommandErrorDto::new(CommandErrorKindDto::InvalidBackup, err.to_string()))?;

    backups::read_text(&backup).map_err(|err| {
        // `read_text`'s own `InvalidData` is the not-valid-UTF-8 case, which
        // already has a kind of its own that the frontend handles for the
        // target; reusing it keeps one meaning per kind.
        let kind = if err.kind() == io::ErrorKind::InvalidData {
            CommandErrorKindDto::NotUtf8
        } else {
            CommandErrorKindDto::ReadBackup
        };
        CommandErrorDto::new(
            kind,
            format!("could not read the backup {}: {err}", backup.display()),
        )
    })
}

/// The body of [`open_backup_location`], with the target passed in (see
/// [`read_backup_of`] for why).
fn open_backup_location_of(target: &Path) -> Result<(), CommandErrorDto> {
    open_backup_location_with(target, open_in_file_manager)
}

/// [`open_backup_location_of`], with the actual launch of a GUI program
/// injectable.
///
/// The seam exists for the same structural reason [`save_document_with`]'s
/// does: a unit test has to be able to assert *which* directory this command
/// would open without truly spawning a file manager window - there is no way
/// to do that in CI, which has no display and no file manager installed at
/// all. The production path is a one-line delegation to
/// [`open_in_file_manager`] that cannot diverge from what ships.
fn open_backup_location_with(
    target: &Path,
    opener: impl Fn(&Path) -> io::Result<()>,
) -> Result<(), CommandErrorDto> {
    let dir = backups::containing_dir(target);
    opener(&dir).map_err(|err| {
        CommandErrorDto::new(
            CommandErrorKindDto::OpenLocation,
            format!("could not open {} in a file manager: {err}", dir.display()),
        )
    })
}

/// Launches the host file manager on `dir` via `xdg-open`.
///
/// `spawn`, never `output`/`status`: this command must return as soon as the
/// file manager process has started, not block the async command handler
/// until the user closes that window. Only a failure to *start* the process
/// (the binary is missing, or launching it was refused) is reported back;
/// what the file manager itself does afterwards is none of this command's
/// business.
fn open_in_file_manager(dir: &Path) -> io::Result<()> {
    std::process::Command::new("xdg-open")
        .arg(dir)
        .spawn()
        .map(|_child| ())
}

/// [`open_external_link`], with the actual browser launch injectable -
/// same structural reason as [`open_backup_location_with`]: a unit test
/// has to assert which URL this command would open without truly
/// spawning a browser.
fn open_external_link_with(
    link: ExternalLinkDto,
    opener: impl Fn(&str) -> io::Result<()>,
) -> Result<(), CommandErrorDto> {
    let url = link.url();
    opener(url).map_err(|err| {
        CommandErrorDto::new(
            CommandErrorKindDto::OpenExternalLink,
            format!("could not open {url}: {err}"),
        )
    })
}

/// Launches the host's default handler for `url` via `xdg-open` - a web
/// browser for an `http(s)` URL, the default mail client for a `mailto:`
/// URI, dispatched by `xdg-open` itself based on the scheme; this function
/// does not need to know which.
/// Deliberately separate from [`open_in_file_manager`] rather than
/// generalized over both (different argument types, `&Path` vs. `&str`,
/// and each is a single one-line `spawn()` call - not worth sharing a
/// generic seam over, and keeping this separate means zero risk of
/// regressing the already-shipped `open_backup_location`).
fn open_browser(url: &str) -> io::Result<()> {
    std::process::Command::new("xdg-open")
        .arg(url)
        .spawn()
        .map(|_child| ())
}

/// The body of [`delete_backup`], with the target passed in (see
/// [`read_backup_of`] for why).
fn delete_backup_from(
    target: &ActiveTarget,
    backup_path: &str,
) -> Result<(), DeleteBackupErrorDto> {
    delete_backup_with(target, backup_path, privileged::delete_backup_elevated)
}

/// The one dispatch point between the two backup-deletion paths, with the
/// elevated half injectable.
///
/// Mirrors [`save_document_with`] exactly, and for the same reason: the
/// dispatch decision ("a real target's backups are never unlinked by this
/// process") has to be *asserted*, and asserting it against the production
/// `privileged::delete_backup_elevated` would mean a unit test that launches
/// `pkexec` and waits on an authentication prompt.
///
/// `backup_path` is resolved before the dispatch, in **both** branches. For
/// the elevated branch that is only a courtesy - it means a path that cannot
/// work is refused without making the user authenticate first, the same
/// courtesy [`privileged::save_elevated`]'s pre-validation provides. The
/// authoritative check is the elevated process's own
/// [`backups::resolve_requested`] call, which does not trust this one (see
/// [`crate::privileged`]).
fn delete_backup_with(
    target: &ActiveTarget,
    backup_path: &str,
    elevate: impl Fn(&Path, &str) -> Result<(), DeleteBackupErrorDto>,
) -> Result<(), DeleteBackupErrorDto> {
    let backup = backups::resolve_requested(&target.path, backup_path).map_err(|err| {
        DeleteBackupErrorDto::new(
            DeleteBackupErrorKindDto::InvalidBackup,
            format!("{err}; nothing was deleted"),
        )
    })?;

    if target.is_real {
        elevate(&target.path, &backup.to_string_lossy())
    } else {
        fs::remove_file(&backup).map_err(|err| {
            DeleteBackupErrorDto::new(
                DeleteBackupErrorKindDto::Delete,
                format!("could not delete the backup {}: {err}", backup.display()),
            )
        })
    }
}

/// The body of [`create_backup`], with the target passed in (see
/// [`read_backup_of`] for why).
fn create_backup_of(target: &ActiveTarget) -> Result<BackupInfoDto, CreateBackupErrorDto> {
    create_backup_with(target, privileged::create_backup_elevated)
}

/// The one dispatch point between the two backup-creation paths, with the
/// elevated half injectable.
///
/// Mirrors [`save_document_with`] and [`delete_backup_with`] exactly, and for
/// the same reason: the dispatch decision ("a real target is never copied by
/// this process") has to be *asserted*, and asserting it against the
/// production [`privileged::create_backup_elevated`] would mean a unit test
/// that launches `pkexec` and waits on an authentication prompt.
///
/// Both branches end in the same `fstab_core::writer::create_backup` - the
/// in-process branch calls it directly, the elevated branch calls it inside a
/// root process that does nothing else. Neither branch is given a path to
/// write to: `create_backup` derives the backup's name from the target it was
/// handed.
///
/// The resulting path is turned back into a [`BackupInfoDto`] by
/// [`backups::describe_one`], the same helper [`backups::list`] builds its
/// rows with, so a freshly created backup is described exactly the way the
/// next `list_backups` will describe it - including its `is_manual` flag,
/// which is read back from the naming scheme rather than assumed here.
fn create_backup_with(
    target: &ActiveTarget,
    elevate: impl Fn(&Path) -> Result<CreateBackupOutcomeDto, CreateBackupErrorDto>,
) -> Result<BackupInfoDto, CreateBackupErrorDto> {
    let outcome = if target.is_real {
        elevate(&target.path)?
    } else {
        writer::create_backup(&target.path)
            .map(|outcome| CreateBackupOutcomeDto::from_outcome(&outcome))
            .map_err(|err| CreateBackupErrorDto::from(&err))?
    };

    let created = Path::new(&outcome.backup);
    backups::describe_one(&target.path, created)
        .map_err(|err| {
            CreateBackupErrorDto::new(CreateBackupErrorKindDto::Backup, err.to_string())
        })?
        .ok_or_else(|| {
            // Reachable only if the backup disappeared (or was never where it
            // said it was) between being created and being described. Reported
            // rather than papered over with a synthesized row: the frontend
            // would otherwise show a restore point that is not there.
            CreateBackupErrorDto::new(
                CreateBackupErrorKindDto::Backup,
                format!(
                    "the backup {} was created but is no longer among {}'s backups; \
                     refresh the backup list to see what exists",
                    created.display(),
                    target.path.display()
                ),
            )
        })
}

/// Resolves the active target for a read-side command.
fn resolve_target() -> Result<ActiveTarget, CommandErrorDto> {
    target::active_target()
        .map_err(|err| CommandErrorDto::new(CommandErrorKindDto::TargetResolution, err.to_string()))
}

/// Resolves the active target for a write-side command, reporting failure in
/// the write-side error shape.
fn resolve_target_for_write() -> Result<ActiveTarget, WriteErrorDto> {
    target::active_target().map_err(|err| {
        WriteErrorDto::new(
            WriteErrorKindDto::TargetResolution,
            true,
            format!("{err}; nothing was written"),
        )
    })
}

/// Resolves the active target for [`delete_backup`], reporting failure in its
/// own error shape.
fn resolve_target_for_delete() -> Result<ActiveTarget, DeleteBackupErrorDto> {
    target::active_target().map_err(|err| {
        DeleteBackupErrorDto::new(
            DeleteBackupErrorKindDto::TargetResolution,
            format!("{err}; nothing was deleted"),
        )
    })
}

/// Resolves the active target for [`create_backup`], reporting failure in its
/// own error shape.
fn resolve_target_for_create_backup() -> Result<ActiveTarget, CreateBackupErrorDto> {
    target::active_target().map_err(|err| {
        CreateBackupErrorDto::new(
            CreateBackupErrorKindDto::TargetResolution,
            format!("{err}; no backup was created"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backups::BackupError;
    use crate::dto::{LineDto, SeverityDto, SourceKindDto, TargetSourceDto};
    use crate::target::TargetSource;
    use crate::testutil::TempDir;
    use fstab_core::model::{FstabEntry, FstabOption, Line, Options, SourceId};
    use std::path::{Path, PathBuf};

    const ORIGINAL: &str = "# the content a save must back up\n\
                            UUID=11111111-2222-3333-4444-555555555555 /mnt/original ext4 noauto 0 2\n";

    /// A non-real target at `path`, as `active_target()` would have produced
    /// for an explicit `RUSTAB_TARGET_PATH`.
    fn sandbox_target(path: PathBuf) -> ActiveTarget {
        ActiveTarget {
            path,
            is_real: false,
            source: TargetSource::Environment,
        }
    }

    fn valid_candidate(mount_point: &str) -> Document {
        Document::from_lines(vec![
            Line::Comment(" written by a commands test".to_string()),
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

    fn invalid_candidate() -> Document {
        parser::parse(
            "UUID=11111111-2222-3333-4444-555555555555 /mnt/clash ext4 noauto 0 2\n\
             UUID=22222222-3333-4444-5555-666666666666 /mnt/clash ext4 noauto 0 2\n",
        )
    }

    // -- the sandbox save path ----------------------------------------------

    /// End-to-end proof that a save of a non-real target calls through to
    /// `writer::write_document`: the file is replaced with the candidate's
    /// serialization *and* a backup of the previous content appears beside
    /// it, which only `write_document` creates.
    #[test]
    fn saving_a_sandbox_target_writes_through_write_document() {
        let dir = TempDir::new("save-sandbox");
        let path = dir.join("fstab.sandbox");
        fs::write(&path, ORIGINAL).expect("seed the target");
        let target = sandbox_target(path.clone());

        let candidate = valid_candidate("/mnt/saved");
        let outcome = save_document(&target, &candidate).expect("the save should succeed");

        assert_eq!(
            fs::read_to_string(&path).expect("read the target"),
            candidate.to_string(),
            "the target must hold the candidate's serialization"
        );
        assert_eq!(outcome.target, path.to_string_lossy());
        let backup = outcome.backup.expect("write_document must have backed up");
        assert_eq!(
            fs::read_to_string(&backup).expect("read the backup"),
            ORIGINAL,
            "the backup must hold the previous content"
        );
        assert!(
            !outcome.elevated,
            "a sandbox save must never report itself as elevated"
        );
        // The real backup list agrees with what the outcome claims.
        let listed = backups::list(&path).expect("listing must succeed");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].path, backup);
    }

    /// The same path, from the command's own entry point (DTO in, DTO out),
    /// to prove the conversions in between are lossless.
    #[test]
    fn saving_a_sandbox_target_from_the_dto_shape_is_lossless() {
        let dir = TempDir::new("save-dto");
        let path = dir.join("fstab.sandbox");
        let target = sandbox_target(path.clone());

        let candidate = parser::parse(
            "# header\n\
             \n\
             UUID=11111111-2222-3333-4444-555555555555 /mnt/data ext4 noauto,fmask=0022 0 2 # note\n\
             LABEL=spare none swap sw,noauto 0 0\n",
        );
        let dto = DocumentDto::from(&candidate);
        let round_tripped = Document::from(
            serde_json::from_str::<DocumentDto>(&serde_json::to_string(&dto).expect("serialize"))
                .expect("deserialize"),
        );
        assert_eq!(round_tripped, candidate, "the DTO must round-trip exactly");

        save_document(&target, &round_tripped).expect("the save should succeed");
        assert_eq!(
            fs::read_to_string(&path).expect("read the target"),
            candidate.to_string()
        );
    }

    /// A first save creates the file and reports no backup.
    #[test]
    fn saving_a_missing_sandbox_target_creates_it_without_a_backup() {
        let dir = TempDir::new("save-create");
        let path = dir.join("fstab.sandbox");
        let target = sandbox_target(path.clone());

        let outcome = save_document(&target, &valid_candidate("/mnt/first"))
            .expect("the save should succeed");

        assert!(outcome.backup.is_none());
        assert!(path.exists());
    }

    /// Validation is enforced by `write_document`, and the error DTO carries
    /// the report plus the "nothing changed" guarantee.
    #[test]
    fn saving_an_invalid_candidate_fails_and_leaves_the_target_untouched() {
        let dir = TempDir::new("save-invalid");
        let path = dir.join("fstab.sandbox");
        fs::write(&path, ORIGINAL).expect("seed the target");
        let target = sandbox_target(path.clone());

        let error = save_document(&target, &invalid_candidate())
            .expect_err("an invalid candidate must be refused");

        assert_eq!(error.kind, WriteErrorKindDto::Validation);
        assert!(error.target_unchanged);
        let report = error.report.expect("a report must be attached");
        assert!(report.has_errors);
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.severity == SeverityDto::Error));
        assert_eq!(
            fs::read_to_string(&path).expect("read the target"),
            ORIGINAL
        );
        assert!(
            backups::list(&path).expect("listing").is_empty(),
            "a refused save must not even create a backup"
        );
    }

    /// Writing into a directory that does not exist fails cleanly, with the
    /// target-untouched guarantee intact (there was nothing to touch).
    #[test]
    fn saving_into_an_unwritable_location_reports_a_typed_error() {
        let dir = TempDir::new("save-nodir");
        let target = sandbox_target(dir.join("missing-dir/fstab"));

        let error = save_document(&target, &valid_candidate("/mnt/nope"))
            .expect_err("a missing directory must fail");

        assert!(error.target_unchanged);
        assert!(
            matches!(
                error.kind,
                WriteErrorKindDto::Write | WriteErrorKindDto::Backup
            ),
            "unexpected kind: {error:?}"
        );
    }

    // -- restore ------------------------------------------------------------

    /// A restore must go through the full save path, not a copy: restoring
    /// backup *A* must itself create a backup of the content it replaced.
    #[test]
    fn restoring_a_backup_goes_through_the_same_save_path() {
        let dir = TempDir::new("restore");
        let path = dir.join("fstab.sandbox");
        fs::write(&path, ORIGINAL).expect("seed the target");
        let target = sandbox_target(path.clone());

        // First save: creates a backup holding ORIGINAL.
        save_document(&target, &valid_candidate("/mnt/second")).expect("first save");
        let first_backup = backups::list(&path).expect("listing")[0].path.clone();

        // Restore it the way the command does.
        let resolved =
            backups::resolve_requested(&path, &first_backup).expect("it is a real backup");
        let text = backups::read_text(&resolved).expect("readable");
        let outcome = save_document(&target, &parser::parse(&text)).expect("the restore succeeds");

        assert_eq!(
            fs::read_to_string(&path).expect("read the target"),
            ORIGINAL,
            "the target must hold the restored content"
        );
        let new_backup = outcome
            .backup
            .expect("a restore must back up what it replaced");
        assert_ne!(new_backup, first_backup);
        assert_eq!(
            fs::read_to_string(&new_backup).expect("read"),
            valid_candidate("/mnt/second").to_string(),
            "the replaced content must be recoverable too"
        );
    }

    #[test]
    fn restoring_a_path_that_is_not_a_backup_is_refused() {
        let dir = TempDir::new("restore-refuse");
        let path = dir.join("fstab.sandbox");
        fs::write(&path, ORIGINAL).expect("seed the target");

        let error = backups::resolve_requested(&path, "/etc/shadow")
            .expect_err("an arbitrary path must be refused");
        assert!(matches!(error, BackupError::NotABackupOfTarget { .. }));
        assert_eq!(
            fs::read_to_string(&path).expect("read the target"),
            ORIGINAL
        );
    }

    // -- read_backup --------------------------------------------------------

    /// Seeds `count` real backups by performing `count` real saves, so the
    /// names under test are the ones `fstab-core` actually produces.
    fn seed_backups(target: &Path, count: usize) {
        fs::write(target, ORIGINAL).expect("seed the target");
        for i in 0..count {
            writer::write_document(target, &valid_candidate(&format!("/mnt/save-{i}")))
                .expect("the save should succeed");
        }
    }

    #[test]
    fn read_backup_returns_the_backup_text_verbatim() {
        let dir = TempDir::new("read-backup");
        let path = dir.join("fstab.sandbox");
        seed_backups(&path, 2);

        let listed = backups::list(&path).expect("listing must succeed");
        // Newest first: the newest backup holds what the *second* save
        // replaced, and the oldest holds the content the target started with.
        let oldest = &listed[listed.len() - 1];

        assert_eq!(
            read_backup_of(&path, &oldest.path).expect("a real backup must be readable"),
            ORIGINAL
        );
        assert_eq!(
            read_backup_of(&path, &listed[0].path).expect("a real backup must be readable"),
            valid_candidate("/mnt/save-0").to_string()
        );
        // Reading does not change anything: the target and the backup list are
        // exactly as they were.
        assert_eq!(
            fs::read_to_string(&path).expect("read the target"),
            valid_candidate("/mnt/save-1").to_string()
        );
        assert_eq!(backups::list(&path).expect("listing").len(), 2);
    }

    /// The same structural guarantee `restore_backup` has, asserted for the
    /// read path: a path that is not in this target's own backup list is never
    /// used as a path at all.
    #[test]
    fn read_backup_refuses_anything_that_is_not_a_backup_of_this_target() {
        let dir = TempDir::new("read-backup-refuse");
        let path = dir.join("fstab.sandbox");
        seed_backups(&path, 1);

        let other = dir.join("fstab.other");
        seed_backups(&other, 1);
        let other_backup = backups::list(&other).expect("listing")[0].path.clone();

        let near_misses = [
            "fstab.sandbox.rustab-backup.1.2",
            "fstab.sandbox.rustab-backup.0000000001.000000002.old",
            "fstab.sandbox.rustab-backup.aaaaaaaaaa.bbbbbbbbb",
            "fstab.sandbox.bak",
        ];
        for name in near_misses {
            fs::write(dir.join(name), "# not a Rustab backup\n").expect("write the near miss");
        }

        let mut refused: Vec<String> = vec![
            other_backup,
            "/etc/shadow".to_string(),
            "/etc/fstab".to_string(),
            "../../etc/fstab".to_string(),
            String::new(),
            path.to_string_lossy().into_owned(),
        ];
        refused.extend(
            near_misses
                .iter()
                .map(|name| dir.join(name).to_string_lossy().into_owned()),
        );

        for requested in &refused {
            let error = read_backup_of(&path, requested)
                .expect_err("only this target's own backups may be read");
            assert_eq!(
                error.kind,
                CommandErrorKindDto::InvalidBackup,
                "unexpected kind for {requested}: {error:?}"
            );
        }
    }

    /// A backup that is not valid UTF-8 is refused with the same kind the
    /// target's own non-UTF-8 case uses - there is nothing to send across the
    /// boundary as a string, and there is no second meaning to invent.
    #[test]
    fn read_backup_refuses_non_utf8_content() {
        let dir = TempDir::new("read-backup-utf8");
        let path = dir.join("fstab.sandbox");
        fs::write(&path, ORIGINAL).expect("seed the target");
        // A hand-made but exactly-named backup, which `backups` deliberately
        // accepts as a backup of this target (see its module docs).
        let backup = dir.join("fstab.sandbox.rustab-backup.0000000001.000000002");
        fs::write(&backup, [0xff, 0xfe, 0x00]).expect("write invalid UTF-8");

        let error = read_backup_of(&path, &backup.to_string_lossy())
            .expect_err("invalid UTF-8 must be refused");
        assert_eq!(error.kind, CommandErrorKindDto::NotUtf8);
        assert!(backup.exists(), "a refused read must not remove anything");
    }

    // -- open_backup_location -------------------------------------------------

    /// The happy path: a fake opener captures the directory it was given, and
    /// the command reports success. The directory handed to the opener must
    /// be exactly what `backups::containing_dir` computes for the target -
    /// not re-derived or approximated here.
    #[test]
    fn open_backup_location_opens_the_target_s_containing_directory() {
        let dir = TempDir::new("open-backup-location");
        let target = dir.join("fstab.sandbox");
        fs::write(&target, ORIGINAL).expect("seed the target");

        let calls = std::cell::RefCell::new(Vec::new());
        open_backup_location_with(&target, |opened| {
            calls.borrow_mut().push(opened.to_path_buf());
            Ok(())
        })
        .expect("the opener stand-in succeeds");

        let calls = calls.into_inner();
        assert_eq!(calls, vec![backups::containing_dir(&target)]);
    }

    /// A failure to launch the file manager (binary missing, permission
    /// denied, ...) is reported as `OpenLocation`, with the directory's own
    /// error folded into the message.
    #[test]
    fn open_backup_location_reports_a_launch_failure() {
        let dir = TempDir::new("open-backup-location-fail");
        let target = dir.join("fstab.sandbox");

        let error =
            open_backup_location_with(&target, |_| Err(io::Error::from(io::ErrorKind::NotFound)))
                .expect_err("a launch failure must be reported");

        assert_eq!(error.kind, CommandErrorKindDto::OpenLocation);
        let expected_dir = backups::containing_dir(&target);
        assert!(
            error.message.contains(&expected_dir.display().to_string()),
            "the message must name the directory that could not be opened: {error:?}"
        );
    }

    // -- open_external_link --------------------------------------------------

    /// The happy path for both known links: a fake opener captures the URL
    /// it was given, and the command reports success. The URL handed to the
    /// opener must be exactly what [`ExternalLinkDto::url`] returns for that
    /// variant - not re-derived or approximated here.
    #[test]
    fn open_external_link_opens_the_link_s_own_url() {
        for link in [
            ExternalLinkDto::GithubRepo,
            ExternalLinkDto::GithubSponsors,
            ExternalLinkDto::Email,
            ExternalLinkDto::Issues,
            ExternalLinkDto::License,
        ] {
            let calls = std::cell::RefCell::new(Vec::new());
            open_external_link_with(link, |opened| {
                calls.borrow_mut().push(opened.to_string());
                Ok(())
            })
            .expect("the opener stand-in succeeds");

            let calls = calls.into_inner();
            assert_eq!(calls, vec![link.url().to_string()]);
        }
    }

    /// A failure to launch the browser (binary missing, permission denied,
    /// ...) is reported as `OpenExternalLink`, with the URL's own error
    /// folded into the message.
    #[test]
    fn open_external_link_reports_a_launch_failure() {
        let link = ExternalLinkDto::GithubSponsors;

        let error =
            open_external_link_with(link, |_| Err(io::Error::from(io::ErrorKind::NotFound)))
                .expect_err("a launch failure must be reported");

        assert_eq!(error.kind, CommandErrorKindDto::OpenExternalLink);
        assert!(
            error.message.contains(link.url()),
            "the message must name the URL that could not be opened: {error:?}"
        );
    }

    // -- delete_backup ------------------------------------------------------

    #[test]
    fn deleting_a_sandbox_backup_removes_exactly_that_file() {
        let dir = TempDir::new("delete-backup");
        let path = dir.join("fstab.sandbox");
        seed_backups(&path, 3);
        let target = sandbox_target(path.clone());
        let before = backups::list(&path).expect("listing");
        assert_eq!(before.len(), 3);
        let target_content = fs::read_to_string(&path).expect("read the target");

        let doomed = before[1].path.clone();
        delete_backup_from(&target, &doomed).expect("deleting a real backup must succeed");

        assert!(
            !Path::new(&doomed).exists(),
            "the named backup must be gone"
        );
        let after = backups::list(&path).expect("listing");
        assert_eq!(after.len(), 2);
        assert!(!after.iter().any(|info| info.path == doomed));
        assert_eq!(
            fs::read_to_string(&path).expect("read the target"),
            target_content,
            "deleting a backup must not touch the target file"
        );
    }

    #[test]
    fn deleting_a_path_that_is_not_a_backup_is_refused() {
        let dir = TempDir::new("delete-backup-refuse");
        let path = dir.join("fstab.sandbox");
        seed_backups(&path, 1);
        let target = sandbox_target(path.clone());
        let real_backup = backups::list(&path).expect("listing")[0].path.clone();

        for requested in [
            "/etc/shadow",
            "../../etc/fstab",
            "",
            &path.to_string_lossy(),
            &dir.join("fstab.sandbox.rustab-backup.1.2")
                .to_string_lossy(),
        ] {
            let error = delete_backup_from(&target, requested)
                .expect_err("only this target's own backups may be deleted");
            assert_eq!(
                error.kind,
                DeleteBackupErrorKindDto::InvalidBackup,
                "unexpected kind for {requested}: {error:?}"
            );
            assert!(error.message.contains("nothing was deleted"));
        }

        assert!(path.exists(), "the target must still exist");
        assert!(
            Path::new(&real_backup).exists(),
            "the one real backup must still exist"
        );
    }

    /// Deleting the same backup twice: the second attempt is `invalid_backup`,
    /// because the path is no longer in the target's backup list. The frontend
    /// can treat that as "already gone" and refresh.
    #[test]
    fn deleting_an_already_deleted_backup_is_reported_as_invalid() {
        let dir = TempDir::new("delete-backup-twice");
        let path = dir.join("fstab.sandbox");
        seed_backups(&path, 1);
        let target = sandbox_target(path.clone());
        let backup = backups::list(&path).expect("listing")[0].path.clone();

        delete_backup_from(&target, &backup).expect("the first delete must succeed");
        let error = delete_backup_from(&target, &backup)
            .expect_err("the second delete must not claim success");
        assert_eq!(error.kind, DeleteBackupErrorKindDto::InvalidBackup);
        assert!(backups::list(&path).expect("listing").is_empty());
    }

    /// The dispatch guarantee for the delete path, mirroring
    /// `a_real_target_is_dispatched_to_the_elevated_writer_and_never_written_in_process`:
    /// a target flagged real goes to the elevated deleter and **nothing** is
    /// unlinked by this process. Asserted with a recording stand-in, so the
    /// test neither launches `pkexec` nor waits on a prompt.
    #[test]
    fn a_real_target_delete_is_dispatched_to_the_elevated_deleter_and_never_deleted_in_process() {
        let dir = TempDir::new("real-delete-dispatch");
        let path = dir.join("fstab");
        seed_backups(&path, 2);
        let target = ActiveTarget {
            path: path.clone(),
            is_real: true,
            source: TargetSource::Environment,
        };
        let before = backups::list(&path).expect("listing");
        let wanted = before[0].path.clone();

        let calls = std::cell::RefCell::new(Vec::new());
        delete_backup_with(&target, &wanted, |elevated_target, elevated_backup| {
            calls
                .borrow_mut()
                .push((elevated_target.to_path_buf(), elevated_backup.to_string()));
            Ok(())
        })
        .expect("the elevated stand-in succeeds");

        let calls = calls.into_inner();
        assert_eq!(calls.len(), 1, "exactly one elevated call must be made");
        assert_eq!(calls[0].0, path, "the elevated call must get the target");
        assert_eq!(
            calls[0].1, wanted,
            "the elevated call must get the resolved backup path"
        );
        // Nothing was unlinked here: both backups, and the target, survive.
        assert_eq!(
            backups::list(&path).expect("listing").len(),
            2,
            "the in-process deleter must not have touched a real target's backups"
        );
        assert!(path.exists());
    }

    /// The other half of the dispatch guarantee: a non-real target's backup is
    /// removed in-process and must never escalate (no password prompt for a
    /// sandbox file).
    #[test]
    fn a_sandbox_delete_never_reaches_the_elevated_deleter() {
        let dir = TempDir::new("sandbox-delete-dispatch");
        let path = dir.join("fstab.sandbox");
        seed_backups(&path, 1);
        let target = sandbox_target(path.clone());
        let backup = backups::list(&path).expect("listing")[0].path.clone();

        delete_backup_with(&target, &backup, |_, _| {
            panic!("a sandbox backup deletion must never escalate");
        })
        .expect("the delete should succeed");

        assert!(!Path::new(&backup).exists());
        assert!(path.exists());
    }

    /// A refused `backup_path` must be refused *before* the dispatch, so a
    /// bogus path never costs the user a password prompt - the same courtesy
    /// `save_elevated`'s pre-validation provides.
    #[test]
    fn a_refused_backup_path_never_reaches_the_elevated_deleter() {
        let dir = TempDir::new("real-delete-refuse");
        let path = dir.join("fstab");
        seed_backups(&path, 1);
        let target = ActiveTarget {
            path: path.clone(),
            is_real: true,
            source: TargetSource::Environment,
        };

        let error = delete_backup_with(&target, "/etc/shadow", |_, _| {
            panic!("a path that is not a backup must not reach the elevated deleter");
        })
        .expect_err("an arbitrary path must be refused");
        assert_eq!(error.kind, DeleteBackupErrorKindDto::InvalidBackup);
    }

    // -- create_backup ------------------------------------------------------

    /// A manual backup of a sandbox target: the backup holds the target's
    /// *current* content, the target is untouched, and the row that comes back
    /// is the same row `list_backups` now reports, flagged manual.
    #[test]
    fn creating_a_sandbox_backup_copies_the_target_without_changing_it() {
        let dir = TempDir::new("create-backup");
        let path = dir.join("fstab.sandbox");
        fs::write(&path, ORIGINAL).expect("seed the target");
        let target = sandbox_target(path.clone());

        let info = create_backup_of(&target).expect("creating a backup must succeed");

        assert!(
            info.is_manual,
            "an explicit backup is a manual one: {info:?}"
        );
        assert_eq!(info.target_path, path.to_string_lossy());
        assert_eq!(
            fs::read_to_string(&info.path).expect("read the backup"),
            ORIGINAL,
            "the backup must hold the target's current content"
        );
        assert!(info.size_bytes > 0);
        assert_eq!(
            fs::read_to_string(&path).expect("read the target"),
            ORIGINAL,
            "creating a backup must not touch the target file"
        );

        // The listing agrees, field for field.
        let listed = backups::list(&path).expect("listing must succeed");
        assert_eq!(listed, vec![info]);
    }

    /// Two manual backups in a row are two distinct restore points, and each
    /// captures the content as it was at the time.
    #[test]
    fn creating_two_backups_yields_two_distinct_restore_points() {
        let dir = TempDir::new("create-backup-twice");
        let path = dir.join("fstab.sandbox");
        fs::write(&path, ORIGINAL).expect("seed the target");
        let target = sandbox_target(path.clone());

        let first = create_backup_of(&target).expect("first backup");
        fs::write(&path, "# changed by hand\n").expect("change the target");
        let second = create_backup_of(&target).expect("second backup");

        assert_ne!(first.path, second.path);
        assert_eq!(fs::read_to_string(&first.path).unwrap(), ORIGINAL);
        assert_eq!(
            fs::read_to_string(&second.path).unwrap(),
            "# changed by hand\n"
        );
        let listed = backups::list(&path).expect("listing");
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().all(|info| info.is_manual));
        // Newest first.
        assert_eq!(listed[0].path, second.path);
    }

    /// A target that does not exist yet is reported as its own kind, so the UI
    /// can say "save the file once first" instead of showing an I/O error for
    /// an ordinary first-run state.
    #[test]
    fn creating_a_backup_of_a_missing_target_is_reported_as_target_missing() {
        let dir = TempDir::new("create-backup-missing");
        let path = dir.join("fstab.sandbox");
        let target = sandbox_target(path.clone());

        let error = create_backup_of(&target).expect_err("there is nothing to back up");

        assert_eq!(error.kind, CreateBackupErrorKindDto::TargetMissing);
        assert!(!path.exists(), "nothing may have been created");
        assert!(backups::list(&path).expect("listing").is_empty());
    }

    /// Manual backups are exempt from the automatic retention bound, end to
    /// end through the command layer: a manual backup taken before a long run
    /// of saves is still there afterwards.
    #[test]
    fn a_manual_backup_survives_saves_that_prune_automatic_ones() {
        let dir = TempDir::new("create-backup-retention");
        let path = dir.join("fstab.sandbox");
        fs::write(&path, ORIGINAL).expect("seed the target");
        let target = sandbox_target(path.clone());

        let manual = create_backup_of(&target).expect("a manual backup");
        for i in 0..(writer::MAX_BACKUPS + 3) {
            save_document(&target, &valid_candidate(&format!("/mnt/n{i}")))
                .unwrap_or_else(|err| panic!("save {i} failed: {err:?}"));
        }

        let listed = backups::list(&path).expect("listing");
        assert!(
            listed.iter().any(|info| info.path == manual.path),
            "the manual backup must still be listed after {} saves",
            writer::MAX_BACKUPS + 3
        );
        assert_eq!(
            fs::read_to_string(&manual.path).expect("read the manual backup"),
            ORIGINAL,
            "and must still hold the content it captured"
        );
        assert_eq!(
            listed.iter().filter(|info| !info.is_manual).count(),
            writer::MAX_BACKUPS,
            "while the automatic backups are still bounded"
        );
    }

    /// The dispatch guarantee for the create path, mirroring the save and
    /// delete ones: a target flagged real goes to the elevated helper and
    /// **nothing** is written by this process. Asserted with a recording
    /// stand-in, so the test neither launches `pkexec` nor waits on a prompt.
    #[test]
    fn a_real_target_backup_is_dispatched_to_the_elevated_helper_and_never_copied_in_process() {
        let dir = TempDir::new("real-create-dispatch");
        let path = dir.join("fstab");
        fs::write(&path, ORIGINAL).expect("seed the target");
        let target = ActiveTarget {
            path: path.clone(),
            is_real: true,
            source: TargetSource::Environment,
        };

        // The stand-in reports a backup it did not actually create, so this
        // also pins what the command does with an outcome it cannot verify:
        // it refuses rather than inventing a row.
        let calls = std::cell::RefCell::new(Vec::new());
        let error = create_backup_with(&target, |elevated_target| {
            calls.borrow_mut().push(elevated_target.to_path_buf());
            Ok(CreateBackupOutcomeDto {
                target: elevated_target.to_string_lossy().into_owned(),
                backup: format!(
                    "{}.rustab-backup.0000000001.000000002.manual",
                    path.display()
                ),
                pruned_backups: Vec::new(),
                notes: Vec::new(),
            })
        })
        .expect_err("a backup that is not on disk must not be reported as a row");
        assert_eq!(error.kind, CreateBackupErrorKindDto::Backup);

        let calls = calls.into_inner();
        assert_eq!(calls, vec![path.clone()], "exactly one elevated call");
        assert!(
            backups::list(&path).expect("listing").is_empty(),
            "the in-process copier must not have touched a real target"
        );
        assert_eq!(
            fs::read_to_string(&path).expect("read the target"),
            ORIGINAL
        );
    }

    /// The other half of the dispatch guarantee: a non-real target is copied
    /// in-process and must never escalate (no password prompt for a sandbox
    /// file).
    #[test]
    fn a_sandbox_backup_never_reaches_the_elevated_helper() {
        let dir = TempDir::new("sandbox-create-dispatch");
        let path = dir.join("fstab.sandbox");
        fs::write(&path, ORIGINAL).expect("seed the target");
        let target = sandbox_target(path.clone());

        let info = create_backup_with(&target, |_| {
            panic!("a sandbox backup must never escalate");
        })
        .expect("the backup should succeed");

        assert!(info.is_manual);
        assert!(Path::new(&info.path).exists());
    }

    /// A manual backup is deletable like any other: the kinds differ in how
    /// they are *pruned*, not in whether the user may remove them.
    #[test]
    fn a_manual_backup_can_be_deleted_like_any_other() {
        let dir = TempDir::new("create-then-delete");
        let path = dir.join("fstab.sandbox");
        fs::write(&path, ORIGINAL).expect("seed the target");
        let target = sandbox_target(path.clone());

        let manual = create_backup_of(&target).expect("a manual backup");
        delete_backup_from(&target, &manual.path).expect("deleting a manual backup must succeed");

        assert!(!Path::new(&manual.path).exists());
        assert!(backups::list(&path).expect("listing").is_empty());
        assert!(path.exists(), "the target itself is untouched");
    }

    // -- read-side commands -------------------------------------------------

    /// `list_entries`'s parsing and DTO shape, exercised through the same
    /// code the command runs (the command itself only adds target resolution
    /// and the file read).
    #[test]
    fn parsed_lines_keep_every_line_in_order_including_malformed_ones() {
        let text = "# header\n\
                    \n\
                    UUID=11111111-2222-3333-4444-555555555555 /mnt/data ext4 noauto 0 2\n\
                    /dev/sdb1\n";
        let document = parser::parse(text);
        let dto = DocumentDto::from(&document);

        assert_eq!(dto.lines.len(), 4);
        assert!(matches!(dto.lines[0], LineDto::Comment { .. }));
        assert!(matches!(dto.lines[1], LineDto::Blank));
        let LineDto::Entry { entry } = &dto.lines[2] else {
            panic!("the third line must be an entry");
        };
        assert_eq!(entry.source.kind, SourceKindDto::Uuid);
        assert_eq!(
            entry.source.value, "11111111-2222-3333-4444-555555555555",
            "the UUID= prefix must not leak into the value"
        );
        assert_eq!(entry.mount_point, "/mnt/data");
        assert_eq!(entry.options.len(), 1);
        assert_eq!(entry.options[0].key, "noauto");
        assert_eq!(entry.options[0].value, None);
        assert!(matches!(dto.lines[3], LineDto::Malformed { .. }));
        assert!(document.has_malformed_lines());

        // And the whole thing renders back to the original text.
        assert_eq!(render_candidate(dto), text);
    }

    #[test]
    fn validate_candidate_reports_blocking_findings_without_writing() {
        let dir = TempDir::new("validate");
        let path = dir.join("fstab.sandbox");
        fs::write(&path, ORIGINAL).expect("seed the target");

        let report = validate_candidate(DocumentDto::from(&invalid_candidate()));

        assert!(report.has_errors);
        assert_eq!(
            fs::read_to_string(&path).expect("read the target"),
            ORIGINAL,
            "validation must not touch the target"
        );
        assert!(backups::list(&path).expect("listing").is_empty());
    }

    #[test]
    fn target_info_dto_mirrors_the_resolved_target() {
        let info = TargetInfoDto::from(&sandbox_target(PathBuf::from("/tmp/rustab/fstab.sandbox")));
        assert_eq!(info.path, "/tmp/rustab/fstab.sandbox");
        assert!(!info.is_real);
        assert_eq!(info.source, TargetSourceDto::Environment);

        let real = TargetInfoDto::from(&ActiveTarget {
            path: PathBuf::from(target::REAL_FSTAB_PATH),
            is_real: true,
            source: TargetSource::Environment,
        });
        assert!(real.is_real);
    }

    /// The dispatch guarantee: a target flagged real goes to the elevated
    /// writer and is **never** written by this process. Asserted with a
    /// recording stand-in for the elevated half, so the test neither launches
    /// `pkexec` nor waits on an authentication prompt.
    #[test]
    fn a_real_target_is_dispatched_to_the_elevated_writer_and_never_written_in_process() {
        let dir = TempDir::new("real-dispatch");
        let path = dir.join("fstab");
        let target = ActiveTarget {
            path: path.clone(),
            is_real: true,
            source: TargetSource::Environment,
        };
        let candidate = valid_candidate("/mnt/elevated");

        let calls = std::cell::RefCell::new(Vec::new());
        let outcome = save_document_with(&target, &candidate, |elevated_path, elevated_doc| {
            calls
                .borrow_mut()
                .push((elevated_path.to_path_buf(), elevated_doc.clone()));
            Ok(WriteOutcomeDto {
                target: elevated_path.to_string_lossy().into_owned(),
                backup: None,
                pruned_backups: Vec::new(),
                validation_warnings: Vec::new(),
                notes: Vec::new(),
                elevated: true,
            })
        })
        .expect("the elevated stand-in succeeds");

        assert!(outcome.elevated);
        let calls = calls.into_inner();
        assert_eq!(calls.len(), 1, "exactly one elevated call must be made");
        assert_eq!(calls[0].0, path, "the elevated call must get the target");
        assert_eq!(
            calls[0].1, candidate,
            "the elevated call must get the candidate unchanged"
        );
        assert!(
            !path.exists(),
            "the in-process writer must not have touched a real target"
        );
    }

    /// The other half of the dispatch guarantee: a non-real target is written
    /// in-process and must never escalate (no password prompt for a sandbox
    /// file).
    #[test]
    fn a_sandbox_target_never_reaches_the_elevated_writer() {
        let dir = TempDir::new("sandbox-dispatch");
        let path = dir.join("fstab.sandbox");
        let target = sandbox_target(path.clone());

        let outcome = save_document_with(&target, &valid_candidate("/mnt/sandbox"), |_, _| {
            panic!("a sandbox save must never escalate");
        })
        .expect("the save should succeed");

        assert!(!outcome.elevated);
        assert!(path.exists());
    }

    #[test]
    fn discovered_devices_are_reported_without_failing_on_a_missing_lsblk() {
        let discovered = discover_devices();
        // Nothing can be asserted about *which* devices exist; what matters
        // is that the command is infallible and self-describing.
        if !discovered.lsblk_available {
            assert!(discovered.devices.is_empty());
        }
        for device in &discovered.devices {
            assert!(!device.path.is_empty());
        }
    }

    /// Every committed fixture must survive the full
    /// text -> Document -> DTO -> JSON -> DTO -> Document -> text trip
    /// byte-for-byte. This is the contract the save path depends on: the
    /// frontend sends back what it was given, and it must mean the same file.
    #[test]
    fn fixtures_round_trip_through_the_dto_layer_byte_for_byte() {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the crate has a parent directory")
            .join("tests/fixtures");

        let mut checked = 0;
        for entry in fs::read_dir(&fixtures).expect("the fixtures directory must exist") {
            let path = entry.expect("a fixture entry").path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("fstab") {
                continue;
            }
            let text = fs::read_to_string(&path).expect("a fixture must be readable UTF-8");
            let document = parser::parse(&text);
            let json =
                serde_json::to_string(&DocumentDto::from(&document)).expect("DTO must serialize");
            let back: DocumentDto = serde_json::from_str(&json).expect("DTO must deserialize");
            let restored = Document::from(back);

            assert_eq!(restored, document, "{} lost information", path.display());
            assert_eq!(
                restored.to_string(),
                text,
                "{} did not round-trip byte-for-byte",
                path.display()
            );
            checked += 1;
        }
        assert!(checked >= 3, "expected the committed fixtures to be found");
    }
}
