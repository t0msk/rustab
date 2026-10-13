//! `FsTypeField` — free-text combobox with common-type suggestions (§4
//! item 4). Uses a native `<datalist>` so any value can still be typed and
//! accepted — there is no closed enum, matching SPEC §5.

use web_sys::HtmlInputElement;
use yew::prelude::*;

const SUGGESTIONS: &[&str] = &[
    "ext4", "btrfs", "xfs", "vfat", "ntfs3", "swap", "none", "nfs", "nfs4", "cifs", "overlay",
    "tmpfs", "proc",
];

#[derive(Properties, PartialEq)]
pub struct FsTypeFieldProps {
    pub value: AttrValue,
    pub on_change: Callback<String>,
}

#[function_component(FsTypeField)]
pub fn fs_type_field(props: &FsTypeFieldProps) -> Html {
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
            <label class="block text-sm font-medium text-slate-700 dark:text-slate-300 mb-1" for="fs-type-input">
                { "Filesystem type" }
            </label>
            <input
                id="fs-type-input"
                type="text"
                list="fs-type-suggestions"
                class="w-full rounded-md border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-900 font-mono text-sm px-3 py-2 focus-visible:ring-2 focus-visible:ring-sky-500"
                value={props.value.clone()}
                oninput={on_input}
            />
            <datalist id="fs-type-suggestions">
                { for SUGGESTIONS.iter().map(|suggestion| html! { <option value={*suggestion} /> }) }
            </datalist>
        </div>
    }
}
