//! Thin Tauri IPC bindings for the commands this app uses:
//! `get_target_info`, `list_entries`, `validate_candidate`,
//! `discover_devices` (T11), plus `render_candidate`, `save`,
//! `list_backups` and `restore_backup` (T12), plus `read_backup` and
//! `delete_backup` (T20), plus `create_backup` (T21.1), plus
//! `open_backup_location` (T25), plus `open_external_link` (T26). See
//! `docs/API_CONTRACT.md` for the exact contract.
//!
//! This binds directly to `window.__TAURI_INTERNALS__.invoke` rather than
//! the `@tauri-apps/api` JS package: the frontend has no npm/JS bundling
//! step (see `frontend/Trunk.toml` — Node is only a dev-time Tailwind CSS
//! generator, never bundled). `__TAURI_INTERNALS__.invoke` is the same
//! underlying function the JS package itself calls, and it is always
//! injected by Tauri's core runtime into the webview regardless of the
//! `app.withGlobalTauri` config flag (which only controls whether the
//! friendlier `window.__TAURI__` wrapper is *also* injected).

use serde::{de::DeserializeOwned, Serialize};
use wasm_bindgen::prelude::*;

use crate::dto::DocumentDto;
use crate::dto::{
    BackupInfoDto, CommandErrorDto, CreateBackupErrorDto, DeleteBackupErrorDto, DeviceDiscoveryDto,
    DocumentViewDto, ExternalLink, TargetInfoDto, ValidationReportDto, WriteErrorDto,
    WriteOutcomeDto,
};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI_INTERNALS__"], js_name = invoke)]
    fn invoke_raw(cmd: &str, args: JsValue) -> js_sys::Promise;
}

/// Calls a Tauri command by name, serializing `args` as its argument object
/// and deserializing the resolved value as `R`. On rejection, returns the
/// rejection's `message` field if present, else the rejection rendered as a
/// string (covers the "plain string" shape serde's own arg validation
/// rejects with, per API_CONTRACT.md §1).
async fn invoke<A: Serialize, R: DeserializeOwned>(cmd: &str, args: &A) -> Result<R, String> {
    let args_js = serde_wasm_bindgen::to_value(args)
        .map_err(|err| format!("failed to serialize arguments for {cmd}: {err}"))?;
    let promise = invoke_raw(cmd, args_js);
    let resolved = wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map_err(|rejection| rejection_message(&rejection))?;
    serde_wasm_bindgen::from_value(resolved)
        .map_err(|err| format!("failed to deserialize response from {cmd}: {err}"))
}

fn rejection_message(rejection: &JsValue) -> String {
    if let Some(s) = rejection.as_string() {
        return s;
    }
    if let Ok(err) = serde_wasm_bindgen::from_value::<CommandErrorDto>(rejection.clone()) {
        return err.message;
    }
    js_sys::Reflect::get(rejection, &JsValue::from_str("message"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_else(|| format!("{rejection:?}"))
}

/// Argument object for commands that take none. `invoke`'s JS side expects
/// an object (or nothing), not `undefined`/`null`, so this serializes to
/// `{}` rather than relying on unit's `null` serialization.
#[derive(Serialize)]
struct EmptyArgs {}

/// `get_target_info() -> TargetInfoDto` (API_CONTRACT.md §3.1).
pub async fn get_target_info() -> Result<TargetInfoDto, String> {
    invoke("get_target_info", &EmptyArgs {}).await
}

/// `list_entries() -> DocumentViewDto` (API_CONTRACT.md §3.2).
pub async fn list_entries() -> Result<DocumentViewDto, String> {
    invoke("list_entries", &EmptyArgs {}).await
}

#[derive(Serialize)]
struct ValidateCandidateArgs<'a> {
    candidate: &'a DocumentDto,
}

/// `validate_candidate(candidate: DocumentDto) -> ValidationReportDto`
/// (API_CONTRACT.md §3.3). Contractually never rejects; a `Result::Err`
/// here means an actual frontend bug (bad (de)serialization, missing IPC
/// bridge), not a user-facing validation outcome.
pub async fn validate_candidate(candidate: &DocumentDto) -> Result<ValidationReportDto, String> {
    invoke("validate_candidate", &ValidateCandidateArgs { candidate }).await
}

/// `discover_devices() -> DeviceDiscoveryDto` (API_CONTRACT.md §3.8).
/// Contractually never rejects (a system without `lsblk` resolves to
/// `{devices: [], lsblk_available: false}`); a `Result::Err` here likewise
/// means a frontend bug, not an expected soft failure.
pub async fn discover_devices() -> Result<DeviceDiscoveryDto, String> {
    invoke("discover_devices", &EmptyArgs {}).await
}

#[derive(Serialize)]
struct RenderCandidateArgs<'a> {
    candidate: &'a DocumentDto,
}

/// `render_candidate(candidate: DocumentDto) -> string`
/// (API_CONTRACT.md §3.4). Contractually never rejects (it is pure, no
/// I/O); a `Result::Err` here means a frontend bug.
pub async fn render_candidate(candidate: &DocumentDto) -> Result<String, String> {
    invoke("render_candidate", &RenderCandidateArgs { candidate }).await
}

/// `list_backups() -> BackupInfoDto[]` (API_CONTRACT.md §3.6), newest
/// first, rejecting with a `CommandErrorDto` like the other read-side
/// commands.
pub async fn list_backups() -> Result<Vec<BackupInfoDto>, String> {
    invoke("list_backups", &EmptyArgs {}).await
}

/// The rejection shape of `save`/`restore_backup` (API_CONTRACT.md §8):
/// either the documented `WriteErrorDto`, or — per §1 — an unstructured
/// rejection, which is a frontend bug rather than a user-facing outcome but
/// is still captured here instead of panicking.
#[derive(Clone, Debug, PartialEq)]
pub enum WriteError {
    Write(WriteErrorDto),
    Other(String),
}

impl WriteError {
    /// A message safe to display verbatim (`WriteErrorDto::message` already
    /// is, per the contract; the unstructured fallback is best-effort).
    pub fn message(&self) -> String {
        match self {
            WriteError::Write(err) => err.message.clone(),
            WriteError::Other(message) => message.clone(),
        }
    }

    /// Whether the target is guaranteed to still hold its previous bytes
    /// (API_CONTRACT.md §8: "`target_unchanged` is the field to branch
    /// on"). The unstructured fallback case is not a documented outcome at
    /// all, so it is treated conservatively as `false` — "the guarantee
    /// cannot be made" — rather than assumed safe.
    pub fn target_unchanged(&self) -> bool {
        match self {
            WriteError::Write(err) => err.target_unchanged,
            WriteError::Other(_) => false,
        }
    }
}

/// Like `invoke`, but for the two commands that reject with a structured
/// `WriteErrorDto` (`save`, `restore_backup`) rather than a plain message —
/// callers need `target_unchanged` to decide how to recover
/// (API_CONTRACT.md §8).
async fn invoke_write<A: Serialize>(cmd: &str, args: &A) -> Result<WriteOutcomeDto, WriteError> {
    let args_js = serde_wasm_bindgen::to_value(args).map_err(|err| {
        WriteError::Other(format!("failed to serialize arguments for {cmd}: {err}"))
    })?;
    let promise = invoke_raw(cmd, args_js);
    match wasm_bindgen_futures::JsFuture::from(promise).await {
        Ok(resolved) => serde_wasm_bindgen::from_value(resolved).map_err(|err| {
            WriteError::Other(format!("failed to deserialize response from {cmd}: {err}"))
        }),
        Err(rejection) => {
            match serde_wasm_bindgen::from_value::<WriteErrorDto>(rejection.clone()) {
                Ok(write_error) => Err(WriteError::Write(write_error)),
                Err(_) => Err(WriteError::Other(rejection_message(&rejection))),
            }
        }
    }
}

#[derive(Serialize)]
struct SaveArgs<'a> {
    candidate: &'a DocumentDto,
}

/// `save(candidate: DocumentDto) -> WriteOutcomeDto` (API_CONTRACT.md
/// §3.5). The only write path in the application.
pub async fn save(candidate: &DocumentDto) -> Result<WriteOutcomeDto, WriteError> {
    invoke_write("save", &SaveArgs { candidate }).await
}

#[derive(Serialize)]
struct RestoreBackupArgs<'a> {
    backup_path: &'a str,
}

/// `restore_backup(backup_path: string) -> WriteOutcomeDto`
/// (API_CONTRACT.md §3.7). `backup_path` must be a `path` value from a
/// `list_backups` result for the current target.
pub async fn restore_backup(backup_path: &str) -> Result<WriteOutcomeDto, WriteError> {
    invoke_write("restore_backup", &RestoreBackupArgs { backup_path }).await
}

#[derive(Serialize)]
struct ReadBackupArgs<'a> {
    backup_path: &'a str,
}

/// `read_backup(backup_path: string) -> string` (API_CONTRACT.md §3.9).
/// `backup_path` must be a `path` value from a `list_backups` result for
/// the current target. Never escalates privilege, never prompts.
pub async fn read_backup(backup_path: &str) -> Result<String, String> {
    invoke("read_backup", &ReadBackupArgs { backup_path }).await
}

/// The rejection shape of `delete_backup` (API_CONTRACT.md §8): either the
/// documented `DeleteBackupErrorDto`, or — per §1 — an unstructured
/// rejection, which is a frontend bug rather than a user-facing outcome but
/// is still captured here instead of panicking. Deliberately not a variant
/// of `WriteError`: `DeleteBackupErrorDto` is not `WriteErrorDto`-shaped
/// (no `target_unchanged`, `report`, `path` or `backup`).
#[derive(Clone, Debug, PartialEq)]
pub enum DeleteError {
    Delete(DeleteBackupErrorDto),
    Other(String),
}

impl DeleteError {
    /// A message safe to display verbatim (`DeleteBackupErrorDto::message`
    /// already is, per the contract; the unstructured fallback is
    /// best-effort).
    pub fn message(&self) -> String {
        match self {
            DeleteError::Delete(err) => err.message.clone(),
            DeleteError::Other(message) => message.clone(),
        }
    }
}

/// Like `invoke_write`, but for `delete_backup`, which resolves with `()`
/// (JSON `null`) on success and rejects with a `DeleteBackupErrorDto`
/// rather than a `WriteErrorDto` (API_CONTRACT.md §3.10/§8).
async fn invoke_delete<A: Serialize>(cmd: &str, args: &A) -> Result<(), DeleteError> {
    let args_js = serde_wasm_bindgen::to_value(args).map_err(|err| {
        DeleteError::Other(format!("failed to serialize arguments for {cmd}: {err}"))
    })?;
    let promise = invoke_raw(cmd, args_js);
    match wasm_bindgen_futures::JsFuture::from(promise).await {
        Ok(_resolved) => Ok(()),
        Err(rejection) => {
            match serde_wasm_bindgen::from_value::<DeleteBackupErrorDto>(rejection.clone()) {
                Ok(delete_error) => Err(DeleteError::Delete(delete_error)),
                Err(_) => Err(DeleteError::Other(rejection_message(&rejection))),
            }
        }
    }
}

#[derive(Serialize)]
struct DeleteBackupArgs<'a> {
    backup_path: &'a str,
}

/// `delete_backup(backup_path: string) -> null` (API_CONTRACT.md §3.10).
/// `backup_path` must be a `path` value from a `list_backups` result for
/// the current target. Escalates for the real target exactly like `save`
/// and `restore_backup`; no prompt for any other target.
pub async fn delete_backup(backup_path: &str) -> Result<(), DeleteError> {
    invoke_delete("delete_backup", &DeleteBackupArgs { backup_path }).await
}

/// The rejection shape of `create_backup` (API_CONTRACT.md §8): either the
/// documented `CreateBackupErrorDto`, or — per §1 — an unstructured
/// rejection, which is a frontend bug rather than a user-facing outcome but
/// is still captured here instead of panicking. Deliberately not a variant
/// of `WriteError`/`DeleteError`: `CreateBackupErrorDto` is not
/// `WriteErrorDto`-shaped (no `target_unchanged`, `report`, `path` or
/// `backup`), mirroring `DeleteBackupErrorDto`'s own two-field shape.
#[derive(Clone, Debug, PartialEq)]
pub enum CreateError {
    Create(CreateBackupErrorDto),
    Other(String),
}

impl CreateError {
    /// A message safe to display verbatim (`CreateBackupErrorDto::message`
    /// already is, per the contract; the unstructured fallback is
    /// best-effort).
    pub fn message(&self) -> String {
        match self {
            CreateError::Create(err) => err.message.clone(),
            CreateError::Other(message) => message.clone(),
        }
    }
}

/// Like `invoke_delete`, but for `create_backup`, which resolves with a
/// `BackupInfoDto` (unlike `delete_backup`'s `()`/`null`) and rejects with a
/// `CreateBackupErrorDto` rather than a `DeleteBackupErrorDto`
/// (API_CONTRACT.md §3.11/§8).
async fn invoke_create_backup<A: Serialize>(
    cmd: &str,
    args: &A,
) -> Result<BackupInfoDto, CreateError> {
    let args_js = serde_wasm_bindgen::to_value(args).map_err(|err| {
        CreateError::Other(format!("failed to serialize arguments for {cmd}: {err}"))
    })?;
    let promise = invoke_raw(cmd, args_js);
    match wasm_bindgen_futures::JsFuture::from(promise).await {
        Ok(resolved) => serde_wasm_bindgen::from_value(resolved).map_err(|err| {
            CreateError::Other(format!("failed to deserialize response from {cmd}: {err}"))
        }),
        Err(rejection) => {
            match serde_wasm_bindgen::from_value::<CreateBackupErrorDto>(rejection.clone()) {
                Ok(create_error) => Err(CreateError::Create(create_error)),
                Err(_) => Err(CreateError::Other(rejection_message(&rejection))),
            }
        }
    }
}

/// `create_backup() -> BackupInfoDto` (API_CONTRACT.md §3.11). Takes no
/// arguments: like every other command, which file it acts on is resolved
/// server-side and never supplied by the frontend. Escalates for the real
/// target exactly like `save`/`delete_backup`; no prompt for any other
/// target.
pub async fn create_backup() -> Result<BackupInfoDto, CreateError> {
    invoke_create_backup("create_backup", &EmptyArgs {}).await
}

/// `open_backup_location() -> null` (API_CONTRACT.md §3.12). Opens the
/// host file manager at the directory containing the active target
/// (the same directory its backups live in). Never escalates privilege,
/// never writes.
pub async fn open_backup_location() -> Result<(), String> {
    invoke("open_backup_location", &EmptyArgs {}).await
}

/// `open_external_link(link) -> null` (API_CONTRACT.md §3.13). Opens one
/// of this app's two known external links in the host's default browser.
/// Never escalates privilege, never writes.
pub async fn open_external_link(link: ExternalLink) -> Result<(), String> {
    #[derive(Serialize)]
    struct OpenExternalLinkArgs {
        link: ExternalLink,
    }
    invoke("open_external_link", &OpenExternalLinkArgs { link }).await
}
