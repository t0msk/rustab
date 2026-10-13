//! `MountPointField` — text input + directory-picker trigger (§4 item 3).

use web_sys::HtmlInputElement;
use yew::prelude::*;

use super::directory_picker_button::DirectoryPickerButton;

#[derive(Properties, PartialEq)]
pub struct MountPointFieldProps {
    pub value: AttrValue,
    pub on_change: Callback<String>,
}

#[function_component(MountPointField)]
pub fn mount_point_field(props: &MountPointFieldProps) -> Html {
    let on_input = {
        let cb = props.on_change.clone();
        Callback::from(move |e: InputEvent| {
            if let Some(input) = e.target_dyn_into::<HtmlInputElement>() {
                cb.emit(input.value());
            }
        })
    };

    html! {
        <div>
            <label class="block text-sm font-medium text-slate-700 dark:text-slate-300 mb-1" for="mount-point-input">
                { "Mount point" }
            </label>
            <div class="flex gap-2">
                <input
                    id="mount-point-input"
                    type="text"
                    class="flex-1 rounded-md border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-900 font-mono text-sm px-3 py-2 focus-visible:ring-2 focus-visible:ring-sky-500"
                    value={props.value.clone()}
                    oninput={on_input}
                />
                <DirectoryPickerButton />
            </div>
        </div>
    }
}
