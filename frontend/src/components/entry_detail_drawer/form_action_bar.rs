//! `FormActionBar` - Cancel/Verify/Save buttons inside the drawer (§4),
//! sticky at the bottom. `save_disabled` mirrors §5's "real disabled
//! element, not just a styling warning" requirement, driven by whether
//! real validation errors exist. `Verify`, between Cancel and Save,
//! triggers an immediate, non-debounced `validate_candidate` re-check
//! (distinct from the existing debounced background check) and confirms
//! the result via a toast; it is hidden entirely when `on_verify`
//! is `None` (raw-text edit mode has no structured document to validate).

use yew::prelude::*;

use crate::components::loading_spinner::LoadingSpinner;

#[derive(Properties, PartialEq)]
pub struct FormActionBarProps {
    #[prop_or(false)]
    pub save_disabled: bool,
    pub on_cancel: Callback<()>,
    pub on_save: Callback<()>,
    /// `None` hides the Verify button entirely (raw-text edit mode has no
    /// structured document to validate - see `EntryDetailDrawer`'s `is_raw_mode`).
    /// `Some` shows it, wired to an immediate, non-debounced `validate_candidate`
    /// re-check (distinct from the existing debounced background check).
    #[prop_or_default]
    pub on_verify: Option<Callback<()>>,
    /// Disables the Verify button - true while a Save or a Verify call is
    /// already in flight (prevents overlapping `validate_candidate` calls).
    #[prop_or(false)]
    pub verify_disabled: bool,
    /// True specifically while *this* button's own call is in flight (as
    /// opposed to a Save call) - switches the label to "Verifying…" with a
    /// `LoadingSpinner`, same convention as `HeaderBar`'s "Creating…" state.
    #[prop_or(false)]
    pub verifying: bool,
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
    let on_verify_click = props.on_verify.as_ref().map(|cb| {
        let cb = cb.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    });

    let verify_classes = if props.verify_disabled {
        classes!(
            "bg-white",
            "dark:bg-slate-800",
            "border",
            "border-slate-300",
            "dark:border-slate-600",
            "text-slate-700",
            "dark:text-slate-200",
            "opacity-60",
            "cursor-not-allowed"
        )
    } else {
        classes!(
            "bg-white",
            "dark:bg-slate-800",
            "border",
            "border-slate-300",
            "dark:border-slate-600",
            "text-slate-700",
            "dark:text-slate-200",
            "hover:bg-slate-50",
            "dark:hover:bg-slate-700"
        )
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
            if let Some(on_verify_click) = on_verify_click {
                <button
                    type="button"
                    disabled={props.verify_disabled}
                    class={classes!("px-4", "py-2", "rounded-md", "text-sm", "font-medium", "inline-flex", "items-center", "gap-2", "focus-visible:ring-2", "focus-visible:ring-sky-500", verify_classes)}
                    onclick={on_verify_click}
                >
                    if props.verifying {
                        <LoadingSpinner />
                        { "Verifying…" }
                    } else {
                        { "Verify" }
                    }
                </button>
            }
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
