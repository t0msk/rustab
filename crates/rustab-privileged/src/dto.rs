//! The serializable types that cross the Tauri command boundary - the
//! frozen contract documented in `docs/API_CONTRACT.md`.
//!
//! ## Why hand-written DTOs instead of `Serialize` on `fstab-core`'s types
//!
//! Three reasons `fstab-core`'s types aren't sent to the frontend directly:
//!
//! 1. **The wire format stops being an accident of the Rust types.** The
//!    frontend is a separate Yew/wasm crate that mirrors these shapes by
//!    hand and deliberately doesn't depend on `fstab-core`; a derived
//!    format would silently change shape on a model rename or reorder.
//! 2. **The frontend wants a flatter shape.**
//!    [`fstab_core::model::SourceId`]'s eight variants become a
//!    `{kind, value}` pair the UI can drive a dropdown and text field
//!    from (`docs/UI_DESIGN.md` §4); same for options as `{key, value}` rows.
//! 3. **No new dependency in the safety-critical crate.** `fstab-core`
//!    only depends on `serde_json`; adding `serde` derive macros there to
//!    serve a GUI concern is the wrong direction.
//!
//! ## Losslessness
//!
//! Every conversion here is lossless and total in both directions:
//! `Document -> DocumentDto -> Document` reproduces the original exactly
//! (asserted by `round_trips_*` tests, including against the fixtures in
//! `tests/fixtures/`) - a save sends a `DocumentDto` back, and a lossy
//! round trip would mean the file written isn't the file shown.
//!
//! `DocumentDto -> Document` is infallible by construction: the only
//! values that could be rejected (unknown `kind`, out-of-range
//! `dump`/`pass`) can't be represented in the DTO at all, so serde
//! rejects them at the boundary before this code runs.

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
/// Paths cross the boundary as plain strings. A non-UTF-8 path is
/// rendered lossily (U+FFFD) rather than failing the command - the
/// frontend's job with these strings is to display them, and the
/// commands that take a path back never trust it directly, matching it
/// against the real backup list instead (see `docs/API_CONTRACT.md`
/// "Non-UTF-8 paths").
fn path_to_wire(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// Target info
// ---------------------------------------------------------------------------

/// How the active target was chosen - mirrors `rustab`'s `target::TargetSource`.
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
    /// `true` only when `path` itself is the real `/etc/fstab` - literally,
    /// or because both paths canonicalize to the same file (a symlink in
    /// either direction still counts). One of two inputs the UI's danger
    /// banner needs, together with `source` (`docs/UI_DESIGN.md` §2); it no
    /// longer decides by itself whether a save escalates - a
    /// `sandbox_default` target has `is_real == false` and still escalates
    /// (see `save_document_with` in `commands.rs`).
    pub is_real: bool,
    /// Where the path came from, so the UI can explain itself.
    pub source: TargetSourceDto,
    /// The directory this session's backups of the **real `/etc/fstab`**
    /// are kept in instead of beside the file itself, or `None` if they're
    /// beside it after all.
    ///
    /// `Some(dir)` only when both hold: this session's backups are the
    /// real `/etc/fstab`'s (`source == "sandbox_default"`, or
    /// `is_real == true` via an explicit `RUSTAB_TARGET_PATH=/etc/fstab`),
    /// **and** this process can't manage files in `/etc/` (typically a
    /// Flatpak sandbox, which sees a different `/etc` than the host's).
    /// `None` in every other case, including any `environment` target
    /// that isn't `/etc/fstab` itself.
    ///
    /// When `Some(dir)`: backups created explicitly land in `dir` with no
    /// password prompt, while each save's automatic backup still lands
    /// beside the real `/etc/fstab` via the elevated writer but is **not
    /// listed**, since this process can't enumerate that directory - not
    /// lost, just not shown or managed in-app (`docs/UI_DESIGN.md` §7).
    ///
    /// A display path, never sent back: backup-path-taking commands
    /// always match against the real backup list regardless.
    pub real_target_backup_fallback_dir: Option<String>,
}

impl TargetInfoDto {
    /// Renders an already-resolved target for the frontend, with the
    /// fallback backup directory this session is using for the real
    /// `/etc/fstab`'s backups (`None` if none - see
    /// [`TargetInfoDto::real_target_backup_fallback_dir`]).
    ///
    /// Takes the three facts rather than the GUI crate's `ActiveTarget`,
    /// which this crate deliberately knows nothing about: resolving the
    /// target is the GUI's job (`rustab`'s `target::describe` is the one
    /// caller), and this layer's job is only to carry the answer across the
    /// boundary unchanged.
    pub fn new(
        path: &Path,
        is_real: bool,
        source: TargetSourceDto,
        backup_fallback_dir: Option<&Path>,
    ) -> Self {
        TargetInfoDto {
            path: path_to_wire(path),
            is_real,
            source,
            real_target_backup_fallback_dir: backup_fallback_dir.map(path_to_wire),
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
    /// escaped-whitespace policy - this layer does not decode or re-encode).
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
    /// *empty* options field, which `findmnt --verify` may reject - the UI
    /// should offer `defaults` rather than nothing.
    pub options: Vec<OptionDto>,
    /// Fifth field: the `dump(8)` flag.
    pub dump: u32,
    /// Sixth field: the `fsck` pass number.
    pub pass: u32,
    /// Text of a trailing `#` comment on the same line, without the `#`, or
    /// `null` if there is none.
    pub trailing_comment: Option<String>,
    /// An optional human-readable note about what this entry is *for*, or
    /// `null` if there is none.
    ///
    /// Distinct from [`Self::trailing_comment`]: a trailing comment is
    /// inline on the entry's own line, while a description round-trips
    /// to/from the ordinary `#` comment line(s) directly above the entry,
    /// with no marker - adjacency alone decides it (see
    /// [`fstab_core::model::FstabEntry::description`]).
    ///
    /// May be multi-line, one `\n` per comment line, no trailing newline
    /// or `\r`. Normalized on the way in by
    /// [`From<EntryDto> for FstabEntry`] below.
    pub description: Option<String>,
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
            description: entry.description.clone(),
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
            // This IPC boundary is where a description arriving from the
            // frontend gets the same "empty-after-trim is not a description"
            // normalization the model documents (and the text parser already
            // enforces) - otherwise a whitespace-only UI value would be
            // written out as a bare `#` line and vanish on the next parse.
            // Line endings fold to `\n` first, the only separator the model
            // understands, since a stray `\r` would otherwise land in a
            // comment line's text.
            description: dto
                .description
                .map(|text| text.replace("\r\n", "\n").replace('\r', "\n"))
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty()),
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
/// and lines the parser could not understand - dropping any of them would
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

/// What `rustab`'s `commands::list_entries` returns: the active target, the
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
    /// The entry's human-readable description ([`EntryDto::description`]) -
    /// not one of the six fstab fields, but an editable per-entry value the
    /// UI can focus just the same.
    ///
    /// No backend check currently produces this value: the only one that did
    /// (a description had to be a single line) was deleted when descriptions
    /// became multi-line. The variant is wire-locked and stays - see
    /// [`FindingField::Description`] for the full reasoning. A UI must still
    /// handle it, since it can reappear without a contract change.
    Description,
    /// The entry's trailing inline comment ([`EntryDto::trailing_comment`]) -
    /// the free-form text appended after the entry's six fstab fields on the
    /// same physical line, following a `#`. Distinct from
    /// [`Self::Description`], which is a separate comment line (or lines)
    /// placed above the entry. Produced by the backend's
    /// `trailing_comment_contains_newline` check (see
    /// [`FindingField::TrailingComment`]).
    TrailingComment,
}

impl From<FindingField> for ValidationFieldDto {
    fn from(field: FindingField) -> Self {
        match field {
            FindingField::MountPoint => ValidationFieldDto::MountPoint,
            FindingField::Source => ValidationFieldDto::Source,
            FindingField::FsType => ValidationFieldDto::FsType,
            FindingField::Description => ValidationFieldDto::Description,
            FindingField::TrailingComment => ValidationFieldDto::TrailingComment,
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
    /// means this finding is not about exactly one field - see
    /// [`FindingField`]'s doc comment for the full explanation.
    pub field: Option<ValidationFieldDto>,
    /// The 0-based index into the validated `DocumentDto.lines` array of the
    /// single line this finding is about, if any - the same identity scheme
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
    /// The file actually written - differs from the active target only if the
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
    /// Set by the unprivileged side after the helper reports back - the
    /// helper itself cannot know how it was launched, so it always emits
    /// `false` (see [`crate::protocol`]).
    pub elevated: bool,
    /// The one non-fatal warning a successful save can carry: the real
    /// `/etc/fstab` was written, but the local working draft beside it
    /// couldn't be updated to match. `null` whenever there's nothing to
    /// say - a save with no draft to mirror into, or one whose mirror succeeded.
    ///
    /// Not folded into `notes`, since `notes` describe deviations in how
    /// the file just written was written ("the file is fine, here's a
    /// detail"), while this is about a different file and should be
    /// raised as a warning. Not an error either - the write that mattered
    /// succeeded, and the next launch repairs the draft on its own.
    #[serde(default)]
    pub draft_sync_warning: Option<String>,
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
            // Set, if at all, by the save path after the draft mirror has
            // been attempted - `fstab_core`'s outcome knows nothing about a
            // draft.
            draft_sync_warning: None,
        }
    }
}

/// Which stage of a save failed.
///
/// The ten variants from [`Self::Validation`] to [`Self::Verify`] map
/// one-to-one onto [`WriteError`]'s variants; the rest describe failures
/// *around* the write - resolving the target, escalating privileges, choosing
/// or reading a backup to restore, an external change to the real file that
/// stopped the write before it began, and an internal protocol failure -
/// which `fstab-core` knows nothing about.
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
    /// The real `/etc/fstab` changed outside Rustab since this session
    /// last synced with it, so the write was refused before it started.
    ///
    /// Not a failure and not an I/O problem: nothing was attempted,
    /// `target_unchanged` is always `true`, and the diff is attached as
    /// an `external_change` object. The user cancels, or repeats the call
    /// with `allow_external_change: true` after seeing it.
    ExternalChange,
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
    /// content, byte for byte. `false` means that guarantee cannot be made -
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
    /// For [`WriteErrorKindDto::ExternalChange`], the two file bodies the
    /// UI diffs. `null` for every other kind, same shape as `report`.
    ///
    /// Boxed (invisible on the wire) since [`WriteErrorDto`] is the error
    /// half of every write-side `Result`, and two inline `String`s would
    /// push it past `clippy::result_large_err`'s threshold - one pointer
    /// keeps the common path cheap while the rare one pays for its own content.
    #[serde(default)]
    pub external_change: Option<Box<ExternalChangeDto>>,
}

/// The two file bodies a [`WriteErrorKindDto::ExternalChange`] carries, so
/// the frontend can show what changed outside Rustab before the user decides.
///
/// One nested object rather than two sibling fields, so the pair can't be
/// half-present - there's no meaningful state with one side of a diff and not
/// the other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalChangeDto {
    /// What Rustab last knew the real `/etc/fstab` to hold - the **old** side
    /// of the diff (the content of the last-known-real marker, kept by
    /// `rustab`'s `target` module).
    pub last_known_content: String,
    /// What the real `/etc/fstab` holds **right now** - the new side, i.e. the
    /// content the refused write would have replaced.
    pub current_content: String,
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
            external_change: None,
        }
    }

    /// Attaches the path the failure concerned.
    #[must_use]
    pub fn with_path(mut self, path: &Path) -> Self {
        self.path = Some(path_to_wire(path));
        self
    }

    /// `target` changed outside Rustab, so the write was refused before
    /// it began.
    ///
    /// Built here, in one place, since the check happens twice - a
    /// courtesy before the `pkexec` prompt, and authoritatively inside
    /// the elevated process - and the frontend must not be able to tell
    /// which of the two refused the write.
    ///
    /// `target_unchanged` is unconditionally `true`: nothing was opened
    /// for writing, no backup taken, no temp file created.
    pub fn external_change(target: &Path, last_known: &str, current: &str) -> Self {
        WriteErrorDto {
            external_change: Some(Box::new(ExternalChangeDto {
                last_known_content: last_known.to_string(),
                current_content: current.to_string(),
            })),
            ..WriteErrorDto::new(
                WriteErrorKindDto::ExternalChange,
                true,
                format!(
                    "{} has changed outside Rustab since this session last synced with it, so \
                     nothing was written and the file still holds that newer content. Review what \
                     changed before deciding: cancel and keep it, or write your version over it \
                     anyway.",
                    target.display()
                ),
            )
            .with_path(target)
        }
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
    /// valid UTF-8 (see `commands::read_backup`), rather than adding
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
    /// (`commands::open_backup_location`) - the `xdg-open` binary is
    /// missing, or starting it failed for some other reason.
    OpenLocation,
    /// The host browser could not be launched
    /// (`commands::open_external_link`) - the `xdg-open` binary is
    /// missing, or starting it failed for some other reason.
    OpenExternalLink,
    /// `commands::open_path_in_file_manager`'s own kind - the host
    /// file manager could not be launched for the frontend-supplied path
    /// (the `xdg-open` binary is missing, or starting it failed for some
    /// other reason).
    OpenPath,
    /// `commands::resync_external_change` could not re-sync the
    /// working draft from the real `/etc/fstab` (T37). Covers two distinct
    /// situations, deliberately under one kind because the frontend treats
    /// them identically: the active target is not the default working draft
    /// at all (fail-closed guard, nothing touched), or the real file was read
    /// successfully but publishing it onto the draft and its last-known-real
    /// marker failed.
    Resync,
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
// External links
// ---------------------------------------------------------------------------

/// Which of this app's five known external links to open
/// (`commands::open_external_link`) - a closed set, not a raw
/// URL, by design: see that command's own doc comment for why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalLinkDto {
    /// `https://github.com/t0msk/rustab`.
    GithubRepo,
    /// `https://github.com/sponsors/t0msk`.
    GithubSponsors,
    /// `mailto:opensource@tomsk.dev`. `xdg-open` dispatches a `mailto:` URI to
    /// the host's default mail client, the same way it dispatches an
    /// `https:` URL to the default browser - no separate code path needed.
    Email,
    /// `https://github.com/t0msk/rustab/issues`.
    Issues,
    /// `https://github.com/t0msk/rustab` - the repo root, not a path to one
    /// specific license file. Rustab is dual-licensed (`LICENSE-MIT` and
    /// `LICENSE-APACHE`, the standard Rust ecosystem choice, `docs/SPEC.md`
    /// §10); the repo root is where GitHub itself surfaces both, rather
    /// than this link arbitrarily picking one of the two to point at.
    /// Deliberately a distinct variant from `GithubRepo`, even though the
    /// URL is identical today, because the two buttons represent different
    /// user intent (browse the project vs. check the license) and may
    /// reasonably point elsewhere independently in the future.
    License,
}

impl ExternalLinkDto {
    /// The literal URL (or URI, for `Email`) for this link. Hardcoded here,
    /// not supplied by the frontend - see the command's doc comment.
    pub fn url(self) -> &'static str {
        match self {
            ExternalLinkDto::GithubRepo => "https://github.com/t0msk/rustab",
            ExternalLinkDto::GithubSponsors => "https://github.com/sponsors/t0msk",
            ExternalLinkDto::Email => "mailto:opensource@tomsk.dev",
            ExternalLinkDto::Issues => "https://github.com/t0msk/rustab/issues",
            ExternalLinkDto::License => "https://github.com/t0msk/rustab",
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
/// here - it would be trivially `true` for every variant and would invite the
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

/// A failed `commands::delete_backup`.
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
    /// Full path, and the exact string `commands::restore_backup`,
    /// `commands::read_backup` and
    /// `commands::delete_backup` expect back.
    pub path: String,
    /// Just the file name, for a compact list display.
    pub file_name: String,
    /// The target this is a backup of.
    pub target_path: String,
    /// Size in bytes.
    pub size_bytes: u64,
    /// The backup file's modification time as whole seconds since the Unix
    /// epoch - i.e. when the backup was taken. `null` if the platform could
    /// not report it.
    pub created_unix_secs: Option<u64>,
    /// `true` if a user asked for this backup explicitly
    /// (`commands::create_backup`), `false` if a save made it on its
    /// own. Reported by `fstab-core`, which owns the naming scheme the
    /// distinction is encoded in - the frontend must not try to infer it from
    /// `file_name`.
    ///
    /// Worth surfacing because the two kinds behave differently: automatic
    /// backups are pruned once there are more than ten of them, manual ones
    /// are kept until the user deletes them.
    pub is_manual: bool,
    /// `true` for every backup `commands::delete_backup` accepts in this
    /// session - i.e. every row [`crate::backups::list`] and
    /// [`crate::backups::describe_one`] produce.
    ///
    /// `false` only for a real `/etc/fstab` backup listed from the host's
    /// `/etc/` by a sandboxed (fallback-mode) session: such a backup can be
    /// restored and its content shown, but not deleted from there (see
    /// `docs/API_CONTRACT.md` §3.6).
    pub deletable: bool,
}

// ---------------------------------------------------------------------------
// Manual backup creation
// ---------------------------------------------------------------------------

/// What a successful `commands::create_backup` did, as the elevated
/// helper reports it.
///
/// Internal to the escalation protocol rather than part of the command's own
/// return shape: the command resolves this into a [`BackupInfoDto`] so the
/// frontend gets a row in exactly the shape
/// `commands::list_backups` already hands it, with no second backup
/// shape to mirror.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateBackupOutcomeDto {
    /// The file that was backed up - the active target, or its destination if
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
    /// no file yet - save once first", not a silent success.
    TargetMissing,
    /// The target path cannot be backed up at all (it is a directory, a
    /// dangling symlink, or has no file-name component).
    InvalidTarget,
    /// Copying the target to its new backup, or flushing that copy to disk,
    /// failed. The target itself is untouched - it was only ever read.
    Backup,
    /// Real-target only: `pkexec` is missing, the prompt was dismissed or
    /// refused, or the elevated helper produced no usable response.
    PrivilegeEscalation,
}

/// A failed `commands::create_backup`.
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
        // verbatim rather than re-worded here - the same policy
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
    /// crate's types - so it has to break this test first, loudly, next to
    /// the documentation that has to be updated with it.
    #[test]
    fn document_wire_format_is_exactly_as_documented() {
        // `# hi` is this entry's description (absorbed, so it is not also a
        // `comment` line); `# freestanding` is above a malformed line, which
        // is one of the positions where a comment stays a comment - so all
        // four `kind`s and a non-null `description` appear in one document.
        let document = parser::parse(
            "# hi\n\
             UUID=1111 /mnt/d ext4 noauto,fmask=0022 0 2 # note\n\
             \n\
             # freestanding\n\
             /dev/sdb1\n",
        );

        let json = serde_json::to_string(&DocumentDto::from(&document)).expect("serialize");

        assert_eq!(
            json,
            concat!(
                r#"{"lines":["#,
                r#"{"kind":"entry","entry":{"#,
                r#""source":{"kind":"uuid","value":"1111"},"#,
                r#""mount_point":"/mnt/d","fs_type":"ext4","#,
                r#""options":[{"key":"noauto","value":null},{"key":"fmask","value":"0022"}],"#,
                r#""dump":0,"pass":2,"trailing_comment":" note","description":"hi"}},"#,
                r#"{"kind":"blank"},"#,
                r#"{"kind":"comment","text":" freestanding"},"#,
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
                real_target_backup_fallback_dir: None,
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
            r##"{"target":{"path":"/p","is_real":false,"source":"sandbox_default","real_target_backup_fallback_dir":null},"exists":true,"text":"# hi\n","document":{"lines":[{"kind":"blank"}]},"has_malformed_lines":false}"##
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

        // A field/line-tagged finding - the shape `mount_point_missing`,
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
            draft_sync_warning: None,
        };
        assert_eq!(
            serde_json::to_string(&outcome).expect("serialize"),
            r#"{"target":"/p","backup":"/p.bak","pruned_backups":[],"validation_warnings":[],"notes":["n"],"elevated":true,"draft_sync_warning":null}"#
        );

        // The one T34 addition, on the wire: a successful save that could not
        // update the working draft (see `rustab`'s
        // `commands::save_document_with`).
        let warned = WriteOutcomeDto {
            draft_sync_warning: Some("w".to_string()),
            ..outcome
        };
        assert_eq!(
            serde_json::to_string(&warned).expect("serialize"),
            r#"{"target":"/p","backup":"/p.bak","pruned_backups":[],"validation_warnings":[],"notes":["n"],"elevated":true,"draft_sync_warning":"w"}"#
        );
        // And it is optional on the way in, so a response produced by a build
        // that predates the field still deserializes.
        let without: WriteOutcomeDto = serde_json::from_str(
            r#"{"target":"/p","backup":null,"pruned_backups":[],"validation_warnings":[],"notes":[],"elevated":false}"#,
        )
        .expect("deserialize");
        assert_eq!(without.draft_sync_warning, None);

        let error = WriteErrorDto::new(WriteErrorKindDto::PrivilegeEscalation, true, "m");
        assert_eq!(
            serde_json::to_string(&error).expect("serialize"),
            r#"{"kind":"privilege_escalation","message":"m","target_unchanged":true,"report":null,"path":null,"backup":null,"external_change":null}"#
        );

        // The T36 addition, on the wire: the one error kind that carries two
        // file bodies, because the frontend builds a diff out of them
        // ([`crate::target_identity::ExternalChangeGuard`]).
        let external = WriteErrorDto {
            message: "m".to_string(),
            path: None,
            ..WriteErrorDto::external_change(Path::new("/etc/fstab"), "# old\n", "# new\n")
        };
        assert_eq!(
            serde_json::to_string(&external).expect("serialize"),
            r##"{"kind":"external_change","message":"m","target_unchanged":true,"report":null,"path":null,"backup":null,"external_change":{"last_known_content":"# old\n","current_content":"# new\n"}}"##
        );
        // It is additive and optional on the way in, so a response produced
        // by a build that predates it still deserializes.
        let without: WriteErrorDto = serde_json::from_str(
            r#"{"kind":"write","message":"m","target_unchanged":true,"report":null,"path":null,"backup":null}"#,
        )
        .expect("deserialize");
        assert_eq!(without.external_change, None);

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
            deletable: true,
        };
        assert_eq!(
            serde_json::to_string(&backup).expect("serialize"),
            r#"{"path":"/p.b","file_name":"p.b","target_path":"/p","size_bytes":12,"created_unix_secs":1760000000,"is_manual":true,"deletable":true}"#
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

    /// Every `kind` spelling of the four error enums the frontend matches on
    /// by string, pinned individually.
    ///
    /// `remaining_wire_formats_are_exactly_as_documented` pins one instance of
    /// each shape; this pins every *variant*, because the frontend's `match`
    /// on these strings is exhaustive and a renamed variant would otherwise
    /// only break at runtime, in the error path, where it is least likely to
    /// be noticed.
    #[test]
    fn every_error_kind_spelling_is_exactly_as_documented() {
        let write_kinds = [
            (WriteErrorKindDto::TargetResolution, "target_resolution"),
            (WriteErrorKindDto::Validation, "validation"),
            (WriteErrorKindDto::InvalidTarget, "invalid_target"),
            (WriteErrorKindDto::Backup, "backup"),
            (WriteErrorKindDto::Write, "write"),
            (WriteErrorKindDto::Metadata, "metadata"),
            (WriteErrorKindDto::Fsync, "fsync"),
            (WriteErrorKindDto::Rename, "rename"),
            (WriteErrorKindDto::ReadBack, "read_back"),
            (WriteErrorKindDto::Verify, "verify"),
            (
                WriteErrorKindDto::PrivilegeEscalation,
                "privilege_escalation",
            ),
            (WriteErrorKindDto::InvalidBackup, "invalid_backup"),
            (WriteErrorKindDto::ReadBackup, "read_backup"),
            (WriteErrorKindDto::Internal, "internal"),
            (WriteErrorKindDto::ExternalChange, "external_change"),
        ];
        for (kind, spelling) in write_kinds {
            assert_eq!(
                serde_json::to_string(&kind).expect("serialize"),
                format!("\"{spelling}\"")
            );
        }

        let command_kinds = [
            (CommandErrorKindDto::TargetResolution, "target_resolution"),
            (CommandErrorKindDto::ReadTarget, "read_target"),
            (CommandErrorKindDto::NotUtf8, "not_utf8"),
            (CommandErrorKindDto::ListBackups, "list_backups"),
            (CommandErrorKindDto::InvalidBackup, "invalid_backup"),
            (CommandErrorKindDto::ReadBackup, "read_backup"),
            (CommandErrorKindDto::OpenLocation, "open_location"),
            (CommandErrorKindDto::OpenExternalLink, "open_external_link"),
            (CommandErrorKindDto::OpenPath, "open_path"),
            (CommandErrorKindDto::Resync, "resync"),
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
            (ValidationFieldDto::Description, "description"),
            (ValidationFieldDto::TrailingComment, "trailing_comment"),
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
    /// between the UI saying "there is no file yet - save once first" and the
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

    /// A description is carried across the boundary in both directions, and
    /// serializes under its own key - not merged into `trailing_comment`.
    ///
    /// Which comment lines become a description is `fstab-core`'s concern
    /// (and is pinned by its own tests plus
    /// `fixtures_round_trip_through_the_dto_layer_byte_for_byte` in
    /// `commands.rs`, which runs the `description_blocks.fstab` fixture
    /// through this very layer); what is pinned here is only the wire shape.
    #[test]
    fn a_description_round_trips_separately_from_a_trailing_comment() {
        let document = parser::parse(
            "# photo archive, only mounted for backups\n\
             /dev/sdc1 /mnt/photos ext4 noauto 0 2 # inline\n",
        );
        let dto = DocumentDto::from(&document);

        let LineDto::Entry { entry } = &dto.lines[0] else {
            panic!("expected the comment line to have been absorbed into the entry");
        };
        assert_eq!(
            entry.description.as_deref(),
            Some("photo archive, only mounted for backups")
        );
        assert_eq!(
            entry.trailing_comment.as_deref(),
            Some(" inline"),
            "the two mechanisms must stay independent"
        );
        assert_eq!(
            serde_json::to_string(&entry.description).expect("serialize"),
            r#""photo archive, only mounted for backups""#
        );

        assert_eq!(Document::from(dto), document);
    }

    /// The IPC boundary normalizes a description the same way the text parser
    /// does: surrounding whitespace is dropped, and a value that is empty
    /// after trimming is not a description at all.
    ///
    /// This is the only normalization this module performs, and it exists
    /// because `fstab_core::model` deliberately performs none itself. Without
    /// it, a whitespace-only value typed into the UI would be stored, written
    /// out as a bare `#` line, and then silently vanish on the next parse -
    /// a comment block that trims away to nothing is not absorbed.
    #[test]
    fn a_description_arriving_over_ipc_is_trimmed_and_emptied_to_none() {
        let entry_with = |description: Option<&str>| EntryDto {
            source: SourceDto {
                kind: SourceKindDto::Path,
                value: "/dev/sdc1".to_string(),
            },
            mount_point: "/mnt/photos".to_string(),
            fs_type: "ext4".to_string(),
            options: Vec::new(),
            dump: 0,
            pass: 2,
            trailing_comment: None,
            description: description.map(str::to_string),
        };

        assert_eq!(
            FstabEntry::from(entry_with(Some("  padded note  "))).description,
            Some("padded note".to_string()),
            "surrounding whitespace must be trimmed"
        );
        for empty in ["", "   ", "\t", "\n"] {
            assert_eq!(
                FstabEntry::from(entry_with(Some(empty))).description,
                None,
                "{empty:?} is empty after trimming and must not be stored"
            );
        }
        assert_eq!(FstabEntry::from(entry_with(None)).description, None);

        // Internal whitespace is content, not padding, and stays verbatim -
        // the same rule `fstab-core`'s parser documents for a comment line's
        // own text.
        assert_eq!(
            FstabEntry::from(entry_with(Some("two  inner   spaces"))).description,
            Some("two  inner   spaces".to_string())
        );

        // An interior newline is content too: a description is multi-line
        // now, so trimming the ends must not touch it.
        assert_eq!(
            FstabEntry::from(entry_with(Some("  first line\nsecond line  "))).description,
            Some("first line\nsecond line".to_string()),
            "trimming must not strip or collapse an interior newline"
        );
    }

    /// Line endings arriving over IPC are folded to `\n` before anything else.
    ///
    /// `\n` is the only line separator the model and the serializer know: the
    /// writer emits one comment line per `\n`-separated piece, so a stray
    /// `\r` left in the value would be written into the user's fstab inside a
    /// comment line. A `<textarea>` cannot produce one (the HTML spec says
    /// its `value` is LF-only), but this is an IPC boundary and the frontend
    /// is not the only possible caller.
    #[test]
    fn crlf_and_lone_cr_line_endings_are_folded_before_trimming() {
        let entry_with = |description: &str| EntryDto {
            source: SourceDto {
                kind: SourceKindDto::Path,
                value: "/dev/sdc1".to_string(),
            },
            mount_point: "/mnt/photos".to_string(),
            fs_type: "ext4".to_string(),
            options: Vec::new(),
            dump: 0,
            pass: 2,
            trailing_comment: None,
            description: Some(description.to_string()),
        };

        for sent in ["first\r\nsecond", "first\rsecond", "first\nsecond"] {
            let folded = FstabEntry::from(entry_with(sent)).description;
            assert_eq!(
                folded,
                Some("first\nsecond".to_string()),
                "{sent:?} must normalize to the same LF-separated value"
            );
        }

        // Folding happens *before* the trim/empty check, so a value that is
        // nothing but line endings is still `None` rather than a stored
        // "\n\n".
        assert_eq!(FstabEntry::from(entry_with("\r\n\r")).description, None);
    }

    /// A genuinely multi-line description makes the whole trip through this
    /// layer - DTO out, JSON out, JSON in, DTO in - and comes back as the
    /// same multi-line value on the same entry, with the file text it
    /// serializes to being one comment line per piece.
    #[test]
    fn a_multi_line_description_round_trips_through_the_dto_layer() {
        let original = "# first line of the note\n\
                        # second line of the note\n\
                        /dev/sdc1 /mnt/photos ext4 noauto 0 2\n";
        let document = parser::parse(original);

        let json = serde_json::to_string(&DocumentDto::from(&document)).expect("serialize");
        let back: DocumentDto = serde_json::from_str(&json).expect("deserialize");

        let LineDto::Entry { entry } = &back.lines[0] else {
            panic!("expected the comment block to have been absorbed into the entry");
        };
        assert_eq!(
            entry.description.as_deref(),
            Some("first line of the note\nsecond line of the note"),
            "the embedded newline must survive JSON in both directions"
        );

        let restored = Document::from(back);
        assert_eq!(restored, document);
        assert_eq!(restored.to_string(), original);
    }

    /// The `"description"` field tag still crosses the boundary unchanged,
    /// even though no backend check currently produces it: the enum variant
    /// is wire-locked (`docs/API_CONTRACT.md` §7) and the UI must keep
    /// handling it.
    #[test]
    fn the_description_validation_field_tag_still_maps_across_the_boundary() {
        let finding = Finding {
            severity: Severity::Error,
            message: "something about a description".to_string(),
            field: Some(FindingField::Description),
            line_index: Some(0),
        };
        assert_eq!(
            FindingDto::from(&finding).field,
            Some(ValidationFieldDto::Description)
        );
    }

    /// The `"trailing_comment"` field tag crosses the boundary unchanged -
    /// unlike `Description`, this one is actively produced by the backend's
    /// `trailing_comment_contains_newline` check.
    #[test]
    fn the_trailing_comment_validation_field_tag_maps_across_the_boundary() {
        let finding = Finding {
            severity: Severity::Error,
            message: "something about a trailing comment".to_string(),
            field: Some(FindingField::TrailingComment),
            line_index: Some(0),
        };
        assert_eq!(
            FindingDto::from(&finding).field,
            Some(ValidationFieldDto::TrailingComment)
        );
    }

    #[test]
    fn target_info_reports_the_source_it_was_resolved_from() {
        let sandbox = TargetInfoDto::new(
            Path::new("/home/u/.local/share/rustab/fstab.sandbox"),
            false,
            TargetSourceDto::SandboxDefault,
            None,
        );
        assert_eq!(sandbox.source, TargetSourceDto::SandboxDefault);
        assert!(!sandbox.is_real);

        let explicit = TargetInfoDto::new(
            Path::new("/etc/fstab"),
            true,
            TargetSourceDto::Environment,
            None,
        );
        assert_eq!(explicit.source, TargetSourceDto::Environment);
        assert!(explicit.is_real);
    }

    /// The T39 field, both ways round, including its exact wire spelling: the
    /// frontend builds a visible notice out of it, so the name and the `null`
    /// case are as load-bearing as the path itself.
    ///
    /// *Which* target gets a fallback directory is the GUI crate's
    /// `commands::backup_fallback_dir` decision (asserted there); this
    /// layer's job is only to carry the answer across the boundary
    /// unchanged.
    #[test]
    fn target_info_carries_the_real_target_backup_fallback_dir_verbatim() {
        let draft = Path::new("/home/u/.local/share/rustab/fstab.draft");

        let colocated = TargetInfoDto::new(draft, false, TargetSourceDto::SandboxDefault, None);
        assert_eq!(colocated.real_target_backup_fallback_dir, None);
        assert_eq!(
            serde_json::to_string(&colocated).expect("serialize"),
            r##"{"path":"/home/u/.local/share/rustab/fstab.draft","is_real":false,"source":"sandbox_default","real_target_backup_fallback_dir":null}"##
        );

        let fallback = TargetInfoDto::new(
            draft,
            false,
            TargetSourceDto::SandboxDefault,
            Some(Path::new("/home/u/.local/share/rustab/etc-fstab-backups")),
        );
        assert_eq!(
            fallback.real_target_backup_fallback_dir.as_deref(),
            Some("/home/u/.local/share/rustab/etc-fstab-backups")
        );
        assert_eq!(
            serde_json::to_string(&fallback).expect("serialize"),
            r##"{"path":"/home/u/.local/share/rustab/fstab.draft","is_real":false,"source":"sandbox_default","real_target_backup_fallback_dir":"/home/u/.local/share/rustab/etc-fstab-backups"}"##
        );
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
    /// and the unprivileged side deserializes it (see [`crate::protocol`]).
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
