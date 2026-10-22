//! `LoadingSpinner` - inline loading indicator used inside buttons/panels
//! while a write/restore operation is in flight (`docs/UI_DESIGN.md`
//! §6/§7): dropped into `DiffView`'s and `BackupRestoreConfirmDialog`'s
//! confirm buttons while `save`/`restore_backup` is pending.

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
