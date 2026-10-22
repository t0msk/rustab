//! `DescriptionField` - free-text input for an entry's optional
//! human-readable description (§4 item 4), round-tripped to/from one or
//! more ordinary `#` comment lines directly above the entry
//! (`docs/API_CONTRACT.md` §5). The value is genuinely multi-line:
//! embedded `\n` is a normal, expected shape, not something this field
//! guards against.

use web_sys::HtmlTextAreaElement;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct DescriptionFieldProps {
    pub value: AttrValue,
    pub on_change: Callback<String>,
}

#[function_component(DescriptionField)]
pub fn description_field(props: &DescriptionFieldProps) -> Html {
    let on_input = {
        let cb = props.on_change.clone();
        Callback::from(move |e: InputEvent| {
            if let Some(textarea) = e.target_dyn_into::<HtmlTextAreaElement>() {
                cb.emit(textarea.value());
            }
        })
    };

    html! {
        <div>
            <label class="block text-sm font-medium text-slate-700 dark:text-slate-300 mb-1" for="description-input">
                { "Description (optional)" }
            </label>
            <textarea
                id="description-input"
                rows="3"
                class="w-full rounded-md border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-900 text-sm px-3 py-2 focus-visible:ring-2 focus-visible:ring-sky-500"
                value={props.value.clone()}
                oninput={on_input}
            />
            <p class="text-xs text-slate-500 dark:text-slate-400 mt-1">
                { "Shown as one or more ordinary '#' comment lines directly above this entry in the fstab file. Drag the bottom-right corner to resize." }
            </p>
        </div>
    }
}
