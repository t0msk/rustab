//! `DirectoryPickerButton` - native directory chooser trigger (§4 item 3).
//!
//! A plain presentational button that takes an `on_click` callback and
//! nothing else. `MountPointField` wires that callback to open a native
//! directory chooser via the backend's `pick_directory` command
//! (API_CONTRACT.md §3.16) and feed the chosen path back into the Mount
//! point field on success (a cancelled chooser leaves the field untouched).

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
