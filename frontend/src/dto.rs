//! Hand-written DTOs mirroring `docs/API_CONTRACT.md` §5-§9.
//!
//! The frontend deliberately has no dependency on `fstab-core` or
//! `src-tauri` (see API_CONTRACT.md's opening note), so every shape here is
//! typed out by hand to match the contract's wire format exactly rather than
//! being derived from, or shared with, a backend type. Keep this file and
//! `docs/API_CONTRACT.md` in lockstep: a rename on either side without the
//! other is a bug.

use serde::{Deserialize, Serialize};

/// `DocumentDto` (§5): the full set of lines, in file order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DocumentDto {
    pub lines: Vec<LineDto>,
}

/// `LineDto` (§5): a union tagged by `kind`. `malformed` lines must be kept
/// and displayed verbatim, never dropped (SPEC §5/§9).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LineDto {
    Entry { entry: EntryDto },
    Comment { text: String },
    Blank,
    Malformed { raw: String },
}

/// `EntryDto` (§5).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EntryDto {
    pub source: SourceDto,
    pub mount_point: String,
    pub fs_type: String,
    pub options: Vec<OptionDto>,
    pub dump: u32,
    pub pass: u32,
    pub trailing_comment: Option<String>,
    pub description: Option<String>,
}

/// `source.kind` (§5): never includes the `KEY=` prefix in `value`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Path,
    Uuid,
    Label,
    PartUuid,
    PartLabel,
    None,
    Network,
    Other,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceDto {
    pub kind: SourceKind,
    pub value: String,
}

/// `options[]` entry (§5): `value` is `None` for a bare flag, `Some("")` for
/// a literal `key=` with nothing after the `=`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OptionDto {
    pub key: String,
    pub value: Option<String>,
}

/// `DocumentViewDto` (§5), the return shape of `list_entries`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocumentViewDto {
    pub target: TargetInfoDto,
    pub exists: bool,
    pub text: String,
    pub document: DocumentDto,
    pub has_malformed_lines: bool,
}

/// `TargetInfoDto` (§6).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TargetInfoDto {
    pub path: String,
    pub is_real: bool,
    pub source: TargetSource,
    /// `Some(<directory>)` only when this session's backups are the real
    /// `/etc/fstab`'s *and* this process can't manage files in `/etc/`
    /// (in practice: Flatpak); `None` in every other case. Always present
    /// on the wire (`null`, never omitted) - see `docs/API_CONTRACT.md` §6.
    pub real_target_backup_fallback_dir: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetSource {
    Environment,
    SandboxDefault,
}

/// `ValidationReportDto` (§7), the return shape of `validate_candidate`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ValidationReportDto {
    pub findings: Vec<FindingDto>,
    pub has_errors: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    Error,
    Warning,
}

/// Which of an entry's fields a [`FindingDto`] is about, when it is about
/// exactly one (§7). Mirrors the backend's `ValidationFieldDto`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationField {
    MountPoint,
    Source,
    FsType,
    Description,
    TrailingComment,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FindingDto {
    pub severity: FindingSeverity,
    pub message: String,
    pub field: Option<ValidationField>,
    pub line_index: Option<usize>,
}

/// `CommandErrorDto` (§8), the rejection shape of the read-side commands
/// this task calls (`get_target_info`, `list_entries`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommandErrorDto {
    pub kind: String,
    pub message: String,
}

/// `DeviceDiscoveryDto` / `DeviceInfoDto` (§9), the return shape of
/// `discover_devices`. Never rejects; `lsblk_available: false` or an empty
/// `devices` list are soft-failure states the UI must handle gracefully
/// (`docs/SPEC.md` §8).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceDiscoveryDto {
    pub devices: Vec<DeviceInfoDto>,
    pub lsblk_available: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceInfoDto {
    pub path: String,
    pub uuid: Option<String>,
    pub label: Option<String>,
    pub partuuid: Option<String>,
    pub partlabel: Option<String>,
    pub fs_type: Option<String>,
}

/// `WriteOutcomeDto` (§8), the resolved shape of `save`/`restore_backup`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WriteOutcomeDto {
    pub target: String,
    pub backup: Option<String>,
    pub pruned_backups: Vec<String>,
    pub validation_warnings: Vec<FindingDto>,
    pub notes: Vec<String>,
    pub elevated: bool,
    /// The one **non-fatal warning** a *successful* save/restore can carry
    /// (T34): the real `/etc/fstab` was written, but the local working draft
    /// beside it could not be updated to match. `None` whenever there is
    /// nothing to say - every save that never mirrored into a draft at all
    /// (an explicit `RUSTAB_TARGET_PATH` target) and every one whose mirror
    /// succeeded.
    ///
    /// The write that mattered succeeded, so this must never be rendered as
    /// a failed save; it carries a ready-to-show message (the backend owns
    /// the underlying I/O detail) and `AppShell` shows it as a non-blocking
    /// warning toast in place of the plain success line.
    #[serde(default)]
    pub draft_sync_warning: Option<String>,
}

/// `WriteErrorDto` (§8), the rejection shape of `save`/`restore_backup`.
/// `target_unchanged` is the field the UI must branch on (§8's table): a
/// `true` here is a guarantee the target still holds its previous bytes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WriteErrorDto {
    pub kind: String,
    pub message: String,
    pub target_unchanged: bool,
    pub report: Option<ValidationReportDto>,
    pub path: Option<String>,
    pub backup: Option<String>,
    /// For `kind == "external_change"`: the two file bodies the UI diffs
    /// to show what changed outside Rustab before deciding. `None` for
    /// every other kind - additive on the wire, so an older response
    /// without it still deserializes.
    #[serde(default)]
    pub external_change: Option<ExternalChangeDto>,
}

/// `ExternalChangeDto` (§8): the two file bodies a `kind:
/// "external_change"` `WriteErrorDto` carries, so the UI can show what
/// changed outside Rustab before the user decides whether to write anyway.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExternalChangeDto {
    /// What Rustab last knew the real `/etc/fstab` to hold - the **old**
    /// side of the diff.
    pub last_known_content: String,
    /// What the real `/etc/fstab` holds **right now** - the new side, i.e.
    /// the content the refused write would have replaced.
    pub current_content: String,
}

/// `DeleteBackupErrorDto` (§8), the rejection shape of `delete_backup`.
/// Deliberately NOT the same shape as `WriteErrorDto`: deleting a backup
/// never touches the active target, so there is no `target_unchanged`
/// field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeleteBackupErrorDto {
    pub kind: String,
    pub message: String,
}

/// `BackupInfoDto` (§9), an element of `list_backups`'s return array.
/// `path` is the exact string to hand back to `restore_backup`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BackupInfoDto {
    pub path: String,
    pub file_name: String,
    pub target_path: String,
    pub size_bytes: u64,
    pub created_unix_secs: Option<i64>,
    /// `true` for a backup the user asked for with `create_backup` (§3.11),
    /// `false` for one a `save` took on its own. Never inferred from
    /// `file_name` - the backend owns the naming scheme and reports the
    /// answer here.
    pub is_manual: bool,
}

/// Which of this app's five known external links to open
/// (`open_external_link`, §3.13). Mirrors the backend's `ExternalLinkDto`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalLink {
    GithubRepo,
    GithubSponsors,
    Email,
    Issues,
    License,
}

/// `CreateBackupErrorDto` (§8), the rejection shape of `create_backup`.
/// Same two-field shape as `DeleteBackupErrorDto`, for the same reason:
/// `create_backup` never opens, renames or writes the active target, so
/// there is no `target_unchanged` field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CreateBackupErrorDto {
    pub kind: String,
    pub message: String,
}
