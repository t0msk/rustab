//! `DescriptionField` - free-text input for an entry's optional
//! human-readable description (§4 item 4), round-tripped to/from a
//! `# // <text>` comment line immediately above the entry
//! (`docs/API_CONTRACT.md` §5).

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
                // Defense-in-depth UX guard: a <textarea> lets the user press
                // Enter directly (not just paste a multi-line string), so
                // this strip must run on every input, not just paste - it
                // keeps the backend's `description_contains_newline`
                // rejection practically unreachable from this field, since a
                // description must still serialize as a single comment line.
                let stripped = textarea.value().replace(['\n', '\r'], "");
                cb.emit(stripped);
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
                rows="2"
                class="w-full resize-none rounded-md border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-900 text-sm px-3 py-2 focus-visible:ring-2 focus-visible:ring-sky-500"
                value={props.value.clone()}
                oninput={on_input}
            />
            <p class="text-xs text-slate-500 dark:text-slate-400 mt-1">
                { "Shown as a '# // ...' comment line directly above this entry in the fstab file." }
            </p>
        </div>
    }
}
