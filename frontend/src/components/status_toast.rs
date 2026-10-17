//! `StatusToast` - transient success/warning/error notification.
//!
//! A self-contained, presentational shell: given a message and severity it
//! renders the toast. `AppShell` (T12) triggers it on a successful `save`/
//! `restore_backup`, auto-dismissing it after a few seconds; a *failed*
//! save/restore shows its error inside the still-open modal instead
//! (`docs/UI_DESIGN.md` §6 point 4), which is why `ToastKind::Error` is
//! still unused.

use yew::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Success,
    /// A completed action whose *main* effect succeeded but which has
    /// something the user must still be told. Added for T34's
    /// `WriteOutcomeDto::draft_sync_warning`: the real `/etc/fstab` was
    /// written, only the local working-draft mirror failed.
    ///
    /// Deliberately not `Error`: a red ✕ on a save that actually succeeded
    /// is the kind of misleading safety signal this app goes out of its way
    /// to avoid - the user would reasonably read it as "my fstab write
    /// failed" and retry, or panic. Amber + ⚠ says "done, with a caveat",
    /// which is exactly what happened.
    Warning,
    /// Not constructed anywhere yet: every failure path T12 wires
    /// (`save`/`restore_backup`/`delete_backup`/`create_backup`) shows its
    /// error inside the still-open modal or inline next to its button
    /// (`docs/UI_DESIGN.md` §6 point 4), not as a toast. Kept for a future
    /// fire-and-forget *failure* that isn't modal-gated - which T34's
    /// draft-sync warning is not, since the write it reports on succeeded.
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
    // `role` is part of the severity, not boilerplate: a success line is
    // polite (`status`, announced when the screen reader is idle), while a
    // `Warning`/`Error` carries something the user has to be told before the
    // toast self-dismisses, so it is announced assertively (`alert`).
    let (classes, icon, role) = match props.kind {
        ToastKind::Success => (
            classes!(
                "bg-emerald-600",
                "text-white",
                "border",
                "border-emerald-800"
            ),
            "✓",
            "status",
        ),
        ToastKind::Warning => (
            classes!("bg-amber-600", "text-white", "border", "border-amber-800"),
            "⚠",
            "alert",
        ),
        ToastKind::Error => (
            classes!("bg-red-600", "text-white", "border", "border-red-800"),
            "✕",
            "alert",
        ),
    };

    html! {
        // `max-w-md` + top-aligned icon because a `Warning` carries a
        // backend-written explanation, not a three-word success line: it
        // has to wrap instead of stretching the toast across the window.
        <div
            class={classes!("fixed", "bottom-6", "right-6", "z-[60]", "max-w-md", "rounded-md", "shadow-lg", "px-4", "py-3", "flex", "items-start", "gap-2", "text-sm", "font-medium", classes)}
            role={role}
        >
            <span class="shrink-0" aria-hidden="true">{ icon }</span>
            <span>{ props.message.clone() }</span>
        </div>
    }
}
