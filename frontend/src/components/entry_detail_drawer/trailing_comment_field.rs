//! `TrailingCommentField` - free-text input for an entry's optional
//! trailing inline comment (§4 item 8), round-tripped to/from the text
//! after the `#` appended to the entry's own fstab line
//! (`docs/API_CONTRACT.md` §5). Unlike `DescriptionField`, this value is
//! preserved verbatim (no trimming) and must stay single-line: an embedded
//! newline would split one physical fstab line into two.

use web_sys::HtmlInputElement;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct TrailingCommentFieldProps {
    pub value: AttrValue,
    pub on_change: Callback<String>,
}

#[function_component(TrailingCommentField)]
pub fn trailing_comment_field(props: &TrailingCommentFieldProps) -> Html {
    let on_input = {
        let cb = props.on_change.clone();
        Callback::from(move |e: InputEvent| {
            if let Some(input) = e.target_dyn_into::<HtmlInputElement>() {
                // Defense-in-depth UX guard: strip any newline a paste might
                // carry so the backend's `trailing_comment_contains_newline`
                // rejection becomes practically unreachable from this input.
                let stripped = input.value().replace(['\n', '\r'], "");
                cb.emit(stripped);
            }
        })
    };

    html! {
        <div>
            <label class="block text-sm font-medium text-slate-700 dark:text-slate-300 mb-1" for="trailing-comment-input">
                { "Trailing comment (optional)" }
            </label>
            <input
                id="trailing-comment-input"
                type="text"
                class="w-full rounded-md border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-900 text-sm px-3 py-2 focus-visible:ring-2 focus-visible:ring-sky-500"
                value={props.value.clone()}
                oninput={on_input}
            />
            <p class="text-xs text-slate-500 dark:text-slate-400 mt-1">
                { "Appended inline after this entry's own fields, on the same line, after a '#'. Must be a single line - newlines are not allowed here." }
            </p>
        </div>
    }
}
