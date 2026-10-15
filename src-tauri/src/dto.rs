//! The serializable types that cross the Tauri command boundary — the frozen
//! contract documented in `docs/API_CONTRACT.md`.
//!
//! ## Why hand-written DTOs instead of `Serialize` on `fstab-core`'s types
//!
//! It would have been fewer lines to add `#[derive(Serialize, Deserialize)]`
//! to [`fstab_core::model`]'s types and send them straight to the frontend.
//! This module exists instead, and `fstab-core` was left completely
//! untouched, for three reasons:
//!
//! 1. **The wire format stops being an accident of the Rust types.** The
//!    frontend (`frontend/`, a separate Yew/wasm crate that deliberately does
//!    *not* depend on `fstab-core`) has to mirror these shapes by hand. A
//!    derived format would silently change shape the day someone renames a
//!    field or reorders an enum variant in the domain model; here, the wire
//!    format can only change by editing this file, next to the doc comment
//!    that says what the frontend expects.
//! 2. **The frontend wants a flatter shape than the model.**
//!    [`fstab_core::model::SourceId`] is an eight-variant enum whose payload
//!    is only meaningful together with the variant; the UI wants a
//!    `{kind, value}` pair it can drive a dropdown and a text field from
//!    (`docs/UI_DESIGN.md` §4). Same for options, which the UI edits as
//!    `{key, value}` rows.
//! 3. **No new dependency in the safety-critical crate.** `fstab-core` is the
//!    crate holding the atomic-write guarantee; it currently depends only on
//!    `serde_json`. Adding `serde` + derive macros there to serve a GUI
//!    concern is the wrong direction of dependency.
//!
//! ## Losslessness
//!
//! Every conversion in this module is lossless and total in both directions:
//! `Document -> DocumentDto -> Document` reproduces the original document
//! exactly (asserted by `round_trips_*` tests below, including against the
//! committed fixtures in `tests/fixtures/`). That matters because a save
//! sends a `DocumentDto` back and a lossy round trip would mean the file
//! written is not the file the user was shown.
//!
//! `DocumentDto -> Document` is infallible *by construction*: the only values
//! that could be rejected (an unknown `kind` string, a negative or
//! out-of-range `dump`/`pass`) cannot be represented in the DTO at all, so
//! they are rejected by serde at the Tauri boundary before any of this code
//! runs. See `docs/API_CONTRACT.md` ("Argument validation") for what the
//! frontend sees when that happens.

use fstab_core::discovery::{DeviceInfo, DiscoveryResult};
use fstab_core::model::{Document, FstabEntry, FstabOption, Line, Options, SourceId};
use fstab_core::validate::{Finding, FindingField, Severity, ValidationReport};
use fstab_core::writer::{
    CreateBackupError, CreateBackupOutcome, FsyncStage, WriteError, WriteOutcome,
};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Renders a path for the wire.
///
/// Paths cross the boundary as plain strings. A path that is not valid UTF-8
/// is rendered lossily (U+FFFD replacement characters) rather than failing the
/// whole command: the frontend's job with these strings is to display them,
/// and the only commands that take a path *back*
/// ([`crate::commands::restore_backup`], [`crate::commands::read_backup`] and
/// [`crate::commands::delete_backup`]) do not trust the string they are given —
/// each matches it against the real backup list, so a mangled path fails
/// closed with "not a backup of the active target" instead of reading,
/// writing or deleting somewhere unintended. See `docs/API_CONTRACT.md`
/// ("Non-UTF-8 paths").
fn path_to_wire(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// Target info
// ---------------------------------------------------------------------------

/// How the active target was chosen — mirrors [`crate::target::TargetSource`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetSourceDto {
    /// `RUSTAB_TARGET_PATH` was set, so the user asked for this file.
    Environment,
    /// Nothing was set, so this is the auto-created per-user sandbox.
    SandboxDefault,
}

/// The active target: which file every read and write in this session uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetInfoDto {
    /// Absolute (or as-configured) path of the active target.
    pub path: String,
    /// `true` only for the real `/etc/fstab`. Drives the UI's danger banner
    /// (`docs/UI_DESIGN.md` §2) *and* decides whether a save escalates.
    pub is_real: bool,
    /// Where the path came from, so the UI can explain itself.
    pub source: TargetSourceDto,
}

impl From<&crate::target::ActiveTarget> for TargetInfoDto {
    fn from(target: &crate::target::ActiveTarget) -> Self {
        TargetInfoDto {
            path: path_to_wire(&target.path),
            is_real: target.is_real,
            source: match target.source {
                crate::target::TargetSource::Environment => TargetSourceDto::Environment,
                crate::target::TargetSource::SandboxDefault => TargetSourceDto::SandboxDefault,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Documents, lines, entries
// ---------------------------------------------------------------------------

/// Which `fstab` source syntax an entry's first field uses.
///
/// Wire values are the snake_case spellings: `"path"`, `"uuid"`, `"label"`,
/// `"part_uuid"`, `"part_label"`, `"none"`, `"network"`, `"other"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKindDto {
    /// A plain filesystem path, e.g. `/dev/sda1` or a bind-mount source.
    Path,
    /// `UUID=<value>`.
    Uuid,
    /// `LABEL=<value>`.
    Label,
    /// `PARTUUID=<value>`.
    PartUuid,
    /// `PARTLABEL=<value>`.
    PartLabel,
    /// The literal `none` source (swap files, bind mounts, pseudo filesystems).
    None,
    /// A network source such as `//server/share` or `server:/export`.
    Network,
    /// Anything else, preserved verbatim.
    Other,
}

/// An entry's source field, split into "which syntax" and "the value".
///
/// `value` never includes the `KEY=` prefix: for [`SourceKindDto::Uuid`] it is
/// the bare UUID text. For [`SourceKindDto::None`] the value is ignored on the
/// way in and reported as `"none"` on the way out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceDto {
    /// Which source syntax this is.
    pub kind: SourceKindDto,
    /// The raw value, prefix-free, with any octal-escaped whitespace left
    /// exactly as it appears in the file (see `fstab_core::model`'s
    /// escaped-whitespace policy — this layer does not decode or re-encode).
    pub value: String,
}

impl From<&SourceId> for SourceDto {
    fn from(source: &SourceId) -> Self {
        let kind = match source {
            SourceId::Path(_) => SourceKindDto::Path,
            SourceId::Uuid(_) => SourceKindDto::Uuid,
            SourceId::Label(_) => SourceKindDto::Label,
            SourceId::PartUuid(_) => SourceKindDto::PartUuid,
            SourceId::PartLabel(_) => SourceKindDto::PartLabel,
            SourceId::None => SourceKindDto::None,
            SourceId::Network(_) => SourceKindDto::Network,
            SourceId::Other(_) => SourceKindDto::Other,
        };
        SourceDto {
            kind,
            value: source.raw_value().to_string(),
        }
    }
}

impl From<SourceDto> for SourceId {
    fn from(dto: SourceDto) -> Self {
        match dto.kind {
            SourceKindDto::Path => SourceId::Path(dto.value),
            SourceKindDto::Uuid => SourceId::Uuid(dto.value),
            SourceKindDto::Label => SourceId::Label(dto.value),
            SourceKindDto::PartUuid => SourceId::PartUuid(dto.value),
            SourceKindDto::PartLabel => SourceId::PartLabel(dto.value),
            SourceKindDto::None => SourceId::None,
            SourceKindDto::Network => SourceId::Network(dto.value),
            SourceKindDto::Other => SourceId::Other(dto.value),
        }
    }
}

/// One mount option: a bare flag when `value` is `null`, a `key=value` pair
/// otherwise. An empty-string `value` is a literal `key=` with nothing after
/// the `=` and is preserved as such.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptionDto {
    /// The option name (`noauto`, `fmask`, `x-systemd.automount`, ...).
    pub key: String,
    /// The option's value, or `null` for a bare flag.
    pub value: Option<String>,
}

impl From<&FstabOption> for OptionDto {
    fn from(option: &FstabOption) -> Self {
        OptionDto {
            key: option.key().to_string(),
            value: option.value().map(str::to_string),
        }
    }
}

impl From<OptionDto> for FstabOption {
    fn from(dto: OptionDto) -> Self {
        match dto.value {
            Some(value) => FstabOption::KeyValue(dto.key, value),
            None => FstabOption::Flag(dto.key),
        }
    }
}

/// A successfully parsed fstab entry (one six-field line).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryDto {
    /// First field: the source identifier.
    pub source: SourceDto,
    /// Second field: the mount point, raw (escape sequences undecoded).
    pub mount_point: String,
    /// Third field: the filesystem type, free text.
    pub fs_type: String,
    /// Fourth field: the options, in file order. An empty list renders as an
    /// *empty* options field, which `findmnt --verify` may reject — the UI
    /// should offer `defaults` rather than nothing.
    pub options: Vec<OptionDto>,
    /// Fifth field: the `dump(8)` flag.
    pub dump: u32,
    /// Sixth field: the `fsck` pass number.
    pub pass: u32,
    /// Text of a trailing `#` comment on the same line, without the `#`, or
    /// `null` if there is none.
    pub trailing_comment: Option<String>,
}

impl From<&FstabEntry> for EntryDto {
    fn from(entry: &FstabEntry) -> Self {
        EntryDto {
            source: SourceDto::from(&entry.source),
            mount_point: entry.mount_point.clone(),
            fs_type: entry.fs_type.clone(),
            options: entry
                .options
                .as_slice()
                .iter()
                .map(OptionDto::from)
                .collect(),
            dump: entry.dump,
            pass: entry.pass,
            trailing_comment: entry.trailing_comment.clone(),
        }
    }
}

impl From<EntryDto> for FstabEntry {
    fn from(dto: EntryDto) -> Self {
        FstabEntry {
            source: SourceId::from(dto.source),
            mount_point: dto.mount_point,
            fs_type: dto.fs_type,
            options: Options::from_vec(dto.options.into_iter().map(FstabOption::from).collect()),
            dump: dto.dump,
            pass: dto.pass,
            trailing_comment: dto.trailing_comment,
        }
    }
}

/// One line of the document, tagged by `kind`.
///
/// Wire shapes: `{"kind":"entry","entry":{...}}`,
/// `{"kind":"comment","text":"..."}`, `{"kind":"blank"}`,
/// `{"kind":"malformed","raw":"..."}`.
///
/// Every line of the file is present, in order, including comments, blanks
/// and lines the parser could not understand — dropping any of them would
/// break the byte-faithful round trip the whole design rests on. A line's
/// identity for edit/delete purposes is simply its index in
/// [`DocumentDto::lines`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LineDto {
    /// A parsed entry.
    Entry {
        /// The entry's fields.
        entry: EntryDto,
    },
    /// A whole-line comment: the text after the leading `#`, without the `#`.
    Comment {
        /// Comment text, `#` excluded.
        text: String,
    },
    /// An empty or whitespace-only line.
    Blank,
    /// A line the parser could not read as an entry, comment or blank. Shown
    /// to the user verbatim with a warning, never silently dropped
    /// (`docs/SPEC.md` §5, §9).
    Malformed {
        /// The original line text.
        raw: String,
    },
}

impl From<&Line> for LineDto {
    fn from(line: &Line) -> Self {
        match line {
            Line::Entry(entry) => LineDto::Entry {
                entry: EntryDto::from(entry),
            },
            Line::Comment(text) => LineDto::Comment { text: text.clone() },
            Line::Blank => LineDto::Blank,
            Line::Malformed(raw) => LineDto::Malformed { raw: raw.clone() },
        }
    }
}

impl From<LineDto> for Line {
    fn from(dto: LineDto) -> Self {
        match dto {
            LineDto::Entry { entry } => Line::Entry(FstabEntry::from(entry)),
            LineDto::Comment { text } => Line::Comment(text),
            LineDto::Blank => Line::Blank,
            LineDto::Malformed { raw } => Line::Malformed(raw),
        }
    }
}

/// A whole fstab document: every line, in file order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentDto {
    /// The lines, in order.
    pub lines: Vec<LineDto>,
}

impl From<&Document> for DocumentDto {
    fn from(doc: &Document) -> Self {
        DocumentDto {
            lines: doc.lines.iter().map(LineDto::from).collect(),
        }
    }
}

impl From<DocumentDto> for Document {
    fn from(dto: DocumentDto) -> Self {
        Document::from_lines(dto.lines.into_iter().map(Line::from).collect())
    }
}

/// What [`crate::commands::list_entries`] returns: the active target, the
/// file's exact text, and its parsed form.
///
/// `text` is included deliberately even though `document` could be
/// re-rendered from it: the frontend cannot render a `Document` (it has no
/// dependency on `fstab-core`), and the diff view (`docs/UI_DESIGN.md` §6)
/// needs the *before* text to diff a candidate against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentViewDto {
    /// The target this content was read from.
    pub target: TargetInfoDto,
    /// `false` if the target does not exist yet (a valid first-save state, in
    /// which case `text` is empty and `document.lines` is empty).
    pub exists: bool,
    /// The file's exact content as read.
    pub text: String,
    /// The parsed document.
    pub document: DocumentDto,
    /// `true` if any line is [`LineDto::Malformed`], so the UI can show the
    /// "some lines could not be understood" warning without scanning.
    pub has_malformed_lines: bool,
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Whether a finding blocks a save.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeverityDto {
    /// Blocking: a save will be refused while this is present.
    Error,
    /// Advisory: worth showing, does not block a save.
    Warning,
}

impl From<Severity> for SeverityDto {
    fn from(severity: Severity) -> Self {
        match severity {
            Severity::Error => SeverityDto::Error,
            Severity::Warning => SeverityDto::Warning,
        }
    }
}

/// Which of an entry's fields a [`FindingDto`] is about, when it is about
/// exactly one. Mirrors [`FindingField`]; see that type's doc comment for
/// why `None` on [`FindingDto::field`] does not mean "unknown".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationFieldDto {
    /// The entry's mount point.
    MountPoint,
    /// The entry's source device/identifier.
    Source,
    /// The entry's filesystem type.
    FsType,
}

impl From<FindingField> for ValidationFieldDto {
    fn from(field: FindingField) -> Self {
        match field {
            FindingField::MountPoint => ValidationFieldDto::MountPoint,
            FindingField::Source => ValidationFieldDto::Source,
            FindingField::FsType => ValidationFieldDto::FsType,
        }
    }
}

/// A single validation finding, ready to display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingDto {
    /// Blocking or advisory.
    pub severity: SeverityDto,
    /// Human-readable message, suitable for showing verbatim.
    pub message: String,
    /// Which single field of an entry this finding is about, if any. `null`
    /// means this finding is not about exactly one field — see
    /// [`FindingField`]'s doc comment for the full explanation.
    pub field: Option<ValidationFieldDto>,
    /// The 0-based index into the validated `DocumentDto.lines` array of the
    /// single line this finding is about, if any — the same identity scheme
    /// [`LineDto`]'s doc comment already describes elsewhere in this file.
    /// `null` for the same reason [`Self::field`] is `null`.
    pub line_index: Option<usize>,
}

impl From<&Finding> for FindingDto {
    fn from(finding: &Finding) -> Self {
        FindingDto {
            severity: SeverityDto::from(finding.severity),
            message: finding.message.clone(),
            field: finding.field.map(ValidationFieldDto::from),
            line_index: finding.line_index,
        }
    }
}

/// The result of validating a candidate document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationReportDto {
    /// Every finding, structural then semantic, in the order produced.
    pub findings: Vec<FindingDto>,
    /// `true` if at least one finding is [`SeverityDto::Error`]. Precomputed
    /// so the UI's "can I enable Save?" check cannot disagree with the
    /// backend's.
    pub has_errors: bool,
}

impl From<&ValidationReport> for ValidationReportDto {
    fn from(report: &ValidationReport) -> Self {
        ValidationReportDto {
            findings: report.findings.iter().map(FindingDto::from).collect(),
            has_errors: report.has_errors(),
        }
    }
}

// ---------------------------------------------------------------------------
// Write outcome and write errors
// ---------------------------------------------------------------------------

/// What a successful save did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteOutcomeDto {
    /// The file actually written — differs from the active target only if the
    /// target was a symlink, in which case this is its destination.
    pub target: String,
    /// The backup holding the previous content, or `null` if the target did
    /// not exist before (first-ever save).
    pub backup: Option<String>,
    /// Backups deleted to honour the retention bound, oldest first.
    pub pruned_backups: Vec<String>,
    /// Advisory validation findings that did *not* block the save.
    pub validation_warnings: Vec<FindingDto>,
    /// Non-fatal notes (symlink resolved, ownership not carried over, an old
    /// backup that could not be pruned). None of these affect the integrity
    /// of the file just written.
    pub notes: Vec<String>,
    /// `true` if the write was performed by the elevated helper via `pkexec`.
    /// Set by the unprivileged side after the helper reports back — the
    /// helper itself cannot know how it was launched, so it always emits
    /// `false` (see [`crate::privileged`]).
    pub elevated: bool,
}

impl WriteOutcomeDto {
    /// Converts a [`WriteOutcome`], recording whether it came from the
    /// elevated helper.
    pub fn from_outcome(outcome: &WriteOutcome, elevated: bool) -> Self {
        WriteOutcomeDto {
            target: path_to_wire(&outcome.target),
            backup: outcome.backup.as_deref().map(path_to_wire),
            pruned_backups: outcome
                .pruned_backups
                .iter()
                .map(|path| path_to_wire(path))
                .collect(),
            validation_warnings: outcome
                .validation_warnings
                .iter()
                .map(FindingDto::from)
                .collect(),
            notes: outcome.notes.clone(),
            elevated,
        }
    }
}

/// Which stage of a save failed.
///
/// The first ten variants map one-to-one onto [`WriteError`]'s variants; the
/// rest describe failures *around* the write — resolving the target,
/// escalating privileges, choosing or reading a backup to restore, and an
/// internal protocol failure — which `fstab-core` knows nothing about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteErrorKindDto {
    /// The active target could not be determined (so nothing was attempted).
    TargetResolution,
    /// The candidate failed validation. `report` carries every finding.
    Validation,
    /// The target path cannot be written to at all (a directory, a dangling
    /// symlink, no file-name component).
    InvalidTarget,
    /// Backing up the previous content failed.
    Backup,
    /// Writing the same-directory temp file failed.
    Write,
    /// Applying the previous file's permissions to the temp file failed.
    Metadata,
    /// An `fsync` failed. Check `target_unchanged`: a failure of the
    /// *post-rename* directory `fsync` means the new content is already live
    /// but its durability is unconfirmed.
    Fsync,
    /// The atomic rename failed.
    Rename,
    /// The target could not be re-read for verification after a successful
    /// rename.
    ReadBack,
    /// The target was re-read but did not match what was written. Should be
    /// unreachable; treated as a hard error, never a warning.
    Verify,
    /// The elevated helper could not be launched, was dismissed by the user,
    /// or did not report a usable result.
    PrivilegeEscalation,
    /// The backup a restore asked for is not one of the backups the active
    /// target actually has (or the backup list could not be read). Nothing
    /// was written.
    InvalidBackup,
    /// The requested backup exists but could not be read, or is not valid
    /// UTF-8. Nothing was written.
    ReadBackup,
    /// A bug or a protocol violation between the two halves of the elevated
    /// write.
    Internal,
}

/// A failed save or restore.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteErrorDto {
    /// Which stage failed.
    pub kind: WriteErrorKindDto,
    /// The full human-readable message, already explaining whether the
    /// original file survived. Safe to show verbatim.
    pub message: String,
    /// `true` means the target is **guaranteed** to still hold its previous
    /// content, byte for byte. `false` means that guarantee cannot be made —
    /// not that the file is definitely damaged. Never the other way round:
    /// nothing sets this to `true` on a maybe.
    pub target_unchanged: bool,
    /// For [`WriteErrorKindDto::Validation`], the full report (warnings
    /// included). `null` for every other kind.
    pub report: Option<ValidationReportDto>,
    /// The path the failure concerned, when there is a single meaningful one.
    pub path: Option<String>,
    /// For [`WriteErrorKindDto::Verify`], the backup holding the pre-save
    /// content, if one was made.
    pub backup: Option<String>,
}

impl WriteErrorDto {
    /// Builds an error of `kind` with `message`, with no report or paths
    /// attached.
    pub fn new(
        kind: WriteErrorKindDto,
        target_unchanged: bool,
        message: impl Into<String>,
    ) -> Self {
        WriteErrorDto {
            kind,
            message: message.into(),
            target_unchanged,
            report: None,
            path: None,
            backup: None,
        }
    }

    /// Attaches the path the failure concerned.
    #[must_use]
    pub fn with_path(mut self, path: &Path) -> Self {
        self.path = Some(path_to_wire(path));
        self
    }
}

impl From<&WriteError> for WriteErrorDto {
    fn from(error: &WriteError) -> Self {
        // `Display` already spells out the consequence ("the original file is
        // unchanged" / "it is in place but may not survive a power loss"), so
        // the message is taken verbatim rather than re-worded here.
        let message = error.to_string();
        match error {
            WriteError::Validation(report) => WriteErrorDto {
                report: Some(ValidationReportDto::from(report)),
                ..WriteErrorDto::new(WriteErrorKindDto::Validation, true, message)
            },
            WriteError::InvalidTarget { path, .. } => {
                WriteErrorDto::new(WriteErrorKindDto::InvalidTarget, true, message).with_path(path)
            }
            WriteError::Backup { path, .. } => {
                WriteErrorDto::new(WriteErrorKindDto::Backup, true, message).with_path(path)
            }
            WriteError::Write { path, .. } => {
                WriteErrorDto::new(WriteErrorKindDto::Write, true, message).with_path(path)
            }
            WriteError::Metadata { path, .. } => {
                WriteErrorDto::new(WriteErrorKindDto::Metadata, true, message).with_path(path)
            }
            WriteError::Fsync { stage, path, .. } => {
                // The only `fsync` that happens after the rename is the
                // second directory sync; at that point the new content is
                // already visible under the target's name.
                let unchanged = !matches!(stage, FsyncStage::DirectoryAfterRename);
                WriteErrorDto::new(WriteErrorKindDto::Fsync, unchanged, message).with_path(path)
            }
            WriteError::Rename { to, .. } => {
                WriteErrorDto::new(WriteErrorKindDto::Rename, true, message).with_path(to)
            }
            WriteError::ReadBack { path, .. } => {
                // The rename succeeded, so the new content is live; only the
                // read-back failed.
                WriteErrorDto::new(WriteErrorKindDto::ReadBack, false, message).with_path(path)
            }
            WriteError::Verify { target, backup, .. } => WriteErrorDto {
                backup: backup.as_deref().map(path_to_wire),
                // `expected`/`actual` are deliberately not forwarded: they
                // can be whole files, and the UI's recovery path is "restore
                // the backup", not "diff two blobs in a toast".
                ..WriteErrorDto::new(WriteErrorKindDto::Verify, false, message).with_path(target)
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Read-side command errors
// ---------------------------------------------------------------------------

/// Which part of a read-only command failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandErrorKindDto {
    /// The active target could not be determined (e.g. the sandbox file could
    /// not be created).
    TargetResolution,
    /// The target exists but could not be read (most often permissions).
    ReadTarget,
    /// The target's bytes are not valid UTF-8, so it cannot be represented or
    /// safely re-serialized. Also reported for a *backup* whose bytes are not
    /// valid UTF-8 (see [`crate::commands::read_backup`]), rather than adding
    /// a second kind that would mean the same thing to the frontend.
    NotUtf8,
    /// The target's directory could not be listed for backups.
    ListBackups,
    /// The requested backup is not one of the backups the active target
    /// actually has (or the backup list could not be read). The read-only
    /// counterpart of [`WriteErrorKindDto::InvalidBackup`].
    InvalidBackup,
    /// The requested backup is one of this target's own backups but could not
    /// be read (permissions, I/O error). A backup that exists and is readable
    /// but is not valid UTF-8 is reported as [`CommandErrorKindDto::NotUtf8`].
    ReadBackup,
    /// The host file manager could not be launched
    /// ([`crate::commands::open_backup_location`]) — the `xdg-open` binary is
    /// missing, or starting it failed for some other reason.
    OpenLocation,
}

/// A failed read-only command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandErrorDto {
    /// Which part failed.
    pub kind: CommandErrorKindDto,
    /// Human-readable message, safe to show verbatim.
    pub message: String,
}

impl CommandErrorDto {
    /// Builds an error of `kind` with `message`.
    pub fn new(kind: CommandErrorKindDto, message: impl Into<String>) -> Self {
        CommandErrorDto {
            kind,
            message: message.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Backup deletion errors
// ---------------------------------------------------------------------------

/// Why deleting one of the active target's backups failed.
///
/// Deliberately *not* [`WriteErrorKindDto`]: deleting a backup never opens,
/// renames or otherwise touches the active target file, so the whole notion of
/// `target_unchanged` (the field the UI branches on for a save) has no meaning
/// here — it would be trivially `true` for every variant and would invite the
/// frontend to read significance into it. A small, separate shape says the
/// true thing instead: either the backup is gone, or it is not, and nothing
/// else moved either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteBackupErrorKindDto {
    /// The active target could not be determined, so its backups could not be
    /// identified. Nothing was deleted.
    TargetResolution,
    /// The requested path is not one of the backups the active target actually
    /// has (or the backup list could not be read). Nothing was deleted, and
    /// the path was never used as a path (see [`crate::backups`]).
    InvalidBackup,
    /// Real-target only: `pkexec` is missing, the prompt was dismissed or
    /// refused, or the elevated helper produced no usable response.
    PrivilegeEscalation,
    /// The file was a genuine backup of the active target but could not be
    /// removed (permissions, I/O error, or the elevated helper refusing a
    /// target other than the one it is allowed to manage).
    Delete,
}

/// A failed [`crate::commands::delete_backup`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteBackupErrorDto {
    /// Which part failed.
    pub kind: DeleteBackupErrorKindDto,
    /// Human-readable message, safe to show verbatim.
    pub message: String,
}

impl DeleteBackupErrorDto {
    /// Builds an error of `kind` with `message`.
    pub fn new(kind: DeleteBackupErrorKindDto, message: impl Into<String>) -> Self {
        DeleteBackupErrorDto {
            kind,
            message: message.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Backups
// ---------------------------------------------------------------------------

/// One backup of the active target, automatic or manual.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupInfoDto {
    /// Full path, and the exact string [`crate::commands::restore_backup`],
    /// [`crate::commands::read_backup`] and
    /// [`crate::commands::delete_backup`] expect back.
    pub path: String,
    /// Just the file name, for a compact list display.
    pub file_name: String,
    /// The target this is a backup of.
    pub target_path: String,
    /// Size in bytes.
    pub size_bytes: u64,
    /// The backup file's modification time as whole seconds since the Unix
    /// epoch — i.e. when the backup was taken. `null` if the platform could
    /// not report it.
    pub created_unix_secs: Option<u64>,
    /// `true` if a user asked for this backup explicitly
    /// ([`crate::commands::create_backup`]), `false` if a save made it on its
    /// own. Reported by `fstab-core`, which owns the naming scheme the
    /// distinction is encoded in — the frontend must not try to infer it from
    /// `file_name`.
    ///
    /// Worth surfacing because the two kinds behave differently: automatic
    /// backups are pruned once there are more than ten of them, manual ones
    /// are kept until the user deletes them.
    pub is_manual: bool,
}

// ---------------------------------------------------------------------------
// Manual backup creation
// ---------------------------------------------------------------------------

/// What a successful [`crate::commands::create_backup`] did, as the elevated
/// helper reports it.
///
/// Internal to the escalation protocol rather than part of the command's own
/// return shape: the command resolves this into a [`BackupInfoDto`] so the
/// frontend gets a row in exactly the shape
/// [`crate::commands::list_backups`] already hands it, with no second backup
/// shape to mirror.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateBackupOutcomeDto {
    /// The file that was backed up — the active target, or its destination if
    /// the target is a symlink.
    pub target: String,
    /// The backup that was created. Always a manual one.
    pub backup: String,
    /// *Automatic* backups deleted to honour the retention bound, oldest
    /// first. Normally empty: a manual backup does not count towards that
    /// bound, so this is non-empty only if the target was already over it.
    pub pruned_backups: Vec<String>,
    /// Non-fatal notes (a symlink that was followed, an old automatic backup
    /// that could not be pruned). None of these affect the backup that was
    /// taken.
    pub notes: Vec<String>,
}

impl CreateBackupOutcomeDto {
    /// Converts a [`CreateBackupOutcome`].
    pub fn from_outcome(outcome: &CreateBackupOutcome) -> Self {
        CreateBackupOutcomeDto {
            target: path_to_wire(&outcome.target),
            backup: path_to_wire(&outcome.backup),
            pruned_backups: outcome
                .pruned_backups
                .iter()
                .map(|path| path_to_wire(path))
                .collect(),
            notes: outcome.notes.clone(),
        }
    }
}

/// Why creating a backup of the active target failed.
///
/// Deliberately *not* [`WriteErrorKindDto`], for the same reason
/// [`DeleteBackupErrorKindDto`] is not: creating a backup never opens, renames
/// or writes the active target, so `target_unchanged` would be trivially
/// `true` for every variant and would invite the frontend to read significance
/// into it. Either a new backup exists, or it does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CreateBackupErrorKindDto {
    /// The active target could not be determined, so there was nothing to back
    /// up. No backup was created.
    TargetResolution,
    /// The target does not exist on disk yet (a first-ever-save state), so
    /// there is nothing to copy. Its own kind rather than a generic failure:
    /// the user clicked "back this up now", and the honest answer is "there is
    /// no file yet — save once first", not a silent success.
    TargetMissing,
    /// The target path cannot be backed up at all (it is a directory, a
    /// dangling symlink, or has no file-name component).
    InvalidTarget,
    /// Copying the target to its new backup, or flushing that copy to disk,
    /// failed. The target itself is untouched — it was only ever read.
    Backup,
    /// Real-target only: `pkexec` is missing, the prompt was dismissed or
    /// refused, or the elevated helper produced no usable response.
    PrivilegeEscalation,
}

/// A failed [`crate::commands::create_backup`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateBackupErrorDto {
    /// Which part failed.
    pub kind: CreateBackupErrorKindDto,
    /// Human-readable message, safe to show verbatim.
    pub message: String,
}

impl CreateBackupErrorDto {
    /// Builds an error of `kind` with `message`.
    pub fn new(kind: CreateBackupErrorKindDto, message: impl Into<String>) -> Self {
        CreateBackupErrorDto {
            kind,
            message: message.into(),
        }
    }
}

impl From<&CreateBackupError> for CreateBackupErrorDto {
    fn from(error: &CreateBackupError) -> Self {
        // `Display` already words the consequence for a user ("no backup was
        // created", "save the file once first"), so the message is taken
        // verbatim rather than re-worded here — the same policy
        // [`WriteErrorDto::from`] follows.
        let message = error.to_string();
        let kind = match error {
            CreateBackupError::InvalidTarget { .. } => CreateBackupErrorKindDto::InvalidTarget,
            CreateBackupError::TargetMissing { .. } => CreateBackupErrorKindDto::TargetMissing,
            CreateBackupError::Backup { .. } => CreateBackupErrorKindDto::Backup,
        };
        CreateBackupErrorDto::new(kind, message)
    }
}

// ---------------------------------------------------------------------------
// Device discovery
// ---------------------------------------------------------------------------

/// One discovered block device, for the source picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfoDto {
    /// Device node path, e.g. `/dev/sda1`.
    pub path: String,
    /// Filesystem UUID, if the device has one.
    pub uuid: Option<String>,
    /// Filesystem label, if set.
    pub label: Option<String>,
    /// Partition UUID, if applicable.
    pub partuuid: Option<String>,
    /// Partition label, if set.
    pub partlabel: Option<String>,
    /// Detected filesystem type, if any.
    pub fs_type: Option<String>,
}

impl From<&DeviceInfo> for DeviceInfoDto {
    fn from(device: &DeviceInfo) -> Self {
        DeviceInfoDto {
            path: device.path.clone(),
            uuid: device.uuid.clone(),
            label: device.label.clone(),
            partuuid: device.partuuid.clone(),
            partlabel: device.partlabel.clone(),
            fs_type: device.fs_type.clone(),
        }
    }
}

/// The result of device discovery. Never an error: a system without `lsblk`
/// yields an empty list with `lsblk_available: false`, and manual entry still
/// works (`docs/SPEC.md` §8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceDiscoveryDto {
    /// The devices found, in `lsblk` order.
    pub devices: Vec<DeviceInfoDto>,
    /// `false` if `lsblk` could not be run at all, so the UI can say "device
    /// discovery is unavailable" instead of "no devices found".
    pub lsblk_available: bool,
}

impl From<&DiscoveryResult> for DeviceDiscoveryDto {
    fn from(result: &DiscoveryResult) -> Self {
        DeviceDiscoveryDto {
            devices: result.devices.iter().map(DeviceInfoDto::from).collect(),
            lsblk_available: result.lsblk_available,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::{ActiveTarget, TargetSource};
    use fstab_core::model::Line;
    use fstab_core::parser;
    use fstab_core::validate::Severity;
    use std::io;
    use std::path::PathBuf;

    /// The frozen wire format, asserted literally.
    ///
    /// `docs/API_CONTRACT.md` quotes this JSON, and `frontend/` mirrors these
    /// shapes by hand. Any change to a field name, a `kind` spelling or the
    /// nesting breaks a frontend that cannot be recompiled against this
    /// crate's types — so it has to break this test first, loudly, next to
    /// the documentation that has to be updated with it.
    #[test]
    fn document_wire_format_is_exactly_as_documented() {
        let document = parser::parse(
            "# hi\n\
             \n\
             UUID=1111 /mnt/d ext4 noauto,fmask=0022 0 2 # note\n\
             /dev/sdb1\n",
        );

        let json = serde_json::to_string(&DocumentDto::from(&document)).expect("serialize");

        assert_eq!(
            json,
            concat!(
                r#"{"lines":["#,
                r#"{"kind":"comment","text":" hi"},"#,
                r#"{"kind":"blank"},"#,
                r#"{"kind":"entry","entry":{"#,
                r#""source":{"kind":"uuid","value":"1111"},"#,
                r#""mount_point":"/mnt/d","fs_type":"ext4","#,
                r#""options":[{"key":"noauto","value":null},{"key":"fmask","value":"0022"}],"#,
                r#""dump":0,"pass":2,"trailing_comment":" note"}},"#,
                r#"{"kind":"malformed","raw":"/dev/sdb1"}"#,
                r#"]}"#,
            )
        );
    }

    #[test]
    fn remaining_wire_formats_are_exactly_as_documented() {
        let view = DocumentViewDto {
            target: TargetInfoDto {
                path: "/p".to_string(),
                is_real: false,
                source: TargetSourceDto::SandboxDefault,
            },
            exists: true,
            text: "# hi\n".to_string(),
            document: DocumentDto {
                lines: vec![LineDto::Blank],
            },
            has_malformed_lines: false,
        };
        assert_eq!(
            serde_json::to_string(&view).expect("serialize"),
            r##"{"target":{"path":"/p","is_real":false,"source":"sandbox_default"},"exists":true,"text":"# hi\n","document":{"lines":[{"kind":"blank"}]},"has_malformed_lines":false}"##
        );

        let report = ValidationReportDto {
            findings: vec![FindingDto {
                severity: SeverityDto::Warning,
                message: "m".to_string(),
                field: None,
                line_index: None,
            }],
            has_errors: false,
        };
        assert_eq!(
            serde_json::to_string(&report).expect("serialize"),
            r#"{"findings":[{"severity":"warning","message":"m","field":null,"line_index":null}],"has_errors":false}"#
        );

        // A field/line-tagged finding — the shape `mount_point_missing`,
        // `malformed_uuid`/`malformed_partuuid`, and `empty_fs_type` produce.
        let tagged_report = ValidationReportDto {
            findings: vec![FindingDto {
                severity: SeverityDto::Warning,
                message: "m".to_string(),
                field: Some(ValidationFieldDto::MountPoint),
                line_index: Some(2),
            }],
            has_errors: false,
        };
        assert_eq!(
            serde_json::to_string(&tagged_report).expect("serialize"),
            r#"{"findings":[{"severity":"warning","message":"m","field":"mount_point","line_index":2}],"has_errors":false}"#
        );

        let outcome = WriteOutcomeDto {
            target: "/p".to_string(),
            backup: Some("/p.bak".to_string()),
            pruned_backups: Vec::new(),
            validation_warnings: Vec::new(),
            notes: vec!["n".to_string()],
            elevated: true,
        };
        assert_eq!(
            serde_json::to_string(&outcome).expect("serialize"),
            r#"{"target":"/p","backup":"/p.bak","pruned_backups":[],"validation_warnings":[],"notes":["n"],"elevated":true}"#
        );

        let error = WriteErrorDto::new(WriteErrorKindDto::PrivilegeEscalation, true, "m");
        assert_eq!(
            serde_json::to_string(&error).expect("serialize"),
            r#"{"kind":"privilege_escalation","message":"m","target_unchanged":true,"report":null,"path":null,"backup":null}"#
        );

        let command_error = CommandErrorDto::new(CommandErrorKindDto::NotUtf8, "m");
        assert_eq!(
            serde_json::to_string(&command_error).expect("serialize"),
            r#"{"kind":"not_utf8","message":"m"}"#
        );

        let delete_error = DeleteBackupErrorDto::new(DeleteBackupErrorKindDto::InvalidBackup, "m");
        assert_eq!(
            serde_json::to_string(&delete_error).expect("serialize"),
            r#"{"kind":"invalid_backup","message":"m"}"#
        );

        let backup = BackupInfoDto {
            path: "/p.b".to_string(),
            file_name: "p.b".to_string(),
            target_path: "/p".to_string(),
            size_bytes: 12,
            created_unix_secs: Some(1_760_000_000),
            is_manual: true,
        };
        assert_eq!(
            serde_json::to_string(&backup).expect("serialize"),
            r#"{"path":"/p.b","file_name":"p.b","target_path":"/p","size_bytes":12,"created_unix_secs":1760000000,"is_manual":true}"#
        );

        let create_outcome = CreateBackupOutcomeDto {
            target: "/p".to_string(),
            backup: "/p.b".to_string(),
            pruned_backups: Vec::new(),
            notes: vec!["n".to_string()],
        };
        assert_eq!(
            serde_json::to_string(&create_outcome).expect("serialize"),
            r#"{"target":"/p","backup":"/p.b","pruned_backups":[],"notes":["n"]}"#
        );

        let create_error = CreateBackupErrorDto::new(CreateBackupErrorKindDto::TargetMissing, "m");
        assert_eq!(
            serde_json::to_string(&create_error).expect("serialize"),
            r#"{"kind":"target_missing","message":"m"}"#
        );

        let devices = DeviceDiscoveryDto {
            devices: vec![DeviceInfoDto {
                path: "/dev/sda1".to_string(),
                uuid: Some("u".to_string()),
                label: None,
                partuuid: None,
                partlabel: None,
                fs_type: Some("ext4".to_string()),
            }],
            lsblk_available: true,
        };
        assert_eq!(
            serde_json::to_string(&devices).expect("serialize"),
            r#"{"devices":[{"path":"/dev/sda1","uuid":"u","label":null,"partuuid":null,"partlabel":null,"fs_type":"ext4"}],"lsblk_available":true}"#
        );
    }

    /// Every `kind` spelling of the three error enums the frontend matches on
    /// by string, pinned individually.
    ///
    /// `remaining_wire_formats_are_exactly_as_documented` pins one instance of
    /// each shape; this pins every *variant*, because the frontend's `match`
    /// on these strings is exhaustive and a renamed variant would otherwise
    /// only break at runtime, in the error path, where it is least likely to
    /// be noticed.
    #[test]
    fn every_error_kind_spelling_is_exactly_as_documented() {
        let command_kinds = [
            (CommandErrorKindDto::TargetResolution, "target_resolution"),
            (CommandErrorKindDto::ReadTarget, "read_target"),
            (CommandErrorKindDto::NotUtf8, "not_utf8"),
            (CommandErrorKindDto::ListBackups, "list_backups"),
            (CommandErrorKindDto::InvalidBackup, "invalid_backup"),
            (CommandErrorKindDto::ReadBackup, "read_backup"),
            (CommandErrorKindDto::OpenLocation, "open_location"),
        ];
        for (kind, spelling) in command_kinds {
            assert_eq!(
                serde_json::to_string(&kind).expect("serialize"),
                format!("\"{spelling}\"")
            );
        }

        let delete_kinds = [
            (
                DeleteBackupErrorKindDto::TargetResolution,
                "target_resolution",
            ),
            (DeleteBackupErrorKindDto::InvalidBackup, "invalid_backup"),
            (
                DeleteBackupErrorKindDto::PrivilegeEscalation,
                "privilege_escalation",
            ),
            (DeleteBackupErrorKindDto::Delete, "delete"),
        ];
        for (kind, spelling) in delete_kinds {
            assert_eq!(
                serde_json::to_string(&kind).expect("serialize"),
                format!("\"{spelling}\"")
            );
        }

        let create_kinds = [
            (
                CreateBackupErrorKindDto::TargetResolution,
                "target_resolution",
            ),
            (CreateBackupErrorKindDto::TargetMissing, "target_missing"),
            (CreateBackupErrorKindDto::InvalidTarget, "invalid_target"),
            (CreateBackupErrorKindDto::Backup, "backup"),
            (
                CreateBackupErrorKindDto::PrivilegeEscalation,
                "privilege_escalation",
            ),
        ];
        for (kind, spelling) in create_kinds {
            assert_eq!(
                serde_json::to_string(&kind).expect("serialize"),
                format!("\"{spelling}\"")
            );
        }
    }

    /// Every `field` spelling of [`ValidationFieldDto`], pinned individually
    /// for the same reason `every_error_kind_spelling_is_exactly_as_documented`
    /// pins the `kind` enums: the frontend matches on these strings, so a
    /// renamed variant would otherwise only break silently at runtime.
    #[test]
    fn every_validation_field_spelling_is_exactly_as_documented() {
        let field_kinds = [
            (ValidationFieldDto::MountPoint, "mount_point"),
            (ValidationFieldDto::Source, "source"),
            (ValidationFieldDto::FsType, "fs_type"),
        ];
        for (field, spelling) in field_kinds {
            assert_eq!(
                serde_json::to_string(&field).expect("serialize"),
                format!("\"{spelling}\"")
            );
        }
    }

    /// Each `fstab-core` backup-creation failure maps to the kind the
    /// contract promises, and carries the message `Display` already worded.
    ///
    /// `target_missing` is the one that matters most: it is the difference
    /// between the UI saying "there is no file yet — save once first" and the
    /// UI showing a generic I/O failure for a perfectly ordinary state.
    #[test]
    fn create_backup_errors_map_to_their_documented_kinds() {
        let path = PathBuf::from("/etc/fstab");
        let cases: Vec<(CreateBackupError, CreateBackupErrorKindDto)> = vec![
            (
                CreateBackupError::TargetMissing { path: path.clone() },
                CreateBackupErrorKindDto::TargetMissing,
            ),
            (
                CreateBackupError::InvalidTarget {
                    path: path.clone(),
                    reason: "it is a directory".to_string(),
                },
                CreateBackupErrorKindDto::InvalidTarget,
            ),
            (
                CreateBackupError::Backup {
                    path: path.clone(),
                    source: io::Error::from(io::ErrorKind::PermissionDenied),
                },
                CreateBackupErrorKindDto::Backup,
            ),
        ];

        for (error, expected) in &cases {
            let dto = CreateBackupErrorDto::from(error);
            assert_eq!(dto.kind, *expected, "{error:?}");
            assert_eq!(dto.message, error.to_string());
            assert!(!dto.message.is_empty());
        }

        // And the shape survives the trip through the elevated helper's
        // stdout, like the other two error DTOs.
        let dto = CreateBackupErrorDto::from(&cases[0].0);
        let json = serde_json::to_string(&dto).expect("serialize");
        let back: CreateBackupErrorDto = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, dto);
    }

    /// Every source syntax survives the trip, with the `KEY=` prefix living
    /// in the enum rather than in the value.
    #[test]
    fn every_source_kind_round_trips() {
        let sources = [
            SourceId::Path("/dev/sda1".to_string()),
            SourceId::Uuid("1111-2222".to_string()),
            SourceId::Label("home".to_string()),
            SourceId::PartUuid("abcd-ef01".to_string()),
            SourceId::PartLabel("boot".to_string()),
            SourceId::None,
            SourceId::Network("//server/share".to_string()),
            SourceId::Other("weird".to_string()),
        ];
        for source in sources {
            let dto = SourceDto::from(&source);
            assert!(
                !dto.value.contains('='),
                "the prefix must not leak into the value: {dto:?}"
            );
            assert_eq!(SourceId::from(dto), source);
        }
    }

    /// A bare flag and a `key=value` option (including an empty value) are
    /// distinguishable in both directions.
    #[test]
    fn options_round_trip_including_an_empty_value() {
        for option in [
            FstabOption::Flag("noauto".to_string()),
            FstabOption::KeyValue("fmask".to_string(), "0022".to_string()),
            FstabOption::KeyValue("x-systemd.automount".to_string(), String::new()),
        ] {
            let dto = OptionDto::from(&option);
            assert_eq!(FstabOption::from(dto), option);
        }
        assert_eq!(
            OptionDto::from(&FstabOption::Flag("noauto".to_string())).value,
            None,
            "a bare flag must be null, not an empty string"
        );
    }

    /// Escaped whitespace stays escaped: this layer must not decode `\040`,
    /// or a round trip would silently rewrite the user's mount point.
    #[test]
    fn escaped_whitespace_is_not_decoded() {
        let document = parser::parse("/dev/sda2 /mnt/My\\040Backup ext4 noauto 0 2\n");
        let restored = Document::from(DocumentDto::from(&document));
        assert_eq!(restored, document);
        let Line::Entry(entry) = &restored.lines[0] else {
            panic!("expected an entry");
        };
        assert_eq!(entry.mount_point, "/mnt/My\\040Backup");
    }

    #[test]
    fn target_info_reports_the_source_it_was_resolved_from() {
        let sandbox = TargetInfoDto::from(&ActiveTarget {
            path: PathBuf::from("/home/u/.local/share/rustab/fstab.sandbox"),
            is_real: false,
            source: TargetSource::SandboxDefault,
        });
        assert_eq!(sandbox.source, TargetSourceDto::SandboxDefault);
        assert!(!sandbox.is_real);

        let explicit = TargetInfoDto::from(&ActiveTarget {
            path: PathBuf::from("/etc/fstab"),
            is_real: true,
            source: TargetSource::Environment,
        });
        assert_eq!(explicit.source, TargetSourceDto::Environment);
        assert!(explicit.is_real);
    }

    /// The single most consequential field in this module.
    ///
    /// `target_unchanged` is what the UI uses to decide between "nothing
    /// happened, fix it and try again" and "look at the file / restore a
    /// backup". It must be `true` for every failure that provably cannot have
    /// modified the target, and `false` for the three that happen after the
    /// atomic rename has already succeeded.
    #[test]
    fn target_unchanged_is_true_only_where_write_document_guarantees_it() {
        let io_error = || io::Error::from(io::ErrorKind::PermissionDenied);
        let path = PathBuf::from("/etc/fstab");

        let unchanged: Vec<WriteError> = vec![
            WriteError::Validation(ValidationReport {
                findings: vec![Finding {
                    severity: Severity::Error,
                    message: "nope".to_string(),
                    field: None,
                    line_index: None,
                }],
            }),
            WriteError::InvalidTarget {
                path: path.clone(),
                reason: "it is a directory".to_string(),
            },
            WriteError::Backup {
                path: path.clone(),
                source: io_error(),
            },
            WriteError::Write {
                path: path.clone(),
                source: io_error(),
            },
            WriteError::Metadata {
                path: path.clone(),
                source: io_error(),
            },
            WriteError::Fsync {
                stage: FsyncStage::TempFile,
                path: path.clone(),
                source: io_error(),
            },
            WriteError::Fsync {
                stage: FsyncStage::DirectoryBeforeRename,
                path: path.clone(),
                source: io_error(),
            },
            WriteError::Rename {
                from: PathBuf::from("/etc/.fstab.rustab.tmp.1"),
                to: path.clone(),
                source: io_error(),
            },
        ];
        for error in &unchanged {
            let dto = WriteErrorDto::from(error);
            assert!(
                dto.target_unchanged,
                "{error:?} leaves the target untouched but was reported otherwise"
            );
            assert!(!dto.message.is_empty());
        }

        let not_guaranteed: Vec<WriteError> = vec![
            WriteError::Fsync {
                stage: FsyncStage::DirectoryAfterRename,
                path: path.clone(),
                source: io_error(),
            },
            WriteError::ReadBack {
                path: path.clone(),
                source: io_error(),
            },
            WriteError::Verify {
                target: path.clone(),
                backup: Some(PathBuf::from(
                    "/etc/fstab.rustab-backup.0000000001.000000002",
                )),
                expected: "a".to_string(),
                actual: "b".to_string(),
            },
        ];
        for error in &not_guaranteed {
            let dto = WriteErrorDto::from(error);
            assert!(
                !dto.target_unchanged,
                "{error:?} happens after the rename; the target must not be claimed unchanged"
            );
        }
    }

    /// A validation failure carries the whole report, warnings included, so
    /// the UI can show every finding without a second round trip.
    #[test]
    fn a_validation_error_carries_the_full_report() {
        let report = ValidationReport {
            findings: vec![
                Finding {
                    severity: Severity::Error,
                    message: "duplicate mount point".to_string(),
                    field: None,
                    line_index: None,
                },
                Finding {
                    severity: Severity::Warning,
                    message: "mount point does not exist".to_string(),
                    field: Some(FindingField::MountPoint),
                    line_index: Some(1),
                },
            ],
        };

        let dto = WriteErrorDto::from(&WriteError::Validation(report));

        assert_eq!(dto.kind, WriteErrorKindDto::Validation);
        let carried = dto.report.expect("the report must be attached");
        assert!(carried.has_errors);
        assert_eq!(carried.findings.len(), 2);
        assert_eq!(carried.findings[0].severity, SeverityDto::Error);
        assert_eq!(carried.findings[1].severity, SeverityDto::Warning);
        assert_eq!(carried.findings[0].field, None);
        assert_eq!(
            carried.findings[1].field,
            Some(ValidationFieldDto::MountPoint)
        );
        assert_eq!(carried.findings[1].line_index, Some(1));
    }

    /// A `Verify` failure forwards the backup path (the user's way out) but
    /// not the two file bodies.
    #[test]
    fn a_verify_error_forwards_the_backup_path() {
        let dto = WriteErrorDto::from(&WriteError::Verify {
            target: PathBuf::from("/etc/fstab"),
            backup: Some(PathBuf::from(
                "/etc/fstab.rustab-backup.0000000001.000000002",
            )),
            expected: "expected content".to_string(),
            actual: "actual content".to_string(),
        });

        assert_eq!(
            dto.backup.as_deref(),
            Some("/etc/fstab.rustab-backup.0000000001.000000002")
        );
        assert!(!dto.message.contains("expected content"));
        assert!(!dto.message.contains("actual content"));
    }

    /// Error DTOs make the round trip too: the elevated helper serializes one
    /// and the unprivileged side deserializes it (see [`crate::privileged`]).
    #[test]
    fn error_dtos_round_trip_through_json() {
        let dto = WriteErrorDto::from(&WriteError::Backup {
            path: PathBuf::from("/etc/fstab.rustab-backup.0000000001.000000002"),
            source: io::Error::from(io::ErrorKind::PermissionDenied),
        });
        let json = serde_json::to_string(&dto).expect("serialize");
        let back: WriteErrorDto = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, dto);

        // The same applies to a backup deletion: the elevated helper
        // serializes this one and the unprivileged side reads it back.
        let dto = DeleteBackupErrorDto::new(
            DeleteBackupErrorKindDto::Delete,
            "could not delete the backup /etc/fstab.rustab-backup.0000000001.000000002: \
             Permission denied",
        );
        let json = serde_json::to_string(&dto).expect("serialize");
        let back: DeleteBackupErrorDto = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, dto);
    }
}
