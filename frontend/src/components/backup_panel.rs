//! `BackupPanel` — backup list view (§7), rows are `BackupListItem`.
//! Renders from the real `list_backups` result (T12); `AppShell` owns the
//! fetch, this component is purely presentational.

use wasm_bindgen::JsValue;
use yew::prelude::*;

use crate::components::empty_state::EmptyState;
use crate::dto::BackupInfoDto;

/// Formats a backup's `created_unix_secs` into `(relative, iso)` strings
/// for `BackupListItem`'s display text + hover tooltip
/// (`docs/API_CONTRACT.md` §3.6: "format the relative and ISO timestamps
/// from it in the frontend"). Uses `js_sys::Date` rather than pulling in a
/// date/time crate, since the wasm target already has it for free.
pub fn format_timestamps(created_unix_secs: Option<i64>) -> (String, String) {
    let Some(secs) = created_unix_secs else {
        return ("unknown time".to_string(), "unknown".to_string());
    };
    let date = js_sys::Date::new(&JsValue::from_f64(secs as f64 * 1000.0));
    let iso = date
        .to_iso_string()
        .as_string()
        .unwrap_or_else(|| "unknown".to_string());
    let diff_secs = ((js_sys::Date::now() / 1000.0) - secs as f64).max(0.0) as i64;
    (relative_time(diff_secs), iso)
}

fn relative_time(diff_secs: i64) -> String {
    if diff_secs < 60 {
        "just now".to_string()
    } else if diff_secs < 3600 {
        let minutes = diff_secs / 60;
        format!(
            "{minutes} minute{} ago",
            if minutes == 1 { "" } else { "s" }
        )
    } else if diff_secs < 86_400 {
        let hours = diff_secs / 3600;
        format!("{hours} hour{} ago", if hours == 1 { "" } else { "s" })
    } else {
        let days = diff_secs / 86_400;
        format!("{days} day{} ago", if days == 1 { "" } else { "s" })
    }
}

#[derive(Properties, PartialEq)]
pub struct BackupPanelProps {
    pub backups: Vec<BackupInfoDto>,
    pub on_restore: Callback<usize>,
}

#[function_component(BackupPanel)]
pub fn backup_panel(props: &BackupPanelProps) -> Html {
    html! {
        <div class="bg-white dark:bg-slate-800 rounded-lg shadow-sm border border-slate-200 dark:border-slate-700 overflow-hidden">
            if props.backups.is_empty() {
                <EmptyState
                    icon="🗄"
                    title="No backups yet"
                    description={Some(AttrValue::from(
                        "No backups yet — one will be created automatically the first time you save."
                    ))}
                />
            } else {
                <ul class="divide-y divide-slate-200 dark:divide-slate-700">
                    { for props.backups.iter().enumerate().map(|(idx, backup)| {
                        let on_restore = {
                            let cb = props.on_restore.clone();
                            Callback::from(move |_: MouseEvent| cb.emit(idx))
                        };
                        html! { <BackupListItem key={idx} backup={backup.clone()} on_restore={on_restore} /> }
                    }) }
                </ul>
            }
        </div>
    }
}

#[derive(Properties, PartialEq)]
pub struct BackupListItemProps {
    pub backup: BackupInfoDto,
    pub on_restore: Callback<MouseEvent>,
}

#[function_component(BackupListItem)]
pub fn backup_list_item(props: &BackupListItemProps) -> Html {
    let backup = &props.backup;
    let (relative, iso) = format_timestamps(backup.created_unix_secs);
    let size_kb = format!("{:.1} KB", backup.size_bytes as f64 / 1024.0);

    html! {
        <li class="flex items-center justify-between px-4 py-3 hover:bg-sky-50 dark:hover:bg-slate-700/60">
            <div>
                <p class="text-sm text-slate-900 dark:text-slate-100" title={iso}>
                    { relative }
                </p>
                <p class="text-xs text-slate-500 dark:text-slate-400 font-mono">
                    { backup.target_path.clone() }{ " · " }{ size_kb }
                </p>
            </div>
            <button
                type="button"
                class="px-3 py-1.5 rounded-md bg-slate-100 hover:bg-slate-200 dark:bg-slate-700 dark:hover:bg-slate-600 text-slate-700 dark:text-slate-200 text-xs font-medium focus-visible:ring-2 focus-visible:ring-sky-500"
                onclick={props.on_restore.clone()}
            >
                { "Restore" }
            </button>
        </li>
    }
}
