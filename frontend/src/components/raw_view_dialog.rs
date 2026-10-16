//! `RawViewDialog` - read-only overlay showing the active target's exact
//! current raw text, with no diff computed against anything. Distinct from
//! `DiffView`/`BackupDiffView`, which both always compare two versions of a
//! file - this is for the simpler "what does the file actually contain
//! right now" question, independent of any pending edit.
//!
//! Reuses the same modal shell `BackupDiffView`/`AboutDialog` already use:
//! this performs no write, so - same judgment call as those two - it is
//! dismissible by clicking the backdrop.

use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct RawViewDialogProps {
    pub target_path: AttrValue,
    pub text: AttrValue,
    pub on_close: Callback<()>,
}

#[function_component(RawViewDialog)]
pub fn raw_view_dialog(props: &RawViewDialogProps) -> Html {
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
                aria-label="Raw file view"
            >
                <div class="shrink-0 flex items-center justify-between px-6 py-4 border-b border-slate-200 dark:border-slate-700">
                    <h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100">
                        { format!("Raw view: {}", props.target_path) }
                    </h2>
                </div>

                <div class="flex-1 overflow-y-auto px-6 py-4">
                    <pre class="font-mono text-sm whitespace-pre-wrap bg-slate-900/5 dark:bg-slate-950/40 rounded-md px-3 py-2">
                        { props.text.clone() }
                    </pre>
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
