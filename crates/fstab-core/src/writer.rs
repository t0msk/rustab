//! Atomic write/backup/rename logic - see `docs/SPEC.md` §6.
//!
//! Implements the project's core safety requirement (`docs/SPEC.md` §2.1):
//! a save is **all-or-nothing**. The target always ends up with either its
//! complete old content or its complete new content, never a mixture,
//! regardless of where a failure (I/O error, full disk, process death) occurs.
//!
//! [`write_document`] is the primary entry point ([`create_backup`] is the
//! other, sharing this module's backup machinery but never touching the
//! target). In order:
//!
//! 1. **Validate in memory** via [`crate::validate::validate`]. A blocking
//!    error returns [`WriteError::Validation`] before any write syscall.
//!    (That step's one external check, `findmnt --verify`, runs in this
//!    process unless the caller uses [`write_document_with_runner`] to say
//!    otherwise - see [`crate::exec`].)
//! 2. **Resolve the effective target.** A symlinked target (e.g.
//!    etckeeper/nix-managed `/etc/fstab`) is followed, and the write
//!    applies to its destination - replacing the symlink itself would
//!    leave the real file holding stale content.
//! 3. **Back up.** If the effective target exists, copy it to
//!    `<target>.rustab-backup.<secs>.<nanos>` and `fsync` the copy before
//!    anything else touches the target, then prune to [`MAX_BACKUPS`]
//!    *automatic* backups (see "two kinds of backup" below). Skipped on a
//!    first-ever save (nothing to back up).
//! 4. **Write a temp file** in the target's own directory, named
//!    `.<target-file-name>.rustab.tmp.<pid>`.
//! 5. **`fsync` temp → `fsync` directory → `rename` onto target → `fsync`
//!    directory again.**
//! 6. **Re-read and verify** the target byte-for-byte; a mismatch is a
//!    hard [`WriteError::Verify`], never just a warning.
//! 7. **Clean up the temp file** on every failure path between steps 4-5,
//!    best-effort, never masking the original error.
//!
//! ## Why the ordering is what it is
//!
//! - **Temp file in the target's own directory**: `rename(2)` is only
//!   atomic within one filesystem - a `/tmp` temp file could hit `EXDEV`,
//!   and copy-then-truncate would reopen the partial-write window this
//!   module exists to close. (The read-only `findmnt --verify` check does
//!   use a throwaway `/tmp` file, fine since it never renames onto a real target.)
//! - **`fsync` the temp file before the rename**: otherwise a crash could
//!   leave the target name pointing at an inode whose data was never
//!   written - a renamed but empty/garbage file.
//! - **The directory is `fsync`ed twice**: the pre-rename `fsync` makes
//!   the temp file's directory entry durable before the rename; a
//!   directory `fsync` only covers changes made *before* it, so making
//!   the rename itself durable needs a second `fsync` after.
//! - **Backup first, and `fsync`ed**: taking it before the temp write
//!   means a backup failure aborts with the target untouched. Without an
//!   `fsync`, a crash right after a successful rename could leave the new
//!   content durable and the backup gone - the one state with no way back.
//! - **Target mode/owner carried over**: a brand-new temp-file inode
//!   doesn't inherit them, and the process umask default could silently
//!   turn `0644 root:root /etc/fstab` into `0600`, breaking unprivileged
//!   readers. The temp file is created `0600`, then `fchmod`/`fchown`ed to
//!   match the old target. A failed `fchown` is a non-fatal note, not an
//!   abort - refusing to save over an ownership mismatch is worse than
//!   saving with the writing user's ownership.
//!
//! ## The two kinds of backup
//!
//! **Automatic** (a save's side effect, step 3) vs. **manual** (the user
//! explicitly snapshotting now, via [`create_backup`]) - distinguished
//! only by file name:
//!
//! - automatic: `<target-file-name>.rustab-backup.<secs>.<nanos>`
//! - manual: `<target-file-name>.rustab-backup.<secs>.<nanos>.manual`
//!
//! [`backup_name_of`] accepts only these two shapes, so [`prune_backups`]
//! never touches a file it didn't create. The file name is the only
//! record of which kind a backup is (no sidecar/database, consistent with
//! `src-tauri/src/backups.rs` treating the name as identity).
//!
//! **Manual backups are exempt from pruning**: [`MAX_BACKUPS`] bounds only
//! automatic backups, so a manual snapshot is never evicted by later saves,
//! and manual snapshots never push automatic ones out either. Only
//! explicit user deletion (`delete_backup` in `src-tauri`) removes a
//! manual backup.
//!
//! ## Testability seam
//!
//! Everything above runs through a crate-private `FileSystemOps` trait;
//! the production impl (`RealFs`) wraps `std::fs` one syscall at a time.
//! Unit tests use a delegating spy that performs every operation for real
//! against a temp directory and replaces exactly one with an injected
//! `io::Error`, so failure-injection tests assert "the target is
//! byte-for-byte untouched" against a real file, not an in-memory model.

use crate::exec::{CommandRunner, DirectRunner};
use crate::model::Document;
use crate::validate::{self, Finding, ValidationReport};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// How many **automatic** timestamped backups are kept per target file.
///
/// After each backup, automatic backups beyond this bound are deleted
/// oldest-first (`docs/SPEC.md` §6 step 5). Manual backups are exempt (see
/// module docs and [`prune_backups`]). Tune here; nothing else encodes this.
pub const MAX_BACKUPS: usize = 10;

/// The fixed infix separating a target's file name from a backup's timestamp,
/// i.e. `fstab` + `.rustab-backup.` + `<secs>.<nanos>`.
const BACKUP_INFIX: &str = ".rustab-backup.";

/// The one optional trailing literal suffix that marks a backup as manual,
/// i.e. `fstab.rustab-backup.<secs>.<nanos>` + `.manual`.
///
/// Matched exactly - no other spelling, casing or repetition of it is
/// recognized as anything but an unrelated file (see [`backup_name_of`]).
const MANUAL_SUFFIX: &str = ".manual";

/// Zero-padded width of the seconds component of a backup timestamp. Fixed
/// width keeps backup names sortable as plain text in a file manager, in
/// addition to the numeric sort [`list_backups`] performs.
const BACKUP_SECS_DIGITS: usize = 10;

/// Zero-padded width of the nanoseconds component of a backup timestamp.
const BACKUP_NANOS_DIGITS: usize = 9;

/// Permission bits used for a target that does not exist yet (first-ever
/// save), matching the conventional mode of `/etc/fstab`.
const DEFAULT_NEW_FILE_MODE: u32 = 0o644;

/// How many times a fresh timestamp is tried when a backup name is already
/// taken. Nanosecond resolution makes a single collision vanishingly
/// unlikely, let alone this many in a row.
const MAX_BACKUP_NAME_ATTEMPTS: usize = 64;

/// Which `fsync` in the sequence failed, so a caller can tell "the new
/// content never became visible" apart from "the new content is live but its
/// durability could not be confirmed".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsyncStage {
    /// Syncing the temp file's own data, before it was renamed into place.
    /// The target still holds its original content.
    TempFile,
    /// Syncing the target's directory before the rename. The target still
    /// holds its original content.
    DirectoryBeforeRename,
    /// Syncing the target's directory *after* a successful rename. The new
    /// content **is** already live and visible under the target's name; only
    /// its durability across a power loss is unconfirmed. Reported as an
    /// error rather than silently ignored so a caller never claims a
    /// confirmed-durable save it cannot actually vouch for.
    DirectoryAfterRename,
}

impl fmt::Display for FsyncStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            FsyncStage::TempFile => "temporary file (before rename)",
            FsyncStage::DirectoryBeforeRename => "containing directory (before rename)",
            FsyncStage::DirectoryAfterRename => "containing directory (after rename)",
        };
        f.write_str(text)
    }
}

/// Everything that can go wrong in [`write_document`], one variant per
/// failure point, so a caller (the Tauri command layer) can tell the user
/// precisely what happened and whether the target changed.
///
/// Except [`WriteError::Verify`] and [`FsyncStage::DirectoryAfterRename`]
/// (both only possible after the rename already succeeded), every variant
/// guarantees the target still holds its original content, byte for byte.
#[derive(Debug)]
pub enum WriteError {
    /// The candidate document failed structural or semantic validation
    /// (`docs/SPEC.md` §6 steps 1-2). **Nothing was written, created, or
    /// opened for writing**; the report carries every finding, including
    /// non-blocking warnings, exactly as [`crate::validate::validate`]
    /// produced it.
    Validation(ValidationReport),

    /// The target path cannot be written to for a structural reason
    /// discovered before any write: it has no final file-name component, it
    /// is a directory, or it is a symlink that could not be resolved. The
    /// target is untouched.
    InvalidTarget {
        /// The path as given to [`write_document`].
        path: PathBuf,
        /// Why it cannot be used as a target.
        reason: String,
    },

    /// Inspecting the existing target, copying it to its timestamped backup,
    /// or `fsync`ing that copy failed. The target is untouched and no temp
    /// file was created yet. Pruning *old* backups is deliberately not part
    /// of this variant - see [`WriteOutcome::notes`].
    Backup {
        /// The path being inspected or written when the failure occurred.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },

    /// Creating or writing the same-directory temp file failed (e.g. the
    /// filesystem is full). The target is untouched; the temp file, which may
    /// hold a partial write, was removed on a best-effort basis.
    Write {
        /// The temp file path.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },

    /// Applying the previous target's permission bits to the temp file
    /// failed. The target is untouched and the temp file was removed on a
    /// best-effort basis. Failing to `fchown` is *not* reported here; it is a
    /// non-fatal note (see the module docs).
    Metadata {
        /// The temp file path.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },

    /// An `fsync` failed. Check [`FsyncStage`] to know whether the rename had
    /// already happened: for [`FsyncStage::TempFile`] and
    /// [`FsyncStage::DirectoryBeforeRename`] the target is untouched, for
    /// [`FsyncStage::DirectoryAfterRename`] the new content is live but not
    /// confirmed durable.
    Fsync {
        /// Which `fsync` in the sequence failed.
        stage: FsyncStage,
        /// The file or directory being synced.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },

    /// The atomic `rename` of the temp file onto the target failed. This is
    /// the all-or-nothing hinge: because `rename(2)` within one filesystem
    /// either completes or does nothing, the target still holds its original
    /// content, byte for byte. The temp file was removed on a best-effort
    /// basis.
    Rename {
        /// The temp file that was to be renamed.
        from: PathBuf,
        /// The target it was to replace.
        to: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },

    /// The target could not be read back for post-rename verification. The
    /// rename itself succeeded, so the new content is live; it simply could
    /// not be confirmed.
    ReadBack {
        /// The target path.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },

    /// The target was read back successfully after the rename but did not
    /// match the bytes that were written (`docs/SPEC.md` §6 step 7). This
    /// should be unreachable - it would mean the kernel, the filesystem, or
    /// another process interfered - and is deliberately a hard error rather
    /// than a warning, so no caller ever reports a verified save it did not
    /// verify. `backup` is carried along because this is the one failure
    /// where the caller may want to offer a restore.
    Verify {
        /// The target path whose content did not match.
        target: PathBuf,
        /// The backup holding the pre-save content, if one was made.
        backup: Option<PathBuf>,
        /// The content that was written.
        expected: String,
        /// The content that was read back.
        actual: String,
    },
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WriteError::Validation(report) => {
                write!(
                    f,
                    "the proposed fstab content failed validation; nothing was written"
                )?;
                for finding in report.errors() {
                    write!(f, "\n  - {}", finding.message)?;
                }
                Ok(())
            }
            WriteError::InvalidTarget { path, reason } => write!(
                f,
                "cannot write to {}: {reason}; nothing was written",
                path.display()
            ),
            WriteError::Backup { path, source } => write!(
                f,
                "could not create a backup at {}: {source}; the original file is unchanged",
                path.display()
            ),
            WriteError::Write { path, source } => write!(
                f,
                "could not write the temporary file {}: {source}; the original file is unchanged",
                path.display()
            ),
            WriteError::Metadata { path, source } => write!(
                f,
                "could not apply the original file's permissions to {}: {source}; \
                 the original file is unchanged",
                path.display()
            ),
            WriteError::Fsync {
                stage,
                path,
                source,
            } => match stage {
                FsyncStage::DirectoryAfterRename => write!(
                    f,
                    "the new content was written to {} but could not be flushed to disk \
                     ({source}); it is in place but may not survive a power loss",
                    path.display()
                ),
                _ => write!(
                    f,
                    "could not flush {} ({stage}) to disk: {source}; \
                     the original file is unchanged",
                    path.display()
                ),
            },
            WriteError::Rename { from, to, source } => write!(
                f,
                "could not atomically replace {} with {}: {source}; \
                 the original file is unchanged",
                to.display(),
                from.display()
            ),
            WriteError::ReadBack { path, source } => write!(
                f,
                "{} was replaced successfully but could not be read back to verify it: {source}",
                path.display()
            ),
            WriteError::Verify { target, .. } => write!(
                f,
                "{} does not match the content that was just written to it; \
                 the file on disk is not what was intended",
                target.display()
            ),
        }
    }
}

impl std::error::Error for WriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WriteError::Backup { source, .. }
            | WriteError::Write { source, .. }
            | WriteError::Metadata { source, .. }
            | WriteError::Fsync { source, .. }
            | WriteError::Rename { source, .. }
            | WriteError::ReadBack { source, .. } => Some(source),
            WriteError::Validation(_)
            | WriteError::InvalidTarget { .. }
            | WriteError::Verify { .. } => None,
        }
    }
}

/// What a successful [`write_document`] actually did, beyond "it worked".
///
/// Every field is informational: a caller can ignore the whole struct and
/// still know the save completed and was verified. Nothing in here indicates
/// a partial or questionable write - those are always `Err`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteOutcome {
    /// The path actually written. Differs from the `target` argument only
    /// when that argument was a symlink, in which case this is its resolved
    /// destination.
    pub target: PathBuf,

    /// The backup that was created, or `None` if the target did not exist
    /// before this call (first-ever save) and there was nothing to back up.
    pub backup: Option<PathBuf>,

    /// Backups deleted to honour [`MAX_BACKUPS`], oldest first.
    pub pruned_backups: Vec<PathBuf>,

    /// Non-blocking [`crate::validate::Severity::Warning`] findings from
    /// validation (e.g. "mount point does not currently exist on disk", or
    /// semantic verification having run in degraded mode because `findmnt`
    /// is unavailable). Worth surfacing to the user; they did not block the
    /// save.
    pub validation_warnings: Vec<Finding>,

    /// Human-readable notes about non-fatal deviations: a symlinked target
    /// having been resolved, an old backup that could not be pruned, or
    /// ownership that could not be carried over. Deliberately surfaced
    /// rather than silently swallowed, but deliberately not errors - none of
    /// them affect the integrity of the file that was just written.
    pub notes: Vec<String>,
}

/// Atomically replaces `target` with the serialized form of `candidate`,
/// following `docs/SPEC.md` §6 exactly.
///
/// **The guarantee.** Never leaves the target partially written. Any
/// failure before the atomic `rename` leaves the original byte-for-byte
/// untouched; after a successful rename, the new content is re-read and
/// compared byte-for-byte, and a mismatch is [`WriteError::Verify`], never
/// a silent warning.
///
/// A non-existent `target` is created with no backup attempt. An existing
/// one is backed up first, keeping at most [`MAX_BACKUPS`]. A symlinked
/// `target` is written through to its destination.
///
/// Privilege escalation is out of scope: this assumes it already has
/// whatever permissions it needs on `target`, so it behaves identically
/// called directly or from inside an elevated helper.
///
/// # Errors
///
/// Returns a [`WriteError`] identifying the exact failure point; see that
/// type for which variants leave the target untouched.
pub fn write_document(target: &Path, candidate: &Document) -> Result<WriteOutcome, WriteError> {
    write_document_with(target, candidate, &RealFs, &DirectRunner)
}

/// [`write_document`], with the process that runs step 1's `findmnt
/// --verify` chosen by the caller.
///
/// Identical in every other respect - same sequence, same guarantees, same
/// errors. The filesystem work is unaffected: `runner` only decides where
/// the validation step's external check runs, which matters for a process
/// containerized away from the system it is editing (see
/// [`crate::exec`]).
///
/// # Errors
///
/// Exactly [`write_document`]'s.
pub fn write_document_with_runner(
    target: &Path,
    candidate: &Document,
    runner: &dyn CommandRunner,
) -> Result<WriteOutcome, WriteError> {
    write_document_with(target, candidate, &RealFs, runner)
}

/// One backup of a target, as [`list_backups`] reports it.
///
/// Carries the kind alongside the path on purpose: this module owns the
/// naming scheme, so a caller never has to re-implement (and risk
/// disagreeing with) the "was this manual?" grammar itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupEntry {
    /// Full path to the backup file, in the target's own directory.
    pub path: PathBuf,
    /// `true` for a backup a user explicitly asked for via [`create_backup`],
    /// `false` for one a save made on its own. Manual backups are exempt from
    /// [`MAX_BACKUPS`] pruning (see the module docs).
    pub manual: bool,
}

/// Lists the backups this module has created for `target`, oldest first,
/// automatic and manual alike.
///
/// Only files matching the exact
/// `<target-file-name>.rustab-backup.<10-digit secs>.<9-digit nanos>`
/// scheme (optionally + `.manual`) in the target's own directory are
/// returned - unrelated files (a user's own `fstab.bak`, or a different
/// target's backup in the same directory) are never reported or pruned.
///
/// # Errors
///
/// Returns the underlying [`io::Error`] if the target's directory can't
/// be read. A `target` with no file-name component yields an empty list.
pub fn list_backups(target: &Path) -> io::Result<Vec<BackupEntry>> {
    if target.file_name().is_none() {
        return Ok(Vec::new());
    }
    let names = RealFs.list_dir_file_names(&containing_dir(target))?;
    Ok(list_backups_among(target, &names))
}

/// The subset of `names` that are backups of `target`, oldest first - the
/// classification [`list_backups`] performs once it has read the directory,
/// and nothing else.
///
/// `names` are the plain file names of the entries in `target`'s own
/// directory. Exists so a caller that obtained those names *some other way* -
/// from another process, or from another machine - can apply this module's
/// backup-naming grammar without duplicating it: the grammar is the only
/// thing standing between a restore (or a prune) and a user's unrelated
/// files, so there must be exactly one implementation of it.
///
/// Every name that is not *exactly* a backup of `target` is dropped,
/// including `target` itself, a different target's backup in the same
/// directory, and any near miss (see [`list_backups`] for the scheme). A
/// `target` with no file-name component yields an empty list, exactly as
/// [`list_backups`] does.
pub fn list_backups_among(target: &Path, names: &[OsString]) -> Vec<BackupEntry> {
    let Some(file_name) = target.file_name() else {
        return Vec::new();
    };
    sorted_backups(&containing_dir(target), names, file_name)
        .into_iter()
        .map(|(name, path)| BackupEntry {
            path,
            manual: name.manual,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Manual backups
// ---------------------------------------------------------------------------

/// What a successful [`create_backup`] did.
///
/// Shaped like [`WriteOutcome`] minus what only makes sense for a save: no
/// `validation_warnings` (nothing was written), and `backup` isn't an
/// `Option` (a run that creates no backup is an `Err`, not a quiet success).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateBackupOutcome {
    /// The file that was backed up. Differs from the `target` argument only
    /// when that argument was a symlink, in which case this is its resolved
    /// destination - the same resolution a save performs.
    pub target: PathBuf,

    /// The backup that was created. Always a *manual* backup, so it carries
    /// the `.manual` suffix and is exempt from [`MAX_BACKUPS`] pruning.
    pub backup: PathBuf,

    /// *Automatic* backups deleted to honour [`MAX_BACKUPS`], oldest first.
    /// Normally empty: taking a manual backup does not add to the automatic
    /// count, so this is non-empty only when the target was already over the
    /// bound.
    pub pruned_backups: Vec<PathBuf>,

    /// Human-readable notes about non-fatal deviations: a symlinked target
    /// having been resolved, or an old automatic backup that could not be
    /// pruned. Same meaning, and same deliberate "surfaced, not swallowed"
    /// policy, as [`WriteOutcome::notes`].
    pub notes: Vec<String>,
}

/// Everything that can go wrong in [`create_backup`].
///
/// Deliberately not [`WriteError`]: this never opens, renames, or writes
/// the target, so "did the target change?" has no meaning here - every
/// variant leaves the target byte-for-byte untouched.
#[derive(Debug)]
pub enum CreateBackupError {
    /// The target path cannot be backed up for a structural reason: no
    /// final file-name component, it's a directory, or an unresolvable
    /// symlink. The same check [`write_document`] applies, via the same code.
    InvalidTarget {
        /// The path as given to [`create_backup`].
        path: PathBuf,
        /// Why it cannot be used as a target.
        reason: String,
    },

    /// Nothing exists at the target yet. Its own variant rather than a
    /// silent no-op, since quietly reporting success for a snapshot that
    /// doesn't exist is the one answer the user can't act on.
    TargetMissing {
        /// The resolved target that does not exist.
        path: PathBuf,
    },

    /// Inspecting the target, reserving a backup name, copying it, or
    /// `fsync`ing the copy failed. Pruning old automatic backups isn't
    /// part of this variant (see [`CreateBackupOutcome::notes`]).
    Backup {
        /// The path being inspected or written when the failure occurred.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
}

impl fmt::Display for CreateBackupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CreateBackupError::InvalidTarget { path, reason } => write!(
                f,
                "cannot back up {}: {reason}; no backup was created",
                path.display()
            ),
            CreateBackupError::TargetMissing { path } => write!(
                f,
                "{} does not exist yet, so there is nothing to back up; \
                 save the file once first",
                path.display()
            ),
            CreateBackupError::Backup { path, source } => write!(
                f,
                "could not create a backup at {}: {source}; the original file is unchanged",
                path.display()
            ),
        }
    }
}

impl std::error::Error for CreateBackupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CreateBackupError::Backup { source, .. } => Some(source),
            CreateBackupError::InvalidTarget { .. } | CreateBackupError::TargetMissing { .. } => {
                None
            }
        }
    }
}

/// Copies `target`'s current content to a fresh **manual** backup beside it,
/// without touching `target` itself.
///
/// This is the "snapshot it now" entry point, and it is deliberately the only
/// thing it sounds like: resolve the target (following a symlink exactly as
/// [`write_document`] does), reserve an unused manual backup name, copy and
/// `fsync` the copy, then enforce the automatic-backup retention bound. No
/// temp file is created, nothing is renamed, nothing is validated, and
/// [`write_document`] is not called - the target file is only ever *read*.
///
/// The backup it creates is exempt from [`MAX_BACKUPS`] pruning (see the
/// module docs).
///
/// Privilege escalation is out of scope, for the same reason as
/// [`write_document`].
///
/// # Errors
///
/// [`CreateBackupError::TargetMissing`] if there is nothing on disk to copy,
/// [`CreateBackupError::InvalidTarget`] if the path cannot be a target at all,
/// [`CreateBackupError::Backup`] for any I/O failure while taking the copy.
/// The target file is untouched in every case.
pub fn create_backup(target: &Path) -> Result<CreateBackupOutcome, CreateBackupError> {
    create_backup_with(target, &RealFs)
}

// ---------------------------------------------------------------------------
// The algorithm
// ---------------------------------------------------------------------------

/// The [`FileSystemOps`]-generic implementation behind [`write_document`].
///
/// Crate-private: production callers get the concrete, `std::fs`-backed
/// [`write_document`] (or [`write_document_with_runner`]); the generic
/// parameter lets unit tests inject an I/O failure at one specific syscall.
fn write_document_with<F: FileSystemOps>(
    target: &Path,
    candidate: &Document,
    fsops: &F,
    runner: &dyn CommandRunner,
) -> Result<WriteOutcome, WriteError> {
    // Step 1 (and 2 - `validate_with` already runs the `findmnt --verify`
    // semantic check internally). Deliberately the very first thing that
    // happens: on error we return before any syscall that could modify
    // anything.
    let report = validate::validate_with(candidate, runner);
    if report.has_errors() {
        return Err(WriteError::Validation(report));
    }
    let validation_warnings: Vec<Finding> = report.warnings().cloned().collect();
    let mut notes: Vec<String> = Vec::new();

    // Step 2': resolve a symlinked target to the file it points at.
    let (effective_target, existing) =
        resolve_target(target, fsops, &mut notes).map_err(WriteError::from)?;

    let file_name = effective_target
        .file_name()
        .ok_or_else(|| WriteError::InvalidTarget {
            path: effective_target.clone(),
            reason: "the path has no final file-name component".to_string(),
        })?
        .to_os_string();
    let dir = containing_dir(&effective_target);

    // Step 3: back up the current content, then enforce retention.
    let mut pruned_backups = Vec::new();
    let backup = match &existing {
        None => None,
        Some(_) => {
            let backup_path = take_backup(fsops, &effective_target, &dir, &file_name, false)
                .map_err(|failure| WriteError::Backup {
                    path: failure.path,
                    source: failure.source,
                })?;
            prune_backups(fsops, &dir, &file_name, &mut pruned_backups, &mut notes);
            Some(backup_path)
        }
    };

    // Steps 4 and 5.
    let contents = candidate.to_string().into_bytes();
    let temp_path = dir.join(temp_file_name(&file_name));
    let plan = SwapPlan {
        temp_path: &temp_path,
        target: &effective_target,
        dir: &dir,
        contents: &contents,
        mode: existing
            .as_ref()
            .map_or(DEFAULT_NEW_FILE_MODE, |stat| stat.mode),
        owner: existing.as_ref().map(|stat| (stat.uid, stat.gid)),
    };
    if let Err(err) = write_temp_and_swap(fsops, &plan, &mut notes) {
        // Step 7. `temp_path` is distinct from the target by construction
        // (leading `.`, `.rustab.tmp.<pid>` suffix), so this can never remove
        // live data - including in the one case where this runs after a
        // successful rename (a failed post-rename directory fsync), where it
        // is simply a no-op. Any cleanup error is dropped on purpose so it
        // cannot mask `err`.
        let _ = fsops.remove_file(&temp_path);
        return Err(err);
    }

    // Step 6: re-read and verify rather than trusting the OS blindly.
    let readback = fsops
        .read(&effective_target)
        .map_err(|source| WriteError::ReadBack {
            path: effective_target.clone(),
            source,
        })?;
    if readback != contents {
        return Err(WriteError::Verify {
            target: effective_target,
            backup,
            expected: String::from_utf8_lossy(&contents).into_owned(),
            actual: String::from_utf8_lossy(&readback).into_owned(),
        });
    }

    Ok(WriteOutcome {
        target: effective_target,
        backup,
        pruned_backups,
        validation_warnings,
        notes,
    })
}

/// The [`FileSystemOps`]-generic implementation behind [`create_backup`].
/// Crate-private, for the same reason as [`write_document_with`].
///
/// Shares [`resolve_target`], [`take_backup`], and [`prune_backups`] with
/// the save path, so the two can never disagree about the target, backup
/// naming, or which backups are excess. Not shared: everything after the
/// copy - no temp file, no rename, no read-back, since this never writes
/// the target.
fn create_backup_with<F: FileSystemOps>(
    target: &Path,
    fsops: &F,
) -> Result<CreateBackupOutcome, CreateBackupError> {
    let mut notes: Vec<String> = Vec::new();
    let (effective_target, existing) =
        resolve_target(target, fsops, &mut notes).map_err(CreateBackupError::from)?;

    // A target that does not exist is a normal state for a save (it creates
    // the file) but the whole point of this operation, so it is reported as
    // its own error instead of being folded into an I/O failure.
    if existing.is_none() {
        return Err(CreateBackupError::TargetMissing {
            path: effective_target,
        });
    }

    let file_name = effective_target
        .file_name()
        .ok_or_else(|| CreateBackupError::InvalidTarget {
            path: effective_target.clone(),
            reason: "the path has no final file-name component".to_string(),
        })?
        .to_os_string();
    let dir = containing_dir(&effective_target);

    let backup =
        take_backup(fsops, &effective_target, &dir, &file_name, true).map_err(|failure| {
            CreateBackupError::Backup {
                path: failure.path,
                source: failure.source,
            }
        })?;

    // Retention still runs, even though the backup just taken is exempt from
    // it: a target that was already over the automatic bound (because an
    // earlier prune failed, say) should not stay over it forever just because
    // the next backup happened to be a manual one.
    let mut pruned_backups = Vec::new();
    prune_backups(fsops, &dir, &file_name, &mut pruned_backups, &mut notes);

    Ok(CreateBackupOutcome {
        target: effective_target,
        backup,
        pruned_backups,
        notes,
    })
}

/// The already-validated inputs to steps 4-5, grouped so
/// [`write_temp_and_swap`] takes a readable number of parameters.
struct SwapPlan<'a> {
    /// Same-directory temp file to write and then rename.
    temp_path: &'a Path,
    /// The file the temp file will replace.
    target: &'a Path,
    /// The directory containing both, which must be `fsync`ed.
    dir: &'a Path,
    /// Exact bytes to write.
    contents: &'a [u8],
    /// Permission bits to put on the new file.
    mode: u32,
    /// uid/gid to carry over from the previous target, if it existed.
    owner: Option<(u32, u32)>,
}

/// Steps 4 and 5: write the temp file, give it the target's identity, make it
/// durable, and atomically rename it into place.
///
/// Returns `Ok(())` only once the rename has succeeded *and* been flushed.
/// On any `Err`, the caller removes the temp file; see
/// [`WriteError`]/[`FsyncStage`] for which errors leave the target untouched.
fn write_temp_and_swap<F: FileSystemOps>(
    fsops: &F,
    plan: &SwapPlan<'_>,
    notes: &mut Vec<String>,
) -> Result<(), WriteError> {
    let file = fsops
        .create_and_write(plan.temp_path, plan.contents)
        .map_err(|source| WriteError::Write {
            path: plan.temp_path.to_path_buf(),
            source,
        })?;

    fsops
        .set_mode(&file, plan.mode)
        .map_err(|source| WriteError::Metadata {
            path: plan.temp_path.to_path_buf(),
            source,
        })?;
    if let Some((uid, gid)) = plan.owner {
        if let Err(err) = fsops.set_owner(&file, uid, gid) {
            notes.push(format!(
                "could not carry the previous owner ({uid}:{gid}) of {} over to the saved file \
                 ({err}); it is owned by the saving process instead",
                plan.target.display()
            ));
        }
    }

    fsops.sync_file(&file).map_err(|source| WriteError::Fsync {
        stage: FsyncStage::TempFile,
        path: plan.temp_path.to_path_buf(),
        source,
    })?;
    fsops
        .sync_dir(plan.dir)
        .map_err(|source| WriteError::Fsync {
            stage: FsyncStage::DirectoryBeforeRename,
            path: plan.dir.to_path_buf(),
            source,
        })?;

    fsops
        .rename(plan.temp_path, plan.target)
        .map_err(|source| WriteError::Rename {
            from: plan.temp_path.to_path_buf(),
            to: plan.target.to_path_buf(),
            source,
        })?;

    // Only an `fsync` of the directory *after* the rename makes the rename
    // itself durable - see the module docs.
    fsops
        .sync_dir(plan.dir)
        .map_err(|source| WriteError::Fsync {
            stage: FsyncStage::DirectoryAfterRename,
            path: plan.dir.to_path_buf(),
            source,
        })
}

/// Why a path could not be turned into "the file to act on".
///
/// Its own type rather than a [`WriteError`] because both entry points share
/// [`resolve_target`] but report in their own error shapes; each maps these
/// two cases onto its own variants, which keeps the resolution logic - the
/// symlink walk in particular - written exactly once.
enum TargetFailure {
    /// An `lstat` of the path, or of a symlink's destination, failed.
    Stat {
        /// The path that could not be inspected.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// The path is structurally unusable as a target: a directory, or a
    /// symlink that could not be resolved.
    Invalid {
        /// The offending path.
        path: PathBuf,
        /// Why it cannot be used.
        reason: String,
    },
}

impl From<TargetFailure> for WriteError {
    fn from(failure: TargetFailure) -> Self {
        match failure {
            // Historically (and still) reported as a backup-stage failure:
            // this `lstat` is the first thing the backup step needs, and the
            // target is provably untouched either way.
            TargetFailure::Stat { path, source } => WriteError::Backup { path, source },
            TargetFailure::Invalid { path, reason } => WriteError::InvalidTarget { path, reason },
        }
    }
}

impl From<TargetFailure> for CreateBackupError {
    fn from(failure: TargetFailure) -> Self {
        match failure {
            TargetFailure::Stat { path, source } => CreateBackupError::Backup { path, source },
            TargetFailure::Invalid { path, reason } => {
                CreateBackupError::InvalidTarget { path, reason }
            }
        }
    }
}

/// A failed attempt to reserve or write a backup copy: which path was being
/// worked on, and why it failed.
///
/// Shared by both entry points for the same reason [`TargetFailure`] is -
/// [`take_backup`] is written once and each caller words the failure in its
/// own error type.
struct BackupFailure {
    /// The backup path being reserved or written.
    path: PathBuf,
    /// The underlying I/O error.
    source: io::Error,
}

/// Determines the file that will actually be acted on and stats it.
///
/// Returns the effective target path and its [`FileStat`], or `None` if it
/// doesn't exist yet. A symlinked target resolves to its destination; a
/// directory or dangling symlink is rejected as [`TargetFailure::Invalid`].
fn resolve_target<F: FileSystemOps>(
    target: &Path,
    fsops: &F,
    notes: &mut Vec<String>,
) -> Result<(PathBuf, Option<FileStat>), TargetFailure> {
    let stat = fsops
        .symlink_stat(target)
        .map_err(|source| TargetFailure::Stat {
            path: target.to_path_buf(),
            source,
        })?;

    let (path, stat) = match stat {
        Some(stat) if stat.is_symlink => {
            let resolved = fsops
                .canonicalize(target)
                .map_err(|source| TargetFailure::Invalid {
                    path: target.to_path_buf(),
                    reason: format!("it is a symlink that could not be resolved ({source})"),
                })?;
            notes.push(format!(
                "{} is a symlink; it was followed through to {}, which is the file that was \
                 acted on",
                target.display(),
                resolved.display()
            ));
            let resolved_stat =
                fsops
                    .symlink_stat(&resolved)
                    .map_err(|source| TargetFailure::Stat {
                        path: resolved.clone(),
                        source,
                    })?;
            (resolved, resolved_stat)
        }
        other => (target.to_path_buf(), other),
    };

    if stat.as_ref().is_some_and(|stat| stat.is_dir) {
        return Err(TargetFailure::Invalid {
            path,
            reason: "it is a directory".to_string(),
        });
    }
    Ok((path, stat))
}

/// Reserves a fresh backup name for `file_name` in `dir` and copies `source`
/// into it durably, returning the backup's path.
///
/// The single place a backup copy is ever made, by either entry point - the
/// durability argument in the module docs holds for a manual snapshot
/// exactly as for an automatic one. `manual` picks which name shape is reserved.
fn take_backup<F: FileSystemOps>(
    fsops: &F,
    source: &Path,
    dir: &Path,
    file_name: &OsStr,
    manual: bool,
) -> Result<PathBuf, BackupFailure> {
    let backup_path = reserve_backup_path(fsops, dir, file_name, manual)?;
    fsops
        .copy_durably(source, &backup_path)
        .map_err(|source| BackupFailure {
            path: backup_path.clone(),
            source,
        })?;
    Ok(backup_path)
}

/// Returns the directory containing `path`, mapping both "no parent" and the
/// empty parent of a bare relative name (e.g. `fstab`) to `.`, so the
/// directory can always be opened and `fsync`ed.
fn containing_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Builds the same-directory temp file name for `file_name`:
/// `.<file_name>.rustab.tmp.<pid>`.
///
/// The pid keeps concurrent processes (or an elevated helper and the app
/// itself) from colliding. Built via [`OsString`] concatenation, not
/// `str`, so a non-UTF-8 file name isn't lossily mangled.
fn temp_file_name(file_name: &OsStr) -> OsString {
    let mut name = OsString::from(".");
    name.push(file_name);
    name.push(format!(".rustab.tmp.{}", std::process::id()));
    name
}

/// Everything a backup's file name encodes: when it was taken, and whether a
/// user asked for it explicitly. The parsed counterpart of
/// [`backup_file_name`] - kept adjacent so the two can't drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BackupName {
    /// Whole seconds since the Unix epoch, from the name's first component.
    secs: u64,
    /// Nanoseconds within that second, from the name's second component.
    nanos: u32,
    /// `true` if the name carries the [`MANUAL_SUFFIX`].
    manual: bool,
}

impl BackupName {
    /// The chronological sort key, deliberately excluding `manual`: both kinds
    /// of backup live in one list ordered purely by when they were taken.
    fn when(&self) -> (u64, u32) {
        (self.secs, self.nanos)
    }
}

/// Builds the backup file name for `file_name` at the given timestamp:
/// `<file_name>.rustab-backup.<secs>.<nanos>`, zero-padded so names sort
/// chronologically as text too, plus a trailing [`MANUAL_SUFFIX`] when
/// `manual` is set (after the timestamp, so the text sort is unaffected).
fn backup_file_name(file_name: &OsStr, secs: u64, nanos: u32, manual: bool) -> OsString {
    let mut name = OsString::from(file_name);
    name.push(format!(
        "{BACKUP_INFIX}{secs:0secs_width$}.{nanos:0nanos_width$}",
        secs_width = BACKUP_SECS_DIGITS,
        nanos_width = BACKUP_NANOS_DIGITS,
    ));
    if manual {
        name.push(MANUAL_SUFFIX);
    }
    name
}

/// Parses a directory entry name as a backup of `target_name`, or `None`
/// if it isn't *exactly* a Rustab backup of that target.
///
/// Strict on purpose - this is the only thing standing between
/// [`prune_backups`] and a user's unrelated files. Must reject anything
/// that isn't an exact `<target_name>.rustab-backup.<10 digits>.<9
/// digits>`, optionally + exactly `.manual`.
///
/// The manual suffix is stripped before the timestamp is parsed, so
/// adding it widened the accepted set by exactly one shape without
/// loosening the timestamp check.
fn backup_name_of(name: &OsStr, target_name: &OsStr) -> Option<BackupName> {
    let rest = name.as_bytes().strip_prefix(target_name.as_bytes())?;
    let rest = rest.strip_prefix(BACKUP_INFIX.as_bytes())?;
    let rest = std::str::from_utf8(rest).ok()?;

    let (stamp, manual) = match rest.strip_suffix(MANUAL_SUFFIX) {
        Some(stamp) => (stamp, true),
        None => (rest, false),
    };

    let (secs, nanos) = stamp.split_once('.')?;
    if secs.len() != BACKUP_SECS_DIGITS || nanos.len() != BACKUP_NANOS_DIGITS {
        return None;
    }
    if !secs
        .bytes()
        .chain(nanos.bytes())
        .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    Some(BackupName {
        secs: secs.parse().ok()?,
        nanos: nanos.parse().ok()?,
        manual,
    })
}

/// Filters `names` down to backups of `target_name` - automatic and manual
/// alike - and returns them paired with their parsed names, oldest first. The
/// path is a secondary sort key purely so the order is total and
/// deterministic.
fn sorted_backups(
    dir: &Path,
    names: &[OsString],
    target_name: &OsStr,
) -> Vec<(BackupName, PathBuf)> {
    let mut backups: Vec<(BackupName, PathBuf)> = names
        .iter()
        .filter_map(|name| backup_name_of(name, target_name).map(|parsed| (parsed, dir.join(name))))
        .collect();
    backups.sort_by(|a, b| a.0.when().cmp(&b.0.when()).then_with(|| a.1.cmp(&b.1)));
    backups
}

/// Picks an unused timestamped backup path in `dir` for `file_name`, of the
/// kind `manual` selects.
///
/// Retries with a fresh timestamp on a collision, so a backup never
/// silently overwrites an older one. Running out of attempts aborts the
/// operation via [`BackupFailure`] with the target untouched.
fn reserve_backup_path<F: FileSystemOps>(
    fsops: &F,
    dir: &Path,
    file_name: &OsStr,
    manual: bool,
) -> Result<PathBuf, BackupFailure> {
    for _ in 0..MAX_BACKUP_NAME_ATTEMPTS {
        let (secs, nanos) = unix_timestamp_parts();
        let candidate = dir.join(backup_file_name(file_name, secs, nanos, manual));
        match fsops.symlink_stat(&candidate) {
            Ok(None) => return Ok(candidate),
            Ok(Some(_)) => continue,
            Err(source) => {
                return Err(BackupFailure {
                    path: candidate,
                    source,
                })
            }
        }
    }
    Err(BackupFailure {
        path: dir.join(file_name),
        source: io::Error::other(format!(
            "no unused timestamped backup name was available after {MAX_BACKUP_NAME_ATTEMPTS} \
             attempts"
        )),
    })
}

/// Deletes **automatic** backups of `file_name` in `dir` beyond
/// [`MAX_BACKUPS`], oldest first, recording what it deleted in `pruned`.
///
/// Manual backups are invisible here in both directions: not counted as
/// excess, and never deleted to make room (see module docs).
///
/// Deliberately infallible: a stale extra backup is harmless, so neither
/// an unreadable directory nor an undeletable old backup aborts an
/// otherwise-safe operation. Failures are still recorded in `notes`.
fn prune_backups<F: FileSystemOps>(
    fsops: &F,
    dir: &Path,
    file_name: &OsStr,
    pruned: &mut Vec<PathBuf>,
    notes: &mut Vec<String>,
) {
    let names = match fsops.list_dir_file_names(dir) {
        Ok(names) => names,
        Err(err) => {
            notes.push(format!(
                "could not read {} to prune old backups ({err}); existing backups were left \
                 in place",
                dir.display()
            ));
            return;
        }
    };

    // Manual backups are filtered out *before* the count, so they neither
    // occupy a slot nor can be chosen for deletion.
    let automatic: Vec<PathBuf> = sorted_backups(dir, &names, file_name)
        .into_iter()
        .filter(|(name, _)| !name.manual)
        .map(|(_, path)| path)
        .collect();
    let Some(excess) = automatic.len().checked_sub(MAX_BACKUPS) else {
        return;
    };
    for path in automatic.into_iter().take(excess) {
        match fsops.remove_file(&path) {
            Ok(()) => pruned.push(path),
            Err(err) => notes.push(format!(
                "could not delete the old backup {} ({err}); it was left in place",
                path.display()
            )),
        }
    }
}

/// Current wall-clock time as `(seconds, nanoseconds)` since the Unix epoch.
///
/// A clock set before the epoch yields `(0, 0)` rather than panicking:
/// backup naming is not worth failing a save over, and the collision retry in
/// [`reserve_backup_path`] still keeps names unique.
fn unix_timestamp_parts() -> (u64, u32) {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or((0, 0), |d| (d.as_secs(), d.subsec_nanos()))
}

// ---------------------------------------------------------------------------
// The filesystem seam
// ---------------------------------------------------------------------------

/// The subset of a file's metadata this module needs: enough to tell a
/// symlink and a directory apart from a regular file, and to carry the
/// previous target's identity onto its replacement.
///
/// A hand-rolled struct rather than [`fs::Metadata`], since the test spy
/// needs to synthesize one and `fs::Metadata` can't be built outside `std`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStat {
    /// Whether the path itself is a symlink (i.e. from an `lstat`, not a
    /// `stat`).
    is_symlink: bool,
    /// Whether the path is a directory.
    is_dir: bool,
    /// Permission bits only (`st_mode & 0o7777`).
    mode: u32,
    /// Owning user id.
    uid: u32,
    /// Owning group id.
    gid: u32,
}

/// The individual filesystem operations [`write_document_with`] performs.
///
/// A failure-injection seam: a thin, policy-free wrapper over the
/// syscalls, so the production path ([`RealFs`]) stays auditable as plain
/// `std::fs` code while tests can replace exactly one operation with an
/// error. Every method maps to one syscall (or one obvious pair, for
/// [`Self::create_and_write`]/[`Self::copy_durably`]).
trait FileSystemOps {
    /// A handle from [`Self::create_and_write`]. Operations on the open
    /// file description (`fchmod`/`fchown`/`fsync`) take this rather than
    /// a path, so they provably affect the file just written.
    type File;

    /// `lstat`: metadata for `path` itself, not following a final symlink.
    /// `Ok(None)` means the path does not exist, which is a normal, expected
    /// state (first-ever save), not an error.
    fn symlink_stat(&self, path: &Path) -> io::Result<Option<FileStat>>;

    /// Fully resolves `path`, following symlinks.
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;

    /// Copies `from` to `to` (creating or truncating `to`, preserving `from`'s
    /// permission bits) and flushes the copy to disk before returning.
    fn copy_durably(&self, from: &Path, to: &Path) -> io::Result<()>;

    /// Returns the file names (not full paths) of `dir`'s entries.
    fn list_dir_file_names(&self, dir: &Path) -> io::Result<Vec<OsString>>;

    /// Deletes `path`.
    fn remove_file(&self, path: &Path) -> io::Result<()>;

    /// Creates `path` (truncating any leftover file of the same name) with
    /// restrictive `0600` permissions, writes `contents` in full, and returns
    /// the still-open handle. Does **not** flush - that is
    /// [`Self::sync_file`], kept separate so a flush failure is
    /// distinguishable from a write failure.
    fn create_and_write(&self, path: &Path, contents: &[u8]) -> io::Result<Self::File>;

    /// `fchmod`: sets `file`'s permission bits.
    fn set_mode(&self, file: &Self::File, mode: u32) -> io::Result<()>;

    /// `fchown`: sets `file`'s owning user and group.
    fn set_owner(&self, file: &Self::File, uid: u32, gid: u32) -> io::Result<()>;

    /// `fsync`: flushes `file`'s data and metadata to stable storage.
    fn sync_file(&self, file: &Self::File) -> io::Result<()>;

    /// `fsync` on a directory: flushes `dir`'s entries to stable storage,
    /// which is what makes a file's creation or rename within it durable.
    fn sync_dir(&self, dir: &Path) -> io::Result<()>;

    /// `rename`: atomically replaces `to` with `from` within one filesystem.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;

    /// Reads `path`'s full contents, used only for post-rename verification.
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
}

/// The production [`FileSystemOps`] implementation: direct `std::fs` calls,
/// one per method, no buffering, caching, or retrying.
struct RealFs;

impl FileSystemOps for RealFs {
    type File = fs::File;

    fn symlink_stat(&self, path: &Path) -> io::Result<Option<FileStat>> {
        match fs::symlink_metadata(path) {
            Ok(metadata) => Ok(Some(FileStat {
                is_symlink: metadata.file_type().is_symlink(),
                is_dir: metadata.is_dir(),
                // `PermissionsExt::mode` returns the full `st_mode`; mask off
                // the file-type bits so the value can be fed straight back to
                // `fchmod`.
                mode: metadata.permissions().mode() & 0o7777,
                uid: metadata.uid(),
                gid: metadata.gid(),
            })),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        fs::canonicalize(path)
    }

    fn copy_durably(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::copy(from, to)?;
        // Opened for writing (rather than reusing a read handle) so the
        // `fsync` is unambiguously valid on every filesystem.
        fs::OpenOptions::new().write(true).open(to)?.sync_all()
    }

    fn list_dir_file_names(&self, dir: &Path) -> io::Result<Vec<OsString>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(dir)? {
            names.push(entry?.file_name());
        }
        Ok(names)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    fn create_and_write(&self, path: &Path, contents: &[u8]) -> io::Result<Self::File> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            // Born restrictive: the file is widened to the target's real mode
            // by `set_mode` below, so it is never momentarily readable by
            // anyone who could not already read the target.
            .mode(0o600)
            .open(path)?;
        file.write_all(contents)?;
        Ok(file)
    }

    fn set_mode(&self, file: &Self::File, mode: u32) -> io::Result<()> {
        file.set_permissions(fs::Permissions::from_mode(mode))
    }

    fn set_owner(&self, file: &Self::File, uid: u32, gid: u32) -> io::Result<()> {
        std::os::unix::fs::fchown(file, Some(uid), Some(gid))
    }

    fn sync_file(&self, file: &Self::File) -> io::Result<()> {
        file.sync_all()
    }

    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        // On Linux a directory can be opened read-only and `fsync`ed; this is
        // the only way to make a rename or creation within it durable.
        fs::File::open(dir)?.sync_all()
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        fs::read(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FstabEntry, FstabOption, Line, Options, SourceId};
    use crate::parser;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    // -- test scaffolding ---------------------------------------------------

    /// A self-deleting temp directory, so tests never depend on an external
    /// crate and never leave files behind. Deliberately inside
    /// `std::env::temp_dir()` (one filesystem), matching how `validate.rs`'s
    /// own tests get scratch space.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "rustab-writer-test-{}-{tag}-{n}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("could not create test temp dir");
            TempDir { path }
        }

        fn join(&self, name: &str) -> PathBuf {
            self.path.join(name)
        }

        /// Entry names currently in the directory, sorted for stable
        /// assertions.
        fn entry_names(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(&self.path)
                .expect("could not read test temp dir")
                .map(|e| {
                    e.expect("dir entry")
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// Which [`FileSystemOps`] call the spy should turn into an error.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    enum Op {
        SymlinkStat,
        Canonicalize,
        CopyBackup,
        ListDir,
        RemoveFile,
        CreateAndWrite,
        SetMode,
        SetOwner,
        SyncFile,
        SyncDir,
        Rename,
        Read,
    }

    /// A delegating spy: every operation runs for real against the
    /// filesystem, except one chosen operation (optionally its nth
    /// occurrence) which fails with an injected `io::Error`. Also records
    /// the operation sequence, so the spec-mandated write/fsync/fsync/rename
    /// ordering can be asserted rather than eyeballed.
    struct SpyFs {
        inner: RealFs,
        fail_on: Option<Op>,
        /// 1-based occurrence of `fail_on` to fail at (the directory is
        /// `fsync`ed twice and `symlink_stat` called more than once).
        fail_on_nth: u32,
        /// When set, `read` returns these bytes instead of the file's real
        /// content, simulating the "OS handed back something else" case the
        /// verify step exists to catch.
        corrupt_readback: Option<Vec<u8>>,
        log: RefCell<Vec<(Op, String)>>,
        counts: RefCell<HashMap<Op, u32>>,
    }

    impl SpyFs {
        fn new() -> Self {
            SpyFs {
                inner: RealFs,
                fail_on: None,
                fail_on_nth: 1,
                corrupt_readback: None,
                log: RefCell::new(Vec::new()),
                counts: RefCell::new(HashMap::new()),
            }
        }

        fn failing_at(op: Op) -> Self {
            SpyFs {
                fail_on: Some(op),
                ..SpyFs::new()
            }
        }

        fn failing_at_nth(op: Op, nth: u32) -> Self {
            SpyFs {
                fail_on: Some(op),
                fail_on_nth: nth,
                ..SpyFs::new()
            }
        }

        fn with_corrupt_readback(bytes: &str) -> Self {
            SpyFs {
                corrupt_readback: Some(bytes.as_bytes().to_vec()),
                ..SpyFs::new()
            }
        }

        /// Records the call and decides whether this occurrence is the
        /// injected failure.
        fn check(&self, op: Op, detail: impl Into<String>) -> io::Result<()> {
            self.log.borrow_mut().push((op, detail.into()));
            let nth = {
                let mut counts = self.counts.borrow_mut();
                let slot = counts.entry(op).or_insert(0);
                *slot += 1;
                *slot
            };
            if self.fail_on == Some(op) && nth == self.fail_on_nth {
                return Err(io::Error::other(format!("injected failure at {op:?}")));
            }
            Ok(())
        }

        fn ops(&self) -> Vec<Op> {
            self.log.borrow().iter().map(|(op, _)| *op).collect()
        }

        fn detail_of(&self, op: Op) -> String {
            self.log
                .borrow()
                .iter()
                .find(|(logged, _)| *logged == op)
                .map(|(_, detail)| detail.clone())
                .unwrap_or_default()
        }
    }

    impl FileSystemOps for SpyFs {
        type File = fs::File;

        fn symlink_stat(&self, path: &Path) -> io::Result<Option<FileStat>> {
            self.check(Op::SymlinkStat, path.display().to_string())?;
            self.inner.symlink_stat(path)
        }

        fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
            self.check(Op::Canonicalize, path.display().to_string())?;
            self.inner.canonicalize(path)
        }

        fn copy_durably(&self, from: &Path, to: &Path) -> io::Result<()> {
            self.check(Op::CopyBackup, to.display().to_string())?;
            self.inner.copy_durably(from, to)
        }

        fn list_dir_file_names(&self, dir: &Path) -> io::Result<Vec<OsString>> {
            self.check(Op::ListDir, dir.display().to_string())?;
            self.inner.list_dir_file_names(dir)
        }

        fn remove_file(&self, path: &Path) -> io::Result<()> {
            self.check(Op::RemoveFile, path.display().to_string())?;
            self.inner.remove_file(path)
        }

        fn create_and_write(&self, path: &Path, contents: &[u8]) -> io::Result<Self::File> {
            if self
                .check(Op::CreateAndWrite, path.display().to_string())
                .is_err()
            {
                // Mimic a real ENOSPC rather than a clean no-op: the temp file
                // is created and left holding a truncated prefix, which is the
                // state the untouched-target guarantee has to survive.
                let half = contents.len() / 2;
                drop(self.inner.create_and_write(path, &contents[..half])?);
                return Err(io::Error::other("injected failure at CreateAndWrite"));
            }
            self.inner.create_and_write(path, contents)
        }

        fn set_mode(&self, file: &Self::File, mode: u32) -> io::Result<()> {
            self.check(Op::SetMode, format!("{mode:o}"))?;
            self.inner.set_mode(file, mode)
        }

        fn set_owner(&self, file: &Self::File, uid: u32, gid: u32) -> io::Result<()> {
            self.check(Op::SetOwner, format!("{uid}:{gid}"))?;
            self.inner.set_owner(file, uid, gid)
        }

        fn sync_file(&self, file: &Self::File) -> io::Result<()> {
            self.check(Op::SyncFile, "")?;
            self.inner.sync_file(file)
        }

        fn sync_dir(&self, dir: &Path) -> io::Result<()> {
            self.check(Op::SyncDir, dir.display().to_string())?;
            self.inner.sync_dir(dir)
        }

        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            self.check(
                Op::Rename,
                format!("{} -> {}", from.display(), to.display()),
            )?;
            self.inner.rename(from, to)
        }

        fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
            self.check(Op::Read, path.display().to_string())?;
            match &self.corrupt_readback {
                Some(bytes) => Ok(bytes.clone()),
                None => self.inner.read(path),
            }
        }
    }

    // -- fixtures -----------------------------------------------------------

    /// The content a test target starts with. Every failure-injection test
    /// asserts the target still holds exactly these bytes afterwards.
    const ORIGINAL: &str = "# original fstab, must survive every failure\n\
                            UUID=11111111-2222-3333-4444-555555555555 /mnt/original ext4 noauto 0 2\n";

    /// A valid candidate document (`noauto` + well-formed UUIDs, so real
    /// `findmnt --verify` has nothing to object to - the same approach
    /// `validate.rs`'s tests use).
    ///
    /// The leading note is carried as the entry's `description`, not as a
    /// standalone `Line::Comment`: it serializes to the exact same bytes
    /// (`# rewritten ...` directly above the entry), but it is the shape the
    /// parser itself produces for those bytes, so
    /// [`saved_file_reparses_to_an_equal_document`] can compare documents
    /// rather than just text.
    fn valid_candidate(mount_point: &str) -> Document {
        Document::from_lines(vec![Line::Entry(
            FstabEntry::new(
                SourceId::Uuid("11111111-2222-3333-4444-555555555555".into()),
                mount_point,
                "ext4",
                Options::from_vec(vec![FstabOption::Flag("noauto".into())]),
                0,
                2,
            )
            .with_description("rewritten by the atomic writer test"),
        )])
    }

    /// A candidate that fails *structural* validation: two entries claiming
    /// the same mount point.
    fn structurally_invalid_candidate() -> Document {
        Document::from_lines(vec![
            Line::Entry(FstabEntry::new(
                SourceId::Uuid("11111111-2222-3333-4444-555555555555".into()),
                "/mnt/clash",
                "ext4",
                Options::from_vec(vec![FstabOption::Flag("noauto".into())]),
                0,
                2,
            )),
            Line::Entry(FstabEntry::new(
                SourceId::Uuid("22222222-3333-4444-5555-666666666666".into()),
                "/mnt/clash",
                "ext4",
                Options::from_vec(vec![FstabOption::Flag("noauto".into())]),
                0,
                2,
            )),
        ])
    }

    fn write_original(path: &Path) {
        fs::write(path, ORIGINAL).expect("could not seed the test target file");
    }

    /// Writes a hand-made backup of `fstab` into `dir` at a controlled
    /// timestamp, of the kind `manual` selects, and returns its path.
    ///
    /// Built through [`backup_file_name`] rather than by formatting a name
    /// here, so a test can never accidentally assert against a name shape the
    /// production code does not actually produce.
    fn seed_backup_file(dir: &TempDir, secs: u64, manual: bool) -> PathBuf {
        let path = dir
            .path
            .join(backup_file_name(OsStr::new("fstab"), secs, 0, manual));
        let kind = if manual { "manual" } else { "automatic" };
        fs::write(&path, format!("old {kind} backup {secs}\n")).expect("seed a backup file");
        path
    }

    /// Just the paths [`list_backups`] reports for `target`, oldest first.
    fn backup_paths(target: &Path) -> Vec<PathBuf> {
        list_backups(target)
            .expect("listing backups")
            .into_iter()
            .map(|entry| entry.path)
            .collect()
    }

    /// The assertion at the heart of this module's acceptance criteria.
    fn assert_target_untouched(path: &Path) {
        let actual = fs::read(path).expect("the original target file must still exist");
        assert_eq!(
            String::from_utf8_lossy(&actual),
            ORIGINAL,
            "the original target file was modified"
        );
    }

    fn is_findmnt_available() -> bool {
        Command::new("findmnt")
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success())
    }

    // -- happy path ---------------------------------------------------------

    /// Proves the full §6 sequence on the normal path: the target ends up
    /// holding exactly the candidate's serialization, a backup holding the
    /// *old* content is created next to it, the post-rename verification
    /// passes, and nothing else is left in the directory.
    #[test]
    fn happy_path_replaces_target_and_backs_up_old_content() {
        let dir = TempDir::new("happy");
        let target = dir.join("fstab");
        write_original(&target);

        let candidate = valid_candidate("/mnt/new");
        let outcome = write_document(&target, &candidate).expect("the save should succeed");

        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            candidate.to_string(),
            "the target must hold the new content"
        );
        let backup = outcome.backup.expect("a backup must have been created");
        assert_eq!(
            fs::read_to_string(&backup).unwrap(),
            ORIGINAL,
            "the backup must hold the old content"
        );
        assert_eq!(outcome.target, target);
        assert!(outcome.pruned_backups.is_empty());
        assert_eq!(
            dir.entry_names().len(),
            2,
            "only the target and its backup should exist, found {:?}",
            dir.entry_names()
        );
    }

    /// Proves a saved file round-trips back through the parser to the same
    /// document - i.e. the writer serializes via the canonical `Display` impl
    /// and the bytes on disk are a faithful fstab file, not just some blob.
    #[test]
    fn saved_file_reparses_to_an_equal_document() {
        let dir = TempDir::new("roundtrip");
        let target = dir.join("fstab");
        let candidate = valid_candidate("/mnt/roundtrip");

        write_document(&target, &candidate).expect("the save should succeed");

        let reparsed = parser::parse(&fs::read_to_string(&target).unwrap());
        assert_eq!(reparsed, candidate);
    }

    /// Proves the first-ever save works with no target present: the file is
    /// created, no backup is attempted (there is nothing to back up, so no
    /// `CopyBackup`/`ListDir` call is even made), and nothing reports an
    /// error.
    #[test]
    fn first_ever_save_creates_target_without_attempting_a_backup() {
        let dir = TempDir::new("first-save");
        let target = dir.join("fstab");
        assert!(!target.exists());

        let spy = SpyFs::new();
        let candidate = valid_candidate("/mnt/fresh");
        let outcome = write_document_with(&target, &candidate, &spy, &DirectRunner)
            .expect("the first save should succeed");

        assert_eq!(fs::read_to_string(&target).unwrap(), candidate.to_string());
        assert!(
            outcome.backup.is_none(),
            "there was nothing to back up, so no backup must be reported"
        );
        assert!(!spy.ops().contains(&Op::CopyBackup));
        assert!(!spy.ops().contains(&Op::ListDir));
        assert_eq!(dir.entry_names(), vec!["fstab".to_string()]);
    }

    /// Proves a brand-new target gets the conventional `/etc/fstab` mode
    /// (0644) regardless of the process umask, so a save can never produce an
    /// fstab that unprivileged tools cannot read.
    #[test]
    fn new_target_gets_the_conventional_0644_mode() {
        let dir = TempDir::new("new-mode");
        let target = dir.join("fstab");

        write_document(&target, &valid_candidate("/mnt/fresh")).expect("save");

        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, DEFAULT_NEW_FILE_MODE);
    }

    /// Proves the replacement file inherits the previous target's permission
    /// bits rather than the umask default - the temp file is a new inode, so
    /// without this a save could silently change who can read `/etc/fstab`.
    #[test]
    fn existing_target_mode_is_carried_over_to_the_replacement() {
        let dir = TempDir::new("keep-mode");
        let target = dir.join("fstab");
        write_original(&target);
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();

        write_document(&target, &valid_candidate("/mnt/mode")).expect("save");

        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o640);
    }

    /// Proves non-blocking validation warnings do not block a save and are
    /// handed back to the caller: a candidate whose mount point does not
    /// exist on disk yet (a warning per `validate.rs`, and only `[W]`-level
    /// for `findmnt --verify`) saves successfully, and the warning is
    /// reported in the outcome rather than swallowed.
    #[test]
    fn warnings_do_not_block_the_save_and_are_reported() {
        let dir = TempDir::new("warnings");
        let target = dir.join("fstab");
        write_original(&target);

        let candidate = valid_candidate("/mnt/does-not-exist-yet");
        let outcome = write_document(&target, &candidate).expect("warnings must not block a save");

        assert_eq!(fs::read_to_string(&target).unwrap(), candidate.to_string());
        assert!(
            outcome
                .validation_warnings
                .iter()
                .any(|f| f.message.contains("does not currently exist on disk")),
            "the advisory finding must reach the caller, got {:?}",
            outcome.validation_warnings
        );
    }

    // -- the spec-mandated operation ordering -------------------------------

    /// Proves the actual syscall sequence is the one `docs/SPEC.md` §6
    /// prescribes, rather than merely looking right in the source: stat the
    /// target, reserve and copy the backup, prune, write the temp file, set
    /// its mode/owner, `fsync` the temp file, `fsync` the directory, `rename`,
    /// `fsync` the directory again, then read back to verify. Also proves the
    /// temp file is in the target's *own* directory (so the rename is
    /// same-filesystem and therefore atomic) with the `.<name>.rustab.tmp.<pid>`
    /// name the spec asks for.
    #[test]
    fn operation_order_matches_the_spec_sequence() {
        let dir = TempDir::new("order");
        let target = dir.join("fstab");
        write_original(&target);

        let spy = SpyFs::new();
        write_document_with(&target, &valid_candidate("/mnt/order"), &spy, &DirectRunner)
            .expect("save");

        assert_eq!(
            spy.ops(),
            vec![
                Op::SymlinkStat,    // inspect the target
                Op::SymlinkStat,    // reserve an unused backup name
                Op::CopyBackup,     // step 3: back up the old content first
                Op::ListDir,        // step 3: retention
                Op::CreateAndWrite, // step 4: same-directory temp file
                Op::SetMode,
                Op::SetOwner,
                Op::SyncFile, // step 5: fsync the data...
                Op::SyncDir,  // ...then the directory...
                Op::Rename,   // ...then the atomic swap
                Op::SyncDir,  // ...then make the rename itself durable
                Op::Read,     // step 6: verify
            ]
        );

        let temp = PathBuf::from(spy.detail_of(Op::CreateAndWrite));
        assert_eq!(
            temp.parent().unwrap(),
            dir.path,
            "the temp file must live in the target's own directory"
        );
        assert_eq!(
            temp.file_name().unwrap().to_string_lossy(),
            format!(".fstab.rustab.tmp.{}", std::process::id())
        );
    }

    // -- (a) structural validation failure ----------------------------------

    /// Acceptance (a): a structurally invalid candidate is rejected with
    /// `WriteError::Validation` and **nothing touches disk at all** - proven
    /// by the target's bytes being unchanged *and* by the spy having recorded
    /// zero filesystem operations, so not even a stat, backup, or temp file
    /// happened.
    #[test]
    fn structural_validation_failure_touches_nothing() {
        let dir = TempDir::new("invalid-structural");
        let target = dir.join("fstab");
        write_original(&target);

        let spy = SpyFs::new();
        let err = write_document_with(
            &target,
            &structurally_invalid_candidate(),
            &spy,
            &DirectRunner,
        )
        .expect_err("a duplicate mount point must be rejected");

        match &err {
            WriteError::Validation(report) => assert!(report
                .errors()
                .any(|f| f.message.contains("/mnt/clash") && f.message.contains("unique"))),
            other => panic!("expected a validation error, got {other:?}"),
        }
        assert!(
            spy.ops().is_empty(),
            "no filesystem operation may happen before validation passes, saw {:?}",
            spy.ops()
        );
        assert_target_untouched(&target);
        assert_eq!(dir.entry_names(), vec!["fstab".to_string()]);
    }

    // -- (b) semantic check failure -----------------------------------------

    /// Acceptance (b): a candidate that `findmnt --verify` rejects (a
    /// boot-required entry whose source cannot be resolved) is refused with
    /// `WriteError::Validation`, with the target's bytes unchanged and no
    /// temp file or backup created.
    ///
    /// Skipped on machines without `findmnt`, where `validate()` correctly
    /// degrades that check to a non-blocking warning; the degraded path
    /// itself is covered by `validate.rs`'s own unit tests.
    #[test]
    fn semantic_validation_failure_leaves_target_untouched() {
        if !is_findmnt_available() {
            eprintln!("skipping: findmnt is not available on this machine");
            return;
        }

        let dir = TempDir::new("invalid-semantic");
        let target = dir.join("fstab");
        write_original(&target);

        // Auto-mounted (no `noauto`) with a UUID no device has: `findmnt
        // --verify` reports this as an error, while every structural check
        // here passes.
        let candidate = Document::from_lines(vec![Line::Entry(FstabEntry::new(
            SourceId::Uuid("deadbeef-0000-0000-0000-000000000000".into()),
            "/mnt/unreachable",
            "ext4",
            Options::from_vec(vec![FstabOption::Flag("defaults".into())]),
            0,
            2,
        ))]);
        let report = validate::validate(&candidate);
        assert!(
            report.has_errors(),
            "precondition: findmnt --verify should reject this candidate, report: {report:?}"
        );

        let spy = SpyFs::new();
        let err = write_document_with(&target, &candidate, &spy, &DirectRunner)
            .expect_err("a semantically invalid candidate must be rejected");

        assert!(matches!(err, WriteError::Validation(_)), "got {err:?}");
        assert!(spy.ops().is_empty());
        assert_target_untouched(&target);
        assert_eq!(dir.entry_names(), vec!["fstab".to_string()]);
    }

    /// The runner given to [`write_document_with_runner`] really is the one
    /// step 1 verifies through: its verdict blocks the save, and it is
    /// handed the exact bytes the save would have written. Needs no
    /// `findmnt` on this machine, by construction.
    #[test]
    fn the_given_runner_is_the_one_step_one_verifies_through() {
        let dir = TempDir::new("runner-seam");
        let target = dir.join("fstab");
        write_original(&target);

        let candidate = valid_candidate("/mnt/seam");
        let runner = crate::exec::RecordingRunner::answering(
            1,
            "/mnt/seam\n   [E] unreachable on boot required source: UUID=…\n\n\
             0 parse errors, 1 error, 0 warnings\n",
        );

        let err = write_document_with_runner(&target, &candidate, &runner)
            .expect_err("the runner's verdict must block the save");

        assert!(matches!(err, WriteError::Validation(_)), "got {err:?}");
        let runs = runner.runs();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].program, "findmnt");
        assert_eq!(
            runs[0].input.as_deref(),
            Some(candidate.to_string().as_bytes())
        );
        assert_target_untouched(&target);
        assert_eq!(dir.entry_names(), vec!["fstab".to_string()]);
    }

    // -- (c) write failure --------------------------------------------------

    /// Acceptance (c): an I/O error while writing the temp file (injected
    /// after a *partial* write, as a full disk would behave) leaves the target
    /// byte-for-byte unchanged and the half-written temp file removed. Proven
    /// by re-reading the target and by the directory containing only the
    /// target and its backup - no `.fstab.rustab.tmp.*` survivor.
    #[test]
    fn injected_write_failure_leaves_target_untouched_and_cleans_up() {
        let dir = TempDir::new("fail-write");
        let target = dir.join("fstab");
        write_original(&target);

        let spy = SpyFs::failing_at(Op::CreateAndWrite);
        let err = write_document_with(&target, &valid_candidate("/mnt/never"), &spy, &DirectRunner)
            .expect_err("the injected write failure must propagate");

        assert!(matches!(err, WriteError::Write { .. }), "got {err:?}");
        assert_target_untouched(&target);
        assert!(
            spy.ops().contains(&Op::RemoveFile),
            "the leftover temp file must be cleaned up"
        );
        let names = dir.entry_names();
        assert!(
            !names.iter().any(|n| n.contains("rustab.tmp")),
            "no temp file may be left behind, found {names:?}"
        );
        assert_eq!(names.len(), 2, "only the target and its backup: {names:?}");
    }

    /// Proves a failure to apply the previous target's permission bits aborts
    /// the save before the rename, leaving the target untouched - a save must
    /// never go through with the wrong mode on `/etc/fstab`.
    #[test]
    fn injected_set_mode_failure_leaves_target_untouched() {
        let dir = TempDir::new("fail-mode");
        let target = dir.join("fstab");
        write_original(&target);

        let spy = SpyFs::failing_at(Op::SetMode);
        let err = write_document_with(&target, &valid_candidate("/mnt/never"), &spy, &DirectRunner)
            .expect_err("the injected fchmod failure must propagate");

        assert!(matches!(err, WriteError::Metadata { .. }), "got {err:?}");
        assert_target_untouched(&target);
        assert!(!dir.entry_names().iter().any(|n| n.contains("rustab.tmp")));
    }

    /// Proves an `fsync` failure on the temp file aborts *before* the rename,
    /// so unflushed content is never published under the target's name.
    #[test]
    fn injected_temp_fsync_failure_leaves_target_untouched() {
        let dir = TempDir::new("fail-fsync-file");
        let target = dir.join("fstab");
        write_original(&target);

        let spy = SpyFs::failing_at(Op::SyncFile);
        let err = write_document_with(&target, &valid_candidate("/mnt/never"), &spy, &DirectRunner)
            .expect_err("the injected fsync failure must propagate");

        assert!(
            matches!(
                err,
                WriteError::Fsync {
                    stage: FsyncStage::TempFile,
                    ..
                }
            ),
            "got {err:?}"
        );
        assert!(!spy.ops().contains(&Op::Rename), "the rename must not run");
        assert_target_untouched(&target);
        assert!(!dir.entry_names().iter().any(|n| n.contains("rustab.tmp")));
    }

    /// Proves a failure of the *pre-rename* directory `fsync` also aborts
    /// before the rename, with the target untouched.
    #[test]
    fn injected_pre_rename_dir_fsync_failure_leaves_target_untouched() {
        let dir = TempDir::new("fail-fsync-dir");
        let target = dir.join("fstab");
        write_original(&target);

        let spy = SpyFs::failing_at_nth(Op::SyncDir, 1);
        let err = write_document_with(&target, &valid_candidate("/mnt/never"), &spy, &DirectRunner)
            .expect_err("the injected directory fsync failure must propagate");

        assert!(
            matches!(
                err,
                WriteError::Fsync {
                    stage: FsyncStage::DirectoryBeforeRename,
                    ..
                }
            ),
            "got {err:?}"
        );
        assert!(!spy.ops().contains(&Op::Rename));
        assert_target_untouched(&target);
        assert!(!dir.entry_names().iter().any(|n| n.contains("rustab.tmp")));
    }

    // -- (d) rename failure -------------------------------------------------

    /// Acceptance (d): an I/O error at the atomic rename - with a complete,
    /// fully-`fsync`ed temp file already sitting next to the target - leaves
    /// the target byte-for-byte unchanged and the temp file removed. This is
    /// the all-or-nothing hinge: the new content was entirely ready and still
    /// never became visible.
    #[test]
    fn injected_rename_failure_leaves_target_untouched_and_cleans_up() {
        let dir = TempDir::new("fail-rename");
        let target = dir.join("fstab");
        write_original(&target);

        let spy = SpyFs::failing_at(Op::Rename);
        let err = write_document_with(&target, &valid_candidate("/mnt/never"), &spy, &DirectRunner)
            .expect_err("the injected rename failure must propagate");

        match &err {
            WriteError::Rename { to, .. } => assert_eq!(to, &target),
            other => panic!("expected a rename error, got {other:?}"),
        }
        assert_target_untouched(&target);
        let names = dir.entry_names();
        assert!(
            !names.iter().any(|n| n.contains("rustab.tmp")),
            "no temp file may be left behind, found {names:?}"
        );
        assert_eq!(names.len(), 2, "only the target and its backup: {names:?}");
    }

    // -- post-rename failure modes ------------------------------------------

    /// Proves the §6 step-7 re-read is a real check and a hard error: when
    /// the read-back content differs from what was written, the save is
    /// reported as `WriteError::Verify` (carrying the backup path for
    /// recovery) rather than being trusted or downgraded to a warning.
    #[test]
    fn verify_mismatch_is_a_hard_error() {
        let dir = TempDir::new("verify-mismatch");
        let target = dir.join("fstab");
        write_original(&target);

        let spy = SpyFs::with_corrupt_readback("something else entirely\n");
        let err = write_document_with(
            &target,
            &valid_candidate("/mnt/verify"),
            &spy,
            &DirectRunner,
        )
        .expect_err("a read-back mismatch must be a hard error");

        match &err {
            WriteError::Verify {
                target: verified,
                backup,
                expected,
                actual,
            } => {
                assert_eq!(verified, &target);
                assert!(backup.is_some(), "the backup is needed for recovery");
                assert_eq!(expected, &valid_candidate("/mnt/verify").to_string());
                assert_eq!(actual, "something else entirely\n");
            }
            other => panic!("expected a verify error, got {other:?}"),
        }
    }

    /// Proves a failed *post-rename* directory `fsync` is surfaced (durability
    /// unconfirmed) while being correctly distinguished from the pre-rename
    /// failures: the swap did happen, so the target holds the new content, and
    /// the cleanup of the no-longer-existing temp file does not disturb it.
    #[test]
    fn post_rename_dir_fsync_failure_is_reported_but_the_swap_already_happened() {
        let dir = TempDir::new("fail-fsync-dir2");
        let target = dir.join("fstab");
        write_original(&target);

        let candidate = valid_candidate("/mnt/after");
        let spy = SpyFs::failing_at_nth(Op::SyncDir, 2);
        let err = write_document_with(&target, &candidate, &spy, &DirectRunner)
            .expect_err("an unconfirmed-durability save must not be reported as success");

        assert!(
            matches!(
                err,
                WriteError::Fsync {
                    stage: FsyncStage::DirectoryAfterRename,
                    ..
                }
            ),
            "got {err:?}"
        );
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            candidate.to_string(),
            "the rename had already succeeded, so the new content is live and intact"
        );
    }

    // -- backup failures ----------------------------------------------------

    /// Proves a failure to create the backup aborts the save before anything
    /// else is written: the target is untouched and no temp file was even
    /// created, so the user is never left without a restore path for a change
    /// that did go through.
    #[test]
    fn injected_backup_failure_leaves_target_untouched_and_writes_nothing() {
        let dir = TempDir::new("fail-backup");
        let target = dir.join("fstab");
        write_original(&target);

        let spy = SpyFs::failing_at(Op::CopyBackup);
        let err = write_document_with(&target, &valid_candidate("/mnt/never"), &spy, &DirectRunner)
            .expect_err("the injected backup failure must propagate");

        assert!(matches!(err, WriteError::Backup { .. }), "got {err:?}");
        assert!(!spy.ops().contains(&Op::CreateAndWrite));
        assert!(!spy.ops().contains(&Op::Rename));
        assert_target_untouched(&target);
        assert_eq!(dir.entry_names(), vec!["fstab".to_string()]);
    }

    /// Proves a failure to *prune* old backups is deliberately non-fatal: the
    /// save still completes (pruning touches neither the target nor the new
    /// backup) and the problem is reported as a note instead of being
    /// silently swallowed.
    #[test]
    fn prune_failure_is_a_note_not_an_error() {
        let dir = TempDir::new("prune-fail");
        let target = dir.join("fstab");
        write_original(&target);

        let candidate = valid_candidate("/mnt/prune");
        let spy = SpyFs::failing_at(Op::ListDir);
        let outcome = write_document_with(&target, &candidate, &spy, &DirectRunner)
            .expect("a pruning problem must not fail the save");

        assert_eq!(fs::read_to_string(&target).unwrap(), candidate.to_string());
        assert!(
            outcome
                .notes
                .iter()
                .any(|n| n.contains("prune old backups")),
            "the pruning failure must be surfaced, notes: {:?}",
            outcome.notes
        );
    }

    // -- backup retention ---------------------------------------------------

    /// Proves the retention bound: with `MAX_BACKUPS + 2` pre-existing
    /// backups plus the one this save creates, exactly `MAX_BACKUPS` remain,
    /// the pruned ones are the oldest by timestamp, and the surviving set is
    /// precisely the newest `MAX_BACKUPS`.
    #[test]
    fn backup_retention_prunes_the_oldest_beyond_the_bound() {
        let dir = TempDir::new("retention");
        let target = dir.join("fstab");
        write_original(&target);

        // Hand-made backups with controlled, strictly increasing timestamps.
        let preexisting: Vec<PathBuf> = (1..=(MAX_BACKUPS as u64 + 2))
            .map(|i| seed_backup_file(&dir, i, false))
            .collect();

        let outcome = write_document(&target, &valid_candidate("/mnt/retained")).expect("save");

        let remaining = backup_paths(&target);
        assert_eq!(
            remaining.len(),
            MAX_BACKUPS,
            "exactly MAX_BACKUPS backups must survive, found {remaining:?}"
        );
        // Three too many existed (MAX+2 old, +1 new), so the three oldest go.
        assert_eq!(outcome.pruned_backups.len(), 3);
        assert_eq!(outcome.pruned_backups, preexisting[..3].to_vec());
        for gone in &preexisting[..3] {
            assert!(!gone.exists(), "{} should have been pruned", gone.display());
        }
        for kept in &preexisting[3..] {
            assert!(kept.exists(), "{} should have been kept", kept.display());
        }
        let newest = outcome.backup.unwrap();
        assert!(newest.exists());
        assert_eq!(remaining.last().unwrap(), &newest, "sorted oldest first");
    }

    /// Proves repeated real saves also stay within the bound (the retention
    /// test above uses hand-made names; this one drives it entirely through
    /// `write_document`, which also proves backup names never collide).
    #[test]
    fn repeated_saves_stay_within_the_retention_bound() {
        let dir = TempDir::new("retention-live");
        let target = dir.join("fstab");
        write_original(&target);

        let mut created = Vec::new();
        for i in 0..(MAX_BACKUPS + 3) {
            let outcome = write_document(&target, &valid_candidate(&format!("/mnt/n{i}")))
                .unwrap_or_else(|e| panic!("save {i} failed: {e}"));
            created.push(
                outcome
                    .backup
                    .expect("each save backs up the previous file"),
            );
        }

        assert_eq!(created.len(), MAX_BACKUPS + 3);
        let unique: std::collections::BTreeSet<&PathBuf> = created.iter().collect();
        assert_eq!(unique.len(), created.len(), "backup names must be unique");
        assert_eq!(list_backups(&target).unwrap().len(), MAX_BACKUPS);
    }

    /// **The retention policy's product decision, asserted.**
    ///
    /// A manual backup - here, the *oldest* file of either kind, so a
    /// kind-blind prune would delete it first - survives a save that prunes
    /// automatic backups past the bound. If this ever regresses, a user's
    /// deliberate "snapshot now" would be silently thrown away by unrelated
    /// later saves, which is exactly what the exemption exists to prevent.
    #[test]
    fn a_manual_backup_is_never_pruned_by_a_save() {
        let dir = TempDir::new("retention-manual");
        let target = dir.join("fstab");
        write_original(&target);

        // Timestamp 1: older than every automatic backup below.
        let manual = seed_backup_file(&dir, 1, true);
        let automatic: Vec<PathBuf> = (2..=(MAX_BACKUPS as u64 + 3))
            .map(|i| seed_backup_file(&dir, i, false))
            .collect();

        let outcome = write_document(&target, &valid_candidate("/mnt/kept")).expect("save");

        assert!(
            manual.exists(),
            "the manual backup must survive, however much pruning happens"
        );
        assert_eq!(
            fs::read_to_string(&manual).unwrap(),
            "old manual backup 1\n",
            "and its content must be untouched"
        );
        assert!(
            !outcome.pruned_backups.contains(&manual),
            "a manual backup must never even be reported as pruned: {:?}",
            outcome.pruned_backups
        );
        // MAX+2 automatics existed and the save added one, so exactly three
        // automatics are excess - the manual one is not part of that count.
        assert_eq!(outcome.pruned_backups.len(), 3);
        assert_eq!(outcome.pruned_backups, automatic[..3].to_vec());

        let remaining = list_backups(&target).expect("listing backups");
        assert_eq!(
            remaining.iter().filter(|entry| !entry.manual).count(),
            MAX_BACKUPS,
            "the automatic bound still holds exactly"
        );
        assert_eq!(
            remaining
                .iter()
                .filter(|entry| entry.manual)
                .map(|entry| entry.path.clone())
                .collect::<Vec<_>>(),
            vec![manual],
            "and the manual backup is still listed, still flagged manual"
        );
    }

    /// The other half of the exemption: manual backups are not *counted*
    /// either, so a target holding many of them still keeps its full
    /// complement of automatic backups instead of having them evicted.
    ///
    /// Without this, a user taking a few snapshots would quietly destroy their
    /// own automatic safety net.
    #[test]
    fn manual_backups_do_not_count_towards_the_automatic_bound() {
        let dir = TempDir::new("retention-manual-count");
        let target = dir.join("fstab");
        write_original(&target);

        // Far more manual backups than the bound, plus one automatic short of
        // it, so a kind-blind prune would delete several automatics.
        let manual: Vec<PathBuf> = (1..=(MAX_BACKUPS as u64 * 2))
            .map(|i| seed_backup_file(&dir, i, true))
            .collect();
        let automatic: Vec<PathBuf> = (100..100 + (MAX_BACKUPS as u64 - 1))
            .map(|i| seed_backup_file(&dir, i, false))
            .collect();

        let outcome = write_document(&target, &valid_candidate("/mnt/counted")).expect("save");

        assert!(
            outcome.pruned_backups.is_empty(),
            "the save's own backup brings the automatic count to exactly \
             MAX_BACKUPS, so nothing is excess: {:?}",
            outcome.pruned_backups
        );
        for path in manual.iter().chain(automatic.iter()) {
            assert!(path.exists(), "{} must still exist", path.display());
        }
        let remaining = list_backups(&target).expect("listing backups");
        assert_eq!(remaining.iter().filter(|e| e.manual).count(), manual.len());
        assert_eq!(remaining.iter().filter(|e| !e.manual).count(), MAX_BACKUPS);
    }

    /// Proves pruning can only ever delete Rustab's own backups *for this
    /// exact target*: decoy files that merely resemble a backup name - a
    /// different target's backup, a non-numeric timestamp, a wrong-width
    /// timestamp, an extra suffix, a hand-made `fstab.bak` - all survive a
    /// save that prunes.
    #[test]
    fn pruning_never_deletes_look_alike_files() {
        let dir = TempDir::new("decoys");
        let target = dir.join("fstab");
        write_original(&target);

        let decoys = [
            "fstab.bak",
            "fstab.rustab-backup.notanumber",
            "fstab.rustab-backup.0000000001.00000000", // 8 nano digits
            "fstab.rustab-backup.0000000001.000000000.old", // extra suffix
            "fstab2.rustab-backup.0000000001.000000000", // different target
            "other.rustab-backup.0000000001.000000000", // different target
        ];
        for decoy in decoys {
            fs::write(dir.join(decoy), "do not delete me\n").unwrap();
        }
        // Enough real backups that this save must prune.
        for i in 1..=(MAX_BACKUPS as u64 + 2) {
            seed_backup_file(&dir, i, false);
        }

        let outcome = write_document(&target, &valid_candidate("/mnt/decoy")).expect("save");

        assert_eq!(outcome.pruned_backups.len(), 3);
        for decoy in decoys {
            let path = dir.join(decoy);
            assert!(path.exists(), "{decoy} must not have been pruned");
            assert_eq!(fs::read_to_string(&path).unwrap(), "do not delete me\n");
        }
    }

    /// Unit-level proof of the strictness of the backup-name predicate that
    /// `pruning_never_deletes_look_alike_files` exercises end to end.
    #[test]
    fn backup_timestamp_parsing_is_strict() {
        let target = OsStr::new("fstab");
        assert_eq!(
            backup_name_of(
                OsStr::new("fstab.rustab-backup.0000000012.000000034"),
                target
            ),
            Some(BackupName {
                secs: 12,
                nanos: 34,
                manual: false,
            })
        );
        for bad in [
            "fstab",
            "fstab.bak",
            "fstab.rustab-backup.",
            "fstab.rustab-backup.12.34",
            "fstab.rustab-backup.0000000012.00000003",
            "fstab.rustab-backup.0000000012.000000034.gz",
            "fstab.rustab-backup.000000001x.000000034",
            "fstab2.rustab-backup.0000000012.000000034",
            "xfstab.rustab-backup.0000000012.000000034",
        ] {
            assert_eq!(
                backup_name_of(OsStr::new(bad), target),
                None,
                "{bad} must not be treated as a backup of fstab"
            );
        }
    }

    /// The companion of `backup_timestamp_parsing_is_strict` for the one new
    /// shape the grammar gained: `.manual` is accepted **only** spelled
    /// exactly that way, exactly once, directly after an otherwise-valid
    /// timestamp.
    ///
    /// This is the test that keeps the grammar extension from quietly becoming
    /// "anything after the nanoseconds is fine": every rejected case below is
    /// a file `prune_backups` and `list_backups` must continue to treat as
    /// somebody else's, exactly as they did before `.manual` existed.
    #[test]
    fn manual_suffix_is_the_only_accepted_name_extension() {
        let target = OsStr::new("fstab");

        // The one new accepted shape, and the old one, side by side.
        assert_eq!(
            backup_name_of(
                OsStr::new("fstab.rustab-backup.0000000012.000000034.manual"),
                target
            ),
            Some(BackupName {
                secs: 12,
                nanos: 34,
                manual: true,
            })
        );
        assert_eq!(
            backup_name_of(
                OsStr::new("fstab.rustab-backup.0000000012.000000034"),
                target
            )
            .map(|name| name.manual),
            Some(false),
            "an automatic backup's name is unchanged, byte for byte"
        );

        for bad in [
            // Wrong case, in each of the plausible spellings.
            "fstab.rustab-backup.0000000012.000000034.Manual",
            "fstab.rustab-backup.0000000012.000000034.MANUAL",
            // Doubled, or glued to another suffix.
            "fstab.rustab-backup.0000000012.000000034.manual.manual",
            "fstab.rustab-backup.0000000012.000000034.manual.gz",
            "fstab.rustab-backup.0000000012.000000034.gz.manual",
            // Extra characters on either side of the suffix itself.
            "fstab.rustab-backup.0000000012.000000034.manually",
            "fstab.rustab-backup.0000000012.000000034.manual ",
            "fstab.rustab-backup.0000000012.000000034manual",
            "fstab.rustab-backup.0000000012.000000034..manual",
            "fstab.rustab-backup.0000000012.000000034.-manual",
            // The suffix cannot rescue a timestamp that was never valid:
            // wrong widths, non-numeric components, a missing component.
            "fstab.rustab-backup.0000000012.00000003.manual",
            "fstab.rustab-backup.12.34.manual",
            "fstab.rustab-backup.000000001x.000000034.manual",
            "fstab.rustab-backup.aaaaaaaaaa.bbbbbbbbb.manual",
            "fstab.rustab-backup.0000000012.manual",
            "fstab.rustab-backup..manual",
            "fstab.rustab-backup.manual",
            // ...nor a prefix belonging to a different target.
            "fstab2.rustab-backup.0000000012.000000034.manual",
            "xfstab.rustab-backup.0000000012.000000034.manual",
            // ...nor the suffix in the wrong position.
            "fstab.manual.rustab-backup.0000000012.000000034",
            "fstab.rustab-backup.manual.0000000012.000000034",
        ] {
            assert_eq!(
                backup_name_of(OsStr::new(bad), target),
                None,
                "{bad} must not be treated as a backup of fstab"
            );
        }
    }

    /// Proves [`backup_file_name`] and [`backup_name_of`] are inverses for
    /// both kinds - the property that keeps a backup Rustab just wrote from
    /// being unrecognizable to the code that lists and prunes it.
    #[test]
    fn written_backup_names_parse_back_to_what_was_written() {
        let target = OsStr::new("fstab");
        for manual in [false, true] {
            let name = backup_file_name(target, 12, 34, manual);
            assert_eq!(
                backup_name_of(&name, target),
                Some(BackupName {
                    secs: 12,
                    nanos: 34,
                    manual,
                }),
                "{name:?} must parse back to the name it was built from"
            );
        }
        assert_eq!(
            backup_file_name(target, 12, 34, true).to_string_lossy(),
            "fstab.rustab-backup.0000000012.000000034.manual",
            "the manual shape is exactly the automatic one plus one suffix"
        );
    }

    // -- target path handling -----------------------------------------------

    /// Proves a symlinked target is written *through* rather than replaced:
    /// the link survives as a link, the file it points at gets the new
    /// content, the backup is made next to that real file, and the
    /// redirection is reported as a note. Replacing the link would leave the
    /// real `/etc/fstab` holding stale content - the boot-breaking outcome
    /// SPEC §2.1 forbids.
    #[test]
    fn symlinked_target_is_written_through_to_its_destination() {
        let dir = TempDir::new("symlink");
        let real = dir.join("fstab.real");
        let link = dir.join("fstab");
        write_original(&real);
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let candidate = valid_candidate("/mnt/through");
        let outcome = write_document(&link, &candidate).expect("save through the symlink");

        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the symlink itself must not be replaced"
        );
        assert_eq!(fs::read_to_string(&real).unwrap(), candidate.to_string());
        assert_eq!(outcome.target, fs::canonicalize(&real).unwrap());
        let backup = outcome.backup.expect("backup");
        assert_eq!(fs::read_to_string(&backup).unwrap(), ORIGINAL);
        assert!(backup
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("fstab.real.rustab-backup."));
        assert!(outcome.notes.iter().any(|n| n.contains("symlink")));
    }

    /// Proves a dangling symlink is rejected up front instead of being
    /// quietly turned into a regular file.
    #[test]
    fn dangling_symlink_target_is_rejected() {
        let dir = TempDir::new("dangling");
        let link = dir.join("fstab");
        std::os::unix::fs::symlink(dir.join("nowhere"), &link).unwrap();

        let err = write_document(&link, &valid_candidate("/mnt/nope"))
            .expect_err("a dangling symlink must be rejected");

        assert!(
            matches!(err, WriteError::InvalidTarget { .. }),
            "got {err:?}"
        );
    }

    /// Proves a directory (or any path with no usable file-name component) is
    /// rejected before any write is attempted.
    #[test]
    fn directory_target_is_rejected() {
        let dir = TempDir::new("dir-target");
        let err = write_document(&dir.path, &valid_candidate("/mnt/nope"))
            .expect_err("a directory must be rejected as a target");
        assert!(
            matches!(err, WriteError::InvalidTarget { .. }),
            "got {err:?}"
        );

        let err = write_document(Path::new("/"), &valid_candidate("/mnt/nope"))
            .expect_err("the root directory must be rejected as a target");
        assert!(
            matches!(err, WriteError::InvalidTarget { .. }),
            "got {err:?}"
        );
    }

    /// Proves a nonexistent parent directory fails cleanly at the temp-file
    /// write (the first operation that needs the directory) rather than
    /// panicking or half-doing something.
    #[test]
    fn missing_parent_directory_fails_cleanly() {
        let dir = TempDir::new("no-parent");
        let target = dir.join("missing-subdir").join("fstab");

        let err = write_document(&target, &valid_candidate("/mnt/nope"))
            .expect_err("a missing parent directory must fail");
        assert!(matches!(err, WriteError::Write { .. }), "got {err:?}");
    }

    /// Proves `list_backups` reports only this target's backups, oldest
    /// first, reports both kinds in one chronological list with the kind
    /// attached, and reports an empty list (not an error) for a target that
    /// has none.
    #[test]
    fn list_backups_is_sorted_and_scoped_to_the_target() {
        let dir = TempDir::new("list");
        let target = dir.join("fstab");
        write_original(&target);
        assert!(list_backups(&target).unwrap().is_empty());

        // Deliberately out of order, and deliberately mixing the two kinds so
        // the manual one is neither first nor last: the order is by timestamp
        // alone, never by kind.
        for (secs, nanos, manual) in [
            (3u64, 0u32, false),
            (1, 500, false),
            (1, 100, false),
            (2, 0, true),
        ] {
            fs::write(
                dir.path
                    .join(backup_file_name(OsStr::new("fstab"), secs, nanos, manual)),
                "x",
            )
            .unwrap();
        }
        fs::write(
            dir.path
                .join(backup_file_name(OsStr::new("other"), 9, 0, false)),
            "x",
        )
        .unwrap();

        let listed: Vec<(String, bool)> = list_backups(&target)
            .unwrap()
            .iter()
            .map(|entry| {
                (
                    entry
                        .path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    entry.manual,
                )
            })
            .collect();
        assert_eq!(
            listed,
            vec![
                (
                    "fstab.rustab-backup.0000000001.000000100".to_string(),
                    false
                ),
                (
                    "fstab.rustab-backup.0000000001.000000500".to_string(),
                    false
                ),
                (
                    "fstab.rustab-backup.0000000002.000000000.manual".to_string(),
                    true
                ),
                (
                    "fstab.rustab-backup.0000000003.000000000".to_string(),
                    false
                ),
            ]
        );
    }

    /// `list_backups_among` is the grammar on its own, so it is asserted
    /// against names a directory listing never has to have produced - and
    /// against `list_backups` itself, on a directory seeded with those exact
    /// names, so the two can never drift apart.
    #[test]
    fn list_backups_among_classifies_names_the_way_list_backups_does() {
        let dir = TempDir::new("among");
        let target = dir.join("fstab");
        let names = [
            // Valid, deliberately out of chronological order.
            "fstab.rustab-backup.0000000003.000000000",
            "fstab.rustab-backup.0000000001.000000002",
            "fstab.rustab-backup.0000000002.000000000.manual",
            // Near misses, every one of which must be dropped.
            "fstab.rustab-backup.1.2",
            "fstab.rustab-backup.0000000001.000000002.old",
            "fstab.rustab-backup.aaaaaaaaaa.bbbbbbbbb",
            "other.rustab-backup.0000000001.000000002",
            "fstab.bak",
            "fstab",
            // One name holding a newline: it is a single name, so it must be
            // rejected whole rather than read as the two valid names it
            // textually contains.
            "fstab.rustab-backup.0000000004.000000000\nfstab.rustab-backup.0000000005.000000000",
        ];
        let os_names: Vec<OsString> = names.iter().map(OsString::from).collect();

        let among = list_backups_among(&target, &os_names);
        assert_eq!(
            among,
            vec![
                BackupEntry {
                    path: dir.join("fstab.rustab-backup.0000000001.000000002"),
                    manual: false,
                },
                BackupEntry {
                    path: dir.join("fstab.rustab-backup.0000000002.000000000.manual"),
                    manual: true,
                },
                BackupEntry {
                    path: dir.join("fstab.rustab-backup.0000000003.000000000"),
                    manual: false,
                },
            ]
        );

        for name in names {
            fs::write(dir.join(name), "x").expect("seed the name");
        }
        assert_eq!(list_backups(&target).expect("listing"), among);

        // A target with no file-name component has no backups rather than an
        // error, on this entry point too.
        assert_eq!(list_backups_among(Path::new("/"), &os_names), Vec::new());
    }

    /// Proves the temp file name is derived from the target's own name (so two
    /// targets in one directory cannot collide) and carries this process's
    /// pid.
    #[test]
    fn temp_file_name_is_hidden_target_specific_and_pid_tagged() {
        let name = temp_file_name(OsStr::new("fstab"));
        assert_eq!(
            name.to_string_lossy(),
            format!(".fstab.rustab.tmp.{}", std::process::id())
        );
        assert_ne!(name, temp_file_name(OsStr::new("fstab.sandbox")));
    }

    /// Proves an empty document is a legal save (it serializes to zero bytes)
    /// and still goes through the full verified sequence - an edge case that
    /// must not be special-cased into a silent no-op.
    #[test]
    fn empty_document_saves_and_verifies() {
        let dir = TempDir::new("empty-doc");
        let target = dir.join("fstab");
        write_original(&target);

        let outcome =
            write_document(&target, &Document::new()).expect("an empty document is valid");

        assert_eq!(fs::read_to_string(&target).unwrap(), "");
        assert_eq!(
            fs::read_to_string(outcome.backup.unwrap()).unwrap(),
            ORIGINAL
        );
    }

    // -- manual backups (`create_backup`) -----------------------------------

    /// The core claim of [`create_backup`]: the target's current content is
    /// copied to a *manual* backup beside it, and the target itself is never
    /// written - no temp file, no rename, not even a mode change.
    #[test]
    fn create_backup_copies_the_target_and_never_touches_it() {
        let dir = TempDir::new("manual-ok");
        let target = dir.join("fstab");
        write_original(&target);
        let before = fs::metadata(&target).expect("stat the target");

        let outcome = create_backup(&target).expect("a manual backup of an existing target");

        assert_eq!(outcome.target, target);
        assert_eq!(fs::read_to_string(&outcome.backup).unwrap(), ORIGINAL);
        assert!(
            outcome
                .backup
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("fstab.rustab-backup."),
            "the backup belongs to this target: {:?}",
            outcome.backup
        );
        assert!(
            outcome.backup.to_string_lossy().ends_with(".manual"),
            "a backup taken through create_backup is manual: {:?}",
            outcome.backup
        );
        assert!(outcome.pruned_backups.is_empty());

        // The target is byte-for-byte what it was, and so is its mtime: this
        // operation only ever *reads* it.
        assert_target_untouched(&target);
        assert_eq!(
            fs::metadata(&target).unwrap().modified().unwrap(),
            before.modified().unwrap(),
            "the target must not even have been opened for writing"
        );
        // Nothing else was created - in particular no temp file.
        assert_eq!(
            dir.entry_names(),
            vec![
                "fstab".to_string(),
                outcome
                    .backup
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            ]
        );

        // And the module's own listing agrees about what it just made.
        let listed = list_backups(&target).expect("listing");
        assert_eq!(
            listed,
            vec![BackupEntry {
                path: outcome.backup.clone(),
                manual: true,
            }]
        );
    }

    /// Repeated manual backups never collide and never overwrite each other -
    /// the same guarantee repeated saves get, through the same reservation.
    #[test]
    fn repeated_manual_backups_get_distinct_names() {
        let dir = TempDir::new("manual-repeat");
        let target = dir.join("fstab");
        write_original(&target);

        let first = create_backup(&target).expect("first").backup;
        fs::write(&target, "# changed\n").expect("change the target");
        let second = create_backup(&target).expect("second").backup;

        assert_ne!(first, second);
        assert_eq!(fs::read_to_string(&first).unwrap(), ORIGINAL);
        assert_eq!(fs::read_to_string(&second).unwrap(), "# changed\n");
        let listed = list_backups(&target).expect("listing");
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().all(|entry| entry.manual));
    }

    /// A target that does not exist yet is its own, explicit error: an
    /// explicit "back this up now" must never resolve as a success that
    /// produced nothing.
    #[test]
    fn create_backup_refuses_a_target_that_does_not_exist() {
        let dir = TempDir::new("manual-missing");
        let target = dir.join("fstab");

        let err = create_backup(&target).expect_err("there is nothing to back up");

        assert!(
            matches!(err, CreateBackupError::TargetMissing { .. }),
            "got {err:?}"
        );
        assert!(
            err.to_string().contains("nothing to back up"),
            "the message must say why: {err}"
        );
        assert!(
            dir.entry_names().is_empty(),
            "nothing may be created on this path: {:?}",
            dir.entry_names()
        );
    }

    /// A symlinked target is followed through exactly as a save follows it:
    /// the backup is made next to the *real* file, the link is untouched, and
    /// the redirection is reported as a note.
    #[test]
    fn create_backup_follows_a_symlinked_target() {
        let dir = TempDir::new("manual-symlink");
        let real = dir.join("fstab.real");
        let link = dir.join("fstab");
        write_original(&real);
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let outcome = create_backup(&link).expect("a manual backup through the symlink");

        assert_eq!(outcome.target, fs::canonicalize(&real).unwrap());
        assert!(outcome
            .backup
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("fstab.real.rustab-backup."));
        assert_eq!(fs::read_to_string(&outcome.backup).unwrap(), ORIGINAL);
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the symlink itself must still be a symlink"
        );
        assert!(outcome.notes.iter().any(|note| note.contains("symlink")));
    }

    /// A directory (or any path that cannot be a target) is rejected up front,
    /// through the same check a save applies.
    #[test]
    fn create_backup_rejects_a_directory_target() {
        let dir = TempDir::new("manual-dir");
        let err = create_backup(&dir.path).expect_err("a directory cannot be backed up");
        assert!(
            matches!(err, CreateBackupError::InvalidTarget { .. }),
            "got {err:?}"
        );
        assert!(dir.entry_names().is_empty());
    }

    /// Taking a manual backup still enforces the *automatic* bound (a target
    /// already over it does not stay over it forever), while the manual backup
    /// it just created is itself exempt.
    #[test]
    fn create_backup_prunes_only_automatic_backups() {
        let dir = TempDir::new("manual-prune");
        let target = dir.join("fstab");
        write_original(&target);

        let automatic: Vec<PathBuf> = (1..=(MAX_BACKUPS as u64 + 2))
            .map(|i| seed_backup_file(&dir, i, false))
            .collect();

        let outcome = create_backup(&target).expect("a manual backup");

        // Two automatics too many existed; the new manual backup does not add
        // to that count, so exactly two are pruned.
        assert_eq!(outcome.pruned_backups, automatic[..2].to_vec());
        assert!(outcome.backup.exists(), "the new manual backup survives");
        let remaining = list_backups(&target).expect("listing");
        assert_eq!(remaining.iter().filter(|e| !e.manual).count(), MAX_BACKUPS);
        assert_eq!(remaining.iter().filter(|e| e.manual).count(), 1);
    }

    /// An I/O failure while copying is reported as `Backup`, and the target is
    /// provably untouched - the same failure-injection standard the save path
    /// is held to.
    #[test]
    fn an_injected_copy_failure_fails_the_manual_backup_cleanly() {
        let dir = TempDir::new("manual-copy-fail");
        let target = dir.join("fstab");
        write_original(&target);

        let spy = SpyFs::failing_at(Op::CopyBackup);
        let err = create_backup_with(&target, &spy).expect_err("the copy fails");

        assert!(
            matches!(err, CreateBackupError::Backup { .. }),
            "got {err:?}"
        );
        assert_target_untouched(&target);
        assert!(
            list_backups(&target).expect("listing").is_empty(),
            "a failed copy must not leave a backup behind"
        );
    }

    /// A pruning problem is a note, never a failure: the snapshot the user
    /// asked for exists, and an extra stale automatic backup is harmless.
    #[test]
    fn a_prune_failure_does_not_fail_a_manual_backup() {
        let dir = TempDir::new("manual-prune-fail");
        let target = dir.join("fstab");
        write_original(&target);

        // `ListDir` is only reached by the pruning step here - the copy itself
        // does not list the directory.
        let spy = SpyFs::failing_at(Op::ListDir);
        let outcome =
            create_backup_with(&target, &spy).expect("a pruning problem must not fail the backup");

        assert_eq!(fs::read_to_string(&outcome.backup).unwrap(), ORIGINAL);
        assert!(
            outcome
                .notes
                .iter()
                .any(|note| note.contains("prune old backups")),
            "the pruning failure must be surfaced, notes: {:?}",
            outcome.notes
        );
    }
}
