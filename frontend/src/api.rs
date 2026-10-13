//! Thin Tauri IPC bindings for the commands this task wires up (T11):
//! `get_target_info`, `list_entries`, `validate_candidate`,
//! `discover_devices`. See `docs/API_CONTRACT.md` for the exact contract.
//!
//! This binds directly to `window.__TAURI_INTERNALS__.invoke` rather than
//! the `@tauri-apps/api` JS package: the frontend has no npm/JS bundling
//! step (see `frontend/Trunk.toml` — Node is only a dev-time Tailwind CSS
//! generator, never bundled). `__TAURI_INTERNALS__.invoke` is the same
//! underlying function the JS package itself calls, and it is always
//! injected by Tauri's core runtime into the webview regardless of the
//! `app.withGlobalTauri` config flag (which only controls whether the
//! friendlier `window.__TAURI__` wrapper is *also* injected).
//!
//! `save`, `list_backups`, `restore_backup` and `render_candidate` are
//! intentionally not bound here — they are out of this task's scope (T12).

use serde::{de::DeserializeOwned, Serialize};
use wasm_bindgen::prelude::*;

use crate::dto::DocumentDto;
use crate::dto::{
    CommandErrorDto, DeviceDiscoveryDto, DocumentViewDto, TargetInfoDto, ValidationReportDto,
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
