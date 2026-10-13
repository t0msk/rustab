//! Atomic write/backup/rename logic — see `docs/SPEC.md` §6.
//!
//! This module implements the single most important safety requirement in the
//! project (`docs/SPEC.md` §2.1): a save of an fstab-style file is
//! **all-or-nothing**. The target file always contains either its complete
//! old content or its complete new content — never a truncated, partially
//! written, or interleaved mixture of the two — no matter where a failure
//! (I/O error, full disk, permission error, process death) occurs.
//!
//! [`write_document`] is the only entry point. It performs, in exactly this
//! order:
//!
//! 1. **Validate in memory.** [`crate::validate::validate`] runs both the
//!    structural checks and the `findmnt --verify` semantic check against the
//!    candidate [`Document`]. If it reports any blocking error, this function
//!    returns [`WriteError::Validation`] **before performing a single write
//!    syscall**: nothing on disk has been touched, created, or even opened
//!    for writing at that point.
//! 2. **Resolve the effective target.** If `target` is a symlink (a
//!    legitimate configuration — e.g. `/etc/fstab` symlinked into an
//!    etckeeper//`nix`-managed tree), it is resolved and the write is applied
//!    to the link's destination. Replacing the symlink itself with a regular
//!    file would both discard the user's configuration and leave the real
//!    file holding stale content — which, for `/etc/fstab`, is exactly the
//!    "machine fails to boot" class of outcome §2.1 forbids.
//! 3. **Back up.** If the effective target exists, it is copied to
//!    `<target>.rustab-backup.<secs>.<nanos>` in the same directory and the
//!    copy is `fsync`ed, *before* anything else touches the target or its
//!    directory. Then retention is enforced: at most [`MAX_BACKUPS`] backups
//!    are kept for that target and the oldest are deleted beyond that bound.
//!    If the target does not exist (first-ever save) the whole step is
//!    skipped — there is nothing to back up.
//! 4. **Write a temp file in the same directory** as the target, named
//!    `.<target-file-name>.rustab.tmp.<pid>`.
//! 5. **`fsync` the temp file, `fsync` the directory, `rename` onto the
//!    target**, then `fsync` the directory once more.
//! 6. **Re-read and verify** the target byte-for-byte against the bytes that
//!    were intended; a mismatch is a hard [`WriteError::Verify`], never a
//!    warning.
//! 7. **Clean up the temp file on every failure path** between steps 4 and 5
//!    on a best-effort basis, never letting a cleanup failure mask the
//!    original error.
//!
//! ## Why the ordering is what it is
//!
//! **Why the temp file must live in the target's own directory.**
//! `rename(2)` is only atomic within a single filesystem. A temp file under
//! `/tmp` (frequently a separate `tmpfs`) could not be `rename`d onto
//! `/etc/fstab` at all (`EXDEV`), and working around that with a
//! copy-then-truncate would reintroduce exactly the partial-write window this
//! module exists to eliminate. Hence: same directory, always. (The read-only
//! `findmnt --verify` check in [`crate::validate`] does use a throwaway
//! `/tmp` file — that is fine precisely because it never renames anything
//! onto a real target.)
//!
//! **Why `fsync` the temp file before the rename.** `rename` makes the new
//! content visible under the target's name immediately. If the file's *data*
//! were still only in the page cache at that moment, a crash could leave the
//! target name pointing at an inode whose data blocks were never written —
//! i.e. a visibly-renamed but silently-empty or garbage `/etc/fstab`. Syncing
//! the data first means the rename can only ever publish fully-durable
//! content.
//!
//! **Why the directory is `fsync`ed twice.** `docs/SPEC.md` §6 step 4
//! prescribes `fsync(temp) → fsync(dir) → rename`. The pre-rename directory
//! `fsync` persists the temp file's own directory entry, so the rename cannot
//! be applied to a name that does not durably exist yet. But a directory
//! `fsync` only persists directory changes made *before* it — so making the
//! *rename itself* durable requires a second `fsync` of the directory
//! **after** the rename. The spec's step-4 wording conflates the two; this
//! module does both rather than picking one, since the pre-rename `fsync` is
//! what the spec literally asks for and the post-rename `fsync` is what
//! actually guarantees the swap survives a power loss. Neither can weaken the
//! other.
//!
//! **Why the backup is taken first, and `fsync`ed.** The backup exists so the
//! user always has a restore path (`docs/SPEC.md` §6 step 5, §9). Taking it
//! before the temp file is written means a failure to create it aborts the
//! save with the target still fully intact and nothing else on disk changed.
//! `fs::copy` alone leaves the backup in the page cache, so a crash shortly
//! after a successful rename could leave the *new* content durable and the
//! backup gone — the one state in which the user has no way back. `fsync`ing
//! the backup copy closes that window.
//!
//! **Why the target's mode and owner are carried over.** The temp file is a
//! brand-new inode, so it does not inherit the target's permissions. Letting
//! it keep the process umask's default would mean a save could silently turn
//! a `0644 root:root /etc/fstab` into a `0600` file, breaking every
//! unprivileged tool that reads it (`findmnt`, `lsblk`, `systemd-fstab-generator`
//! runs as root, but `mount` invoked by a user does not). The temp file is
//! therefore created `0600` (never briefly *more* permissive than the file it
//! will replace), then `fchmod`ed to the previous target's mode and
//! `fchown`ed to its uid/gid. A failed `fchown` is reported as a non-fatal
//! note rather than aborting the save, since the common cases (same owner, or
//! running as root) cannot fail and refusing to save a file whose owner
//! happens to be someone else would be worse than saving it with the writing
//! user's ownership.
//!
//! ## Testability seam
//!
//! All of the above runs through a small crate-private `FileSystemOps`
//! trait whose production implementation (`RealFs`) is a direct,
//! one-method-per-syscall wrapper over `std::fs`. Unit tests use a
//! *delegating spy*: it
//! performs every operation for real against a temp directory and replaces
//! exactly one operation with an injected `io::Error`. Failure-injection
//! tests therefore assert "the original target file is byte-for-byte
//! untouched" against a real file on a real filesystem, not against an
//! in-memory model that could itself be wrong.

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

/// How many timestamped backups are kept per target file.
///
/// After each successful backup, backups belonging to the same target beyond
/// this bound are deleted oldest-first, so backups never grow without limit
/// (`docs/SPEC.md` §6 step 5). Tune here; nothing else encodes this number.
pub const MAX_BACKUPS: usize = 10;

/// The fixed infix separating a target's file name from a backup's timestamp,
/// i.e. `fstab` + `.rustab-backup.` + `<secs>.<nanos>`.
const BACKUP_INFIX: &str = ".rustab-backup.";

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

/// Everything that can go wrong in [`write_document`], with one variant per
/// distinguishable failure point so a caller (the Tauri command layer, T8)
/// can tell the user precisely what happened and whether the target changed.
///
/// With the sole exception of [`WriteError::Verify`] and
/// [`FsyncStage::DirectoryAfterRename`] — both of which can only occur
/// *after* the atomic rename already succeeded — every variant guarantees
/// that the target file still holds its original content, byte for byte.
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
    /// of this variant — see [`WriteOutcome::notes`].
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
    /// should be unreachable — it would mean the kernel, the filesystem, or
    /// another process interfered — and is deliberately a hard error rather
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
/// a partial or questionable write — those are always `Err`.
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
    /// rather than silently swallowed, but deliberately not errors — none of
    /// them affect the integrity of the file that was just written.
    pub notes: Vec<String>,
}

/// Atomically replaces `target` with the serialized form of `candidate`,
/// following `docs/SPEC.md` §6 exactly.
///
/// **The guarantee.** This function never leaves the target file partially
/// written. On any failure prior to the atomic `rename` — validation,
/// backup, temp-file write, permission application, or either pre-rename
/// `fsync` — the original file is left byte-for-byte untouched and no
/// partially written data is ever visible under its name. The target
/// therefore always holds either its complete previous content or the
/// complete new content. After a successful rename the new content is
/// re-read and compared byte-for-byte with what was written; a mismatch is
/// returned as [`WriteError::Verify`], never as a silent warning.
///
/// If `target` does not exist it is created (no backup is attempted, since
/// there is nothing to back up). If it exists, a timestamped backup of the
/// previous content is made first and at most [`MAX_BACKUPS`] backups are
/// kept. If `target` is a symlink, the write is applied to its destination
/// rather than replacing the link.
///
/// Privilege escalation is explicitly out of scope: this function assumes it
/// already has whatever permissions it needs on `target` and makes no
/// assumption about how it got them, so it behaves identically whether
/// called directly against a sandbox file or from inside an elevated helper
/// writing the real `/etc/fstab`.
///
/// # Errors
///
/// Returns a [`WriteError`] variant identifying the exact failure point; see
/// that type for which variants imply the target was left untouched (all of
/// them except [`WriteError::Verify`], [`WriteError::ReadBack`] and
/// [`WriteError::Fsync`] with [`FsyncStage::DirectoryAfterRename`], which can
/// only occur after the swap already succeeded).
pub fn write_document(target: &Path, candidate: &Document) -> Result<WriteOutcome, WriteError> {
    write_document_with(target, candidate, &RealFs)
}

/// Lists the backups [`write_document`] has created for `target`, oldest
/// first.
///
/// Only files whose names match the exact
/// `<target-file-name>.rustab-backup.<10-digit secs>.<9-digit nanos>` scheme
/// in the target's own directory are returned, so unrelated files — including
/// a user's own `fstab.bak` or a backup belonging to a *different* target in
/// the same directory — are never reported (and, by the same token, never
/// pruned). Exposed so a restore UI does not have to re-implement, and risk
/// disagreeing with, this naming scheme.
///
/// # Errors
///
/// Returns the underlying [`io::Error`] if the target's directory cannot be
/// read. A `target` with no final file-name component yields an empty list
/// rather than an error, since no backup could ever belong to it.
pub fn list_backups(target: &Path) -> io::Result<Vec<PathBuf>> {
    let Some(file_name) = target.file_name() else {
        return Ok(Vec::new());
    };
    let dir = containing_dir(target);
    let names = RealFs.list_dir_file_names(&dir)?;
    Ok(sorted_backups(&dir, &names, file_name)
        .into_iter()
        .map(|(_, path)| path)
        .collect())
}

// ---------------------------------------------------------------------------
// The algorithm
// ---------------------------------------------------------------------------

/// The [`FileSystemOps`]-generic implementation behind [`write_document`].
///
/// Crate-private: production callers get the concrete, `std::fs`-backed
/// [`write_document`], and the generic parameter exists only so unit tests
/// can inject an I/O failure at one specific syscall.
fn write_document_with<F: FileSystemOps>(
    target: &Path,
    candidate: &Document,
    fsops: &F,
) -> Result<WriteOutcome, WriteError> {
    // Step 1 (and 2 — `validate` already runs the `findmnt --verify`
    // semantic check internally). Deliberately the very first thing that
    // happens: on error we return before any syscall that could modify
    // anything.
    let report = validate::validate(candidate);
    if report.has_errors() {
        return Err(WriteError::Validation(report));
    }
    let validation_warnings: Vec<Finding> = report.warnings().cloned().collect();
    let mut notes: Vec<String> = Vec::new();

    // Step 2': resolve a symlinked target to the file it points at.
    let (effective_target, existing) = resolve_target(target, fsops, &mut notes)?;

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
            let backup_path = reserve_backup_path(fsops, &dir, &file_name)?;
            fsops
                .copy_durably(&effective_target, &backup_path)
                .map_err(|source| WriteError::Backup {
                    path: backup_path.clone(),
                    source,
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
        // live data — including in the one case where this runs after a
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
    // itself durable — see the module docs.
    fsops
        .sync_dir(plan.dir)
        .map_err(|source| WriteError::Fsync {
            stage: FsyncStage::DirectoryAfterRename,
            path: plan.dir.to_path_buf(),
            source,
        })
}

/// Determines the file that will actually be written and stats it.
///
/// Returns the effective target path together with its current [`FileStat`],
/// or `None` for a target that does not exist yet. A symlinked target is
/// resolved to its destination (see the module docs for why); a directory, or
/// a symlink that cannot be resolved (e.g. a dangling one), is rejected as
/// [`WriteError::InvalidTarget`].
fn resolve_target<F: FileSystemOps>(
    target: &Path,
    fsops: &F,
    notes: &mut Vec<String>,
) -> Result<(PathBuf, Option<FileStat>), WriteError> {
    let stat = fsops
        .symlink_stat(target)
        .map_err(|source| WriteError::Backup {
            path: target.to_path_buf(),
            source,
        })?;

    let (path, stat) = match stat {
        Some(stat) if stat.is_symlink => {
            let resolved =
                fsops
                    .canonicalize(target)
                    .map_err(|source| WriteError::InvalidTarget {
                        path: target.to_path_buf(),
                        reason: format!("it is a symlink that could not be resolved ({source})"),
                    })?;
            notes.push(format!(
                "{} is a symlink; the new content was written through to {}",
                target.display(),
                resolved.display()
            ));
            let resolved_stat =
                fsops
                    .symlink_stat(&resolved)
                    .map_err(|source| WriteError::Backup {
                        path: resolved.clone(),
                        source,
                    })?;
            (resolved, resolved_stat)
        }
        other => (target.to_path_buf(), other),
    };

    if stat.as_ref().is_some_and(|stat| stat.is_dir) {
        return Err(WriteError::InvalidTarget {
            path,
            reason: "it is a directory".to_string(),
        });
    }
    Ok((path, stat))
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
/// The pid keeps two concurrent Rustab processes (or an elevated helper and
/// the app itself) from writing the same temp file. Built by concatenating
/// [`OsString`]s rather than going through `str`, so a non-UTF-8 target file
/// name is preserved exactly instead of being lossily mangled.
fn temp_file_name(file_name: &OsStr) -> OsString {
    let mut name = OsString::from(".");
    name.push(file_name);
    name.push(format!(".rustab.tmp.{}", std::process::id()));
    name
}

/// Builds the backup file name for `file_name` at the given timestamp:
/// `<file_name>.rustab-backup.<secs>.<nanos>`, both components zero-padded to
/// a fixed width so names sort chronologically as text as well as numerically.
fn backup_file_name(file_name: &OsStr, secs: u64, nanos: u32) -> OsString {
    let mut name = OsString::from(file_name);
    name.push(format!(
        "{BACKUP_INFIX}{secs:0secs_width$}.{nanos:0nanos_width$}",
        secs_width = BACKUP_SECS_DIGITS,
        nanos_width = BACKUP_NANOS_DIGITS,
    ));
    name
}

/// Parses a directory entry name as a backup of `target_name`, returning its
/// `(secs, nanos)` timestamp, or `None` if the name is not *exactly* a
/// Rustab backup of that specific target.
///
/// Strict on purpose: this predicate is the only thing standing between
/// [`prune_backups`] and a user's unrelated files, so anything that is not an
/// exact `<target_name>.rustab-backup.<10 digits>.<9 digits>` — a different
/// target's backup, a hand-made `fstab.bak`, a backup name with extra
/// suffixes, or a non-numeric timestamp — must be rejected and left alone.
fn backup_timestamp_of(name: &OsStr, target_name: &OsStr) -> Option<(u64, u32)> {
    let rest = name.as_bytes().strip_prefix(target_name.as_bytes())?;
    let rest = rest.strip_prefix(BACKUP_INFIX.as_bytes())?;
    let rest = std::str::from_utf8(rest).ok()?;
    let (secs, nanos) = rest.split_once('.')?;
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
    Some((secs.parse().ok()?, nanos.parse().ok()?))
}

/// Filters `names` down to backups of `target_name` and returns them paired
/// with their timestamps, oldest first. The path is a secondary sort key
/// purely so the order is total and deterministic.
fn sorted_backups(
    dir: &Path,
    names: &[OsString],
    target_name: &OsStr,
) -> Vec<((u64, u32), PathBuf)> {
    let mut backups: Vec<((u64, u32), PathBuf)> = names
        .iter()
        .filter_map(|name| {
            backup_timestamp_of(name, target_name).map(|stamp| (stamp, dir.join(name)))
        })
        .collect();
    backups.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    backups
}

/// Picks an unused timestamped backup path in `dir` for `file_name`.
///
/// Retries with a fresh timestamp if the name is somehow already taken, so a
/// backup never silently overwrites an older one. Failing to find a free name
/// is reported as [`WriteError::Backup`], which aborts the save with the
/// target untouched — the safe direction to fail in.
fn reserve_backup_path<F: FileSystemOps>(
    fsops: &F,
    dir: &Path,
    file_name: &OsStr,
) -> Result<PathBuf, WriteError> {
    for _ in 0..MAX_BACKUP_NAME_ATTEMPTS {
        let (secs, nanos) = unix_timestamp_parts();
        let candidate = dir.join(backup_file_name(file_name, secs, nanos));
        match fsops.symlink_stat(&candidate) {
            Ok(None) => return Ok(candidate),
            Ok(Some(_)) => continue,
            Err(source) => {
                return Err(WriteError::Backup {
                    path: candidate,
                    source,
                })
            }
        }
    }
    Err(WriteError::Backup {
        path: dir.join(file_name),
        source: io::Error::other(format!(
            "no unused timestamped backup name was available after {MAX_BACKUP_NAME_ATTEMPTS} \
             attempts"
        )),
    })
}

/// Deletes backups of `file_name` in `dir` beyond [`MAX_BACKUPS`], oldest
/// first, recording what it deleted in `pruned`.
///
/// Deliberately infallible: pruning happens *after* the new backup exists and
/// *before* the swap, and a stale extra backup is harmless, so neither an
/// unreadable directory nor an undeletable old backup is allowed to abort a
/// save that is otherwise safe to perform. Such failures are not silent
/// either — each one is recorded in `notes` and surfaced via
/// [`WriteOutcome::notes`].
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

    let backups = sorted_backups(dir, &names, file_name);
    let Some(excess) = backups.len().checked_sub(MAX_BACKUPS) else {
        return;
    };
    for (_, path) in backups.into_iter().take(excess) {
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
/// A hand-rollable struct rather than [`fs::Metadata`] specifically so the
/// test spy can synthesize one — `fs::Metadata` cannot be constructed outside
/// `std`.
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
/// This exists purely as a failure-injection seam: it is a thin, mechanical
/// wrapper over the syscalls involved, with no policy of its own, so that the
/// production path ([`RealFs`]) stays auditable as plain `std::fs` code while
/// tests can replace exactly one operation with an error. Every method maps
/// to one syscall (or, for [`Self::create_and_write`] and
/// [`Self::copy_durably`], one obvious pair).
trait FileSystemOps {
    /// An open, writable handle to a file created by
    /// [`Self::create_and_write`]. Operations that must act on the open file
    /// description — `fchmod`, `fchown`, `fsync` — take this rather than a
    /// path, so they provably affect the file that was just written and not
    /// whatever a path might point at by then.
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
    /// the still-open handle. Does **not** flush — that is
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

    /// A delegating spy: every operation is performed for real against the
    /// filesystem, except one chosen operation (optionally its *n*-th
    /// occurrence) which fails with an injected `io::Error`. It also records
    /// the exact sequence of operations, so the spec-mandated
    /// write/fsync/fsync/rename ordering can be asserted rather than
    /// eyeballed.
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
    /// `findmnt --verify` has nothing to object to — the same approach
    /// `validate.rs`'s tests use).
    fn valid_candidate(mount_point: &str) -> Document {
        Document::from_lines(vec![
            Line::Comment(" rewritten by the atomic writer test".to_string()),
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
    /// document — i.e. the writer serializes via the canonical `Display` impl
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
        let outcome =
            write_document_with(&target, &candidate, &spy).expect("the first save should succeed");

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
    /// bits rather than the umask default — the temp file is a new inode, so
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
        write_document_with(&target, &valid_candidate("/mnt/order"), &spy).expect("save");

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
    /// `WriteError::Validation` and **nothing touches disk at all** — proven
    /// by the target's bytes being unchanged *and* by the spy having recorded
    /// zero filesystem operations, so not even a stat, backup, or temp file
    /// happened.
    #[test]
    fn structural_validation_failure_touches_nothing() {
        let dir = TempDir::new("invalid-structural");
        let target = dir.join("fstab");
        write_original(&target);

        let spy = SpyFs::new();
        let err = write_document_with(&target, &structurally_invalid_candidate(), &spy)
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
        let err = write_document_with(&target, &candidate, &spy)
            .expect_err("a semantically invalid candidate must be rejected");

        assert!(matches!(err, WriteError::Validation(_)), "got {err:?}");
        assert!(spy.ops().is_empty());
        assert_target_untouched(&target);
        assert_eq!(dir.entry_names(), vec!["fstab".to_string()]);
    }

    // -- (c) write failure --------------------------------------------------

    /// Acceptance (c): an I/O error while writing the temp file (injected
    /// after a *partial* write, as a full disk would behave) leaves the target
    /// byte-for-byte unchanged and the half-written temp file removed. Proven
    /// by re-reading the target and by the directory containing only the
    /// target and its backup — no `.fstab.rustab.tmp.*` survivor.
    #[test]
    fn injected_write_failure_leaves_target_untouched_and_cleans_up() {
        let dir = TempDir::new("fail-write");
        let target = dir.join("fstab");
        write_original(&target);

        let spy = SpyFs::failing_at(Op::CreateAndWrite);
        let err = write_document_with(&target, &valid_candidate("/mnt/never"), &spy)
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
    /// the save before the rename, leaving the target untouched — a save must
    /// never go through with the wrong mode on `/etc/fstab`.
    #[test]
    fn injected_set_mode_failure_leaves_target_untouched() {
        let dir = TempDir::new("fail-mode");
        let target = dir.join("fstab");
        write_original(&target);

        let spy = SpyFs::failing_at(Op::SetMode);
        let err = write_document_with(&target, &valid_candidate("/mnt/never"), &spy)
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
        let err = write_document_with(&target, &valid_candidate("/mnt/never"), &spy)
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
        let err = write_document_with(&target, &valid_candidate("/mnt/never"), &spy)
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

    /// Acceptance (d): an I/O error at the atomic rename — with a complete,
    /// fully-`fsync`ed temp file already sitting next to the target — leaves
    /// the target byte-for-byte unchanged and the temp file removed. This is
    /// the all-or-nothing hinge: the new content was entirely ready and still
    /// never became visible.
    #[test]
    fn injected_rename_failure_leaves_target_untouched_and_cleans_up() {
        let dir = TempDir::new("fail-rename");
        let target = dir.join("fstab");
        write_original(&target);

        let spy = SpyFs::failing_at(Op::Rename);
        let err = write_document_with(&target, &valid_candidate("/mnt/never"), &spy)
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
        let err = write_document_with(&target, &valid_candidate("/mnt/verify"), &spy)
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
        let err = write_document_with(&target, &candidate, &spy)
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
        let err = write_document_with(&target, &valid_candidate("/mnt/never"), &spy)
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
        let outcome = write_document_with(&target, &candidate, &spy)
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
            .map(|i| {
                let path = dir.path.join(backup_file_name(OsStr::new("fstab"), i, 0));
                fs::write(&path, format!("old backup {i}\n")).unwrap();
                path
            })
            .collect();

        let outcome = write_document(&target, &valid_candidate("/mnt/retained")).expect("save");

        let remaining = list_backups(&target).expect("listing backups");
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

    /// Proves pruning can only ever delete Rustab's own backups *for this
    /// exact target*: decoy files that merely resemble a backup name — a
    /// different target's backup, a non-numeric timestamp, a wrong-width
    /// timestamp, an extra suffix, a hand-made `fstab.bak` — all survive a
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
            fs::write(
                dir.path.join(backup_file_name(OsStr::new("fstab"), i, 0)),
                format!("old {i}\n"),
            )
            .unwrap();
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
            backup_timestamp_of(
                OsStr::new("fstab.rustab-backup.0000000012.000000034"),
                target
            ),
            Some((12, 34))
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
                backup_timestamp_of(OsStr::new(bad), target),
                None,
                "{bad} must not be treated as a backup of fstab"
            );
        }
    }

    // -- target path handling -----------------------------------------------

    /// Proves a symlinked target is written *through* rather than replaced:
    /// the link survives as a link, the file it points at gets the new
    /// content, the backup is made next to that real file, and the
    /// redirection is reported as a note. Replacing the link would leave the
    /// real `/etc/fstab` holding stale content — the boot-breaking outcome
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
    /// first, and reports an empty list (not an error) for a target that has
    /// none.
    #[test]
    fn list_backups_is_sorted_and_scoped_to_the_target() {
        let dir = TempDir::new("list");
        let target = dir.join("fstab");
        write_original(&target);
        assert!(list_backups(&target).unwrap().is_empty());

        for (secs, nanos) in [(3u64, 0u32), (1, 500), (1, 100), (2, 0)] {
            fs::write(
                dir.path
                    .join(backup_file_name(OsStr::new("fstab"), secs, nanos)),
                "x",
            )
            .unwrap();
        }
        fs::write(
            dir.path.join(backup_file_name(OsStr::new("other"), 9, 0)),
            "x",
        )
        .unwrap();

        let listed: Vec<String> = list_backups(&target)
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            listed,
            vec![
                "fstab.rustab-backup.0000000001.000000100".to_string(),
                "fstab.rustab-backup.0000000001.000000500".to_string(),
                "fstab.rustab-backup.0000000002.000000000".to_string(),
                "fstab.rustab-backup.0000000003.000000000".to_string(),
            ]
        );
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
    /// and still goes through the full verified sequence — an edge case that
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
}
