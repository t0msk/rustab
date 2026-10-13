//! The `#[tauri::command]` layer: the complete, frozen set of calls the
//! frontend may make — see `docs/API_CONTRACT.md`.
//!
//! ## Shape of this module
//!
//! Each command is a thin, boring wrapper: it resolves the active target
//! through [`crate::target`], converts DTOs through [`crate::dto`], and
//! delegates the real work to `fstab-core`. No command re-implements
//! validation, serialization, backup naming or writing, and no command takes
//! a target path from the frontend (the single, carefully bounded exception
//! being [`restore_backup`] — see [`crate::backups`]).
//!
//! The genuinely interesting logic — "which save path do I take" — is
//! [`save_document_with`], and it is a single `if` on purpose: both branches
//! end in the same [`writer::write_document`] call, one of them simply has a
//! `pkexec` re-exec in front of it (see [`crate::privileged`]).
//!
//! ## Why every command is `#[tauri::command(async)]`
//!
//! Several of these do blocking work: `lsblk`, `findmnt`, file I/O, and in
//! the `/etc/fstab` case a `pkexec` call that blocks until the user has typed
//! a password. A plain synchronous Tauri command runs on the main thread,
//! which would freeze the window for the duration. `(async)` makes Tauri run
//! these synchronous functions on its thread pool instead, which keeps them
//! ordinary blocking Rust functions — directly callable from the unit tests
//! below, with no runtime to set up and no `AppHandle` to fake.
//!
//! ## Why the argument-taking commands add `rename_all = "snake_case"`
//!
//! Tauri renames command *arguments* to camelCase on the IPC boundary by
//! default, while serde leaves the DTO *fields* inside them in snake_case.
//! That mix would mean a frontend sending `{"backupPath": ..., }` whose
//! nested objects use `{"mount_point": ...}` — one spelling rule per nesting
//! level, which is exactly the kind of detail an integration task gets wrong
//! once and then debugs for an hour. Pinning the argument case to snake_case
//! makes the whole payload, at every level, snake_case.

use crate::backups;
use crate::dto::{
    BackupInfoDto, CommandErrorDto, CommandErrorKindDto, DeviceDiscoveryDto, DocumentDto,
    DocumentViewDto, TargetInfoDto, ValidationReportDto, WriteErrorDto, WriteErrorKindDto,
    WriteOutcomeDto,
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
                        "{} is not valid UTF-8 ({err}), so Rustab will not load it — editing it \
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
/// `fsync`ed, renamed, verified — and, for the real `/etc/fstab`, elevated
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

    // Both failure modes — "the list could not be read" and "the requested
    // path is not in it" — are reported as `InvalidBackup`: from the
    // frontend's point of view they are the same situation (this backup
    // cannot be used, and nothing was written), and the message says which
    // one it was.
    let backup = backups::resolve_requested(&target.path, &backup_path).map_err(|err| {
        WriteErrorDto::new(WriteErrorKindDto::InvalidBackup, true, err.to_string())
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

/// Lists block devices for the source picker.
///
/// Never fails: a system where `lsblk` is missing or unusable yields an empty
/// list with `lsblk_available: false`, and manual entry still works
/// (`docs/SPEC.md` §8).
#[tauri::command(async)]
pub fn discover_devices() -> DeviceDiscoveryDto {
    DeviceDiscoveryDto::from(&discovery::discover_devices())
}

// ---------------------------------------------------------------------------
// Shared internals
// ---------------------------------------------------------------------------

/// The one dispatch point between the two save paths.
///
/// Both branches end in [`writer::write_document`] — the in-process branch
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
