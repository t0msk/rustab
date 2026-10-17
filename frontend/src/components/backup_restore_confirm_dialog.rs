//! `BackupRestoreConfirmDialog` - restore confirmation modal (§7).
//!
//! Centered modal, same non-click-outside-dismiss treatment as `DiffView`
//! since this is also a real write: the backdrop carries no click handler
//! at all.

use yew::prelude::*;

use crate::components::loading_spinner::LoadingSpinner;
use crate::components::real_target_confirm_checkbox::{
    confirm_disabled, RealTargetConfirmCheckbox,
};

#[derive(Properties, PartialEq)]
pub struct BackupRestoreConfirmDialogProps {
    pub backup_timestamp: AttrValue,
    pub target_path: AttrValue,
    /// `true` when confirming here writes the real `/etc/fstab` - which,
    /// since T34, is every launch except an explicit non-real
    /// `RUSTAB_TARGET_PATH` override. A restore is literally a save of the
    /// backup's content (`restore_backup` delegates to the very same
    /// dispatch, `src-tauri/src/commands.rs`), so it is gated, colored and
    /// labelled exactly like `DiffView`'s save, from the one value
    /// `AppShell` derives.
    pub writes_real_file: bool,
    pub confirm_checked: bool,
    pub on_confirm_checked_change: Callback<bool>,
    pub on_cancel: Callback<()>,
    pub on_confirm: Callback<()>,
    /// Set by `AppShell` while the `restore_backup` call is in flight
    /// (§7's "same loading → success/error handling pattern as DiffView's
    /// save").
    #[prop_or(false)]
    pub saving: bool,
    /// The last `restore_backup` failure's message, shown inside this
    /// still-open dialog (not only as a toast) - same treatment as
    /// `DiffView` (§6 point 4).
    #[prop_or_default]
    pub error: Option<AttrValue>,
}

#[function_component(BackupRestoreConfirmDialog)]
pub fn backup_restore_confirm_dialog(props: &BackupRestoreConfirmDialogProps) -> Html {
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
    let confirm_classes = if props.writes_real_file {
        classes!("bg-red-600", "hover:bg-red-700", "text-white")
    } else {
        classes!("bg-sky-600", "hover:bg-sky-700", "text-white")
    };
    let confirm_label = if props.writes_real_file {
        "Restore to /etc/fstab"
    } else {
        "Restore to test file"
    };

    html! {
        // Intentionally no `onclick` here: restoring a backup is a real
        // write, so (per docs/UI_DESIGN.md §7) clicking the backdrop must
        // not dismiss this dialog - only explicit Cancel/Escape should.
        <div class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4">
            <div
                class="w-full max-w-lg rounded-lg bg-white dark:bg-slate-800 shadow-lg p-6"
                onclick={stop_propagation}
                role="dialog"
                aria-modal="true"
                aria-label="Restore backup"
            >
                <h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100 mb-2">
                    { format!("Restore the backup from {}?", props.backup_timestamp) }
                </h2>
                <p class="text-sm text-slate-600 dark:text-slate-300 mb-3 break-all">
                    { format!("This will overwrite the current contents of {}.", props.target_path) }
                </p>
                if props.writes_real_file {
                    // Stated explicitly because `target_path` above is the
                    // backup's own target - the local working draft, on a
                    // default launch - which on its own would read as if
                    // the real file were out of scope here (§7).
                    <p class="text-sm font-semibold text-amber-700 dark:text-amber-400 mb-3">
                        { "Like a save, this restore writes the real /etc/fstab and will ask for administrator access." }
                    </p>
                }
                <p class="text-sm text-slate-500 dark:text-slate-400 mb-4">
                    { "Restoring itself creates a fresh backup of the current file first, following the same atomic write guarantee used everywhere else - but any unsaved pending edits in this session will be discarded." }
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
                            label="I understand this modifies my system's real fstab and takes effect on next mount/boot."
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
                                confirm_classes
                            }
                        )}
                        onclick={on_confirm_click}
                    >
                        if props.saving {
                            <LoadingSpinner />
                            { "Restoring…" }
                        } else {
                            { confirm_label }
                        }
                    </button>
                </div>
            </div>
        </div>
    }
}
