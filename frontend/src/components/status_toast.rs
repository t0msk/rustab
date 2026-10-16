//! `StatusToast` - transient success/error notification.
//!
//! A self-contained, presentational shell: given a message and severity it
//! renders the toast. `AppShell` (T12) triggers it on a successful `save`/
//! `restore_backup`, auto-dismissing it after a few seconds; a *failed*
//! save/restore shows its error inside the still-open modal instead
//! (`docs/UI_DESIGN.md` §6 point 4), so only `ToastKind::Success` is
//! actually used today - `ToastKind::Error` stays available for any future
//! fire-and-forget failure that isn't modal-gated.

use yew::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Success,
    /// Not constructed anywhere yet: every failure path T12 wires
    /// (`save`/`restore_backup`) shows its error inside the still-open
    /// modal instead (`docs/UI_DESIGN.md` §6 point 4), not as a toast.
    /// Kept for a future fire-and-forget failure that isn't modal-gated.
    #[allow(dead_code)]
    Error,
}

#[derive(Properties, PartialEq)]
pub struct StatusToastProps {
    pub kind: ToastKind,
    pub message: AttrValue,
}

#[function_component(StatusToast)]
pub fn status_toast(props: &StatusToastProps) -> Html {
    let (classes, icon) = match props.kind {
        ToastKind::Success => (
            classes!(
                "bg-emerald-600",
                "text-white",
                "border",
                "border-emerald-800"
            ),
            "✓",
        ),
        ToastKind::Error => (
            classes!("bg-red-600", "text-white", "border", "border-red-800"),
            "✕",
        ),
    };

    html! {
        <div
            class={classes!("fixed", "bottom-6", "right-6", "z-[60]", "rounded-md", "shadow-lg", "px-4", "py-3", "flex", "items-center", "gap-2", "text-sm", "font-medium", classes)}
            role="status"
        >
            <span aria-hidden="true">{ icon }</span>
            <span>{ props.message.clone() }</span>
        </div>
    }
}
