//! `DumpPassFields` - the two small numeric inputs (§4 item 6).

use web_sys::HtmlInputElement;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct DumpPassFieldsProps {
    // `u32` to match `EntryDto::dump`/`pass` exactly (API_CONTRACT.md §5:
    // "unsigned integers... fstab imposes no tighter range, so neither
    // does this contract") - narrowing to `u8` would silently corrupt an
    // edit of an entry whose existing value exceeds 255.
    pub dump: u32,
    pub pass: u32,
    pub on_dump_change: Callback<u32>,
    pub on_pass_change: Callback<u32>,
}

#[function_component(DumpPassFields)]
pub fn dump_pass_fields(props: &DumpPassFieldsProps) -> Html {
    let on_dump_input = {
        let cb = props.on_dump_change.clone();
        Callback::from(move |e: InputEvent| {
            if let Some(input) = e.target_dyn_into::<HtmlInputElement>() {
                cb.emit(input.value().parse().unwrap_or(0));
            }
        })
    };
    let on_pass_input = {
        let cb = props.on_pass_change.clone();
        Callback::from(move |e: InputEvent| {
            if let Some(input) = e.target_dyn_into::<HtmlInputElement>() {
                cb.emit(input.value().parse().unwrap_or(0));
            }
        })
    };

    html! {
        <div class="flex gap-6">
            <div>
                <label class="block text-sm font-medium text-slate-700 dark:text-slate-300 mb-1" for="dump-input">
                    { "Dump" }
                </label>
                <input
                    id="dump-input"
                    type="number"
                    min="0"
                    max="9"
                    class="w-16 rounded-md border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-900 text-sm px-2 py-2 focus-visible:ring-2 focus-visible:ring-sky-500"
                    value={props.dump.to_string()}
                    oninput={on_dump_input}
                />
                <p class="text-xs text-slate-500 dark:text-slate-400 mt-1">{ "0 = don't dump" }</p>
            </div>
            <div>
                <label class="block text-sm font-medium text-slate-700 dark:text-slate-300 mb-1" for="pass-input">
                    { "Pass" }
                </label>
                <input
                    id="pass-input"
                    type="number"
                    min="0"
                    max="9"
                    class="w-16 rounded-md border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-900 text-sm px-2 py-2 focus-visible:ring-2 focus-visible:ring-sky-500"
                    value={props.pass.to_string()}
                    oninput={on_pass_input}
                />
                <p class="text-xs text-slate-500 dark:text-slate-400 mt-1">
                    { "0 = skip fsck, 1 = root filesystem, 2 = checked after root" }
                </p>
            </div>
        </div>
    }
}
