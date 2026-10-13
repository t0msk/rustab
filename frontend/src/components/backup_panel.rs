//! `BackupPanel` — backup list view (§7), rows are `BackupListItem`.
//! Renders from a small hardcoded `Vec<MockBackup>` for this task; T12
//! replaces this with real backup data.

use yew::prelude::*;

use crate::components::empty_state::EmptyState;
use crate::mock::MockBackup;

#[derive(Properties, PartialEq)]
pub struct BackupPanelProps {
    pub backups: Vec<MockBackup>,
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
    pub backup: MockBackup,
    pub on_restore: Callback<MouseEvent>,
}

#[function_component(BackupListItem)]
pub fn backup_list_item(props: &BackupListItemProps) -> Html {
    let backup = &props.backup;
    let size_kb = format!("{:.1} KB", backup.size_bytes as f64 / 1024.0);

    html! {
        <li class="flex items-center justify-between px-4 py-3 hover:bg-sky-50 dark:hover:bg-slate-700/60">
            <div>
                <p class="text-sm text-slate-900 dark:text-slate-100" title={backup.iso_timestamp}>
                    { backup.relative_time }
                </p>
                <p class="text-xs text-slate-500 dark:text-slate-400 font-mono">
                    { backup.target_path }{ " · " }{ size_kb }
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
