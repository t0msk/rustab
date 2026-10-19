//! `PendingChangesBar` - "N entries changed" bar, hidden when N is 0.
//!
//! Per `docs/UI_DESIGN.md` §6, this is the only way to reach the diff/write
//! step. For T10 the count is a mock `use_state<usize>` owned by
//! `AppShell` (defaulting to 0) purely to prove the show/hide logic - real
//! pending-change tracking is wired in T11.

use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct PendingChangesBarProps {
    pub count: usize,
    pub on_review: Callback<()>,
    /// Opens `DiscardChangesConfirmDialog` (`AppShell` owns the confirm
    /// step itself - this bar only ever asks for the dialog to be shown,
    /// never resets `candidate` directly).
    pub on_discard: Callback<()>,
}

#[function_component(PendingChangesBar)]
pub fn pending_changes_bar(props: &PendingChangesBarProps) -> Html {
    if props.count == 0 {
        return html! {};
    }

    let label = if props.count == 1 {
        "1 entry changed".to_string()
    } else {
        format!("{} entries changed", props.count)
    };

    let on_review_click = {
        let cb = props.on_review.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };
    let on_discard_click = {
        let cb = props.on_discard.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };

    html! {
        <div class="w-full shrink-0 bg-amber-50 dark:bg-amber-900/20 border-b border-amber-300 dark:border-amber-800 px-6 py-2 flex items-center justify-between">
            <span class="text-sm font-medium text-amber-800 dark:text-amber-300">{ label }</span>
            <div class="flex items-center gap-2">
                <button
                    type="button"
                    class="px-4 py-2 rounded-md bg-white dark:bg-slate-800 border border-amber-300 dark:border-amber-700 text-amber-800 dark:text-amber-300 text-sm font-medium hover:bg-amber-100 dark:hover:bg-amber-900/40 focus-visible:ring-2 focus-visible:ring-sky-500 focus-visible:ring-offset-2"
                    onclick={on_discard_click}
                >
                    { "Discard changes" }
                </button>
                <button
                    type="button"
                    class="px-4 py-2 rounded-md bg-sky-600 hover:bg-sky-700 text-white text-sm font-medium focus-visible:ring-2 focus-visible:ring-sky-500 focus-visible:ring-offset-2"
                    onclick={on_review_click}
                >
                    { "Review & Save…" }
                </button>
            </div>
        </div>
    }
}
