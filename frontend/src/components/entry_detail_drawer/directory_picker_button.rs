//! `DirectoryPickerButton` - native directory chooser trigger (§4 item 3).
//!
//! The concrete Tauri API for opening a native directory chooser is not
//! this task's decision (that's T11/T8) - this is a no-op stub button for
//! now, wired via an optional callback prop so a later task can attach
//! real behavior without changing this component's shape.

use yew::prelude::*;

#[derive(Properties, PartialEq, Default)]
pub struct DirectoryPickerButtonProps {
    #[prop_or_default]
    pub on_click: Callback<MouseEvent>,
}

#[function_component(DirectoryPickerButton)]
pub fn directory_picker_button(props: &DirectoryPickerButtonProps) -> Html {
    html! {
        <button
            type="button"
            class="shrink-0 px-3 py-2 rounded-md bg-slate-100 hover:bg-slate-200 dark:bg-slate-700 dark:hover:bg-slate-600 text-slate-700 dark:text-slate-200 text-sm font-medium focus-visible:ring-2 focus-visible:ring-sky-500"
            onclick={props.on_click.clone()}
        >
            { "Browse…" }
        </button>
    }
}
