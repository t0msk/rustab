//! Listing the active target's backups — automatic and manual alike — and
//! resolving a frontend-supplied backup path against that list.
//!
//! ## Why a frontend-supplied path is safe here
//!
//! Every other command refuses to accept a path at all, precisely so a buggy
//! or compromised UI cannot redirect a read or a write (see
//! [`crate::target`]). `restore_backup`, `read_backup` and `delete_backup`
//! are the only exceptions, and they are safe for a structural reason rather
//! than a careful-string-checking reason: the path they are given is never
//! used as a path until it has been found in the list
//! [`fstab_core::writer::list_backups`] reports for *the active target*.
//! That list is built by `fstab-core` from the backup naming scheme it owns,
//! and only from the target's own directory. So `/etc/shadow`,
//! `../../etc/fstab`, a backup of a *different* target sitting in the same
//! directory, and a name that merely resembles the scheme (wrong timestamp
//! width, a non-numeric timestamp, an extra suffix) all fail closed with
//! [`BackupError::NotABackupOfTarget`].
//!
//! What this does *not* claim — deliberately — is that the file was created
//! by this installation: a file in the target's directory whose name matches
//! the scheme exactly counts as a backup of that target, because in that
//! directory the name *is* the identity. That costs nothing, because creating
//! such a file next to `/etc/fstab` already requires the write access that
//! would let someone edit `/etc/fstab` directly, and a restore still goes
//! through validation, a fresh backup and the atomic write like any other
//! save. Both halves of this rule are asserted by the tests below.
//!
//! Matching is by identity, not by string equality alone, so an equivalent
//! spelling of the same file (a symlinked directory, a `./` prefix) still
//! resolves — but to the canonical path from the list, which is what is then
//! read.
//!
//! ## Called on both sides of the privilege boundary
//!
//! [`resolve_requested`] is also called *inside* the elevated process, by
//! [`crate::privileged`]'s backup-deletion handler, against the backup list of
//! the target that process has already verified it is allowed to manage. The
//! unprivileged side resolving first is only a courtesy (fail without a
//! password prompt for a path that cannot work); the elevated side's own call
//! is the one that decides, because nothing the unprivileged side claims is
//! trusted there. Both calls run this same function, so the two can never
//! disagree about what counts as a backup.

use crate::dto::BackupInfoDto;
use fstab_core::writer;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Why a backup could not be listed or selected.
#[derive(Debug)]
pub enum BackupError {
    /// The target's directory could not be read.
    List {
        /// The directory that could not be listed.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// The requested path is not one of the active target's backups.
    NotABackupOfTarget {
        /// The path that was asked for, as given.
        requested: String,
        /// The target whose backups were searched.
        target: PathBuf,
    },
}

impl fmt::Display for BackupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackupError::List { path, source } => write!(
                f,
                "could not list the backups in {}: {source}",
                path.display()
            ),
            // Deliberately says nothing about the consequence: the same
            // refusal is reported by a restore, a read and a delete, and each
            // caller appends the clause that is true for it ("nothing was
            // restored" / "nothing was deleted").
            BackupError::NotABackupOfTarget { requested, target } => write!(
                f,
                "{requested} is not one of the backups Rustab made of {}",
                target.display()
            ),
        }
    }
}

impl std::error::Error for BackupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            BackupError::List { source, .. } => Some(source),
            BackupError::NotABackupOfTarget { .. } => None,
        }
    }
}

/// Lists `target`'s backups, **newest first**, with the metadata the restore
/// panel displays (`docs/UI_DESIGN.md` §7).
///
/// [`writer::list_backups`] returns them oldest first; this reverses that
/// order because every consumer shows the most recent restore point at the
/// top, and doing the flip once here keeps the frontend from having to know
/// (or get wrong) which end is which. A backup that vanishes between the
/// listing and the `stat` is skipped rather than failing the whole command.
///
/// Both kinds are listed together, in one chronological order, each row
/// carrying [`BackupInfoDto::is_manual`] — the kind is a label on a backup,
/// not a separate list.
///
/// # Errors
///
/// Returns [`BackupError::List`] if the target's directory cannot be read.
pub fn list(target: &Path) -> Result<Vec<BackupInfoDto>, BackupError> {
    let mut entries = writer::list_backups(target).map_err(|source| BackupError::List {
        path: containing_dir(target),
        source,
    })?;
    entries.reverse();

    Ok(entries
        .into_iter()
        .filter_map(|entry| describe(target, &entry.path, entry.manual))
        .collect())
}

/// Builds the [`BackupInfoDto`] for one backup of `target`, `stat`-ing it on
/// the spot.
///
/// `None` if the file cannot be `stat`ed — for [`list`] that means a backup
/// that vanished between the listing and the `stat`, which is a skipped row
/// rather than a failed command.
///
/// `manual` is passed in rather than re-derived from the path, because
/// [`writer::list_backups`] is the one place that owns the naming scheme and
/// re-parsing the name here would create a second answer that could disagree
/// with it (see the module docs).
fn describe(target: &Path, backup: &Path, manual: bool) -> Option<BackupInfoDto> {
    let metadata = fs::metadata(backup).ok()?;
    Some(BackupInfoDto {
        file_name: backup
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        target_path: target.to_string_lossy().into_owned(),
        size_bytes: metadata.len(),
        created_unix_secs: modified_unix_secs(&metadata),
        path: backup.to_string_lossy().into_owned(),
        is_manual: manual,
    })
}

/// Describes a single backup of `target` that has *just* been created, for a
/// caller that already knows its path (currently
/// [`crate::commands::create_backup`]).
///
/// Goes back through [`writer::list_backups`] rather than taking the caller's
/// word for the kind, so the row handed to the frontend is built from the same
/// source of truth as every row [`list`] produces — including when the backup
/// was created inside the elevated helper, where this process never saw it
/// being made. A path that is not (or is no longer) one of `target`'s backups
/// yields `None`.
///
/// # Errors
///
/// [`BackupError::List`] if the target's directory cannot be read.
pub fn describe_one(target: &Path, backup: &Path) -> Result<Option<BackupInfoDto>, BackupError> {
    let entries = writer::list_backups(target).map_err(|source| BackupError::List {
        path: containing_dir(target),
        source,
    })?;
    let Some(entry) = entries
        .into_iter()
        .find(|entry| crate::target::is_same_file(&entry.path, backup))
    else {
        return Ok(None);
    };
    Ok(describe(target, &entry.path, entry.manual))
}

/// Resolves `requested` to one of `target`'s actual backups.
///
/// The returned path always comes from [`writer::list_backups`], never from
/// `requested` — see the module docs for why that is the whole point.
///
/// # Errors
///
/// [`BackupError::List`] if the backup list could not be read;
/// [`BackupError::NotABackupOfTarget`] if `requested` is not in it.
pub fn resolve_requested(target: &Path, requested: &str) -> Result<PathBuf, BackupError> {
    let requested_path = Path::new(requested);
    writer::list_backups(target)
        .map_err(|source| BackupError::List {
            path: containing_dir(target),
            source,
        })?
        .into_iter()
        .find(|candidate| crate::target::is_same_file(&candidate.path, requested_path))
        .map(|candidate| candidate.path)
        .ok_or_else(|| BackupError::NotABackupOfTarget {
            requested: requested.to_string(),
            target: target.to_path_buf(),
        })
}

/// The directory `path` lives in — used for error messages here, and by
/// [`crate::commands::open_backup_location`] as the directory to open in the
/// host file manager, since it is also where `path`'s backups live (see the
/// module docs). Mirrors the `.` fallback [`writer`] itself uses for a bare
/// relative name.
pub(crate) fn containing_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Reads a backup's content as UTF-8 text.
///
/// Separate from [`resolve_requested`] so the caller must resolve first:
/// there is no function in this module that reads a path straight from the
/// frontend.
///
/// # Errors
///
/// Returns the underlying [`io::Error`], including
/// [`io::ErrorKind::InvalidData`] for a backup that is not valid UTF-8 (which
/// can neither be represented as an [`fstab_core::model::Document`], and so
/// must not be restored through one, nor be sent across the Tauri boundary as
/// a string).
pub fn read_text(backup: &Path) -> io::Result<String> {
    let bytes = fs::read(backup)?;
    String::from_utf8(bytes).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} is not valid UTF-8 ({err}), so Rustab can neither show it nor restore it; \
                 copy it into place manually instead",
                backup.display()
            ),
        )
    })
}

/// Reads a backup's modification time as whole seconds since the Unix epoch,
/// or `None` if the platform cannot report it.
///
/// The mtime is used as the "taken at" timestamp rather than re-deriving it
/// from the `<secs>.<nanos>` suffix in the file name: that suffix's exact
/// format is `fstab-core`'s private business (it owns both the writing and
/// the matching of those names), and duplicating the parser here would create
/// a second place that has to agree with it. `fs::copy` gives the backup a
/// fresh mtime at the moment the backup is taken, so the two are equivalent
/// in practice.
fn modified_unix_secs(metadata: &fs::Metadata) -> Option<u64> {
    metadata
        .modified()
        .ok()
        .and_then(|time: SystemTime| time.duration_since(UNIX_EPOCH).ok())
        .map(|since| since.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use fstab_core::model::{Document, FstabEntry, FstabOption, Line, Options, SourceId};

    fn valid_candidate(mount_point: &str) -> Document {
        Document::from_lines(vec![Line::Entry(FstabEntry::new(
            SourceId::Uuid("11111111-2222-3333-4444-555555555555".into()),
            mount_point,
            "ext4",
            Options::from_vec(vec![FstabOption::Flag("noauto".into())]),
            0,
            2,
        ))])
    }

    /// Creates `count` real backups by performing `count` real saves, so the
    /// names under test are the ones `fstab-core` actually produces rather
    /// than ones this test invented.
    fn seed_backups(target: &Path, count: usize) {
        fs::write(target, "# original\n").expect("seed the target");
        for i in 0..count {
            writer::write_document(target, &valid_candidate(&format!("/mnt/save-{i}")))
                .expect("the save should succeed");
        }
    }

    #[test]
    fn list_returns_backups_newest_first_with_metadata() {
        let dir = TempDir::new("backups-list");
        let target = dir.join("fstab");
        seed_backups(&target, 3);

        let backups = list(&target).expect("listing must succeed");
        assert_eq!(backups.len(), 3);

        let oldest_first = writer::list_backups(&target).expect("list_backups");
        let newest = oldest_first.last().expect("there is at least one");
        assert_eq!(backups[0].path, newest.path.to_string_lossy());
        for info in &backups {
            assert_eq!(info.target_path, target.to_string_lossy());
            assert!(info.size_bytes > 0);
            assert!(info.created_unix_secs.is_some());
            assert!(info.file_name.starts_with("fstab.rustab-backup."));
            assert!(
                !info.is_manual,
                "a backup a save took is automatic: {info:?}"
            );
        }
    }

    /// The two kinds appear in one list, each flagged, and the flag comes from
    /// `fstab-core`'s own reading of the name rather than from anything this
    /// layer guesses.
    #[test]
    fn list_flags_manual_backups_and_leaves_automatic_ones_unflagged() {
        let dir = TempDir::new("backups-kinds");
        let target = dir.join("fstab");
        seed_backups(&target, 1);
        let manual = writer::create_backup(&target).expect("a manual backup");

        let listed = list(&target).expect("listing must succeed");
        assert_eq!(listed.len(), 2);

        let (manual_rows, automatic_rows): (Vec<_>, Vec<_>) =
            listed.iter().partition(|info| info.is_manual);
        assert_eq!(
            manual_rows.len(),
            1,
            "exactly one manual backup: {listed:?}"
        );
        assert_eq!(manual_rows[0].path, manual.backup.to_string_lossy());
        assert!(manual_rows[0].file_name.ends_with(".manual"));
        assert_eq!(automatic_rows.len(), 1);
        assert!(!automatic_rows[0].file_name.ends_with(".manual"));
    }

    /// `describe_one` is how a just-created backup becomes a list row; it must
    /// agree with `list` field for field, and must refuse a path that is not
    /// one of this target's backups rather than describing it.
    #[test]
    fn describe_one_matches_the_row_list_would_produce() {
        let dir = TempDir::new("backups-describe");
        let target = dir.join("fstab");
        seed_backups(&target, 1);
        let manual = writer::create_backup(&target).expect("a manual backup");

        let described = describe_one(&target, &manual.backup)
            .expect("the directory is readable")
            .expect("a backup that exists is describable");
        let listed = list(&target).expect("listing");
        let same = listed
            .iter()
            .find(|info| info.path == described.path)
            .expect("the same backup must be in the list");
        assert_eq!(&described, same);
        assert!(described.is_manual);

        // Anything that is not one of this target's backups yields `None`,
        // through the same list-based check every other path-taking entry
        // point uses — it is never described from the path alone.
        for not_a_backup in [
            target.clone(),
            dir.join("fstab.bak"),
            PathBuf::from("/etc/shadow"),
        ] {
            assert_eq!(
                describe_one(&target, &not_a_backup).expect("the directory is readable"),
                None,
                "{} must not be describable as a backup",
                not_a_backup.display()
            );
        }
    }

    #[test]
    fn list_is_empty_for_a_target_without_backups() {
        let dir = TempDir::new("backups-empty");
        let target = dir.join("fstab");
        fs::write(&target, "# original\n").expect("seed");
        assert!(list(&target).expect("listing must succeed").is_empty());
    }

    #[test]
    fn resolve_requested_accepts_a_real_backup_of_this_target() {
        let dir = TempDir::new("backups-resolve");
        let target = dir.join("fstab");
        seed_backups(&target, 2);

        let listed = list(&target).expect("listing must succeed");
        let wanted = &listed[0];
        let resolved = resolve_requested(&target, &wanted.path).expect("it is a real backup");
        assert_eq!(resolved.to_string_lossy(), wanted.path);
    }

    /// The security-relevant case: anything that is not in this target's own
    /// backup list is refused, whatever it looks like.
    #[test]
    fn resolve_requested_refuses_anything_that_is_not_a_backup_of_this_target() {
        let dir = TempDir::new("backups-refuse");
        let target = dir.join("fstab");
        seed_backups(&target, 1);

        // A different target's backup, sitting in the same directory.
        let other_target = dir.join("fstab.other");
        seed_backups(&other_target, 1);
        let other_backup = list(&other_target).expect("listing")[0].path.clone();

        // Files in the right directory whose names are *almost* a backup of
        // this target: a wrong timestamp width, a non-numeric timestamp, an
        // extra suffix, and the target itself.
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
            other_backup,
            "/etc/shadow".to_string(),
            "../../etc/fstab".to_string(),
            String::new(),
            target.to_string_lossy().into_owned(),
        ];
        refused.extend(
            near_misses
                .iter()
                .map(|name| dir.join(name).to_string_lossy().into_owned()),
        );

        for requested in &refused {
            let error = resolve_requested(&target, requested)
                .expect_err("only this target's own backups may resolve");
            assert!(
                matches!(error, BackupError::NotABackupOfTarget { .. }),
                "unexpected error for {requested}: {error}"
            );
        }

        // None of them are listed as backups either.
        let listed = list(&target).expect("listing");
        assert_eq!(listed.len(), 1, "only the one real backup is a backup");
        for requested in &refused {
            assert!(!listed.iter().any(|info| &info.path == requested));
        }
    }

    /// The flip side, stated explicitly because it is a deliberate design
    /// decision rather than an oversight: a file in the target's own
    /// directory whose name matches the backup scheme *exactly* counts as a
    /// backup of that target, even if this process did not create it. The
    /// naming scheme in that directory **is** the identity — `fstab-core`
    /// owns both halves of it — and no privilege is gained by exploiting
    /// this: writing such a file next to `/etc/fstab` already requires the
    /// write access that would let an attacker edit `/etc/fstab` directly,
    /// and a restore still goes through validation, backup and the atomic
    /// write like any other save.
    #[test]
    fn a_file_matching_the_backup_scheme_exactly_counts_as_a_backup() {
        let dir = TempDir::new("backups-scheme");
        let target = dir.join("fstab");
        fs::write(&target, "# original\n").expect("seed the target");
        let handmade = dir.join("fstab.rustab-backup.0000000001.000000002");
        fs::write(&handmade, "# hand-made but correctly named\n").expect("write it");

        let resolved = resolve_requested(&target, &handmade.to_string_lossy())
            .expect("an exactly-named file in the target's directory is a backup");
        assert_eq!(resolved, handmade);
    }

    #[test]
    fn read_text_rejects_non_utf8_content() {
        let dir = TempDir::new("backups-utf8");
        let path = dir.join("fstab.rustab-backup.0000000001.000000002");
        fs::write(&path, [0xff, 0xfe, 0x00]).expect("write invalid UTF-8");

        let error = read_text(&path).expect_err("invalid UTF-8 must be refused");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn read_text_returns_the_backup_content() {
        let dir = TempDir::new("backups-read");
        let target = dir.join("fstab");
        seed_backups(&target, 1);

        let backup = list(&target).expect("listing")[0].path.clone();
        assert_eq!(
            read_text(Path::new(&backup)).expect("the backup must be readable"),
            "# original\n"
        );
    }
}
