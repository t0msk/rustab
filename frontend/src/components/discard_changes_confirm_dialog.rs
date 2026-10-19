//! `DiscardChangesConfirmDialog` - confirms discarding every pending,
//! unsaved candidate change (§6).
//!
//! Same shape as `DeleteConfirmDialog`: only stages a reset of in-memory
//! state (no disk write), so it is dismissible by clicking the backdrop,
//! same judgment call as that dialog's own.

use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct DiscardChangesConfirmDialogProps {
    pub pending_count: usize,
    pub on_cancel: Callback<()>,
    pub on_confirm: Callback<()>,
}

#[function_component(DiscardChangesConfirmDialog)]
pub fn discard_changes_confirm_dialog(props: &DiscardChangesConfirmDialogProps) -> Html {
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

    let entry_word = if props.pending_count == 1 {
        "entry"
    } else {
        "entries"
    };

    html! {
        <div
            class="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4"
            onclick={on_backdrop_click}
        >
            <div
                class="w-full max-w-2xl rounded-lg bg-white dark:bg-slate-800 shadow-lg p-6"
                onclick={stop_propagation}
                role="dialog"
                aria-modal="true"
                aria-label="Discard changes"
            >
                <h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100 mb-2">
                    { "Discard all pending changes?" }
                </h2>
                <p class="text-sm text-slate-600 dark:text-slate-400 mb-4">
                    { format!(
                        "{} changed {entry_word} will be reverted to match the file as it is right now. Nothing on disk is touched either way.",
                        props.pending_count,
                    ) }
                </p>
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
                        { "Discard changes" }
                    </button>
                </div>
            </div>
        </div>
    }
}
