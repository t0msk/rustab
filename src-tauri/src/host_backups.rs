//! The real `/etc/fstab`'s own backups, listed and read **on the host** -
//! the read-only sibling of [`crate::backups`], for a session that cannot
//! see the directory those backups live in.
//!
//! ## Why this exists at all
//!
//! Every save of the real `/etc/fstab` leaves an automatic backup beside
//! it, in `/etc/`. `fstab_core::writer::list_backups` finds those with a
//! plain `fs::read_dir("/etc/")`, which inside a Flatpak sandbox lists the
//! *runtime's* `/etc/` instead - so from there the backups the app itself
//! created are invisible, and neither the Backups panel nor a restore can
//! reach them (`docs/FLATPAK.md` §3.1).
//!
//! No elevation is involved in fixing that: `/etc/` is world-readable
//! (mode 755) and so are the backups in it, so the only thing missing is a
//! listing taken on the *other side of the sandbox boundary*. That is
//! [`crate::host::RealFileReader::list_backup_candidates`] - the same
//! lightweight inline-shell-script pattern [`crate::host`] already uses for
//! reading a host file and for the batched existence check, not a re-exec of
//! this binary (that heavier alternative was considered and rejected when
//! the fallback backup location was designed - see
//! [`crate::backup_location`]'s module docs).
//!
//! ## What it does and does not do
//!
//! Listing, resolving and reading: nothing here creates, writes, renames or
//! unlinks anything, on either side of the boundary. Deleting a file in
//! `/etc/` genuinely needs root, and by the maintainer's scoping decision
//! manual backups keep being created in - and deleted from - the per-user
//! fallback directory instead. Every row this module produces therefore
//! carries `deletable: false` (see
//! [`crate::dto::BackupInfoDto::deletable`]): such a backup can be
//! restored, and its content shown, but not deleted from the sandbox that
//! listed it.
//!
//! The split of responsibilities with the two modules it sits between is
//! deliberate: [`crate::host`] answers "what files sit beside this one, and
//! how big and how old are they?" without any opinion on what a backup is,
//! `fstab_core::writer::list_backups_among` owns the naming grammar that
//! decides which of those names *are* backups, and this module only joins
//! the two and shapes the result like every other backup row the frontend
//! receives. Nothing here re-derives a name, a kind or a timestamp.

use crate::backups::{self, BackupError};
use crate::dto::BackupInfoDto;
use crate::host::RealFileReader;
use fstab_core::writer;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// Lists `target`'s backups as they exist on the host, **newest first** -
/// the same rows, in the same order, [`backups::list`] produces for a
/// directory this process can read itself.
///
/// The listing comes from `reader`, the naming grammar from
/// `fstab_core::writer::list_backups_among`, and the metadata from the
/// listing's own `stat` (never a second one here - this process cannot
/// `stat` these files at all).
///
/// # Errors
///
/// Only `reader`'s own: the underlying [`io::Error`] if the target's
/// directory could not be listed on the host, including
/// [`io::ErrorKind::NotFound`] if it is not there. An empty directory, or
/// one holding nothing that matches the grammar, is an empty list.
pub(crate) fn list(target: &Path, reader: &dyn RealFileReader) -> io::Result<Vec<BackupInfoDto>> {
    let candidates = reader.list_backup_candidates(target)?;
    let names: Vec<OsString> = candidates
        .iter()
        .map(|candidate| candidate.name.clone())
        .collect();

    let mut entries = writer::list_backups_among(target, &names);
    // `list_backups_among` reports oldest first, like `list_backups`;
    // reversed here once, exactly where `backups::list` reverses it, so the
    // frontend never has to know which end is which.
    entries.reverse();

    Ok(entries
        .into_iter()
        .filter_map(|entry| {
            // The name is what the two halves are joined on: it came from
            // `candidates` and `list_backups_among` only ever returns a
            // subset of what it was given, so a miss here is unreachable -
            // and if it ever happened, skipping the row is better than
            // inventing metadata for it.
            let name = entry.path.file_name()?;
            let candidate = candidates.iter().find(|candidate| candidate.name == name)?;
            Some(BackupInfoDto {
                // Already `<target's directory>/<name>` - the path
                // `backups::containing_dir(target).join(name)` names, built
                // by the one module that owns the naming.
                path: entry.path.to_string_lossy().into_owned(),
                file_name: name.to_string_lossy().into_owned(),
                target_path: target.to_string_lossy().into_owned(),
                size_bytes: candidate.size_bytes,
                created_unix_secs: candidate.modified_unix_secs,
                is_manual: entry.manual,
                // The whole point of the flag: this row names a file in the
                // host's `/etc/`, which the session that listed it may read
                // but not unlink (see the module docs).
                deletable: false,
            })
        })
        .collect())
}

/// Resolves `requested` to one of `target`'s actual host-side backups.
///
/// The returned path always comes from [`list`], never from `requested` -
/// the structural reason a frontend-supplied path is safe at all, exactly as
/// in [`backups::resolve_requested`], whose module docs explain it in full.
///
/// Matching is literal path equality, not the identity check (`stat`-based)
/// its local counterpart uses: these paths name files in the *host's*
/// `/etc/`, which this process cannot `stat`, so there is nothing to compare
/// inodes of. That is not a loosening - a path that is not byte-for-byte one
/// of the listed paths is refused - but it does mean an equivalent spelling
/// of the same backup (a `./` prefix, a symlinked directory) does not
/// resolve. The frontend always sends back the exact string from the row it
/// was given, so that costs nothing.
///
/// # Errors
///
/// [`BackupError::List`] if the host-side listing failed;
/// [`BackupError::NotABackupOfTarget`] if `requested` is not in it.
pub(crate) fn resolve_requested(
    target: &Path,
    requested: &str,
    reader: &dyn RealFileReader,
) -> Result<PathBuf, BackupError> {
    let requested_path = Path::new(requested);
    list(target, reader)
        .map_err(|source| BackupError::List {
            path: backups::containing_dir(target),
            source,
        })?
        .into_iter()
        .find(|row| Path::new(&row.path) == requested_path)
        .map(|row| PathBuf::from(row.path))
        .ok_or_else(|| BackupError::NotABackupOfTarget {
            requested: requested.to_string(),
            target: target.to_path_buf(),
        })
}

/// Reads a host-side backup's content as UTF-8 text, through `reader`.
///
/// Separate from [`resolve_requested`] so the caller must resolve first:
/// like [`backups`], this module has no function that reads a path straight
/// from the frontend.
///
/// # Errors
///
/// `reader`'s own [`io::Error`], plus [`io::ErrorKind::InvalidData`] for a
/// backup that is not valid UTF-8 - with the same wording
/// [`backups::read_text`] uses, since it is the same refusal for the same
/// reason and the user sees it in the same place.
pub(crate) fn read_text(backup: &Path, reader: &dyn RealFileReader) -> io::Result<String> {
    let bytes = reader.read(backup)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{BackupCandidate, DirectFs};
    use crate::testutil::TempDir;
    use fstab_core::model::{Document, FstabEntry, FstabOption, Line, Options, SourceId};
    use std::fs;

    /// A [`RealFileReader`] whose *listing* is canned, so the error path can
    /// be driven without a sandbox (`host::StubReader` always lists nothing,
    /// which cannot distinguish "empty" from "failed").
    struct ListingReader(io::Result<Vec<BackupCandidate>>);

    impl RealFileReader for ListingReader {
        fn read(&self, _path: &Path) -> io::Result<Vec<u8>> {
            panic!("these tests only drive the listing")
        }

        fn list_backup_candidates(&self, _target: &Path) -> io::Result<Vec<BackupCandidate>> {
            match &self.0 {
                Ok(candidates) => Ok(candidates.clone()),
                Err(err) => Err(io::Error::new(err.kind(), err.to_string())),
            }
        }
    }

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

    /// Seeds real backups by performing real saves plus one real
    /// `create_backup`, so the names under test are the ones `fstab-core`
    /// actually produces, and adds the files that only look like backups.
    fn seed(target: &Path) {
        fs::write(target, "# original\n").expect("seed the target");
        for i in 0..2 {
            writer::write_document(target, &valid_candidate(&format!("/mnt/save-{i}")))
                .expect("the save must succeed");
        }
        writer::create_backup(target).expect("a manual backup");

        let dir = target.parent().expect("the target is in a directory");
        for near_miss in [
            "fstab.bak",
            "fstab.rustab-backup.1.2",
            "fstab.rustab-backup.0000000001.000000002.old",
            "other.rustab-backup.0000000001.000000002",
        ] {
            fs::write(dir.join(near_miss), "# not a Rustab backup\n").expect("seed a near miss");
        }
    }

    #[test]
    fn list_reports_the_host_side_backups_newest_first_and_never_deletable() {
        let dir = TempDir::new("host-backups-list");
        let target = dir.join("fstab");
        seed(&target);

        let listed = list(&target, &DirectFs).expect("the directory is readable");

        // Two saves plus one manual backup, and nothing else in the
        // directory counts.
        assert_eq!(listed.len(), 3, "{listed:?}");
        let oldest_first = writer::list_backups(&target).expect("the local list");
        assert_eq!(
            listed
                .iter()
                .map(|row| row.path.clone())
                .collect::<Vec<_>>(),
            oldest_first
                .iter()
                .rev()
                .map(|entry| entry.path.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            "the rows must be the same backups in the reverse (newest-first) order"
        );

        for row in &listed {
            assert!(!row.deletable, "a host-side backup is never deletable here");
            assert_eq!(row.target_path, target.to_string_lossy());
            assert_eq!(row.path, dir.join(&row.file_name).to_string_lossy());
            assert!(row.file_name.starts_with("fstab.rustab-backup."));
            assert!(row.size_bytes > 0);
            assert!(row.created_unix_secs.is_some());
            assert_eq!(
                row.size_bytes,
                fs::metadata(&row.path).expect("stat").len(),
                "the size must be the file's own, taken by the listing"
            );
        }

        // The kind comes from `fstab-core`'s reading of the name, and the
        // manual backup is the newest one here (it was taken last).
        let manual: Vec<&BackupInfoDto> = listed.iter().filter(|row| row.is_manual).collect();
        assert_eq!(manual.len(), 1, "{listed:?}");
        assert!(manual[0].file_name.ends_with(".manual"));
        assert_eq!(manual[0].path, listed[0].path);
    }

    #[test]
    fn list_is_empty_for_a_target_without_backups() {
        let dir = TempDir::new("host-backups-empty");
        let target = dir.join("fstab");
        fs::write(&target, "# original\n").expect("seed");
        fs::write(dir.join("fstab.bak"), "# not a backup\n").expect("seed a near miss");

        assert!(list(&target, &DirectFs).expect("listing").is_empty());
    }

    #[test]
    fn resolve_requested_accepts_a_listed_backup_and_refuses_everything_else() {
        let dir = TempDir::new("host-backups-resolve");
        let target = dir.join("fstab");
        seed(&target);

        let listed = list(&target, &DirectFs).expect("listing");
        let wanted = &listed[0];
        assert_eq!(
            resolve_requested(&target, &wanted.path, &DirectFs).expect("it is a listed backup"),
            PathBuf::from(&wanted.path)
        );

        let refused = [
            "/etc/shadow".to_string(),
            "../x".to_string(),
            String::new(),
            target.to_string_lossy().into_owned(),
            dir.join("fstab.bak").to_string_lossy().into_owned(),
            dir.join("fstab.rustab-backup.1.2")
                .to_string_lossy()
                .into_owned(),
            dir.join("other.rustab-backup.0000000001.000000002")
                .to_string_lossy()
                .into_owned(),
        ];
        for requested in &refused {
            let error = resolve_requested(&target, requested, &DirectFs)
                .expect_err("only a listed backup may resolve");
            assert!(
                matches!(error, BackupError::NotABackupOfTarget { .. }),
                "unexpected error for {requested}: {error}"
            );
        }
    }

    #[test]
    fn read_text_returns_the_content_and_refuses_non_utf8() {
        let dir = TempDir::new("host-backups-read");
        let target = dir.join("fstab");
        seed(&target);

        let listed = list(&target, &DirectFs).expect("listing");
        let oldest = &listed[listed.len() - 1];
        assert_eq!(
            read_text(Path::new(&oldest.path), &DirectFs).expect("a backup must be readable"),
            "# original\n"
        );

        let binary = dir.join("fstab.rustab-backup.0000000009.000000000");
        fs::write(&binary, [0xff, 0xfe, 0x00]).expect("write invalid UTF-8");
        let error = read_text(&binary, &DirectFs).expect_err("invalid UTF-8 must be refused");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("copy it into place manually"));
    }

    /// A listing this session cannot take is a failure, never an empty list:
    /// "there are no backups" and "nobody could look" lead to different
    /// decisions in the caller.
    #[test]
    fn a_failed_host_listing_fails_both_entry_points() {
        let reader = ListingReader(Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Portal call failed",
        )));
        let target = Path::new("/etc/fstab");

        let error = list(target, &reader).expect_err("a failed listing must fail");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("Portal call failed"));

        let resolve_error = resolve_requested(
            target,
            "/etc/fstab.rustab-backup.0000000001.000000002",
            &reader,
        )
        .expect_err("a failed listing must fail the resolution too");
        match resolve_error {
            BackupError::List { path, source } => {
                assert_eq!(path, Path::new("/etc"));
                assert_eq!(source.kind(), io::ErrorKind::PermissionDenied);
            }
            other => panic!("a listing failure must not read as a refusal: {other}"),
        }
    }

    /// The host's own answer, reconstructed from a canned listing: the rows
    /// are built from the names and metadata the reader reported, for a
    /// directory this process never touches.
    #[test]
    fn list_builds_its_rows_from_the_listing_alone() {
        let reader = ListingReader(Ok(vec![
            BackupCandidate {
                name: OsString::from("fstab.rustab-backup.0000000001.000000002"),
                size_bytes: 11,
                modified_unix_secs: Some(1_760_000_000),
            },
            BackupCandidate {
                name: OsString::from("fstab.rustab-backup.0000000002.000000000.manual"),
                size_bytes: 22,
                modified_unix_secs: None,
            },
            BackupCandidate {
                name: OsString::from("fstab.bak"),
                size_bytes: 33,
                modified_unix_secs: Some(1_760_000_900),
            },
        ]));

        let listed = list(Path::new("/etc/fstab"), &reader).expect("the canned listing");

        assert_eq!(listed.len(), 2, "the near miss must be dropped: {listed:?}");
        assert_eq!(
            listed[0].path,
            "/etc/fstab.rustab-backup.0000000002.000000000.manual"
        );
        assert!(listed[0].is_manual);
        assert_eq!(listed[0].size_bytes, 22);
        assert_eq!(listed[0].created_unix_secs, None);
        assert_eq!(
            listed[1].path,
            "/etc/fstab.rustab-backup.0000000001.000000002"
        );
        assert!(!listed[1].is_manual);
        assert_eq!(listed[1].size_bytes, 11);
        assert_eq!(listed[1].created_unix_secs, Some(1_760_000_000));
        assert!(listed.iter().all(|row| !row.deletable));
    }
}
