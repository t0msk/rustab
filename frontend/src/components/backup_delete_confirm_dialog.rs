//! `BackupDeleteConfirmDialog` - backup delete confirmation modal (§7).
//!
//! Centered modal, same non-click-outside-dismiss treatment as
//! `BackupRestoreConfirmDialog`/`DiffView`: a backup deletion is an
//! immediate, irreversible, potentially-`pkexec`-elevated action -
//! structurally like a restore, not like the staged, undoable
//! `DeleteConfirmDialog` entry removal - so the backdrop carries no click
//! handler at all.

use yew::prelude::*;

use crate::components::loading_spinner::LoadingSpinner;
use crate::components::real_target_confirm_checkbox::{
    confirm_disabled, RealTargetConfirmCheckbox,
};

#[derive(Properties, PartialEq)]
pub struct BackupDeleteConfirmDialogProps {
    pub backup_timestamp: AttrValue,
    /// "Will confirming this delete reach the real `/etc/fstab`?" - the same
    /// `is_real || source == SandboxDefault` predicate `AppShell` computes
    /// once and already passes to `DiffView`/`BackupRestoreConfirmDialog`
    /// (`docs/UI_DESIGN.md` §2). Deliberately not raw `is_real`: since T35
    /// a default, unconfigured launch's backups live beside - and are
    /// deleted from - `/etc/`, elevated the same way an explicit
    /// `RUSTAB_TARGET_PATH=/etc/fstab` launch's always were, so this gate
    /// must fire for both.
    pub writes_real_file: bool,
    pub confirm_checked: bool,
    pub on_confirm_checked_change: Callback<bool>,
    pub on_cancel: Callback<()>,
    pub on_confirm: Callback<()>,
    /// Set by `AppShell` while the `delete_backup` call is in flight (§7's
    /// "same loading → success/error handling pattern" as restore).
    #[prop_or(false)]
    pub saving: bool,
    /// The last `delete_backup` failure's message, shown inside this
    /// still-open dialog (not only as a toast) - same treatment as
    /// `BackupRestoreConfirmDialog`.
    #[prop_or_default]
    pub error: Option<AttrValue>,
}

#[function_component(BackupDeleteConfirmDialog)]
pub fn backup_delete_confirm_dialog(props: &BackupDeleteConfirmDialogProps) -> Html {
    let on_cancel_click = {
        let cb = props.on_cancel.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };
    let on_confirm_click = {
        let cb = props.on_confirm.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };
    let stop_propagation = Callback::from(|e: MouseEvent| e.stop_propagation());

    let confirm_disabled =
        confirm_disabled(props.writes_real_file, props.confirm_checked, props.saving);

    html! {
        // Intentionally no `onclick` here: deleting a backup is an
        // immediate, irreversible (and, for the real target, elevated)
        // action, so (per docs/UI_DESIGN.md §7) clicking the backdrop must
        // not dismiss this dialog - only explicit Cancel/Escape should.
        <div class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4">
            <div
                class="w-full max-w-lg rounded-lg bg-white dark:bg-slate-800 shadow-lg p-6"
                onclick={stop_propagation}
                role="dialog"
                aria-modal="true"
                aria-label="Delete backup"
            >
                <h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100 mb-2">
                    { format!("Delete the backup from {}?", props.backup_timestamp) }
                </h2>
                <p class="text-sm text-slate-600 dark:text-slate-300 mb-4">
                    { "This cannot be undone - there is no backup of a backup." }
                </p>
                if let Some(error) = &props.error {
                    <div class="mb-4 rounded-md border border-red-300 dark:border-red-800 bg-red-50 dark:bg-red-900/20 text-red-800 dark:text-red-300 px-3 py-2 text-sm" role="alert">
                        { error.clone() }
                    </div>
                }
                if props.writes_real_file {
                    <div class="mb-4">
                        <RealTargetConfirmCheckbox
                            checked={props.confirm_checked}
                            on_change={props.on_confirm_checked_change.clone()}
                            label="I understand this permanently deletes a backup of /etc/fstab and requires administrator privileges."
                        />
                    </div>
                }
                <div class="flex justify-end gap-2">
                    <button
                        type="button"
                        disabled={props.saving}
                        class="px-4 py-2 rounded-md bg-slate-100 dark:bg-slate-700 text-slate-700 dark:text-slate-200 text-sm font-medium focus-visible:ring-2 focus-visible:ring-sky-500 disabled:opacity-60 disabled:cursor-not-allowed"
                        onclick={on_cancel_click}
                    >
                        { "Cancel" }
                    </button>
                    <button
                        type="button"
                        disabled={confirm_disabled}
                        class={classes!(
                            "px-4", "py-2", "rounded-md", "text-sm", "font-medium", "inline-flex", "items-center", "gap-2",
                            "focus-visible:ring-2", "focus-visible:ring-sky-500",
                            if confirm_disabled {
                                classes!("bg-slate-300", "text-slate-500", "dark:bg-slate-700", "dark:text-slate-500", "cursor-not-allowed")
                            } else {
                                classes!("bg-red-600", "hover:bg-red-700", "text-white")
                            }
                        )}
                        onclick={on_confirm_click}
                    >
                        if props.saving {
                            <LoadingSpinner />
                            { "Deleting…" }
                        } else {
                            { "Delete backup" }
                        }
                    </button>
                </div>
            </div>
        </div>
    }
}
