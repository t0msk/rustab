//! The `#[tauri::command]` layer: the complete, frozen set of calls the
//! frontend may make - see `docs/API_CONTRACT.md`.
//!
//! ## Shape of this module
//!
//! Each command is a thin wrapper: resolve the active target via
//! [`crate::target`], convert DTOs via [`crate::dto`], delegate the real
//! work to `fstab-core`. No command re-implements validation,
//! serialization, backup naming, or writing, and no command takes a
//! *target* path from the frontend. The bounded exceptions
//! ([`restore_backup`], [`read_backup`], [`delete_backup`], which take a
//! *backup* path) never use it until it's matched against the active
//! target's real backup list (see [`crate::backups`]).
//!
//! The genuinely interesting logic is [`save_document_with`] ("which save
//! path do I take"): three cases, all ending in the same
//! [`writer::write_document`] call, some via a `pkexec` re-exec (see
//! [`crate::privileged`]) - the default target's save writes the real
//! `/etc/fstab` and mirrors into the working draft, while an explicit
//! `RUSTAB_TARGET_PATH` target is written outright and alone. Read that
//! function's docs before changing anything here; it's where
//! `docs/SPEC.md` §7's "every save writes `/etc/fstab`" is implemented,
//! and where a save first checks whether the real file changed outside
//! Rustab since the session last synced with it
//! ([`target::ExternalChangeGuard`]).
//!
//! The second decision, "whose backups am I looking at", is
//! [`backup_target_path`]. Every backup command routes through it rather
//! than `target.path`, so for a default launch the Backups panel
//! describes the backups of the file a save actually writes - the real
//! `/etc/fstab` - not the draft's own directory.
//! [`delete_backup_with`]/[`create_backup_with`] dispatch on
//! [`backups_are_elevated`] accordingly (the same `is_real || source ==
//! SandboxDefault` shape [`save_document_with`] uses): creating or
//! unlinking a root-owned file in `/etc/` needs the same `pkexec` re-exec
//! a save needs. Reading a backup needs no escalation either way, since
//! `/etc/fstab` and its backups are world-readable.
//!
//! ## Where the real file's manual backups live
//!
//! A third, layered decision: a process that can't enumerate `/etc/` (a
//! Flatpak sandbox, which sees only the runtime's `/etc`) keeps the real
//! `/etc/fstab`'s manually managed backups in a per-user fallback
//! directory instead (see [`crate::backup_location`]).
//! [`backup_target_path`] points every backup command at a mirror file in
//! that directory, redirecting all of them with no change to the
//! functions doing the work, and [`backups_are_elevated`] then answers
//! `false`, since that directory needs no `pkexec`.
//!
//! Nothing about *writing* changes: a save still writes the real
//! `/etc/fstab` through the elevated helper and leaves its automatic
//! backup beside it in `/etc/`. Those automatic backups just aren't
//! visible in a fallback-mode [`list_backups`] - spelled out in
//! [`crate::backup_location`]'s docs and surfaced via
//! [`TargetInfoDto::real_target_backup_fallback_dir`].
//!
//! ## How the real file is read, and how the elevated helper starts
//!
//! Orthogonal to the above, and the reason the Flatpak build works at
//! all: inside a sandbox, neither the real `/etc/fstab` nor a `pkexec`'d
//! child can be reached by the obvious means. Every read of the real
//! file here goes through an injected [`crate::host::RealFileReader`] -
//! production uses [`crate::host::real_file_reader`] (crossing the
//! sandbox boundary with `flatpak-spawn --host` when needed), tests pass
//! [`crate::host::DirectFs`] or a double. The functions that read it
//! ([`resync_external_change_with`], [`save_document_with`]'s courtesy
//! check, [`refresh_backup_mirror`]) take the reader as a parameter so
//! `cargo test` never reads the machine's own `/etc/fstab` or spawns
//! `flatpak-spawn`. The elevated write path's own change lives in
//! [`crate::privileged`] and [`crate::host`]; this module's dispatch is unaffected.
//!
//! The same applies to the two system tools `fstab-core` shells out to:
//! `findmnt --verify` and `lsblk` only describe the machine their own
//! process can see, so every command here that triggers one
//! ([`validate_candidate`], [`discover_devices`], and
//! [`save_document_with`]'s non-elevated branch) passes
//! [`crate::host::command_runner`] to `fstab-core`'s runner-taking entry
//! point. Inside a sandbox they run on the host; everywhere else, in this
//! process, exactly as before.
//!
//! ## Why every command is `#[tauri::command(async)]`
//!
//! Several do blocking work: `lsblk`, `findmnt`, file I/O, and a `pkexec`
//! call that blocks on a password. `(async)` makes Tauri run these
//! ordinary blocking functions on its thread pool instead of the main
//! thread, which also keeps them directly callable from unit tests with
//! no runtime or `AppHandle` to fake.
//!
//! ## Why argument-taking commands add `rename_all = "snake_case"`
//!
//! Tauri renames command *arguments* to camelCase by default, while
//! serde leaves DTO *fields* inside them snake_case. That mix would mean
//! one spelling rule per nesting level; pinning the argument case to
//! snake_case keeps the whole payload consistent.

use crate::backup_location::{self, BackupLocation};
use crate::backups;
use crate::dto::{
    BackupInfoDto, CommandErrorDto, CommandErrorKindDto, CreateBackupErrorDto,
    CreateBackupErrorKindDto, CreateBackupOutcomeDto, DeleteBackupErrorDto,
    DeleteBackupErrorKindDto, DeviceDiscoveryDto, DocumentDto, DocumentViewDto, ExternalLinkDto,
    TargetInfoDto, ValidationReportDto, WriteErrorDto, WriteErrorKindDto, WriteOutcomeDto,
};
use crate::host::{self, RealFileReader};
use crate::privileged;
use crate::target::{self, ActiveTarget, ExternalChangeGuardExt, TargetSource};
use fstab_core::model::Document;
use fstab_core::{discovery, parser, validate, writer};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Returns the active target: which file this session reads and writes,
/// whether it is the real `/etc/fstab`, and - since T39 - whether that file's
/// backups are being kept in a fallback directory instead of beside it.
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
    let target = resolve_target()?;
    Ok(describe_target(
        &target,
        backup_location::real_target_location(),
    ))
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
    let info = describe_target(&target, backup_location::real_target_location());

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

/// Re-syncs the working draft - and its last-known-real marker - from the
/// real `/etc/fstab`'s *current* content, without writing to `/etc/fstab`
/// itself.
///
/// The "Reload and re-sync" action inside `ExternalChangeDialog`
/// (`docs/UI_DESIGN.md` §12, `docs/API_CONTRACT.md` §3.15): when a `save`
/// or `restore_backup` has just been refused with `kind:
/// "external_change"`, the user can ask Rustab to adopt the real file's
/// current content as its new baseline, without discarding pending edits
/// (the frontend's `candidate`, never touched here) or writing to
/// `/etc/fstab`. Reuses [`target::sync_sandbox_after_save`] - the same
/// primitive a successful save calls - from a new, explicit,
/// user-initiated entry point.
///
/// See [`resync_external_change_with`] for the full behavior, including
/// the fail-closed guard this command needs since, unlike every other
/// reader here, it *writes* a file (the draft) as a side effect of
/// reading a different one (the real `/etc/fstab`).
///
/// # Errors
///
/// [`CommandErrorKindDto::TargetResolution`] if the target could not be
/// determined; [`CommandErrorKindDto::Resync`] if the active target is not
/// the default working draft, or if the fresh content could not be published
/// onto it; [`CommandErrorKindDto::ReadTarget`] if the real file exists but
/// could not be read; [`CommandErrorKindDto::NotUtf8`] if its bytes are not
/// valid UTF-8.
#[tauri::command(async)]
pub fn resync_external_change() -> Result<DocumentViewDto, CommandErrorDto> {
    resync_external_change_with(
        &resolve_target()?,
        real_fstab(),
        host::real_file_reader(),
        backup_location::real_target_location(),
    )
}

/// Validates a candidate document without writing anything.
///
/// Runs the identical validation a save runs, so the findings shown in the
/// form are the findings that will block (or not block) the save. Nothing
/// on disk is touched: the semantic `findmnt --verify` check inside
/// `validate_with` renders the candidate to its own throwaway temp file,
/// never to the target.
#[tauri::command(async, rename_all = "snake_case")]
pub fn validate_candidate(candidate: DocumentDto) -> ValidationReportDto {
    ValidationReportDto::from(&validate::validate_with(
        &Document::from(candidate),
        host::command_runner(),
    ))
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

/// Saves `candidate`, atomically.
///
/// Per `docs/SPEC.md` §7, "where" depends on how the active target was
/// chosen, not on whether it happens to be the real file:
///
/// - Default launch: writes the real `/etc/fstab` through a single
///   elevated re-exec, then best-effort updates the local draft to
///   match. The elevated write is what succeeds or fails; a draft that
///   couldn't be updated is reported in `draft_sync_warning` on an
///   otherwise successful outcome.
/// - `RUSTAB_TARGET_PATH` set: writes that path and only that path,
///   in-process, or elevated if the path is `/etc/fstab` itself.
///
/// Every case runs the same [`writer::write_document`]; see
/// [`save_document_with`] for the full reasoning.
///
/// `allow_external_change` is the "write anyway" signal, which the
/// frontend always passes explicitly: `false` normally, `true` only
/// after the user has seen the diff of an external change and asked for
/// the write regardless. Layered on top of the real-target confirmation,
/// never a replacement for it.
///
/// # Errors
///
/// A [`WriteErrorDto`]; check its `target_unchanged` field, which is `true`
/// only when the target is guaranteed to still hold its previous bytes.
/// [`WriteErrorKindDto::ExternalChange`] is the one kind that asks a question
/// rather than reporting a fault: nothing was written, and the error carries
/// both sides of the diff.
#[tauri::command(async, rename_all = "snake_case")]
pub fn save(
    candidate: DocumentDto,
    allow_external_change: bool,
) -> Result<WriteOutcomeDto, WriteErrorDto> {
    let target = resolve_target_for_write()?;
    save_document(
        &target,
        real_fstab(),
        host::real_file_reader(),
        &Document::from(candidate),
        allow_external_change,
    )
}

/// Lists the backups of the file this session's saves write, newest
/// first - the real `/etc/fstab`'s for a default launch (see
/// [`backup_target_path`]), the named path's own for an explicit
/// `RUSTAB_TARGET_PATH`.
///
/// Needs no privilege escalation either way, since `/etc/` and the
/// backups in it are world-readable.
///
/// A session that can't enumerate `/etc/` (a Flatpak sandbox) lists the
/// real file's backups from the fallback directory instead
/// ([`crate::backup_location`]), so it does **not** list the automatic
/// backups each save leaves beside `/etc/fstab` - those are confirmed to
/// still be written by the elevated writer, just invisible from here. The
/// frontend says so via [`TargetInfoDto::real_target_backup_fallback_dir`].
///
/// This is also the one backup operation that stayed unprivileged-only
/// rather than moving into `/etc/` for a sandboxed session: it calls
/// [`writer::list_backups`] in-process, where the `/etc` it would
/// enumerate is the runtime's. See [`crate::backup_location`]'s docs for
/// the full decision.
///
/// # Errors
///
/// [`CommandErrorKindDto::TargetResolution`] if the target could not be
/// determined, [`CommandErrorKindDto::ListBackups`] if the backup-owning
/// directory could not be read.
#[tauri::command(async)]
pub fn list_backups() -> Result<Vec<BackupInfoDto>, CommandErrorDto> {
    let target = resolve_target()?;
    list_backups_for(
        &target,
        real_fstab(),
        backup_location::real_target_location(),
    )
}

/// Restores a previously taken backup onto the active target.
///
/// `backup_path` must be one of the paths [`list_backups`] returned for
/// the *current* target; anything else is refused before being used as a
/// path at all (see [`crate::backups`]).
///
/// Not a privileged copy or a shortcut: the backup's text is parsed into
/// a document and goes through the very same [`save_document`] a normal
/// save uses - validated, backed up, written, `fsync`ed, renamed,
/// verified, elevated exactly once wherever a normal save would be.
/// Restoring a broken backup is refused for the same reasons saving
/// broken content is.
///
/// Because that delegation is the only write this command performs, it
/// inherits the dual write with no write code of its own: on a default
/// launch, restoring writes the real `/etc/fstab` and mirrors it into
/// the working draft, exactly as a save does. `backup_path` resolves
/// against [`backup_target_path`]'s list, so on a default launch the
/// backup being restored is always one of the real `/etc/fstab`'s - the
/// same list [`list_backups`] offers the user.
///
/// # Errors
///
/// A restore is a save, so an external change to the real `/etc/fstab`
/// blocks it exactly as it blocks [`save`], and `allow_external_change`
/// means the same thing here.
///
/// # Errors
///
/// [`WriteErrorKindDto::InvalidBackup`] if `backup_path` is not one of this
/// target's backups, [`WriteErrorKindDto::ReadBackup`] if it cannot be read
/// or is not UTF-8, otherwise whatever the save itself reported - including
/// [`WriteErrorKindDto::ExternalChange`].
#[tauri::command(async, rename_all = "snake_case")]
pub fn restore_backup(
    backup_path: String,
    allow_external_change: bool,
) -> Result<WriteOutcomeDto, WriteErrorDto> {
    let target = resolve_target_for_write()?;
    restore_backup_with(
        &target,
        real_fstab(),
        host::real_file_reader(),
        backup_location::real_target_location(),
        &backup_path,
        allow_external_change,
        privileged::save_elevated,
    )
}

/// Returns a backup's raw text, for the backup-diff view.
///
/// `backup_path` must be one of the paths [`list_backups`] returned for
/// the current target, resolved against [`backup_target_path`]'s backup
/// list exactly as [`restore_backup`] resolves it (see [`crate::backups`]).
///
/// Writes nothing and needs no privilege escalation, since `/etc/fstab`
/// and its backups are world-readable. Pair the returned text with
/// [`list_entries`]'s `text` to diff a restore point against what's live now.
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
    read_backup_of(
        &target,
        real_fstab(),
        backup_location::real_target_location(),
        &backup_path,
    )
}

/// Deletes one of the active target's backups.
///
/// `backup_path` must be one of the paths [`list_backups`] returned for
/// the current target. Only ever a backup file, never the target itself
/// (asserted by [`crate::privileged`]'s and this module's tests).
///
/// Dispatches on [`backups_are_elevated`] like [`save`]/[`restore_backup`]
/// dispatch on `source`: an explicit, non-real target's backups live in
/// a user-owned directory and are removed in-process, while the real
/// `/etc/fstab`'s backups live in root-owned `/etc/` and are removed by
/// a single elevated re-exec - unless they live in the fallback
/// directory instead, which is the user's own and needs no prompt. This
/// never opens, renames, or writes the target file, which is why it
/// rejects with [`DeleteBackupErrorDto`] rather than [`WriteErrorDto`].
///
/// # Errors
///
/// A [`DeleteBackupErrorDto`]; see [`DeleteBackupErrorKindDto`] for the kinds.
#[tauri::command(async, rename_all = "snake_case")]
pub fn delete_backup(backup_path: String) -> Result<(), DeleteBackupErrorDto> {
    let target = resolve_target_for_delete()?;
    delete_backup_from(
        &target,
        real_fstab(),
        backup_location::real_target_location(),
        &backup_path,
    )
}

/// Creates a backup of the active target right now, without changing it.
///
/// The explicit counterpart of the backup a [`save`] takes automatically:
/// copies the target's current content to a new manual backup and
/// returns it as a [`BackupInfoDto`], the same row shape [`list_backups`]
/// produces. A manual backup is exempt from the retention bound that
/// prunes automatic ones; only an explicit [`delete_backup`] removes it.
///
/// Which file it acts on is resolved server-side via
/// [`backup_target_path`], so a default launch backs up the real
/// `/etc/fstab`, not the working draft. Dispatches on
/// [`backups_are_elevated`] like [`delete_backup`]: an explicit, non-real
/// override is copied in-process, while `/etc/fstab` goes through a
/// single elevated re-exec - unless a session can't manage files in
/// `/etc/`, in which case it copies into the fallback directory
/// in-process instead (see [`create_backup_with`], [`crate::backup_location`]).
///
/// Never opens, renames, or writes the target file, only reads it - why
/// it rejects with [`CreateBackupErrorDto`] rather than [`WriteErrorDto`].
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
    create_backup_of(
        &target,
        real_fstab(),
        host::real_file_reader(),
        backup_location::real_target_location(),
    )
}

/// Lists block devices for the source picker.
///
/// Never fails: a system where `lsblk` is missing or unusable yields an empty
/// list with `lsblk_available: false`, and manual entry still works
/// (`docs/SPEC.md` §8).
#[tauri::command(async)]
pub fn discover_devices() -> DeviceDiscoveryDto {
    DeviceDiscoveryDto::from(&discovery::discover_devices_with(host::command_runner()))
}

/// Opens the directory holding this session's backups in the host file
/// manager, via `xdg-open`.
///
/// A target's own directory is also where its backups live (see
/// [`crate::backups`]'s module docs), so this is the one button the
/// Backups panel needs. Which directory that is follows
/// [`backup_target_path`], not the resolved target: a default launch
/// opens `/etc/`, where [`list_backups`] actually found them, or the
/// fallback directory when that's where the rows on screen came from.
///
/// No privilege escalation needed: `/etc/` is world-readable and
/// world-executable, and listing a directory needs strictly less access
/// than reading a file inside it (see [`read_backup`]).
///
/// # Errors
///
/// [`CommandErrorKindDto::TargetResolution`] if the target could not be
/// determined, [`CommandErrorKindDto::OpenLocation`] if the file manager
/// could not be launched.
#[tauri::command(async, rename_all = "snake_case")]
pub fn open_backup_location() -> Result<(), CommandErrorDto> {
    let target = resolve_target()?;
    open_backup_location_of(
        &target,
        real_fstab(),
        backup_location::real_target_location(),
    )
}

/// `open_external_link(link) -> null` (API_CONTRACT.md §3.13). Opens one
/// of this app's five known external links (used by the About dialog) via
/// `xdg-open` - GitHub/Sponsors links in the default browser, the
/// `Email` link's `mailto:` URI in the default mail client.
///
/// Takes a closed enum, not a raw URL string: the set is fixed, baked
/// into [`ExternalLinkDto::url`], so there's no argument surface for a
/// caller to redirect this at an arbitrary URL. This also avoids a plain
/// `<a href="mailto:...">`, since WebKitGTK shows "The URL can't be
/// shown" for `mailto:` instead of falling back to the OS's scheme handler.
///
/// No privilege escalation needed, same reasoning as [`open_backup_location`].
///
/// # Errors
///
/// [`CommandErrorKindDto::OpenExternalLink`] if the external program could
/// not be launched.
#[tauri::command(async, rename_all = "snake_case")]
pub fn open_external_link(link: ExternalLinkDto) -> Result<(), CommandErrorDto> {
    open_external_link_with(link, open_browser)
}

/// `open_path_in_file_manager(path) -> null` (API_CONTRACT.md §3.14). Opens
/// an arbitrary frontend-supplied path in the host file manager, via
/// `xdg-open`.
///
/// `path` is this project's second explicit exception to "no command
/// accepts a target path" (the first is [`restore_backup`]'s
/// `backup_path`), but a different kind: this command is read-only and
/// needs no privilege escalation, so there's no write or privileged read
/// to redirect. The path is also never a secret - it's the Source/Mount
/// point text already visible in the table - so a malicious frontend
/// gains nothing it couldn't already get by opening that path itself
/// with Rustab's own unprivileged access (see `docs/API_CONTRACT.md` §2).
///
/// Decoded via [`fstab_core::model::decode_escaped_whitespace`] first,
/// since the model stores a mount point/source exactly as written -
/// octal escapes and all - so without decoding, `xdg-open` would look
/// for a literal `\040` instead of the space the real path has (same
/// reason `fstab_core::validate`'s mount-point-exists check decodes too).
///
/// # Errors
///
/// [`CommandErrorKindDto::OpenPath`] if the file manager could not be
/// launched.
#[tauri::command(async, rename_all = "snake_case")]
pub fn open_path_in_file_manager(path: String) -> Result<(), CommandErrorDto> {
    let decoded = fstab_core::model::decode_escaped_whitespace(&path);
    open_path_in_file_manager_with(Path::new(&decoded), open_in_file_manager)
}

// ---------------------------------------------------------------------------
// Shared internals
// ---------------------------------------------------------------------------

/// The real `/etc/fstab`, as the `real_fstab` argument every backup internal
/// below takes.
///
/// A one-line accessor for [`target::REAL_FSTAB_PATH`] rather than the
/// constant inlined six times.
fn real_fstab() -> &'static Path {
    Path::new(target::REAL_FSTAB_PATH)
}

/// Which file's backups a backup command acts on - the **backup-owning**
/// target, which is not always the resolved one.
///
/// One `match`, mirroring [`save_document_with`]'s dispatch:
///
/// - [`TargetSource::SandboxDefault`] - `real_fstab`. A save of this
///   target writes the real `/etc/fstab` and leaves its automatic
///   backups beside it, so the Backups panel has to describe that file -
///   the whole point of backups is restoring the real fstab from a
///   rescue environment, which a backup of the working draft couldn't serve.
/// - [`TargetSource::Environment`] - `target.path`, whether or not it
///   happens to be `/etc/fstab`: an explicit override means "this path,
///   outright", so its backups are its own.
///
/// The real path is a parameter, not read from the constant, since a
/// backup operation genuinely reads the backup-owning file's directory -
/// production passes [`real_fstab`], tests pass a temp file, so the code
/// under test is the shipped code, and `cargo test` never touches the
/// machine's real `/etc/fstab`.
///
/// This two-step answer also decides *which directory*: the `match`
/// above is the whole answer when the real file's backups are colocated;
/// when they aren't ([`backup_fallback_dir`]), the path returned is the
/// **mirror file** inside the fallback directory instead. That one
/// substitution redirects the entire backup surface, since every
/// function underneath derives "the backup directory" from "the target
/// file's own parent directory".
fn backup_target_path(
    target: &ActiveTarget,
    real_fstab: &Path,
    location: &BackupLocation,
) -> PathBuf {
    match backup_fallback_dir(target, location) {
        Some(dir) => backup_location::mirror_path(dir),
        None => colocated_backup_target_path(target, real_fstab),
    }
}

/// The backup-owning path when the real file's backups live beside it -
/// still the production answer on every launch not sandboxed away from `/etc/`.
///
/// Split out from [`backup_target_path`] because two callers need exactly
/// this without the fallback substitution: [`backup_target_path`] itself
/// (the colocated branch) and [`refresh_backup_mirror`], which needs to
/// know the file the mirror is a copy of.
fn colocated_backup_target_path(target: &ActiveTarget, real_fstab: &Path) -> PathBuf {
    match target.source {
        TargetSource::SandboxDefault => real_fstab.to_path_buf(),
        TargetSource::Environment => target.path.clone(),
    }
}

/// The directory this session's backups are kept in **instead of** beside
/// the file they belong to, or `None` when they're kept beside it after all.
///
/// `Some(dir)` requires both: the backups in question are the real
/// `/etc/fstab`'s ([`backups_belong_to_the_real_fstab`]), and this
/// process can't manage files in that directory
/// ([`BackupLocation::fallback_dir`]). Short-circuiting matters: an
/// explicit `RUSTAB_TARGET_PATH` naming some other, user-owned file
/// fails the first half and never consults `location` at all - its
/// backups are its own, regardless of what this process can do with `/etc/`.
fn backup_fallback_dir<'a>(
    target: &ActiveTarget,
    location: &'a BackupLocation,
) -> Option<&'a Path> {
    if backups_belong_to_the_real_fstab(target) {
        location.fallback_dir()
    } else {
        None
    }
}

/// Whether this session's backups are the real `/etc/fstab`'s - `true`
/// for a default launch or an explicit `RUSTAB_TARGET_PATH=/etc/fstab`,
/// `false` only for an explicit override naming some other file.
///
/// Named for what it *is*, not for what it used to imply: "these are the
/// real file's backups" and "mutating them needs `pkexec`" used to be
/// the same sentence, but in fallback mode the real file's backups need
/// no elevation. This is the first question; [`backups_are_elevated`] is
/// the second, defined in terms of this one so they can't drift.
fn backups_belong_to_the_real_fstab(target: &ActiveTarget) -> bool {
    target.is_real || target.source == TargetSource::SandboxDefault
}

/// Refreshes the fallback directory's mirror file from the file it
/// mirrors, so a backup taken from it is a copy of the real `/etc/fstab`
/// as of now.
///
/// A no-op in colocated mode or for a non-real target, where there's no
/// mirror to keep fresh. Called only by [`create_backup_with`]:
/// `list_backups`/`delete_backup` only act on files already in the
/// directory, so a fresh mirror would cost them a read-write of the
/// whole file for no effect.
///
/// # Errors
///
/// The underlying [`io::Error`] from [`backup_location::refresh_mirror`],
/// including [`io::ErrorKind::NotFound`] when the mirrored file doesn't
/// exist, which the caller turns into "nothing to back up yet".
fn refresh_backup_mirror(
    target: &ActiveTarget,
    real_fstab: &Path,
    real_reader: &dyn RealFileReader,
    location: &BackupLocation,
) -> io::Result<()> {
    match backup_fallback_dir(target, location) {
        Some(dir) => backup_location::refresh_mirror(
            dir,
            &colocated_backup_target_path(target, real_fstab),
            real_reader,
        ),
        None => Ok(()),
    }
}

/// The [`TargetInfoDto`] for `target` in this session - the one place
/// that builds it, so every command reports the same thing.
///
/// The DTO's fallback-directory field can't be derived from an
/// [`ActiveTarget`] alone; it's [`backup_fallback_dir`]'s answer,
/// rendered for display, so the notice shown and the directory the
/// backup commands act on are one answer, not two that could disagree.
fn describe_target(target: &ActiveTarget, location: &BackupLocation) -> TargetInfoDto {
    target::describe(target, backup_fallback_dir(target, location))
}

/// Whether *mutating* this session's backups needs the elevated helper.
///
/// The backend's equivalent of the frontend's `writes_real_file`
/// (`frontend/src/components/app_shell.rs`, `docs/UI_DESIGN.md` §2), and
/// deliberately the identical predicate: the two answer the same question
/// ("does this session's work land on the real `/etc/fstab`?") and must never
/// drift apart, because one decides whether a `pkexec` prompt appears and the
/// other decides whether the user is asked to confirm it first.
///
/// `true` for a default launch (its backups are `/etc/fstab`'s, in root-owned
/// `/etc/`) and for an explicit `RUSTAB_TARGET_PATH=/etc/fstab`; `false` only
/// for an explicit override naming some other, user-owned file - **and**,
/// since T39, `false` for the real file's backups when they are not in
/// `/etc/` at all.
///
/// That second `false` is the point of the T39 change here: in fallback mode
/// the real file's manual backups live in a directory inside the user's own
/// data directory ([`crate::backup_location`]), which is never root-owned, so
/// creating or unlinking one there must **not** drag the user through a
/// `pkexec` prompt for a file they already own. Writing the real `/etc/fstab`
/// itself is a different question with a different answer and is decided
/// elsewhere, in [`save_document_with`], which T39 does not touch: a restore
/// still elevates in fallback mode, because it writes `/etc/fstab`.
///
/// Note what this does *not* gate: [`list_backups`], [`read_backup`] and
/// [`open_backup_location`] never consult it, because `/etc/` and the backups
/// in it are world-readable - only creating and unlinking a file there needs
/// root.
fn backups_are_elevated(target: &ActiveTarget, location: &BackupLocation) -> bool {
    backups_belong_to_the_real_fstab(target) && backup_fallback_dir(target, location).is_none()
}

/// The body of [`list_backups`], with the target, real-file path, and
/// backup location passed in (see [`backup_target_path`]).
///
/// Doesn't refresh the fallback directory's mirror file: listing
/// enumerates files already there, and the mirror isn't one of them
/// (see [`refresh_backup_mirror`]).
fn list_backups_for(
    target: &ActiveTarget,
    real_fstab: &Path,
    location: &BackupLocation,
) -> Result<Vec<BackupInfoDto>, CommandErrorDto> {
    backups::list(&backup_target_path(target, real_fstab, location))
        .map_err(|err| CommandErrorDto::new(CommandErrorKindDto::ListBackups, err.to_string()))
}

/// The body of [`resync_external_change`], with the real-file path
/// passed in (production passes [`real_fstab`], tests pass a temp file).
///
/// **Fails closed - touching neither the draft nor the marker - unless
/// `target.source == TargetSource::SandboxDefault`.** This guard is
/// load-bearing, not defensive dead code: without it, this command would
/// overwrite *any* resolved target's draft with the real `/etc/fstab`,
/// which for an explicit `RUSTAB_TARGET_PATH` test target would be a
/// genuine data-safety bug.
///
/// Otherwise, reads `real_fstab`'s current bytes: missing is not an
/// error (synced to empty, like [`list_entries`]'s missing-target
/// handling), non-UTF-8 bytes are refused outright (same reason as
/// [`list_entries`]), any other read error is
/// [`CommandErrorKindDto::ReadTarget`].
///
/// On success (missing file included), the result is published onto the
/// draft and its marker atomically with
/// [`target::sync_sandbox_after_save`] - the same primitive a successful
/// save uses. A failure there is [`CommandErrorKindDto::Resync`], same
/// kind as the fail-closed guard above, different message.
///
/// The returned [`DocumentViewDto`] is built directly from `text`, not a
/// second read-back, since the draft now holds exactly this text.
fn resync_external_change_with(
    target: &ActiveTarget,
    real_fstab: &Path,
    real_reader: &dyn RealFileReader,
    location: &BackupLocation,
) -> Result<DocumentViewDto, CommandErrorDto> {
    if target.source != TargetSource::SandboxDefault {
        return Err(CommandErrorDto::new(
            CommandErrorKindDto::Resync,
            format!(
                "{} is not the default working draft, so it has no last-known-real marker to \
                 re-sync from the real /etc/fstab; nothing was touched",
                target.path.display()
            ),
        ));
    }

    let (exists, text) = match real_reader.read(real_fstab) {
        Ok(bytes) => {
            // Not `from_utf8_lossy` - same reasoning as `list_entries`: a
            // lossy load would publish mangled content onto the draft and marker.
            let text = String::from_utf8(bytes).map_err(|err| {
                CommandErrorDto::new(
                    CommandErrorKindDto::NotUtf8,
                    format!(
                        "{} is not valid UTF-8 ({err}), so Rustab will not re-sync from it - \
                         doing so could only publish corrupted content onto the working draft. \
                         Fix the encoding with a text editor first.",
                        real_fstab.display()
                    ),
                )
            })?;
            (true, text)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => (false, String::new()),
        Err(err) => {
            return Err(CommandErrorDto::new(
                CommandErrorKindDto::ReadTarget,
                format!("could not read {}: {err}", real_fstab.display()),
            ))
        }
    };

    target::sync_sandbox_after_save(&target.path, text.as_bytes()).map_err(|err| {
        CommandErrorDto::new(
            CommandErrorKindDto::Resync,
            format!(
                "{} was read successfully, but the local working draft {} could not be updated \
                 to match it ({err}); try again",
                real_fstab.display(),
                target.path.display()
            ),
        )
    })?;

    let document = parser::parse(&text);
    Ok(DocumentViewDto {
        // Same target info `list_entries` reports, built the same way, so this
        // view can't describe the session differently from the one before it.
        target: describe_target(target, location),
        exists,
        text,
        has_malformed_lines: document.has_malformed_lines(),
        document: DocumentDto::from(&document),
    })
}

/// The body of [`restore_backup`], with the real-file path and the
/// elevated half of the save injectable.
///
/// Two steps: resolve `backup_path` against [`backup_target_path`]'s
/// backup list (the real `/etc/fstab`'s, for a default launch, or the
/// fallback directory's list when that's what [`list_backups`] offered),
/// then hand the backup's parsed text to [`save_document_with`]
/// unchanged - that's where the dual write and the external-change check
/// live, so a restore inherits both with no write code of its own. Where
/// a backup is *stored* says nothing about where restoring it writes:
/// step two still writes the real `/etc/fstab` through the same single
/// elevated call.
fn restore_backup_with(
    target: &ActiveTarget,
    real_fstab: &Path,
    real_reader: &dyn RealFileReader,
    location: &BackupLocation,
    backup_path: &str,
    allow_external_change: bool,
    elevate: impl Fn(
        &Path,
        &Document,
        &target::ExternalChangeGuard,
    ) -> Result<WriteOutcomeDto, WriteErrorDto>,
) -> Result<WriteOutcomeDto, WriteErrorDto> {
    let owner = backup_target_path(target, real_fstab, location);

    // Both "list unreadable" and "path not in it" report as `InvalidBackup`:
    // same frontend situation (unusable, nothing written), message says which.
    let backup = backups::resolve_requested(&owner, backup_path).map_err(|err| {
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

    save_document_with(
        target,
        real_fstab,
        real_reader,
        &parser::parse(&text),
        allow_external_change,
        elevate,
    )
}

/// The one dispatch point between the save paths.
///
/// Every branch ends in [`writer::write_document`] - the in-process
/// branch calls it directly, the elevated branch calls it inside a root
/// process that does nothing else (see [`crate::privileged`]). No path
/// writes a target without going through `write_document`.
///
/// Takes the already-resolved [`ActiveTarget`] rather than resolving it
/// itself, so tests can point a save at a temp file without mutating
/// process-wide environment variables. `real_fstab` is a parameter for
/// the same reason as on [`backup_target_path`].
fn save_document(
    target: &ActiveTarget,
    real_fstab: &Path,
    real_reader: &dyn RealFileReader,
    candidate: &Document,
    allow_external_change: bool,
) -> Result<WriteOutcomeDto, WriteErrorDto> {
    save_document_with(
        target,
        real_fstab,
        real_reader,
        candidate,
        allow_external_change,
        privileged::save_elevated,
    )
}

/// [`save_document`], with the elevated half injectable - the place the
/// whole "a save always writes the real `/etc/fstab`" decision lives
/// (`docs/SPEC.md` §7, `docs/API_CONTRACT.md` §3.5).
///
/// The seam exists so the dispatch decisions can be *asserted* without a
/// unit test launching `pkexec` and waiting on a password prompt: tests
/// pass a recording stand-in, production a one-line delegation that
/// can't diverge.
///
/// ## The three cases, keyed on `source`, not `is_real`
///
/// 1. **[`TargetSource::SandboxDefault`]** - the normal, unconfigured
///    launch. The save goes to the real [`target::REAL_FSTAB_PATH`]
///    through `elevate` (never `target.path`, which is written
///    separately, in-process, unprivileged), and that result is *the*
///    result of the save. `target.path` (the draft) is then best-effort
///    updated to match, along with the marker that stops the next
///    launch from reading this save as external drift.
/// 2. **[`TargetSource::Environment`] naming some other file** - the
///    dev/test escape hatch and the only way to run Rustab without
///    writing `/etc/fstab`: one in-process write, no escalation, no
///    mirroring, no marker.
/// 3. **[`TargetSource::Environment`] naming `/etc/fstab` itself** - one
///    elevated write of `target.path`, deliberately with no draft mirror
///    and no marker update.
///
/// Case 3's reasoning: the dual write is documented as a property of the
/// *default pair*, not of the real file, so `RUSTAB_TARGET_PATH` means
/// exactly one thing for every value - "this path, outright" - rather
/// than a special case for `/etc/fstab`. Consequence: a later default
/// launch will see `/etc/fstab` differing from its marker and refresh
/// the draft, which is correct - the file really did change under it,
/// via a run that wasn't keeping the draft in sync.
///
/// ## The external-change check, and why only case 1 has one
///
/// Case 1 compares the marker against what the real file holds *now*,
/// right before elevating, and refuses the write if they differ
/// ([`target::ExternalChangeGuard`], [`WriteErrorKindDto::ExternalChange`]).
/// `allow_external_change` skips that comparison for this one call; a
/// "write anyway" that succeeds is otherwise an ordinary save, mirror
/// and marker included.
///
/// This check is a courtesy, not the safety net - it avoids a password
/// prompt for a write the elevated side will refuse anyway. The binding
/// check is the identical comparison performed inside the elevated
/// process immediately before `write_document`
/// (`crate::privileged::handle_write`), since `pkexec`'s password wait
/// leaves a window for the file to change. The same relationship
/// `privileged::save_elevated` has between its pre-prompt `validate`
/// call and the authoritative validation inside `write_document`.
///
/// Case 2 needs no check (no marker kept); case 3 keeps none either (no
/// marker to compare against, so it passes
/// [`target::ExternalChangeGuard::unchecked`]), leaving an explicit
/// `RUSTAB_TARGET_PATH=/etc/fstab` with a known, recorded race.
///
/// `real_fstab` is a parameter because the external-change check
/// genuinely *reads* that file, unlike the path merely handed to
/// `elevate` - so it needs the same injectable seam as
/// [`backup_target_path`], for the same reason: `cargo test` must never
/// read the machine's own `/etc/fstab`.
fn save_document_with(
    target: &ActiveTarget,
    real_fstab: &Path,
    real_reader: &dyn RealFileReader,
    candidate: &Document,
    allow_external_change: bool,
    elevate: impl Fn(
        &Path,
        &Document,
        &target::ExternalChangeGuard,
    ) -> Result<WriteOutcomeDto, WriteErrorDto>,
) -> Result<WriteOutcomeDto, WriteErrorDto> {
    match target.source {
        TargetSource::SandboxDefault => {
            let guard =
                target::ExternalChangeGuard::from_marker(&target.path, allow_external_change);
            // The courtesy check (see this function's docs). Nothing has been
            // opened for writing here, so the error can promise `target_unchanged`.
            if let Some(change) = guard.detect_with(real_fstab, real_reader) {
                return Err(WriteErrorDto::external_change(
                    real_fstab,
                    &change.last_known,
                    &change.current,
                ));
            }

            // Authoritative: if this fails, the save failed, and nothing is
            // mirrored anywhere - the draft must not start showing content
            // that is not in the real file.
            let mut outcome = elevate(real_fstab, candidate, &guard)?;

            // The exact bytes the elevated writer just put into the real file
            // (`writer::write_document` writes `candidate.to_string()`), so
            // the draft is a copy of reality and the marker records reality.
            let saved = candidate.to_string();
            if let Err(err) = target::sync_sandbox_after_save(&target.path, saved.as_bytes()) {
                outcome.draft_sync_warning = Some(format!(
                    "{} was saved successfully, but the local working draft {} could not be \
                     updated to match it ({err}). Nothing is lost: the saved file is the real \
                     one, and the draft is refreshed from it the next time Rustab starts.",
                    real_fstab.display(),
                    target.path.display()
                ));
            }
            Ok(outcome)
        }
        // No marker for an explicit override, so nothing to compare here.
        TargetSource::Environment if target.is_real => elevate(
            &target.path,
            candidate,
            &target::ExternalChangeGuard::unchecked(),
        ),
        TargetSource::Environment => {
            writer::write_document_with_runner(&target.path, candidate, host::command_runner())
                .map(|outcome| WriteOutcomeDto::from_outcome(&outcome, false))
                .map_err(|err| WriteErrorDto::from(&err))
        }
    }
}

/// The body of [`read_backup`], with the target and the real-file path
/// passed in, for the same reason as [`save_document`].
fn read_backup_of(
    target: &ActiveTarget,
    real_fstab: &Path,
    location: &BackupLocation,
    backup_path: &str,
) -> Result<String, CommandErrorDto> {
    let owner = backup_target_path(target, real_fstab, location);
    let backup = backups::resolve_requested(&owner, backup_path)
        .map_err(|err| CommandErrorDto::new(CommandErrorKindDto::InvalidBackup, err.to_string()))?;

    backups::read_text(&backup).map_err(|err| {
        // `InvalidData` is the not-valid-UTF-8 case, already a kind of its
        // own the frontend handles for the target; reuse it here too.
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

/// The body of [`open_backup_location`], with the target and the real-file
/// path passed in (see [`read_backup_of`] for why).
fn open_backup_location_of(
    target: &ActiveTarget,
    real_fstab: &Path,
    location: &BackupLocation,
) -> Result<(), CommandErrorDto> {
    open_backup_location_with(target, real_fstab, location, open_in_file_manager)
}

/// [`open_backup_location_of`], with the actual launch of a GUI program
/// injectable, so a unit test can assert *which* directory this command
/// would open without spawning a file manager window (impossible in CI,
/// which has no display). Production delegates to
/// [`open_in_file_manager`] in one line, which can't diverge.
fn open_backup_location_with(
    target: &ActiveTarget,
    real_fstab: &Path,
    location: &BackupLocation,
    opener: impl Fn(&Path) -> io::Result<()>,
) -> Result<(), CommandErrorDto> {
    let dir = backups::containing_dir(&backup_target_path(target, real_fstab, location));
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

/// [`open_path_in_file_manager`], with the actual launch of a GUI program
/// injectable - same structural reason as [`open_backup_location_with`]: a
/// unit test has to assert which path this command would open without truly
/// spawning a file manager window.
///
/// Unlike [`open_backup_location_with`], there is no target to resolve here
/// first: `path` comes straight from the frontend argument, never from
/// [`target::active_target`], so this function does nothing before handing
/// `path` to `opener`.
fn open_path_in_file_manager_with(
    path: &Path,
    opener: impl Fn(&Path) -> io::Result<()>,
) -> Result<(), CommandErrorDto> {
    opener(path).map_err(|err| {
        CommandErrorDto::new(
            CommandErrorKindDto::OpenPath,
            format!("could not open {} in a file manager: {err}", path.display()),
        )
    })
}

/// The body of [`delete_backup`], with the target and the real-file path
/// passed in (see [`read_backup_of`] and [`backup_target_path`] for why).
fn delete_backup_from(
    target: &ActiveTarget,
    real_fstab: &Path,
    location: &BackupLocation,
    backup_path: &str,
) -> Result<(), DeleteBackupErrorDto> {
    delete_backup_with(
        target,
        real_fstab,
        location,
        backup_path,
        privileged::delete_backup_elevated,
    )
}

/// The one dispatch point between the two backup-deletion paths, with the
/// elevated half injectable.
///
/// Mirrors [`save_document_with`], for the same reason: the dispatch
/// decision ("the real `/etc/fstab`'s backups are never unlinked by this
/// process") has to be asserted without a unit test launching `pkexec`.
///
/// `backup_path` is resolved before dispatch in both branches. For the
/// elevated branch that's only a courtesy, refusing a path that can't
/// work before the password prompt - the authoritative check is the
/// elevated process's own [`backups::resolve_requested`] call, which
/// doesn't trust this one (see [`crate::privileged`]).
///
/// The backup-owning path is [`backup_target_path`] and the dispatch is
/// [`backups_are_elevated`]: on a default launch this unlinks one of the
/// real `/etc/fstab`'s backups through the elevated helper, since those
/// files live in root-owned `/etc/`. The frontend side of the same
/// decision is `BackupDeleteConfirmDialog`'s `writes_real_file` gate, so
/// the privileged delete is still behind the explicit checkbox required
/// for every real-touching action (`docs/UI_DESIGN.md` §2).
fn delete_backup_with(
    target: &ActiveTarget,
    real_fstab: &Path,
    location: &BackupLocation,
    backup_path: &str,
    elevate: impl Fn(&Path, &str) -> Result<(), DeleteBackupErrorDto>,
) -> Result<(), DeleteBackupErrorDto> {
    let owner = backup_target_path(target, real_fstab, location);
    let backup = backups::resolve_requested(&owner, backup_path).map_err(|err| {
        DeleteBackupErrorDto::new(
            DeleteBackupErrorKindDto::InvalidBackup,
            format!("{err}; nothing was deleted"),
        )
    })?;

    if backups_are_elevated(target, location) {
        elevate(&owner, &backup.to_string_lossy())
    } else {
        fs::remove_file(&backup).map_err(|err| {
            DeleteBackupErrorDto::new(
                DeleteBackupErrorKindDto::Delete,
                format!("could not delete the backup {}: {err}", backup.display()),
            )
        })
    }
}

/// The body of [`create_backup`], with the target and the real-file path
/// passed in (see [`read_backup_of`] and [`backup_target_path`] for why).
fn create_backup_of(
    target: &ActiveTarget,
    real_fstab: &Path,
    real_reader: &dyn RealFileReader,
    location: &BackupLocation,
) -> Result<BackupInfoDto, CreateBackupErrorDto> {
    create_backup_with(
        target,
        real_fstab,
        real_reader,
        location,
        privileged::create_backup_elevated,
    )
}

/// The one dispatch point between the two backup-creation paths, with the
/// elevated half injectable.
///
/// Mirrors [`save_document_with`]/[`delete_backup_with`], for the same
/// reason: the dispatch decision ("the real `/etc/fstab` is never copied
/// by this process") has to be asserted without a unit test launching `pkexec`.
///
/// Which file is copied is [`backup_target_path`], not the resolved
/// target, so a default launch backs up the real `/etc/fstab` rather
/// than the working draft. Dispatch follows the same
/// [`backups_are_elevated`] predicate [`delete_backup_with`] uses.
///
/// Both branches end in the same `fstab_core::writer::create_backup` -
/// in-process directly, or elevated inside a root process that does
/// nothing else - and neither is given a path to write to; the backup's
/// name comes from the target it was handed.
///
/// The resulting path is turned back into a [`BackupInfoDto`] by
/// [`backups::describe_one`], the same helper [`backups::list`] uses, so
/// a freshly created backup is described exactly as the next
/// `list_backups` will describe it.
///
/// In fallback mode `owner` is the mirror file rather than the real
/// `/etc/fstab`, so this is the one backup operation that writes before
/// it reads: [`refresh_backup_mirror`] copies the real file's current
/// bytes onto the mirror first, so the result is a snapshot as of the
/// moment the user pressed the button. The dispatch below then takes
/// the in-process branch, since the fallback directory is the user's own.
fn create_backup_with(
    target: &ActiveTarget,
    real_fstab: &Path,
    real_reader: &dyn RealFileReader,
    location: &BackupLocation,
    elevate: impl Fn(&Path) -> Result<CreateBackupOutcomeDto, CreateBackupErrorDto>,
) -> Result<BackupInfoDto, CreateBackupErrorDto> {
    let owner = backup_target_path(target, real_fstab, location);

    // Fallback mode only (a no-op otherwise): `owner` is then the mirror file,
    // so it has to be brought up to date with the real `/etc/fstab` *before*
    // it is copied - a backup of a stale mirror would be a snapshot of
    // something that is no longer true.
    refresh_backup_mirror(target, real_fstab, real_reader, location).map_err(|err| {
        let source = colocated_backup_target_path(target, real_fstab);
        if err.kind() == io::ErrorKind::NotFound {
            // The same answer `writer::create_backup` gives for a target that
            // is not on disk yet, reported with the same kind so the frontend
            // needs no second case for it.
            CreateBackupErrorDto::new(
                CreateBackupErrorKindDto::TargetMissing,
                format!(
                    "{} does not exist yet, so there is nothing to back up; \
                     save the file once first",
                    source.display()
                ),
            )
        } else {
            CreateBackupErrorDto::new(
                CreateBackupErrorKindDto::Backup,
                format!(
                    "could not copy {} into {} to back it up ({err}); no backup was created, \
                     and {} itself is unchanged",
                    source.display(),
                    owner.display(),
                    source.display()
                ),
            )
        }
    })?;

    let outcome = if backups_are_elevated(target, location) {
        elevate(&owner)?
    } else {
        writer::create_backup(&owner)
            .map(|outcome| CreateBackupOutcomeDto::from_outcome(&outcome))
            .map_err(|err| CreateBackupErrorDto::from(&err))?
    };

    let created = Path::new(&outcome.backup);
    backups::describe_one(&owner, created)
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
                    owner.display()
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
    use crate::host::{DirectFs, StubReader};
    use crate::testutil::TempDir;
    use fstab_core::model::{FstabEntry, FstabOption, Line, Options, SourceId};
    use std::path::{Path, PathBuf};

    const ORIGINAL: &str = "# the content a save must back up\n\
                            UUID=11111111-2222-3333-4444-555555555555 /mnt/original ext4 noauto 0 2\n";

    /// A realistic real-`/etc/fstab` stand-in for the `resync_external_change`
    /// tests - content a parse-and-reserialize round trip could plausibly
    /// normalize (tabs, a trailing comment), so asserting byte equality
    /// against it really does prove a literal copy, not a round trip.
    const REAL_FSTAB_FOR_RESYNC: &str = "\
# /etc/fstab: static file system information.
UUID=aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\t/\text4\trw,relatime\t0 1
LABEL=data   /mnt/data   xfs   defaults,nofail   0   2 # a trailing note";

    /// A non-real target at `path`, as `active_target()` would have produced
    /// for an explicit `RUSTAB_TARGET_PATH` pointing at a scratch file.
    ///
    /// This - not the default sandbox - is the shape most tests in here want:
    /// it is the one case whose save stays entirely in-process, so a test can
    /// exercise the real [`writer::write_document`] end to end without
    /// `pkexec` and without an elevate stand-in. Post-T34 it is also the
    /// dev/test escape hatch itself, so these tests double as proof that the
    /// hatch still behaves exactly as it did (see
    /// [`an_explicit_non_real_override_never_reaches_the_elevated_writer`]).
    /// Named `override_target` rather than the pre-T34 `sandbox_target`
    /// because "sandbox" now means the working draft, which is a *different*
    /// case with a *different* dispatch.
    fn override_target(path: PathBuf) -> ActiveTarget {
        ActiveTarget {
            path,
            is_real: false,
            source: TargetSource::Environment,
        }
    }

    /// The default, unconfigured target: the working draft at `path`, with no
    /// `RUSTAB_TARGET_PATH` set. A save of this writes the real `/etc/fstab`
    /// (through the injected `elevate` stand-in, never for real) and mirrors
    /// into `path`.
    fn default_draft_target(path: PathBuf) -> ActiveTarget {
        ActiveTarget {
            path,
            is_real: false,
            source: TargetSource::SandboxDefault,
        }
    }

    /// The `real_fstab` argument for a test whose target is an explicit
    /// [`TargetSource::Environment`] override.
    ///
    /// [`backup_target_path`] never consults it for that source, so the value
    /// cannot affect such a test's outcome - and naming a path that does not
    /// exist (rather than reusing the test's own temp file) makes that
    /// irrelevance visible at the call site *and* turns any future accidental
    /// read of it into an obvious failure instead of a silently passing test.
    /// Tests of the default-launch case pass a real temp file instead; that is
    /// the whole point of the parameter (see [`backup_target_path`]).
    fn unused_real_fstab() -> &'static Path {
        Path::new("/nonexistent/rustab-tests-must-never-touch-this/fstab")
    }

    /// The backup location every launch outside a Flatpak sandbox gets, and
    /// the one every test written before T39 assumes: the real file's own
    /// directory is usable, so backups stay beside it and nothing is
    /// redirected anywhere (see [`crate::backup_location::BackupLocation`]).
    ///
    /// Passing it explicitly - rather than letting the production
    /// [`backup_location::real_target_location`] probe decide - is what keeps
    /// these tests off the machine's own `/etc/`, the same reason
    /// `real_fstab` is a parameter (see [`backup_target_path`]).
    fn colocated() -> &'static BackupLocation {
        static COLOCATED: BackupLocation = BackupLocation::Colocated;
        &COLOCATED
    }

    /// A fallback location whose directory is `dir` - what a Flatpak session's
    /// probe produces (see [`crate::backup_location::detect`]).
    fn fallback_to(dir: &Path) -> BackupLocation {
        BackupLocation::Fallback {
            dir: dir.to_path_buf(),
        }
    }

    /// An elevate stand-in that records what it was asked to write - path,
    /// document and, since T36, the [`target::ExternalChangeGuard`] it was
    /// handed to pass on to the elevated side - and reports the success the
    /// real helper would report, **without writing anything at all**, which is
    /// what keeps these tests away from `pkexec` and from the machine's own
    /// `/etc/fstab`.
    #[allow(clippy::type_complexity)]
    fn recording_elevate(
        calls: &std::cell::RefCell<Vec<(PathBuf, Document, target::ExternalChangeGuard)>>,
    ) -> impl Fn(
        &Path,
        &Document,
        &target::ExternalChangeGuard,
    ) -> Result<WriteOutcomeDto, WriteErrorDto>
           + '_ {
        move |path, document, guard| {
            calls
                .borrow_mut()
                .push((path.to_path_buf(), document.clone(), guard.clone()));
            Ok(WriteOutcomeDto {
                target: path.to_string_lossy().into_owned(),
                backup: None,
                pruned_backups: Vec::new(),
                validation_warnings: Vec::new(),
                notes: Vec::new(),
                elevated: true,
                draft_sync_warning: None,
            })
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

    // -- the in-process save path (an explicit non-real override) ----------
    //
    // Post-T34 this is the `RUSTAB_TARGET_PATH`-naming-a-scratch-file case
    // (see `override_target`), not the default launch - the default launch's
    // save elevates and is covered under "T34 save dispatch" below.

    /// End-to-end proof that a save of a non-real target calls through to
    /// `writer::write_document`: the file is replaced with the candidate's
    /// serialization *and* a backup of the previous content appears beside
    /// it, which only `write_document` creates.
    #[test]
    fn saving_an_override_target_writes_through_write_document() {
        let dir = TempDir::new("save-override");
        let path = dir.join("fstab.custom");
        fs::write(&path, ORIGINAL).expect("seed the target");
        let target = override_target(path.clone());

        let candidate = valid_candidate("/mnt/saved");
        let outcome = save_document(&target, unused_real_fstab(), &DirectFs, &candidate, false)
            .expect("the save should succeed");

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
            "an in-process save must never report itself as elevated"
        );
        // The real backup list agrees with what the outcome claims.
        let listed = backups::list(&path).expect("listing must succeed");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].path, backup);
    }

    /// The same path, from the command's own entry point (DTO in, DTO out),
    /// to prove the conversions in between are lossless.
    #[test]
    fn saving_an_override_target_from_the_dto_shape_is_lossless() {
        let dir = TempDir::new("save-dto");
        let path = dir.join("fstab.draft");
        let target = override_target(path.clone());

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

        save_document(
            &target,
            unused_real_fstab(),
            &DirectFs,
            &round_tripped,
            false,
        )
        .expect("the save should succeed");
        assert_eq!(
            fs::read_to_string(&path).expect("read the target"),
            candidate.to_string()
        );
    }

    /// A first save creates the file and reports no backup.
    #[test]
    fn saving_a_missing_override_target_creates_it_without_a_backup() {
        let dir = TempDir::new("save-create");
        let path = dir.join("fstab.draft");
        let target = override_target(path.clone());

        let outcome = save_document(
            &target,
            unused_real_fstab(),
            &DirectFs,
            &valid_candidate("/mnt/first"),
            false,
        )
        .expect("the save should succeed");

        assert!(outcome.backup.is_none());
        assert!(path.exists());
    }

    /// Validation is enforced by `write_document`, and the error DTO carries
    /// the report plus the "nothing changed" guarantee.
    #[test]
    fn saving_an_invalid_candidate_fails_and_leaves_the_target_untouched() {
        let dir = TempDir::new("save-invalid");
        let path = dir.join("fstab.draft");
        fs::write(&path, ORIGINAL).expect("seed the target");
        let target = override_target(path.clone());

        let error = save_document(
            &target,
            unused_real_fstab(),
            &DirectFs,
            &invalid_candidate(),
            false,
        )
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
        let target = override_target(dir.join("missing-dir/fstab"));

        let error = save_document(
            &target,
            unused_real_fstab(),
            &DirectFs,
            &valid_candidate("/mnt/nope"),
            false,
        )
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
        let path = dir.join("fstab.draft");
        fs::write(&path, ORIGINAL).expect("seed the target");
        let target = override_target(path.clone());

        // First save: creates a backup holding ORIGINAL.
        save_document(
            &target,
            unused_real_fstab(),
            &DirectFs,
            &valid_candidate("/mnt/second"),
            false,
        )
        .expect("first save");
        let first_backup = backups::list(&path).expect("listing")[0].path.clone();

        // Restore it the way the command does.
        let resolved =
            backups::resolve_requested(&path, &first_backup).expect("it is a real backup");
        let text = backups::read_text(&resolved).expect("readable");
        let outcome = save_document(
            &target,
            unused_real_fstab(),
            &DirectFs,
            &parser::parse(&text),
            false,
        )
        .expect("the restore succeeds");

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
        let path = dir.join("fstab.draft");
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
        let path = dir.join("fstab.draft");
        seed_backups(&path, 2);
        let target = override_target(path.clone());

        let listed = backups::list(&path).expect("listing must succeed");
        // Newest first: the newest backup holds what the *second* save
        // replaced, and the oldest holds the content the target started with.
        let oldest = &listed[listed.len() - 1];

        assert_eq!(
            read_backup_of(&target, unused_real_fstab(), colocated(), &oldest.path)
                .expect("a real backup must be readable"),
            ORIGINAL
        );
        assert_eq!(
            read_backup_of(&target, unused_real_fstab(), colocated(), &listed[0].path)
                .expect("a real backup must be readable"),
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
        let path = dir.join("fstab.draft");
        seed_backups(&path, 1);
        let target = override_target(path.clone());

        let other = dir.join("fstab.other");
        seed_backups(&other, 1);
        let other_backup = backups::list(&other).expect("listing")[0].path.clone();

        let near_misses = [
            "fstab.draft.rustab-backup.1.2",
            "fstab.draft.rustab-backup.0000000001.000000002.old",
            "fstab.draft.rustab-backup.aaaaaaaaaa.bbbbbbbbb",
            "fstab.draft.bak",
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
            let error = read_backup_of(&target, unused_real_fstab(), colocated(), requested)
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
        let path = dir.join("fstab.draft");
        fs::write(&path, ORIGINAL).expect("seed the target");
        let target = override_target(path.clone());
        // A hand-made but exactly-named backup, which `backups` deliberately
        // accepts as a backup of this target (see its module docs).
        let backup = dir.join("fstab.draft.rustab-backup.0000000001.000000002");
        fs::write(&backup, [0xff, 0xfe, 0x00]).expect("write invalid UTF-8");

        let error = read_backup_of(
            &target,
            unused_real_fstab(),
            colocated(),
            &backup.to_string_lossy(),
        )
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
        let path = dir.join("fstab.draft");
        fs::write(&path, ORIGINAL).expect("seed the target");
        let target = override_target(path.clone());

        let calls = std::cell::RefCell::new(Vec::new());
        open_backup_location_with(&target, unused_real_fstab(), colocated(), |opened| {
            calls.borrow_mut().push(opened.to_path_buf());
            Ok(())
        })
        .expect("the opener stand-in succeeds");

        let calls = calls.into_inner();
        assert_eq!(calls, vec![backups::containing_dir(&path)]);
    }

    /// A failure to launch the file manager (binary missing, permission
    /// denied, ...) is reported as `OpenLocation`, with the directory's own
    /// error folded into the message.
    #[test]
    fn open_backup_location_reports_a_launch_failure() {
        let dir = TempDir::new("open-backup-location-fail");
        let path = dir.join("fstab.draft");
        let target = override_target(path.clone());

        let error = open_backup_location_with(&target, unused_real_fstab(), colocated(), |_| {
            Err(io::Error::from(io::ErrorKind::NotFound))
        })
        .expect_err("a launch failure must be reported");

        assert_eq!(error.kind, CommandErrorKindDto::OpenLocation);
        let expected_dir = backups::containing_dir(&path);
        assert!(
            error.message.contains(&expected_dir.display().to_string()),
            "the message must name the directory that could not be opened: {error:?}"
        );
    }

    // -- open_path_in_file_manager -------------------------------------------

    /// The happy path: a fake opener captures the exact path it was given,
    /// with no resolution or re-derivation applied to it first - unlike
    /// `open_backup_location`, this command has no target to resolve.
    #[test]
    fn open_path_in_file_manager_opens_the_exact_path_it_was_given() {
        let path = PathBuf::from("/mnt/data");

        let calls = std::cell::RefCell::new(Vec::new());
        open_path_in_file_manager_with(&path, |opened| {
            calls.borrow_mut().push(opened.to_path_buf());
            Ok(())
        })
        .expect("the opener stand-in succeeds");

        let calls = calls.into_inner();
        assert_eq!(calls, vec![path]);
    }

    /// A failure to launch the file manager (binary missing, permission
    /// denied, ...) is reported as `OpenPath`, with the path's own error
    /// folded into the message.
    #[test]
    fn open_path_in_file_manager_reports_a_launch_failure() {
        let path = PathBuf::from("/mnt/data");

        let error = open_path_in_file_manager_with(&path, |_| {
            Err(io::Error::from(io::ErrorKind::NotFound))
        })
        .expect_err("a launch failure must be reported");

        assert_eq!(error.kind, CommandErrorKindDto::OpenPath);
        assert!(
            error.message.contains(&path.display().to_string()),
            "the message must name the path that could not be opened: {error:?}"
        );
    }

    // Octal-escape decoding itself is tested once, where it now lives:
    // `fstab_core::model::decode_escaped_whitespace`'s own test module.

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
        let path = dir.join("fstab.draft");
        seed_backups(&path, 3);
        let target = override_target(path.clone());
        let before = backups::list(&path).expect("listing");
        assert_eq!(before.len(), 3);
        let target_content = fs::read_to_string(&path).expect("read the target");

        let doomed = before[1].path.clone();
        delete_backup_from(&target, unused_real_fstab(), colocated(), &doomed)
            .expect("deleting a real backup must succeed");

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
        let path = dir.join("fstab.draft");
        seed_backups(&path, 1);
        let target = override_target(path.clone());
        let real_backup = backups::list(&path).expect("listing")[0].path.clone();

        for requested in [
            "/etc/shadow",
            "../../etc/fstab",
            "",
            &path.to_string_lossy(),
            &dir.join("fstab.draft.rustab-backup.1.2").to_string_lossy(),
        ] {
            let error = delete_backup_from(&target, unused_real_fstab(), colocated(), requested)
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
        let path = dir.join("fstab.draft");
        seed_backups(&path, 1);
        let target = override_target(path.clone());
        let backup = backups::list(&path).expect("listing")[0].path.clone();

        delete_backup_from(&target, unused_real_fstab(), colocated(), &backup)
            .expect("the first delete must succeed");
        let error = delete_backup_from(&target, unused_real_fstab(), colocated(), &backup)
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
        delete_backup_with(
            &target,
            unused_real_fstab(),
            colocated(),
            &wanted,
            |elevated_target, elevated_backup| {
                calls
                    .borrow_mut()
                    .push((elevated_target.to_path_buf(), elevated_backup.to_string()));
                Ok(())
            },
        )
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

    /// The other half of the dispatch guarantee: an **explicit, non-real
    /// override's** backup is removed in-process and must never escalate (no
    /// password prompt for a file in the user's own directory).
    ///
    /// Narrowed in meaning by T35, not in behaviour: this used to be the only
    /// non-`is_real` case there was, and so stood for the default launch too.
    /// Since T35 a default launch's backups are the real `/etc/fstab`'s and do
    /// elevate - see
    /// [`a_default_launch_delete_is_dispatched_to_the_elevated_deleter`] for
    /// that case. This test now covers exactly the `RUSTAB_TARGET_PATH`
    /// escape hatch, which is unchanged.
    #[test]
    fn an_explicit_non_real_override_delete_never_reaches_the_elevated_deleter() {
        let dir = TempDir::new("override-delete-dispatch");
        let path = dir.join("fstab.draft");
        seed_backups(&path, 1);
        let target = override_target(path.clone());
        let backup = backups::list(&path).expect("listing")[0].path.clone();

        delete_backup_with(
            &target,
            unused_real_fstab(),
            colocated(),
            &backup,
            |_, _| {
                panic!("an explicit non-real override's backup deletion must never escalate");
            },
        )
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

        let error = delete_backup_with(
            &target,
            unused_real_fstab(),
            colocated(),
            "/etc/shadow",
            |_, _| {
                panic!("a path that is not a backup must not reach the elevated deleter");
            },
        )
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
        let path = dir.join("fstab.draft");
        fs::write(&path, ORIGINAL).expect("seed the target");
        let target = override_target(path.clone());

        let info = create_backup_of(&target, unused_real_fstab(), &DirectFs, colocated())
            .expect("creating a backup must succeed");

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
        let path = dir.join("fstab.draft");
        fs::write(&path, ORIGINAL).expect("seed the target");
        let target = override_target(path.clone());

        let first = create_backup_of(&target, unused_real_fstab(), &DirectFs, colocated())
            .expect("first backup");
        fs::write(&path, "# changed by hand\n").expect("change the target");
        let second = create_backup_of(&target, unused_real_fstab(), &DirectFs, colocated())
            .expect("second backup");

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
        let path = dir.join("fstab.draft");
        let target = override_target(path.clone());

        let error = create_backup_of(&target, unused_real_fstab(), &DirectFs, colocated())
            .expect_err("there is nothing to back up");

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
        let path = dir.join("fstab.draft");
        fs::write(&path, ORIGINAL).expect("seed the target");
        let target = override_target(path.clone());

        let manual = create_backup_of(&target, unused_real_fstab(), &DirectFs, colocated())
            .expect("a manual backup");
        for i in 0..(writer::MAX_BACKUPS + 3) {
            save_document(
                &target,
                unused_real_fstab(),
                &DirectFs,
                &valid_candidate(&format!("/mnt/n{i}")),
                false,
            )
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
        let error = create_backup_with(
            &target,
            unused_real_fstab(),
            &DirectFs,
            colocated(),
            |elevated_target| {
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
            },
        )
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

    /// The other half of the dispatch guarantee: an **explicit, non-real
    /// override** is copied in-process and must never escalate (no password
    /// prompt for a file in the user's own directory).
    ///
    /// Narrowed in meaning by T35 exactly like
    /// [`an_explicit_non_real_override_delete_never_reaches_the_elevated_deleter`]:
    /// a default launch's manual backup now does elevate, because it is a
    /// backup of the real `/etc/fstab` - see
    /// [`a_default_launch_backup_is_dispatched_to_the_elevated_helper`].
    #[test]
    fn an_explicit_non_real_override_backup_never_reaches_the_elevated_helper() {
        let dir = TempDir::new("override-create-dispatch");
        let path = dir.join("fstab.draft");
        fs::write(&path, ORIGINAL).expect("seed the target");
        let target = override_target(path.clone());

        let info = create_backup_with(&target, unused_real_fstab(), &DirectFs, colocated(), |_| {
            panic!("an explicit non-real override's backup must never escalate");
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
        let path = dir.join("fstab.draft");
        fs::write(&path, ORIGINAL).expect("seed the target");
        let target = override_target(path.clone());

        let manual = create_backup_of(&target, unused_real_fstab(), &DirectFs, colocated())
            .expect("a manual backup");
        delete_backup_from(&target, unused_real_fstab(), colocated(), &manual.path)
            .expect("deleting a manual backup must succeed");

        assert!(!Path::new(&manual.path).exists());
        assert!(backups::list(&path).expect("listing").is_empty());
        assert!(path.exists(), "the target itself is untouched");
    }

    // -- T35: whose backups a default launch acts on ------------------------
    //
    // The whole T35 decision, asserted per command. Every test here uses a
    // `SandboxDefault` target (`default_draft_target`) plus a *stand-in* for
    // the real `/etc/fstab` passed as the `real_fstab` argument - which is
    // exactly why that is a parameter of these functions and not the
    // `target::REAL_FSTAB_PATH` constant inlined: these tests create, list and
    // unlink real backup files beside the stand-in, and must never read, write
    // or even list the machine's own `/etc/fstab`.

    /// The two-directory fixture every test in this section wants: a working
    /// draft and a stand-in for the real `/etc/fstab`, in **separate**
    /// directories, so "which directory did this command act on" is answerable
    /// by looking at the paths alone.
    ///
    /// Returns `(draft, real_stand_in)`. Both files exist; neither has backups
    /// yet.
    fn draft_and_real_stand_in(dir: &TempDir) -> (PathBuf, PathBuf) {
        let draft_dir = dir.join("share/rustab");
        let etc_dir = dir.join("etc");
        fs::create_dir_all(&draft_dir).expect("mkdir the draft's directory");
        fs::create_dir_all(&etc_dir).expect("mkdir the real file's directory");

        let draft = draft_dir.join("fstab.draft");
        let real = etc_dir.join("fstab");
        fs::write(&draft, ORIGINAL).expect("seed the draft");
        fs::write(&real, ORIGINAL).expect("seed the real stand-in");
        (draft, real)
    }

    /// The T35 headline for the read side: on a default launch the Backups
    /// panel lists the **real file's** backups, in the real file's directory -
    /// not the draft's own, which before T35 was the only thing it ever showed.
    ///
    /// Both files have backups here, with different counts, so a listing that
    /// looked at the wrong one could not pass by accident.
    #[test]
    fn a_default_launch_lists_the_real_fstab_s_backups_not_the_draft_s() {
        let dir = TempDir::new("default-list-backups");
        let (draft, real) = draft_and_real_stand_in(&dir);
        seed_backups(&draft, 1);
        seed_backups(&real, 3);
        let target = default_draft_target(draft.clone());

        let listed = list_backups_for(&target, &real, colocated()).expect("listing must succeed");

        assert_eq!(
            listed,
            backups::list(&real).expect("the real stand-in's own listing"),
            "a default launch must list exactly the real file's backups"
        );
        assert_eq!(listed.len(), 3);
        for info in &listed {
            assert_eq!(
                info.target_path,
                real.to_string_lossy(),
                "every row must name the real file as its target: {info:?}"
            );
        }
        assert_eq!(
            backups::list(&draft)
                .expect("the draft's own listing")
                .len(),
            1,
            "and the draft's own backup is still there, just not what is listed"
        );
    }

    /// An explicit override is unaffected: it lists its own backups, and the
    /// `real_fstab` argument is never consulted at all. The counterpart to the
    /// test above, so the two cases are pinned together.
    #[test]
    fn an_explicit_override_lists_its_own_backups_and_ignores_the_real_fstab() {
        let dir = TempDir::new("override-list-backups");
        let (draft, real) = draft_and_real_stand_in(&dir);
        seed_backups(&draft, 2);
        seed_backups(&real, 3);
        let target = override_target(draft.clone());

        let listed = list_backups_for(&target, &real, colocated()).expect("listing must succeed");

        assert_eq!(listed, backups::list(&draft).expect("the draft's listing"));
        assert_eq!(listed.len(), 2);
    }

    // -- resync_external_change (T37) ---------------------------------------
    //
    // `resync_external_change_with` never takes an `elevate` closure and
    // never calls into `crate::privileged` anywhere in its body (see its own
    // definition, a few hundred lines up) - unlike every write path in this
    // module, it needs no privilege escalation at all, which these tests
    // confirm by construction: none of them passes, or could pass, a
    // privileged stand-in, because the function being tested has no such
    // parameter to receive one.

    /// The marker path belonging to `sandbox_path`, for asserting what
    /// [`target::sync_sandbox_after_save`] did or did not do to it.
    fn marker_of(sandbox_path: &Path) -> PathBuf {
        target::last_known_real_marker(sandbox_path).expect("a named file has a marker sibling")
    }

    /// The headline case: a `SandboxDefault` target's resync publishes exactly
    /// the real file's current bytes onto both the draft and the marker,
    /// replacing whatever stale content either held before - byte-exact, which
    /// is what proves this is a copy and not a parse/reserialize round trip.
    #[test]
    fn a_sandbox_default_resync_publishes_the_real_fstabs_bytes_onto_draft_and_marker() {
        let dir = TempDir::new("resync-sandbox-default");
        let real = dir.join("real-fstab");
        fs::write(&real, REAL_FSTAB_FOR_RESYNC).expect("seed the real stand-in");
        let draft = dir.join("fstab.draft");
        fs::write(&draft, "# stale draft content from before the drift\n").expect("seed draft");
        fs::write(marker_of(&draft), "# stale marker content\n").expect("seed marker");
        let target = default_draft_target(draft.clone());

        let view = resync_external_change_with(&target, &real, &DirectFs, colocated())
            .expect("a sandbox-default resync must succeed");

        assert!(view.exists);
        assert_eq!(view.text, REAL_FSTAB_FOR_RESYNC);
        assert_eq!(
            fs::read(&draft).expect("read the draft"),
            REAL_FSTAB_FOR_RESYNC.as_bytes(),
            "the draft must become a byte-for-byte copy of the real file"
        );
        assert_eq!(
            fs::read(marker_of(&draft)).expect("read the marker"),
            REAL_FSTAB_FOR_RESYNC.as_bytes(),
            "the marker must be moved forward to the same bytes"
        );
    }

    /// Fail-closed guard, non-real-override half: an explicit
    /// `RUSTAB_TARGET_PATH` naming some other file must never have its draft
    /// silently replaced with the real `/etc/fstab`'s content - that target is
    /// not a mirror of the real file at all. Nothing is touched: the draft and
    /// the marker (even one set up in advance) are left exactly as they were.
    #[test]
    fn an_explicit_non_real_override_fails_closed_and_touches_nothing() {
        let dir = TempDir::new("resync-env-non-real");
        let real = dir.join("real-fstab");
        fs::write(&real, REAL_FSTAB_FOR_RESYNC).expect("seed the real stand-in");
        let draft = dir.join("fstab.custom");
        fs::write(&draft, "# this is not a mirror of /etc/fstab\n").expect("seed draft");
        fs::write(
            marker_of(&draft),
            "# a marker that should never be touched\n",
        )
        .expect("seed marker");
        let target = override_target(draft.clone());

        let error = resync_external_change_with(&target, &real, &DirectFs, colocated())
            .expect_err("a non-SandboxDefault target must be refused");

        assert_eq!(error.kind, CommandErrorKindDto::Resync);
        assert_eq!(
            fs::read_to_string(&draft).expect("read the draft"),
            "# this is not a mirror of /etc/fstab\n",
            "the draft must be left exactly as it was"
        );
        assert_eq!(
            fs::read_to_string(marker_of(&draft)).expect("read the marker"),
            "# a marker that should never be touched\n",
            "the marker must be left exactly as it was"
        );
    }

    /// Fail-closed guard, explicit-real-override half: `RUSTAB_TARGET_PATH`
    /// pointed straight at the real `/etc/fstab` (`is_real: true`) is still
    /// `TargetSource::Environment`, not `SandboxDefault`, and must be refused
    /// for the identical reason - this command's authority is "the default
    /// working draft", not "any target that happens to be real".
    #[test]
    fn an_explicit_real_override_fails_closed_and_touches_nothing() {
        let dir = TempDir::new("resync-env-real");
        let real = dir.join("real-fstab");
        fs::write(&real, REAL_FSTAB_FOR_RESYNC).expect("seed the real stand-in");
        let draft = dir.join("fstab");
        fs::write(&draft, "# the real file, named explicitly\n").expect("seed draft");
        fs::write(
            marker_of(&draft),
            "# a marker that should never be touched\n",
        )
        .expect("seed marker");
        let target = ActiveTarget {
            path: draft.clone(),
            is_real: true,
            source: TargetSource::Environment,
        };

        let error = resync_external_change_with(&target, &real, &DirectFs, colocated())
            .expect_err("an is_real: true, Environment-sourced target must still be refused");

        assert_eq!(error.kind, CommandErrorKindDto::Resync);
        assert_eq!(
            fs::read_to_string(&draft).expect("read the draft"),
            "# the real file, named explicitly\n",
            "the draft must be left exactly as it was"
        );
        assert_eq!(
            fs::read_to_string(marker_of(&draft)).expect("read the marker"),
            "# a marker that should never be touched\n",
            "the marker must be left exactly as it was"
        );
    }

    /// A missing real file is not an error - the same non-error handling
    /// `list_entries` already gives a missing target - and it resyncs the
    /// draft and marker to empty, not to whatever stale content they held.
    #[test]
    fn a_missing_real_fstab_resyncs_to_empty_rather_than_erroring() {
        let dir = TempDir::new("resync-missing-real");
        let real = dir.join("this-file-does-not-exist");
        let draft = dir.join("fstab.draft");
        fs::write(&draft, "# stale content that must be cleared\n").expect("seed draft");
        fs::write(marker_of(&draft), "# a stale marker\n").expect("seed marker");
        let target = default_draft_target(draft.clone());

        let view = resync_external_change_with(&target, &real, &DirectFs, colocated())
            .expect("a missing real file must not be an error");

        assert!(!view.exists);
        assert_eq!(view.text, "");
        assert_eq!(
            fs::read(&draft).expect("read the draft"),
            b"",
            "a missing real file must clear the draft, not leave it stale"
        );
        assert_eq!(
            fs::read(marker_of(&draft)).expect("read the marker"),
            b"",
            "and clear the marker the same way"
        );
    }

    /// T40, and the reason the host reader's `NotFound` mapping has to be
    /// exact: a *transport* failure (`flatpak-spawn` missing, the
    /// `org.freedesktop.Flatpak` portal denied, the host read killed) must not
    /// be mistaken for "the real `/etc/fstab` does not exist", because this
    /// command's answer to the latter is to **blank the draft and the
    /// marker**. The two cases are asserted side by side here so neither can
    /// quietly start behaving like the other.
    #[test]
    fn a_host_read_failure_is_not_treated_as_a_missing_real_fstab() {
        let dir = TempDir::new("resync-host-read-failure");
        let draft = dir.join("fstab.draft");
        fs::write(&draft, "# content that must survive a transport failure\n").expect("seed draft");
        fs::write(marker_of(&draft), "# a marker that must survive\n").expect("seed marker");
        let target = default_draft_target(draft.clone());
        let unreachable = StubReader::failing(
            io::ErrorKind::Other,
            "Portal call failed: org.freedesktop.DBus.Error.ServiceUnknown",
        );

        let error = resync_external_change_with(&target, real_fstab(), &unreachable, colocated())
            .expect_err("an unreachable host read must be reported, not silently accepted");

        assert_eq!(error.kind, CommandErrorKindDto::ReadTarget);
        assert!(error.message.contains("Portal call failed"));
        assert_eq!(
            fs::read_to_string(&draft).expect("read the draft"),
            "# content that must survive a transport failure\n",
            "a failed read must leave the draft exactly as it was"
        );
        assert_eq!(
            fs::read_to_string(marker_of(&draft)).expect("read the marker"),
            "# a marker that must survive\n"
        );
        // The only path it asked for is the real file - never the draft.
        assert_eq!(unreachable.asked(), vec![real_fstab().to_path_buf()]);

        // The genuinely-missing case still clears both, through the same seam.
        let gone = StubReader::failing(io::ErrorKind::NotFound, "no such file");
        let view = resync_external_change_with(&target, real_fstab(), &gone, colocated())
            .expect("a missing real file must not be an error");
        assert!(!view.exists);
        assert_eq!(fs::read(&draft).expect("read the draft"), b"");
    }

    /// T40 - and a defect T39 shipped: fallback mode's `create_backup` copies
    /// the **real** `/etc/fstab` onto the mirror before backing it up, and in
    /// the sandbox that is the one case where the file being copied is on the
    /// other side of the sandbox boundary. With the pre-T40 plain `fs::read`
    /// that copy always failed with `NotFound`, so a sandboxed install's
    /// "Create backup" button could only ever report "`/etc/fstab` does not
    /// exist yet, so there is nothing to back up" - the single operation
    /// fallback mode exists for. The real-file path here does not exist on
    /// disk, so a successful backup holding the reader's bytes proves the
    /// mirror refresh goes through the seam.
    #[test]
    fn a_fallback_create_backup_copies_the_real_file_through_the_host_reader() {
        let dir = TempDir::new("create-fallback-host-read");
        let draft = dir.join("fstab.draft");
        fs::write(&draft, "# the draft, which is not what gets backed up\n").expect("seed");
        let fallback = dir.join("data/rustab/etc-fstab-backups");
        let location = fallback_to(&fallback);
        let target = default_draft_target(draft);
        let unreachable_real = dir.join("not-on-this-disk/fstab");
        let real_content = "# the real /etc/fstab, reachable only via the host\n";
        let reader = StubReader::serving(real_content.as_bytes());

        let info = create_backup_with(&target, &unreachable_real, &reader, &location, |_| {
            panic!("a fallback-mode backup is the user's own file and must not escalate")
        })
        .expect("a fallback-mode backup must succeed when the host read works");

        assert!(info.is_manual);
        assert_eq!(
            fs::read_to_string(&info.path).expect("read the backup"),
            real_content,
            "the backup must hold the real file's bytes as served by the reader"
        );
        assert!(
            Path::new(&info.path).starts_with(&fallback),
            "the backup must be in the fallback directory: {}",
            info.path
        );
        assert_eq!(reader.asked(), vec![unreachable_real]);
    }

    /// The other half of the same fix: when the host read reports the real
    /// file as genuinely missing, the answer is still "nothing to back up" -
    /// so the pre-T40 message was not *wrong*, it was simply the only answer
    /// that case could ever produce.
    #[test]
    fn a_fallback_create_backup_still_reports_a_genuinely_missing_real_file() {
        let dir = TempDir::new("create-fallback-missing-real");
        let draft = dir.join("fstab.draft");
        fs::write(&draft, "# a draft\n").expect("seed");
        let location = fallback_to(&dir.join("data/rustab/etc-fstab-backups"));
        let target = default_draft_target(draft);
        let gone = StubReader::failing(io::ErrorKind::NotFound, "no such file");

        let error = create_backup_with(&target, real_fstab(), &gone, &location, |_| {
            panic!("nothing may escalate")
        })
        .expect_err("a missing real file must be reported");

        assert_eq!(error.kind, CreateBackupErrorKindDto::TargetMissing);
    }

    /// A non-UTF-8 real file is refused outright, exactly as `list_entries`
    /// refuses one for its own target - and, because nothing was read
    /// successfully, nothing is published: the draft and the marker are left
    /// exactly as they were.
    #[test]
    fn a_non_utf8_real_fstab_is_refused_and_touches_nothing() {
        let dir = TempDir::new("resync-not-utf8");
        let real = dir.join("real-fstab");
        fs::write(&real, [0xff, 0xfe, 0x00, 0x01]).expect("seed invalid-UTF-8 bytes");
        let draft = dir.join("fstab.draft");
        fs::write(&draft, "# valid content that must survive\n").expect("seed draft");
        fs::write(marker_of(&draft), "# a marker that must survive\n").expect("seed marker");
        let target = default_draft_target(draft.clone());

        let error = resync_external_change_with(&target, &real, &DirectFs, colocated())
            .expect_err("non-UTF-8 content must be refused");

        assert_eq!(error.kind, CommandErrorKindDto::NotUtf8);
        assert_eq!(
            fs::read_to_string(&draft).expect("read the draft"),
            "# valid content that must survive\n",
            "the draft must be left exactly as it was"
        );
        assert_eq!(
            fs::read_to_string(marker_of(&draft)).expect("read the marker"),
            "# a marker that must survive\n",
            "the marker must be left exactly as it was"
        );
    }

    /// The test that proves this feature actually closes the loop a plain
    /// retry depends on, not just that bytes landed somewhere: after a
    /// successful resync, comparing the (now moved-forward) marker against the
    /// same real content the resync just read reports no external change at
    /// all - the exact guard a subsequent `save`/`restore_backup` consults.
    #[test]
    fn a_successful_resync_makes_the_external_change_guard_report_no_drift() {
        let dir = TempDir::new("resync-closes-the-loop");
        let real = dir.join("real-fstab");
        fs::write(&real, REAL_FSTAB_FOR_RESYNC).expect("seed the real stand-in");
        let draft = dir.join("fstab.draft");
        fs::write(&draft, "# a draft that disagrees with the real file\n").expect("seed draft");
        let target = default_draft_target(draft.clone());

        resync_external_change_with(&target, &real, &DirectFs, colocated())
            .expect("the resync must succeed");

        let guard = target::ExternalChangeGuard::from_marker(&target.path, false);
        assert_eq!(
            guard.detect(&real),
            None,
            "the marker the resync just recorded must agree with the real file it was read from"
        );
    }

    /// `read_backup` follows the same list: on a default launch a backup of
    /// the real file is readable, and a backup of the *draft* - which is not in
    /// the list `list_backups` offered - is refused before being used as a path
    /// at all.
    #[test]
    fn a_default_launch_reads_the_real_fstab_s_backups_and_refuses_the_draft_s() {
        let dir = TempDir::new("default-read-backup");
        let (draft, real) = draft_and_real_stand_in(&dir);
        seed_backups(&draft, 1);
        seed_backups(&real, 1);
        let target = default_draft_target(draft.clone());

        let real_backup = backups::list(&real).expect("listing")[0].path.clone();
        assert_eq!(
            read_backup_of(&target, &real, colocated(), &real_backup)
                .expect("a real backup must be readable"),
            ORIGINAL,
            "the real file's backup holds the content it had before its save"
        );

        let draft_backup = backups::list(&draft).expect("listing")[0].path.clone();
        let error = read_backup_of(&target, &real, colocated(), &draft_backup)
            .expect_err("the draft's backups are not this session's backups any more");
        assert_eq!(error.kind, CommandErrorKindDto::InvalidBackup);
    }

    /// `open_backup_location` opens the directory the backups it just listed
    /// are actually in - the real file's, for a default launch. Without this
    /// the Backups panel's "open folder" button would point at a directory
    /// holding none of the rows on screen.
    #[test]
    fn a_default_launch_opens_the_real_fstab_s_directory() {
        let dir = TempDir::new("default-open-location");
        let (draft, real) = draft_and_real_stand_in(&dir);
        let target = default_draft_target(draft.clone());

        let calls = std::cell::RefCell::new(Vec::new());
        open_backup_location_with(&target, &real, colocated(), |opened| {
            calls.borrow_mut().push(opened.to_path_buf());
            Ok(())
        })
        .expect("the opener stand-in succeeds");

        let calls = calls.into_inner();
        assert_eq!(
            calls,
            vec![backups::containing_dir(&real)],
            "a default launch must open the real file's directory, not the draft's"
        );
        assert_ne!(
            backups::containing_dir(&real),
            backups::containing_dir(&draft),
            "the fixture itself must keep the two directories distinct"
        );
    }

    /// The T35 headline for the write side of the create path: a default
    /// launch's manual backup goes to the **elevated** helper, aimed at the
    /// real file, and nothing is copied by this process - because the backup
    /// has to be created in root-owned `/etc/`.
    ///
    /// The counterpart of
    /// [`a_real_target_backup_is_dispatched_to_the_elevated_helper_and_never_copied_in_process`],
    /// which before T35 was the only case that elevated.
    #[test]
    fn a_default_launch_backup_is_dispatched_to_the_elevated_helper() {
        let dir = TempDir::new("default-create-dispatch");
        let (draft, real) = draft_and_real_stand_in(&dir);
        let target = default_draft_target(draft.clone());

        // The stand-in reports a plausible backup it did not create, the same
        // way the real-override test does, which also pins that an unverifiable
        // outcome is refused rather than turned into an invented row.
        let calls = std::cell::RefCell::new(Vec::new());
        let error = create_backup_with(&target, &real, &DirectFs, colocated(), |elevated_target| {
            calls.borrow_mut().push(elevated_target.to_path_buf());
            Ok(CreateBackupOutcomeDto {
                target: elevated_target.to_string_lossy().into_owned(),
                backup: format!(
                    "{}.rustab-backup.0000000001.000000002.manual",
                    real.display()
                ),
                pruned_backups: Vec::new(),
                notes: Vec::new(),
            })
        })
        .expect_err("a backup that is not on disk must not be reported as a row");
        assert_eq!(error.kind, CreateBackupErrorKindDto::Backup);

        let calls = calls.into_inner();
        assert_eq!(
            calls,
            vec![real.clone()],
            "exactly one elevated call, aimed at the real file and not at the draft"
        );
        assert!(
            backups::list(&real).expect("listing").is_empty(),
            "the in-process copier must not have touched the real file"
        );
        assert!(
            backups::list(&draft).expect("listing").is_empty(),
            "and must not have backed up the draft instead"
        );
    }

    /// The T35 headline for the write side of the delete path: a default
    /// launch's delete goes to the **elevated** deleter, with the real file as
    /// the target and one of *its* backups as the doomed path, and nothing is
    /// unlinked by this process.
    #[test]
    fn a_default_launch_delete_is_dispatched_to_the_elevated_deleter() {
        let dir = TempDir::new("default-delete-dispatch");
        let (draft, real) = draft_and_real_stand_in(&dir);
        seed_backups(&draft, 1);
        seed_backups(&real, 2);
        let target = default_draft_target(draft.clone());
        let wanted = backups::list(&real).expect("listing")[0].path.clone();

        let calls = std::cell::RefCell::new(Vec::new());
        delete_backup_with(
            &target,
            &real,
            colocated(),
            &wanted,
            |elevated_target, elevated_backup| {
                calls
                    .borrow_mut()
                    .push((elevated_target.to_path_buf(), elevated_backup.to_string()));
                Ok(())
            },
        )
        .expect("the elevated stand-in succeeds");

        let calls = calls.into_inner();
        assert_eq!(calls.len(), 1, "exactly one elevated call must be made");
        assert_eq!(
            calls[0].0, real,
            "the elevated call must get the real file, not the draft"
        );
        assert_eq!(
            calls[0].1, wanted,
            "the elevated call must get the resolved backup path"
        );
        assert_eq!(
            backups::list(&real).expect("listing").len(),
            2,
            "the in-process deleter must not have touched the real file's backups"
        );
        assert_eq!(
            backups::list(&draft).expect("listing").len(),
            1,
            "and must not have unlinked something beside the draft instead"
        );
    }

    /// A default launch's delete refuses a backup of the *draft*: it is not in
    /// the list `list_backups` offered, so it is refused before the dispatch
    /// and never costs the user a password prompt.
    #[test]
    fn a_default_launch_delete_refuses_a_backup_of_the_draft() {
        let dir = TempDir::new("default-delete-refuse");
        let (draft, real) = draft_and_real_stand_in(&dir);
        seed_backups(&draft, 1);
        seed_backups(&real, 1);
        let target = default_draft_target(draft.clone());
        let draft_backup = backups::list(&draft).expect("listing")[0].path.clone();

        let error = delete_backup_with(&target, &real, colocated(), &draft_backup, |_, _| {
            panic!("a path that is not one of the real file's backups must not escalate");
        })
        .expect_err("the draft's backups are not this session's backups any more");

        assert_eq!(error.kind, DeleteBackupErrorKindDto::InvalidBackup);
        assert!(
            Path::new(&draft_backup).exists(),
            "and a refused delete removes nothing"
        );
    }

    // -- T39: where the real file's manual backups live ---------------------
    //
    // Every test here uses the same `draft_and_real_stand_in` fixture as the
    // T35 section above, plus a third directory standing in for the fallback
    // one, and passes `fallback_to(...)` where the T35 tests pass
    // `colocated()`. The capability probe itself
    // (`crate::backup_location::detect`) is tested in its own module, against
    // its own temp directories; what is asserted *here* is what each command
    // does once the answer is known - which is the half that could silently
    // elevate, or act on the wrong directory.

    /// The fallback directory for these tests, in a third directory distinct
    /// from both the draft's and the real file's, so "which directory did this
    /// command act on" is answerable from the paths alone.
    ///
    /// Created up front because that is what production does the moment it
    /// selects fallback mode (see `crate::backup_location::detect`), and
    /// listing a directory that does not exist is an error rather than an
    /// empty list.
    fn fallback_dir_in(dir: &TempDir) -> PathBuf {
        let fallback = dir.join("share/rustab/etc-fstab-backups");
        fs::create_dir_all(&fallback).expect("mkdir the fallback directory");
        fallback
    }

    /// The headline: in fallback mode an explicitly created backup of the real
    /// `/etc/fstab` lands in the fallback directory, holds the real file's
    /// current content, is named exactly as it would have been in `/etc/`, and
    /// is created **without the elevated helper being called at all** - the
    /// directory is the user's own.
    #[test]
    fn a_fallback_launch_creates_a_manual_backup_in_the_fallback_directory_without_elevating() {
        let dir = TempDir::new("fallback-create");
        let (draft, real) = draft_and_real_stand_in(&dir);
        let fallback = fallback_dir_in(&dir);
        let target = default_draft_target(draft.clone());

        let info = create_backup_with(&target, &real, &DirectFs, &fallback_to(&fallback), |_| {
            panic!("a backup in the user's own directory must never ask for a password")
        })
        .expect("creating a backup in the fallback directory must succeed");

        let created = PathBuf::from(&info.path);
        assert_eq!(
            created.parent(),
            Some(fallback.as_path()),
            "the backup must be in the fallback directory: {info:?}"
        );
        assert_eq!(
            fs::read_to_string(&created).expect("read the backup"),
            ORIGINAL,
            "the backup must hold the real file's content, not the draft's"
        );
        assert!(info.is_manual);
        assert!(
            info.file_name.starts_with("fstab.rustab-backup.")
                && info.file_name.ends_with(".manual"),
            "the name must be the one this backup would have had in /etc/: {info:?}"
        );
        // The real file and the draft are both left exactly as they were: no
        // backup beside either, and neither file touched.
        assert!(backups::list(&real).expect("listing").is_empty());
        assert!(backups::list(&draft).expect("listing").is_empty());
        assert_eq!(fs::read_to_string(&real).expect("read"), ORIGINAL);
        assert_eq!(fs::read_to_string(&draft).expect("read"), ORIGINAL);
    }

    /// The backup is a snapshot of the real file **as of the moment the user
    /// asked for it**, not of whatever the mirror happened to hold from a
    /// previous run - which is the one thing the mirror refresh exists for.
    #[test]
    fn a_fallback_launch_backs_up_the_real_files_content_as_of_now() {
        let dir = TempDir::new("fallback-fresh");
        let (draft, real) = draft_and_real_stand_in(&dir);
        let fallback = fallback_dir_in(&dir);
        let location = fallback_to(&fallback);
        let target = default_draft_target(draft.clone());
        let never_elevate = |_: &Path| -> Result<CreateBackupOutcomeDto, CreateBackupErrorDto> {
            panic!("fallback mode must never elevate")
        };

        let first = create_backup_with(&target, &real, &DirectFs, &location, never_elevate)
            .expect("the first backup must succeed");

        // Something else - a save through the elevated writer, say - changes
        // the real file between the two button presses.
        let changed = "# the real file moved on\nLABEL=new /mnt/new ext4 noauto 0 2\n";
        fs::write(&real, changed).expect("change the real stand-in");

        let second = create_backup_with(&target, &real, &DirectFs, &location, never_elevate)
            .expect("the second backup must succeed");

        assert_eq!(
            fs::read_to_string(&first.path).expect("read the first backup"),
            ORIGINAL
        );
        assert_eq!(
            fs::read_to_string(&second.path).expect("read the second backup"),
            changed,
            "the second backup must hold what the real file holds now"
        );
        assert_eq!(
            fs::read_to_string(backup_location::mirror_path(&fallback)).expect("read the mirror"),
            changed,
            "and the mirror must have been refreshed before the copy"
        );
    }

    /// A real file that does not exist yet is reported as "nothing to back up"
    /// in fallback mode too, with the same kind
    /// `fstab_core::writer::create_backup` reports for a missing target of its
    /// own - rather than quietly backing up a stale mirror.
    #[test]
    fn a_fallback_launch_reports_a_missing_real_file_as_nothing_to_back_up() {
        let dir = TempDir::new("fallback-create-missing");
        let (draft, real) = draft_and_real_stand_in(&dir);
        let fallback = fallback_dir_in(&dir);
        fs::write(backup_location::mirror_path(&fallback), "# stale\n").expect("stale mirror");
        fs::remove_file(&real).expect("the real file is gone");
        let target = default_draft_target(draft);

        let error = create_backup_with(&target, &real, &DirectFs, &fallback_to(&fallback), |_| {
            panic!("fallback mode must never elevate")
        })
        .expect_err("there is nothing to back up");

        assert_eq!(error.kind, CreateBackupErrorKindDto::TargetMissing);
        assert!(
            backups::list(&backup_location::mirror_path(&fallback))
                .expect("listing")
                .is_empty(),
            "a refused create must not have copied the stale mirror"
        );
    }

    /// Listing in fallback mode reports exactly the fallback directory's
    /// backups - and, the accepted trade-off T39 makes, **not** the automatic
    /// backups the elevated writer leaves beside the real `/etc/fstab`, which
    /// this process cannot enumerate. Asserted explicitly, because it is a
    /// deliberate consequence the user is told about rather than a bug.
    #[test]
    fn a_fallback_launch_lists_the_fallback_directorys_backups_and_not_the_real_files() {
        let dir = TempDir::new("fallback-list");
        let (draft, real) = draft_and_real_stand_in(&dir);
        let fallback = fallback_dir_in(&dir);
        let location = fallback_to(&fallback);
        let target = default_draft_target(draft.clone());
        // Two automatic backups beside the real file (what saves leave there)
        // and one backup beside the draft, none of which may be listed.
        seed_backups(&real, 2);
        seed_backups(&draft, 1);

        assert!(
            list_backups_for(&target, &real, &location)
                .expect("listing an empty fallback directory must succeed")
                .is_empty(),
            "nothing has been created in the fallback directory yet"
        );

        let created = create_backup_with(&target, &real, &DirectFs, &location, |_| {
            panic!("fallback mode must never elevate")
        })
        .expect("creating a backup must succeed");

        let listed = list_backups_for(&target, &real, &location).expect("listing must succeed");
        assert_eq!(listed, vec![created.clone()]);
        assert_eq!(
            listed[0].path, created.path,
            "the row must be the backup that was just created"
        );
        // And the ones that exist elsewhere are still there, just invisible.
        assert_eq!(backups::list(&real).expect("listing").len(), 2);
        assert_eq!(backups::list(&draft).expect("listing").len(), 1);
    }

    /// Deleting in fallback mode unlinks the file in-process, with no
    /// `pkexec` prompt - and refuses anything that is not in the list
    /// `list_backups` offered, including one of the real file's own backups,
    /// before any dispatch.
    #[test]
    fn a_fallback_launch_deletes_a_fallback_backup_in_process_and_refuses_the_real_files() {
        let dir = TempDir::new("fallback-delete");
        let (draft, real) = draft_and_real_stand_in(&dir);
        let fallback = fallback_dir_in(&dir);
        let location = fallback_to(&fallback);
        let target = default_draft_target(draft.clone());
        seed_backups(&real, 1);
        let real_backup = backups::list(&real).expect("listing")[0].path.clone();

        let created = create_backup_with(&target, &real, &DirectFs, &location, |_| {
            panic!("fallback mode must never elevate")
        })
        .expect("creating a backup must succeed");

        // One of the real file's backups is not one of this session's any
        // more: refused, nothing unlinked, nothing escalated.
        let error = delete_backup_with(&target, &real, &location, &real_backup, |_, _| {
            panic!("a path outside the fallback directory must not escalate")
        })
        .expect_err("only the fallback directory's backups are this session's");
        assert_eq!(error.kind, DeleteBackupErrorKindDto::InvalidBackup);
        assert!(Path::new(&real_backup).exists());

        delete_backup_with(&target, &real, &location, &created.path, |_, _| {
            panic!("a backup in the user's own directory must never ask for a password")
        })
        .expect("deleting a fallback backup must succeed");

        assert!(!Path::new(&created.path).exists());
        assert!(list_backups_for(&target, &real, &location)
            .expect("listing")
            .is_empty());
        assert!(
            backup_location::mirror_path(&fallback).exists(),
            "the mirror itself is not a backup and must not be deleted"
        );
    }

    /// The three operations above are the *only* ones that stop elevating.
    /// A restore still writes the real `/etc/fstab`, so it still goes through
    /// the single elevated write - the backup it restores just comes from the
    /// fallback directory now. This is the test that would fail if fallback
    /// mode had been allowed to leak into the write path.
    #[test]
    fn a_fallback_launch_still_elevates_a_restore_of_a_fallback_backup() {
        let dir = TempDir::new("fallback-restore");
        let (draft, real) = draft_and_real_stand_in(&dir);
        let fallback = fallback_dir_in(&dir);
        let location = fallback_to(&fallback);
        let target = default_draft_target(draft.clone());

        let created = create_backup_with(&target, &real, &DirectFs, &location, |_| {
            panic!("creating must not elevate")
        })
        .expect("creating a backup must succeed");

        let calls = std::cell::RefCell::new(Vec::new());
        let outcome = restore_backup_with(
            &target,
            &real,
            &DirectFs,
            &location,
            &created.path,
            false,
            recording_elevate(&calls),
        )
        .expect("the restore must succeed");

        let calls = calls.into_inner();
        assert_eq!(calls.len(), 1, "exactly one elevated write");
        assert_eq!(
            calls[0].0, real,
            "the elevated write must be aimed at the real file"
        );
        assert_eq!(
            calls[0].1,
            parser::parse(ORIGINAL),
            "and must carry the backup's content"
        );
        assert!(outcome.elevated);
    }

    /// `open_backup_location` opens the directory whose backups were just
    /// listed - the fallback one, not `/etc/`, which in fallback mode holds
    /// none of the rows on screen (and which the file manager could not show
    /// the user's own backups in anyway).
    #[test]
    fn a_fallback_launch_opens_the_fallback_directory() {
        let dir = TempDir::new("fallback-open");
        let (draft, real) = draft_and_real_stand_in(&dir);
        let fallback = fallback_dir_in(&dir);
        let target = default_draft_target(draft);

        let calls = std::cell::RefCell::new(Vec::new());
        open_backup_location_with(&target, &real, &fallback_to(&fallback), |opened| {
            calls.borrow_mut().push(opened.to_path_buf());
            Ok(())
        })
        .expect("the opener stand-in succeeds");

        assert_eq!(calls.into_inner(), vec![fallback]);
    }

    /// `read_backup` follows the same list as everything else: a backup in the
    /// fallback directory is readable, one beside the real file is refused
    /// before being used as a path.
    #[test]
    fn a_fallback_launch_reads_the_fallback_directorys_backups_only() {
        let dir = TempDir::new("fallback-read");
        let (draft, real) = draft_and_real_stand_in(&dir);
        let fallback = fallback_dir_in(&dir);
        let location = fallback_to(&fallback);
        let target = default_draft_target(draft);
        seed_backups(&real, 1);
        let real_backup = backups::list(&real).expect("listing")[0].path.clone();
        // `seed_backups` saves, so this - not `ORIGINAL` - is what the real
        // file holds by the time the backup below is taken.
        let real_now = fs::read_to_string(&real).expect("read the real stand-in");

        let created = create_backup_with(&target, &real, &DirectFs, &location, |_| {
            panic!("fallback mode must never elevate")
        })
        .expect("creating a backup must succeed");

        assert_eq!(
            read_backup_of(&target, &real, &location, &created.path)
                .expect("a fallback backup must be readable"),
            real_now
        );
        let error = read_backup_of(&target, &real, &location, &real_backup)
            .expect_err("a backup beside the real file is not in this session's list");
        assert_eq!(error.kind, CommandErrorKindDto::InvalidBackup);
    }

    /// An explicit `RUSTAB_TARGET_PATH=/etc/fstab` is the second half of
    /// "these are the real file's backups" (`is_real`), so it gets the same
    /// fallback treatment as a default launch: same directory, same
    /// in-process create.
    #[test]
    fn an_explicit_real_override_also_uses_the_fallback_directory() {
        let dir = TempDir::new("fallback-real-override");
        let (_draft, real) = draft_and_real_stand_in(&dir);
        let fallback = fallback_dir_in(&dir);
        let location = fallback_to(&fallback);
        // What `active_target()` produces for `RUSTAB_TARGET_PATH=/etc/fstab`,
        // with the stand-in standing in for that path.
        let target = ActiveTarget {
            path: real.clone(),
            is_real: true,
            source: TargetSource::Environment,
        };

        assert_eq!(
            backup_target_path(&target, unused_real_fstab(), &location),
            backup_location::mirror_path(&fallback)
        );
        assert!(!backups_are_elevated(&target, &location));

        let created =
            create_backup_with(&target, unused_real_fstab(), &DirectFs, &location, |_| {
                panic!("fallback mode must never elevate")
            })
            .expect("creating a backup must succeed");
        assert_eq!(
            PathBuf::from(&created.path).parent(),
            Some(fallback.as_path())
        );
        assert_eq!(
            fs::read_to_string(&created.path).expect("read the backup"),
            ORIGINAL,
            "the backup must be a copy of the named real file"
        );
    }

    /// The other side of that line, and the one the T39 design promises
    /// outright: an explicit override naming some **other** file never
    /// consults the backup location at all.
    ///
    /// Proved rather than assumed: the location passed in is a fallback one
    /// pointing at a directory that does not exist and must never be created.
    /// If any of these operations consulted it, they would act in (and create)
    /// that directory instead of the target's own - so the final assertion
    /// that it is still absent is the "never consulted" claim itself, not just
    /// "still passes".
    #[test]
    fn an_explicit_override_never_consults_the_backup_location() {
        let dir = TempDir::new("override-ignores-location");
        let path = dir.join("custom/fstab.custom");
        fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
        fs::write(&path, ORIGINAL).expect("seed the target");
        let target = override_target(path.clone());
        let poison = dir.join("must-never-be-touched");
        let location = fallback_to(&poison);

        // The two decisions, directly.
        assert_eq!(
            backup_target_path(&target, unused_real_fstab(), &location),
            path
        );
        assert!(!backups_are_elevated(&target, &location));
        assert_eq!(
            describe_target(&target, &location).real_target_backup_fallback_dir,
            None,
            "and the frontend is told there is no fallback in play"
        );

        // And the three operations end to end, through the production
        // wrappers, acting on the target's own directory throughout.
        let created = create_backup_of(&target, unused_real_fstab(), &DirectFs, &location)
            .expect("creating a backup must succeed");
        assert_eq!(PathBuf::from(&created.path).parent(), path.parent());
        assert_eq!(
            list_backups_for(&target, unused_real_fstab(), &location).expect("listing"),
            vec![created.clone()]
        );
        delete_backup_from(&target, unused_real_fstab(), &location, &created.path)
            .expect("deleting must succeed");
        assert!(list_backups_for(&target, unused_real_fstab(), &location)
            .expect("listing")
            .is_empty());

        assert!(
            !poison.exists(),
            "a non-real override must not have looked at the fallback location at all"
        );
    }

    /// What the frontend is told, in both modes, for the default launch - the
    /// contract `docs/UI_DESIGN.md` §7's notice is built from.
    #[test]
    fn target_info_reports_the_fallback_directory_only_when_one_is_in_use() {
        let dir = TempDir::new("fallback-target-info");
        let (draft, _real) = draft_and_real_stand_in(&dir);
        let fallback = fallback_dir_in(&dir);
        let target = default_draft_target(draft);

        assert_eq!(
            describe_target(&target, colocated()).real_target_backup_fallback_dir,
            None,
            "a colocated launch must report no fallback"
        );
        assert_eq!(
            describe_target(&target, &fallback_to(&fallback))
                .real_target_backup_fallback_dir
                .as_deref(),
            Some(fallback.to_string_lossy().as_ref()),
            "a fallback launch must report the directory its backups are in"
        );
    }

    // -- read-side commands -------------------------------------------------

    /// `list_entries`'s parsing and DTO shape, exercised through the same
    /// code the command runs (the command itself only adds target resolution
    /// and the file read).
    #[test]
    fn parsed_lines_keep_every_line_in_order_including_malformed_ones() {
        // `# header` is the entry's own description, so it is carried *on*
        // the entry rather than as a `comment` line; the comment above the
        // malformed line is not claimed by anything and stays a line of its
        // own (see `fstab-core`'s parser docs).
        let text = "# header\n\
                    UUID=11111111-2222-3333-4444-555555555555 /mnt/data ext4 noauto 0 2\n\
                    \n\
                    # the next line cannot be parsed\n\
                    /dev/sdb1\n";
        let document = parser::parse(text);
        let dto = DocumentDto::from(&document);

        assert_eq!(dto.lines.len(), 4);
        assert!(matches!(dto.lines[1], LineDto::Blank));
        assert!(matches!(dto.lines[2], LineDto::Comment { .. }));
        let LineDto::Entry { entry } = &dto.lines[0] else {
            panic!("the first line must be the entry, with its description");
        };
        assert_eq!(entry.description.as_deref(), Some("header"));
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
        let path = dir.join("fstab.draft");
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
        let info = describe_target(
            &override_target(PathBuf::from("/tmp/rustab/fstab.draft")),
            colocated(),
        );
        assert_eq!(info.path, "/tmp/rustab/fstab.draft");
        assert!(!info.is_real);
        assert_eq!(info.source, TargetSourceDto::Environment);
        assert_eq!(info.real_target_backup_fallback_dir, None);

        let real = describe_target(
            &ActiveTarget {
                path: PathBuf::from(target::REAL_FSTAB_PATH),
                is_real: true,
                source: TargetSource::Environment,
            },
            colocated(),
        );
        assert!(real.is_real);
        assert_eq!(real.real_target_backup_fallback_dir, None);
    }

    // -- T34 save dispatch ---------------------------------------------------

    /// Case 3 of [`save_document_with`]: an explicit `RUSTAB_TARGET_PATH`
    /// naming `/etc/fstab` itself goes to the elevated writer, is **never**
    /// written by this process - and, the T34 decision this pins, gets
    /// **no draft mirror and no marker**: the one path the user named is the
    /// only file touched. Asserted with a recording stand-in for the elevated
    /// half, so the test neither launches `pkexec` nor waits on an
    /// authentication prompt.
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
        let outcome = save_document_with(
            &target,
            unused_real_fstab(),
            &DirectFs,
            &candidate,
            false,
            recording_elevate(&calls),
        )
        .expect("the elevated stand-in succeeds");

        assert!(outcome.elevated);
        assert_eq!(
            outcome.draft_sync_warning, None,
            "an explicit override has no draft to warn about"
        );
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
        assert!(
            !target::last_known_real_marker(&path)
                .expect("a named path has a marker sibling")
                .exists(),
            "an explicit real override must not start keeping a sync marker"
        );
    }

    /// Case 2 of [`save_document_with`], unchanged by T34 and load-bearing
    /// for the whole test suite: an explicit non-`/etc/fstab` override is
    /// written in-process and must never escalate (no password prompt, no
    /// `pkexec`, nothing mirrored anywhere). This branch is why `cargo test`
    /// and CI can run a real save at all.
    #[test]
    fn an_explicit_non_real_override_never_reaches_the_elevated_writer() {
        let dir = TempDir::new("override-dispatch");
        let path = dir.join("fstab.custom");
        let target = override_target(path.clone());

        let outcome = save_document_with(
            &target,
            unused_real_fstab(),
            &DirectFs,
            &valid_candidate("/mnt/custom"),
            false,
            |_, _, _| panic!("an explicit non-real override must never escalate"),
        )
        .expect("the save should succeed");

        assert!(!outcome.elevated);
        assert_eq!(outcome.draft_sync_warning, None);
        assert!(path.exists());
        assert_eq!(
            fs::read_dir(dir.path())
                .expect("the directory must be readable")
                .count(),
            1,
            "no marker, no mirror, no second file of any kind"
        );
    }

    /// Case 1, the T34 headline: a **default** launch's save always writes
    /// the real `/etc/fstab` through the elevated path, even though the
    /// resolved target is the draft and `is_real` is `false`. The elevated
    /// call must be aimed at the real file, never at the draft.
    ///
    /// Since T36 the real file's path is the injected `real_fstab` argument
    /// rather than the inlined constant, because the drift check genuinely
    /// reads it (see [`save_document_with`]); this test therefore passes a
    /// stand-in and asserts the call went to *that*, exactly as the T35
    /// backup tests above do. That production passes
    /// [`target::REAL_FSTAB_PATH`] is a one-line delegation in [`save`] and
    /// [`restore_backup`], the same arrangement [`backup_target_path`]
    /// documents.
    #[test]
    fn a_default_launch_save_always_elevates_the_real_fstab() {
        let dir = TempDir::new("default-save-elevates");
        let (draft, real) = draft_and_real_stand_in(&dir);
        let target = default_draft_target(draft.clone());
        let candidate = valid_candidate("/mnt/for-real");

        let calls = std::cell::RefCell::new(Vec::new());
        let outcome = save_document_with(
            &target,
            &real,
            &DirectFs,
            &candidate,
            false,
            recording_elevate(&calls),
        )
        .expect("the elevated stand-in succeeds");

        let calls = calls.into_inner();
        assert_eq!(calls.len(), 1, "exactly one elevated call must be made");
        assert_eq!(
            calls[0].0, real,
            "the elevated call must be aimed at the real file, not at the draft"
        );
        assert_eq!(
            calls[0].1, candidate,
            "the elevated call must get the candidate unchanged"
        );
        assert!(outcome.elevated);
        assert_eq!(
            outcome.target,
            real.to_string_lossy(),
            "the reported outcome is the real write's, which is the authoritative one"
        );
    }

    /// The other half of case 1: after the real write succeeds, the draft is
    /// updated to exactly the bytes that went into the real file, and the
    /// marker is moved with it so the next launch does not read this save as
    /// external drift.
    #[test]
    fn a_default_launch_save_mirrors_into_the_draft_and_moves_the_marker() {
        let dir = TempDir::new("default-save-mirrors");
        let (draft, real) = draft_and_real_stand_in(&dir);
        let target = default_draft_target(draft.clone());
        let candidate = valid_candidate("/mnt/mirrored");

        let calls = std::cell::RefCell::new(Vec::new());
        let outcome = save_document_with(
            &target,
            &real,
            &DirectFs,
            &candidate,
            false,
            recording_elevate(&calls),
        )
        .expect("the elevated stand-in succeeds");

        assert_eq!(outcome.draft_sync_warning, None, "{outcome:?}");
        assert_eq!(
            fs::read_to_string(&draft).expect("read the draft"),
            candidate.to_string(),
            "the draft must hold exactly what the real file was given"
        );
        assert_eq!(
            fs::read_to_string(
                target::last_known_real_marker(&draft).expect("a marker sibling exists")
            )
            .expect("read the marker"),
            candidate.to_string(),
            "the marker must record the just-saved content as the known reality"
        );
    }

    /// The mirror is a plain publish of bytes, not a second save: it must not
    /// create a backup chain of its own beside the draft. Backups of the file
    /// that was actually written are made by the elevated writer, beside
    /// `/etc/fstab`.
    #[test]
    fn the_draft_mirror_does_not_create_its_own_backups() {
        let dir = TempDir::new("default-save-no-draft-backups");
        let (draft, real) = draft_and_real_stand_in(&dir);
        let target = default_draft_target(draft.clone());

        let calls = std::cell::RefCell::new(Vec::new());
        for i in 0..3 {
            // Every save after the first moves the marker, so the stand-in is
            // kept in step with it - these are ordinary saves, not drift.
            let candidate = valid_candidate(&format!("/mnt/n{i}"));
            save_document_with(
                &target,
                &real,
                &DirectFs,
                &candidate,
                false,
                recording_elevate(&calls),
            )
            .expect("the elevated stand-in succeeds");
            fs::write(&real, candidate.to_string()).expect("the elevated write, re-enacted");
        }

        assert!(
            backups::list(&draft).expect("listing").is_empty(),
            "mirroring must not back up the draft on every save"
        );
    }

    /// A failed real write is authoritative: the save fails, and nothing is
    /// mirrored - the draft must never show content the real file does not
    /// have.
    #[test]
    fn a_failed_elevated_write_fails_the_save_and_mirrors_nothing() {
        let dir = TempDir::new("default-save-elevate-fails");
        let (draft, real) = draft_and_real_stand_in(&dir);
        let target = default_draft_target(draft.clone());

        let error = save_document_with(
            &target,
            &real,
            &DirectFs,
            &valid_candidate("/mnt/nope"),
            false,
            |_, _, _| {
                Err(WriteErrorDto::new(
                    WriteErrorKindDto::PrivilegeEscalation,
                    true,
                    "the user dismissed the password prompt; nothing was written",
                ))
            },
        )
        .expect_err("a refused escalation must fail the save");

        assert_eq!(error.kind, WriteErrorKindDto::PrivilegeEscalation);
        assert!(error.target_unchanged);
        assert_eq!(
            fs::read_to_string(&draft).expect("read the draft"),
            ORIGINAL,
            "a failed save must leave the draft exactly as it was"
        );
        assert!(
            !target::last_known_real_marker(&draft)
                .expect("a marker sibling exists")
                .exists(),
            "and must not claim to know anything new about the real file"
        );
    }

    /// A draft that cannot be updated is **not** a failed save: the real file
    /// already holds the new content, which is what the user asked for. It is
    /// reported as a distinguishable non-fatal warning instead, so the UI can
    /// toast it rather than silently losing sync.
    ///
    /// The mirror is made to fail by putting a directory where the draft file
    /// belongs - renaming a file onto a directory fails on any Unix, and
    /// unlike a permission trick it behaves the same whether or not the suite
    /// runs as root.
    #[test]
    fn a_draft_mirror_failure_is_a_warning_not_a_failed_save() {
        let dir = TempDir::new("default-save-mirror-fails");
        let draft = dir.join("fstab.draft");
        fs::create_dir_all(&draft).expect("mkdir in the draft's place");
        let real = dir.join("etc-fstab-stand-in");
        fs::write(&real, ORIGINAL).expect("seed the real stand-in");
        let target = default_draft_target(draft.clone());

        let calls = std::cell::RefCell::new(Vec::new());
        let outcome = save_document_with(
            &target,
            &real,
            &DirectFs,
            &valid_candidate("/mnt/warned"),
            false,
            recording_elevate(&calls),
        )
        .expect("a draft-sync failure must not fail the save");

        assert_eq!(calls.into_inner().len(), 1, "the real write still happened");
        let warning = outcome
            .draft_sync_warning
            .expect("the draft failure must be surfaced, not swallowed");
        assert!(
            warning.contains(&real.display().to_string())
                && warning.contains(&draft.display().to_string()),
            "the warning must name both files so the user knows which one is fine: {warning}"
        );
        assert!(
            !target::last_known_real_marker(&draft)
                .expect("a marker sibling exists")
                .exists(),
            "a marker written despite a failed mirror would keep the stale draft forever"
        );
    }

    /// Asserts, through the production [`restore_backup_with`], that
    /// backup resolution and the dual write agree on which file is real:
    ///
    /// - **Resolution:** `backup_path` resolves against the *real file's*
    ///   backup list, the only list `list_backups` offers a default
    ///   launch - the draft has a different backup, asserted refused below.
    /// - **Write:** the restore delegates to [`save_document_with`] with
    ///   no write code of its own, so it inherits the dual write: one
    ///   elevated write of the real `/etc/fstab`, then the draft mirrored
    ///   to match, marker included.
    ///
    /// The elevate seam is injected, so this neither launches `pkexec`
    /// nor touches the machine's own `/etc/fstab`; production's own call
    /// site passes [`target::REAL_FSTAB_PATH`] (see [`restore_backup`]).
    #[test]
    fn restoring_a_backup_resolves_against_the_real_fstab_and_inherits_the_dual_write() {
        let dir = TempDir::new("restore-dual-write");
        let (draft, real) = draft_and_real_stand_in(&dir);
        // A real backup of each file, made the way a pre-restore save would.
        // The two hold different content, so restoring the wrong one could not
        // pass by accident.
        writer::write_document(&real, &valid_candidate("/mnt/newer")).expect("seed a real backup");
        fs::write(&draft, "# the draft drifted\n").expect("change the draft");
        writer::write_document(&draft, &valid_candidate("/mnt/drift"))
            .expect("seed a draft backup");
        let real_backup = backups::list(&real).expect("listing")[0].path.clone();
        let draft_backup = backups::list(&draft).expect("listing")[0].path.clone();
        let target = default_draft_target(draft.clone());

        let calls = std::cell::RefCell::new(Vec::new());
        let outcome = restore_backup_with(
            &target,
            &real,
            &DirectFs,
            colocated(),
            &real_backup,
            false,
            recording_elevate(&calls),
        )
        .expect("the restore must succeed");

        let calls = calls.into_inner();
        assert_eq!(calls.len(), 1, "a restore must elevate exactly once");
        assert_eq!(
            calls[0].0, real,
            "a restore must write the real file, like any other save"
        );
        assert_eq!(
            calls[0].1,
            parser::parse(ORIGINAL),
            "and must write the content of the real file's backup, verbatim"
        );
        assert_eq!(outcome.draft_sync_warning, None, "{outcome:?}");
        assert_eq!(
            fs::read_to_string(&draft).expect("read the draft"),
            ORIGINAL,
            "and the draft must be mirrored to the restored content too"
        );
        assert_eq!(
            fs::read_to_string(
                target::last_known_real_marker(&draft).expect("a marker sibling exists")
            )
            .expect("read the marker"),
            ORIGINAL,
            "marker included - a restore is a save and moves the sync point"
        );

        // The other half of T35's resolution change: a backup of the *draft*
        // is not one of this session's backups any more, and is refused
        // before anything is written.
        let error = restore_backup_with(
            &target,
            &real,
            &DirectFs,
            colocated(),
            &draft_backup,
            false,
            |_, _, _| {
                panic!("a path that is not one of the real file's backups must not be written");
            },
        )
        .expect_err("the draft's backups are not this session's restore points");
        assert_eq!(error.kind, WriteErrorKindDto::InvalidBackup);
        assert!(error.target_unchanged);
    }

    // -- T36: an external change to the real file, before a write -----------
    //
    // Every test here uses a `SandboxDefault` target plus a stand-in for the
    // real `/etc/fstab`, because the check reads that file (see
    // `save_document_with`'s docs on why `real_fstab` became a parameter).
    // The elevate seam stays injected, so a "proceeds" case still writes
    // nothing outside the temp directory and a "blocked" case can assert the
    // elevated half was never even reached.

    /// Records `content` as what Rustab last knew the real file to hold -
    /// written straight into the marker `target::last_known_real_marker`
    /// names, which is what a refresh or a previous save would have left
    /// there.
    fn seed_marker(draft: &Path, content: &str) {
        fs::write(
            target::last_known_real_marker(draft).expect("a draft has a marker sibling"),
            content,
        )
        .expect("seed the marker");
    }

    /// The regression half: nothing changed externally, so the save behaves
    /// exactly as it did before T36 - one elevated call, mirror and marker
    /// moved - and the expectation the elevated side will re-check is threaded
    /// through to it (that re-check, not this one, is the authoritative one;
    /// see `privileged::handle_write`).
    #[test]
    fn a_save_proceeds_untouched_when_the_real_fstab_has_not_changed() {
        let dir = TempDir::new("t36-no-drift");
        let (draft, real) = draft_and_real_stand_in(&dir);
        seed_marker(&draft, ORIGINAL);
        let target = default_draft_target(draft.clone());
        let candidate = valid_candidate("/mnt/undisturbed");

        let calls = std::cell::RefCell::new(Vec::new());
        let outcome = save_document_with(
            &target,
            &real,
            &DirectFs,
            &candidate,
            false,
            recording_elevate(&calls),
        )
        .expect("an unchanged real file must not block a save");

        let calls = calls.into_inner();
        assert_eq!(calls.len(), 1, "exactly one elevated call must be made");
        assert_eq!(
            calls[0].2.last_known_real_content.as_deref(),
            Some(ORIGINAL),
            "the elevated side must be told what to re-check against"
        );
        assert!(
            !calls[0].2.allow_external_change,
            "a normal save must not carry a write-anyway override"
        );
        assert_eq!(outcome.draft_sync_warning, None, "{outcome:?}");
        assert_eq!(
            fs::read_to_string(&draft).expect("read the draft"),
            candidate.to_string()
        );
    }

    /// Unsaved edits are not drift: the draft differs from both the marker and
    /// the real file, which is the normal state of a session with pending
    /// changes, and must not be mistaken for someone else having written
    /// `/etc/fstab`.
    #[test]
    fn an_unsaved_draft_edit_is_not_mistaken_for_an_external_change() {
        let dir = TempDir::new("t36-draft-edits");
        let (draft, real) = draft_and_real_stand_in(&dir);
        seed_marker(&draft, ORIGINAL);
        fs::write(&draft, "# the user has been typing\n").expect("edit the draft");
        let target = default_draft_target(draft.clone());

        let calls = std::cell::RefCell::new(Vec::new());
        save_document_with(
            &target,
            &real,
            &DirectFs,
            &valid_candidate("/mnt/pending"),
            false,
            recording_elevate(&calls),
        )
        .expect("a drifted draft must not block its own save");

        assert_eq!(calls.into_inner().len(), 1);
    }

    /// The headline: the real file no longer holds what Rustab last knew, so
    /// the save is refused before anything is written - the elevated half is
    /// never reached (no password prompt), the real file keeps the external
    /// content, and the draft and marker are left exactly as they were. The
    /// error carries both sides of the diff.
    #[test]
    fn a_save_is_blocked_when_the_real_fstab_changed_externally() {
        let dir = TempDir::new("t36-save-blocked");
        let (draft, real) = draft_and_real_stand_in(&dir);
        seed_marker(&draft, ORIGINAL);
        let external = "# someone else edited /etc/fstab while Rustab was open\n";
        fs::write(&real, external).expect("let the real file drift");
        let target = default_draft_target(draft.clone());

        let error = save_document_with(
            &target,
            &real,
            &DirectFs,
            &valid_candidate("/mnt/blocked"),
            false,
            |_, _, _| panic!("a detected external change must not reach the elevated writer"),
        )
        .expect_err("an external change must block the save");

        assert_eq!(error.kind, WriteErrorKindDto::ExternalChange);
        assert!(
            error.target_unchanged,
            "nothing was written, so this is a guarantee"
        );
        assert_eq!(error.path.as_deref(), Some(real.to_string_lossy().as_ref()));
        let change = error
            .external_change
            .as_deref()
            .expect("the diff payload must be attached");
        assert_eq!(
            change.last_known_content, ORIGINAL,
            "the old side of the diff is the marker's content"
        );
        assert_eq!(
            change.current_content, external,
            "the new side is what the file holds right now"
        );
        assert_eq!(
            fs::read(&real).expect("read the real stand-in"),
            external.as_bytes(),
            "the external content must still be on disk, byte for byte"
        );
        assert_eq!(
            fs::read_to_string(&draft).expect("read the draft"),
            ORIGINAL,
            "a blocked save must not touch the draft either"
        );
        assert_eq!(
            fs::read_to_string(
                target::last_known_real_marker(&draft).expect("a marker sibling exists")
            )
            .expect("read the marker"),
            ORIGINAL,
            "and must not move the sync point it just refused to act on"
        );
    }

    /// The same drift blocks a **restore**, not just a save - the point of
    /// landing the check in `save_document_with`, which both commands go
    /// through. Asserted through the production [`restore_backup_with`], and
    /// then its write-anyway half, so neither is left to inference.
    #[test]
    fn a_restore_is_blocked_by_an_external_change_exactly_like_a_save() {
        let dir = TempDir::new("t36-restore-blocked");
        let (draft, real) = draft_and_real_stand_in(&dir);
        // A real backup of the real stand-in, the way a previous save made it.
        writer::write_document(&real, &valid_candidate("/mnt/newer")).expect("seed a real backup");
        let backup = backups::list(&real).expect("listing")[0].path.clone();
        // What Rustab last knew, and what the file holds now: different.
        seed_marker(&draft, ORIGINAL);
        let external = "# changed between opening the Backups panel and clicking Restore\n";
        fs::write(&real, external).expect("let the real file drift");
        let target = default_draft_target(draft.clone());

        let error = restore_backup_with(
            &target,
            &real,
            &DirectFs,
            colocated(),
            &backup,
            false,
            |_, _, _| panic!("a detected external change must not reach the elevated writer"),
        )
        .expect_err("an external change must block the restore");

        assert_eq!(error.kind, WriteErrorKindDto::ExternalChange);
        assert!(error.target_unchanged);
        let change = error
            .external_change
            .as_deref()
            .expect("the diff payload must be attached");
        assert_eq!(change.last_known_content, ORIGINAL);
        assert_eq!(change.current_content, external);
        assert_eq!(
            fs::read(&real).expect("read the real stand-in"),
            external.as_bytes(),
            "nothing may reach the real file"
        );

        // And the user's second way forward works for a restore too.
        let calls = std::cell::RefCell::new(Vec::new());
        restore_backup_with(
            &target,
            &real,
            &DirectFs,
            colocated(),
            &backup,
            true,
            recording_elevate(&calls),
        )
        .expect("an explicit write-anyway must let the restore through");
        let calls = calls.into_inner();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].2.allow_external_change);
        assert_eq!(
            fs::read_to_string(&draft).expect("read the draft"),
            ORIGINAL,
            "a restore that went through is an ordinary save: the draft follows it"
        );
    }

    /// "Write anyway": the user has seen the diff and asked for the write, so
    /// the save proceeds and its outcome is indistinguishable from any other
    /// successful T34 save - same single elevated call, same mirror, same
    /// marker move. The override is also *passed on* to the elevated side,
    /// which would otherwise refuse the write on its own re-check.
    #[test]
    fn writing_anyway_proceeds_despite_an_external_change_and_resyncs_the_draft() {
        let dir = TempDir::new("t36-write-anyway");
        let (draft, real) = draft_and_real_stand_in(&dir);
        seed_marker(&draft, ORIGINAL);
        fs::write(&real, "# someone else's content\n").expect("let the real file drift");
        let target = default_draft_target(draft.clone());
        let candidate = valid_candidate("/mnt/anyway");

        let calls = std::cell::RefCell::new(Vec::new());
        let outcome = save_document_with(
            &target,
            &real,
            &DirectFs,
            &candidate,
            true,
            recording_elevate(&calls),
        )
        .expect("an explicit write-anyway must let the save through");

        let calls = calls.into_inner();
        assert_eq!(calls.len(), 1, "exactly one elevated call, as always");
        assert_eq!(calls[0].0, real);
        assert_eq!(calls[0].1, candidate);
        assert!(
            calls[0].2.allow_external_change,
            "the elevated side must be told the user already confirmed, or it refuses too"
        );
        assert_eq!(
            calls[0].2.last_known_real_content.as_deref(),
            Some(ORIGINAL),
            "the expectation still travels, so a report of what happened stays honest"
        );
        assert_eq!(outcome.draft_sync_warning, None, "{outcome:?}");
        assert_eq!(
            fs::read_to_string(&draft).expect("read the draft"),
            candidate.to_string(),
            "the draft must be mirrored exactly like any other successful save"
        );
        assert_eq!(
            fs::read_to_string(
                target::last_known_real_marker(&draft).expect("a marker sibling exists")
            )
            .expect("read the marker"),
            candidate.to_string(),
            "and the marker must move with it, so the next save starts from the truth"
        );
    }

    /// The recorded scope boundary (`PROJECT_PLAN.md` §T36): an explicit
    /// `RUSTAB_TARGET_PATH=/etc/fstab` keeps no marker, so there is nothing to
    /// compare and this case is deliberately left exactly as T34 left it -
    /// even when a marker-shaped sibling happens to exist beside the named
    /// path. Pinned so the gap stays a decision rather than becoming a
    /// surprise.
    #[test]
    fn an_explicit_real_override_is_outside_the_external_change_check() {
        let dir = TempDir::new("t36-override-unchecked");
        let path = dir.join("fstab");
        fs::write(&path, "# content nobody recorded\n").expect("seed the target");
        seed_marker(&path, ORIGINAL);
        let target = ActiveTarget {
            path: path.clone(),
            is_real: true,
            source: TargetSource::Environment,
        };

        let calls = std::cell::RefCell::new(Vec::new());
        save_document_with(
            &target,
            unused_real_fstab(),
            &DirectFs,
            &valid_candidate("/mnt/override"),
            false,
            recording_elevate(&calls),
        )
        .expect("case 3 must be unaffected by T36");

        let calls = calls.into_inner();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].2,
            target::ExternalChangeGuard::unchecked(),
            "case 3 must send no expectation and no override of its own"
        );
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
