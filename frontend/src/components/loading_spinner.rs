//! `LoadingSpinner` — inline loading indicator used inside buttons/panels
//! while a (future) write/restore/load operation is in flight.
//!
//! Deliberately deferred: this task has no real async write/restore
//! operation to show a loading state for (that's T12's job, see
//! `docs/UI_DESIGN.md` §6/§7) — present per the §9 inventory, ready to be
//! dropped into `DiffView`'s/`BackupRestoreConfirmDialog`'s confirm button.
#![allow(dead_code)]

use yew::prelude::*;

#[derive(Properties, PartialEq, Default)]
pub struct LoadingSpinnerProps {
    /// Extra Tailwind classes, e.g. to control size/color per call site.
    #[prop_or_default]
    pub class: Classes,
}

#[function_component(LoadingSpinner)]
pub fn loading_spinner(props: &LoadingSpinnerProps) -> Html {
    html! {
        <span
            class={classes!("inline-block", "h-4", "w-4", "rounded-full", "border-2", "border-current", "border-t-transparent", "animate-spin", props.class.clone())}
            role="status"
            aria-label="Loading"
        ></span>
    }
}
