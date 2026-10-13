//! `FormActionBar` — Cancel/Save buttons inside the drawer (§4), sticky at
//! the bottom. `save_disabled` mirrors §5's "real disabled element, not
//! just a styling warning" requirement; this task doesn't wire real
//! validation so it is always enabled by default, driven by a prop so T11
//! can disable it once real validation errors exist.

use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct FormActionBarProps {
    #[prop_or(false)]
    pub save_disabled: bool,
    pub on_cancel: Callback<()>,
    pub on_save: Callback<()>,
}

#[function_component(FormActionBar)]
pub fn form_action_bar(props: &FormActionBarProps) -> Html {
    let on_cancel_click = {
        let cb = props.on_cancel.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };
    let on_save_click = {
        let cb = props.on_save.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };

    let save_classes = if props.save_disabled {
        classes!(
            "bg-slate-300",
            "text-slate-500",
            "dark:bg-slate-700",
            "dark:text-slate-500",
            "cursor-not-allowed"
        )
    } else {
        classes!("bg-sky-600", "hover:bg-sky-700", "text-white")
    };

    html! {
        <div class="shrink-0 flex justify-end gap-2 border-t border-slate-200 dark:border-slate-700 px-6 py-4">
            <button
                type="button"
                class="px-4 py-2 rounded-md bg-slate-100 dark:bg-slate-700 text-slate-700 dark:text-slate-200 text-sm font-medium focus-visible:ring-2 focus-visible:ring-sky-500"
                onclick={on_cancel_click}
            >
                { "Cancel" }
            </button>
            <button
                type="button"
                disabled={props.save_disabled}
                title={if props.save_disabled { "Fix validation errors before saving" } else { "" }}
                class={classes!("px-4", "py-2", "rounded-md", "text-sm", "font-medium", "focus-visible:ring-2", "focus-visible:ring-sky-500", save_classes)}
                onclick={on_save_click}
            >
                { "Save entry" }
            </button>
        </div>
    }
}
