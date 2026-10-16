//! `BackupDiffView` — read-only backup-vs-current-target diff overlay (§7).
//!
//! Unlike `BackupRestoreConfirmDialog`/`BackupDeleteConfirmDialog`, this
//! performs no write and no deletion — it is purely informational — so,
//! judgment call mirroring `DeleteConfirmDialog`'s reasoning, it *is*
//! dismissible by clicking the backdrop. Reuses `DiffRawText` directly:
//! raw tab only, no Summary tab, since there is no `render_candidate`
//! equivalent for arbitrary backup text (API_CONTRACT.md §3.9).

use yew::prelude::*;

use crate::components::diff_view::DiffRawText;
use crate::diff::RawDiffLine;

#[derive(Properties, PartialEq)]
pub struct BackupDiffViewProps {
    pub backup_timestamp: AttrValue,
    pub target_path: AttrValue,
    /// `None` while the `read_backup` call hasn't resolved yet (or failed);
    /// forwarded to `DiffRawText`, which already renders a "Rendering…"
    /// placeholder for `None`.
    pub lines: Option<Vec<RawDiffLine>>,
    /// Set if the `read_backup` call rejected; shown instead of/above
    /// `DiffRawText`.
    #[prop_or_default]
    pub error: Option<AttrValue>,
    pub on_close: Callback<()>,
}

#[function_component(BackupDiffView)]
pub fn backup_diff_view(props: &BackupDiffViewProps) -> Html {
    let on_close_click = {
        let cb = props.on_close.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };
    let on_backdrop_click = {
        let cb = props.on_close.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };
    let stop_propagation = Callback::from(|e: MouseEvent| e.stop_propagation());

    html! {
        // Dismissible by backdrop click, unlike the write/delete dialogs:
        // this is read-only, so there is nothing a stray click could lose.
        <div
            class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
            onclick={on_backdrop_click}
        >
            <div
                class="w-full max-w-4xl max-h-[85vh] flex flex-col rounded-lg bg-white dark:bg-slate-800 shadow-lg"
                onclick={stop_propagation}
                role="dialog"
                aria-modal="true"
                aria-label="Backup diff"
            >
                <div class="shrink-0 flex items-center justify-between px-6 py-4 border-b border-slate-200 dark:border-slate-700">
                    <h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100">
                        { format!(
                            "Diff: backup from {} vs. current {}",
                            props.backup_timestamp, props.target_path
                        ) }
                    </h2>
                </div>

                <div class="flex-1 overflow-y-auto px-6 py-4">
                    if let Some(error) = &props.error {
                        <div class="mb-4 rounded-md border border-red-300 dark:border-red-800 bg-red-50 dark:bg-red-900/20 text-red-800 dark:text-red-300 px-3 py-2 text-sm" role="alert">
                            { error.clone() }
                        </div>
                    } else {
                        <DiffRawText lines={props.lines.clone()} />
                    }
                </div>

                <div class="shrink-0 border-t border-slate-200 dark:border-slate-700 px-6 py-4 flex justify-end">
                    <button
                        type="button"
                        class="px-4 py-2 rounded-md bg-slate-100 dark:bg-slate-700 text-slate-700 dark:text-slate-200 text-sm font-medium focus-visible:ring-2 focus-visible:ring-sky-500"
                        onclick={on_close_click}
                    >
                        { "Close" }
                    </button>
                </div>
            </div>
        </div>
    }
}
