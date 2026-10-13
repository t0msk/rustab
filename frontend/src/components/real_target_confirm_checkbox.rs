//! `RealTargetConfirmCheckbox` — the extra "I understand..." gate shown
//! only for real-target writes, reused by `DiffView` (§6) and
//! `BackupRestoreConfirmDialog` (§7).

use web_sys::HtmlInputElement;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct RealTargetConfirmCheckboxProps {
    pub checked: bool,
    pub on_change: Callback<bool>,
    /// Copy varies slightly between the diff-save flow and the restore
    /// flow, so the caller supplies the exact sentence (§6, §7).
    pub label: AttrValue,
}

#[function_component(RealTargetConfirmCheckbox)]
pub fn real_target_confirm_checkbox(props: &RealTargetConfirmCheckboxProps) -> Html {
    let on_change = {
        let cb = props.on_change.clone();
        Callback::from(move |e: Event| {
            let checked = e
                .target_dyn_into::<HtmlInputElement>()
                .is_some_and(|el| el.checked());
            cb.emit(checked);
        })
    };

    html! {
        <label class="flex items-start gap-2 text-sm text-slate-700 dark:text-slate-300 bg-red-50 dark:bg-red-900/20 border border-red-300 dark:border-red-800 rounded-md px-3 py-2">
            <input
                type="checkbox"
                class="mt-0.5 h-4 w-4 rounded border-slate-300 text-red-600 focus-visible:ring-2 focus-visible:ring-sky-500"
                checked={props.checked}
                onchange={on_change}
            />
            <span>{ props.label.clone() }</span>
        </label>
    }
}
