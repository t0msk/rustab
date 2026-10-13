//! `StatusToast` — transient success/error notification.
//!
//! No real save/restore result exists yet (T10 is mock-only), so this
//! component is a self-contained, presentational shell: given a message and
//! severity it renders the toast; nothing in this task triggers it from a
//! real operation. Deliberately deferred: present per the §9 inventory,
//! ready for T12 to trigger on save/restore success or failure.
#![allow(dead_code)]

use yew::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Success,
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
