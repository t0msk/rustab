//! `ValidationMessage` — generic per-field error/warning renderer (§5).
//!
//! The severity classification (which checks are hard-blocking vs.
//! advisory) is a backend/validator decision (T5) wired up in T11 — this
//! component only renders whatever severity it is given. Not actively
//! triggered with mock error data in this task's shipped shell, since the
//! static placeholder fields have nothing to validate yet.
//!
//! Deliberately deferred: not wired to any field in this task's shell (no
//! real validation results exist until T11), so nothing in this crate
//! calls it yet — allowed here rather than deleted, since the component
//! itself is part of the required §9 inventory.
#![allow(dead_code)]

use yew::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Properties, PartialEq)]
pub struct ValidationMessageProps {
    pub message: AttrValue,
    pub severity: Severity,
}

#[function_component(ValidationMessage)]
pub fn validation_message(props: &ValidationMessageProps) -> Html {
    let (classes, icon) = match props.severity {
        Severity::Error => (classes!("text-red-700", "dark:text-red-400"), "⚠"),
        Severity::Warning => (classes!("text-amber-600", "dark:text-amber-400"), "ℹ"),
    };

    html! {
        <p class={classes!("flex", "items-center", "gap-1", "text-xs", "mt-1", classes)}>
            <span aria-hidden="true">{ icon }</span>
            <span>{ props.message.clone() }</span>
        </p>
    }
}

/// The matching field-level input ring class for a given severity, for
/// callers that need to highlight the input itself (§5: `ring-2
/// ring-red-500` for errors, `ring-1 ring-amber-400` for warnings).
pub fn severity_ring_classes(severity: Severity) -> Classes {
    match severity {
        Severity::Error => classes!("ring-2", "ring-red-500"),
        Severity::Warning => classes!("ring-1", "ring-amber-400"),
    }
}
