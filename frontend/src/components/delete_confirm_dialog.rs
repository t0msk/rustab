//! `DeleteConfirmDialog` - single-entry delete confirmation (§4).
//!
//! Small centered modal. Unlike `DiffView`/`BackupRestoreConfirmDialog`
//! this only stages a pending removal (no disk write), so - judgment call -
//! it *is* dismissible by clicking the backdrop, same as the non-destructive
//! `EntryDetailDrawer`.

use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct DeleteConfirmDialogProps {
    pub source_summary: AttrValue,
    pub mount_point: AttrValue,
    pub fs_type: AttrValue,
    pub on_cancel: Callback<()>,
    pub on_confirm: Callback<()>,
}

#[function_component(DeleteConfirmDialog)]
pub fn delete_confirm_dialog(props: &DeleteConfirmDialogProps) -> Html {
    let on_backdrop_click = {
        let cb = props.on_cancel.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };
    let on_cancel_click = {
        let cb = props.on_cancel.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };
    let on_confirm_click = {
        let cb = props.on_confirm.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };
    let stop_propagation = Callback::from(|e: MouseEvent| e.stop_propagation());

    html! {
        <div
            class="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4"
            onclick={on_backdrop_click}
        >
            <div
                class="w-full max-w-sm rounded-lg bg-white dark:bg-slate-800 shadow-lg p-6"
                onclick={stop_propagation}
                role="dialog"
                aria-modal="true"
                aria-label="Remove entry"
            >
                <h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100 mb-2">
                    { format!("Remove this entry from {}?", props.mount_point) }
                </h2>
                <dl class="text-sm font-mono bg-slate-100 dark:bg-slate-950/40 rounded-md px-3 py-2 mb-4 space-y-1 text-slate-700 dark:text-slate-300">
                    <div><dt class="inline text-slate-500 dark:text-slate-400">{ "Source: " }</dt><dd class="inline">{ props.source_summary.clone() }</dd></div>
                    <div><dt class="inline text-slate-500 dark:text-slate-400">{ "Mount point: " }</dt><dd class="inline">{ props.mount_point.clone() }</dd></div>
                    <div><dt class="inline text-slate-500 dark:text-slate-400">{ "FS type: " }</dt><dd class="inline">{ props.fs_type.clone() }</dd></div>
                </dl>
                <div class="flex justify-end gap-2">
                    <button
                        type="button"
                        class="px-4 py-2 rounded-md bg-slate-100 dark:bg-slate-700 text-slate-700 dark:text-slate-200 text-sm font-medium focus-visible:ring-2 focus-visible:ring-sky-500"
                        onclick={on_cancel_click}
                    >
                        { "Cancel" }
                    </button>
                    <button
                        type="button"
                        class="px-4 py-2 rounded-md bg-red-600 hover:bg-red-700 text-white text-sm font-medium focus-visible:ring-2 focus-visible:ring-sky-500"
                        onclick={on_confirm_click}
                    >
                        { "Remove" }
                    </button>
                </div>
            </div>
        </div>
    }
}
