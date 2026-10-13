//! `BackupRestoreConfirmDialog` — restore confirmation modal (§7).
//!
//! Centered modal, same non-click-outside-dismiss treatment as `DiffView`
//! since this is also a real write: the backdrop carries no click handler
//! at all.

use yew::prelude::*;

use crate::components::real_target_confirm_checkbox::RealTargetConfirmCheckbox;

#[derive(Properties, PartialEq)]
pub struct BackupRestoreConfirmDialogProps {
    pub backup_timestamp: AttrValue,
    pub target_path: AttrValue,
    pub is_real: bool,
    pub confirm_checked: bool,
    pub on_confirm_checked_change: Callback<bool>,
    pub on_cancel: Callback<()>,
    pub on_confirm: Callback<()>,
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

    let confirm_disabled = props.is_real && !props.confirm_checked;
    let confirm_classes = if props.is_real {
        classes!("bg-red-600", "hover:bg-red-700", "text-white")
    } else {
        classes!("bg-sky-600", "hover:bg-sky-700", "text-white")
    };
    let confirm_label = if props.is_real {
        "Restore to /etc/fstab"
    } else {
        "Restore to sandbox file"
    };

    html! {
        // Intentionally no `onclick` here: restoring a backup is a real
        // write, so (per docs/UI_DESIGN.md §7) clicking the backdrop must
        // not dismiss this dialog — only explicit Cancel/Escape should.
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
                <p class="text-sm text-slate-600 dark:text-slate-300 mb-3">
                    { format!("This will overwrite the current contents of {}.", props.target_path) }
                </p>
                <p class="text-sm text-slate-500 dark:text-slate-400 mb-4">
                    { "Restoring itself creates a fresh backup of the current file first, following the same atomic write guarantee used everywhere else — but any unsaved pending edits in this session will be discarded." }
                </p>
                if props.is_real {
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
                        class="px-4 py-2 rounded-md bg-slate-100 dark:bg-slate-700 text-slate-700 dark:text-slate-200 text-sm font-medium focus-visible:ring-2 focus-visible:ring-sky-500"
                        onclick={on_cancel_click}
                    >
                        { "Cancel" }
                    </button>
                    <button
                        type="button"
                        disabled={confirm_disabled}
                        class={classes!(
                            "px-4", "py-2", "rounded-md", "text-sm", "font-medium",
                            "focus-visible:ring-2", "focus-visible:ring-sky-500",
                            if confirm_disabled {
                                classes!("bg-slate-300", "text-slate-500", "dark:bg-slate-700", "dark:text-slate-500", "cursor-not-allowed")
                            } else {
                                confirm_classes
                            }
                        )}
                        onclick={on_confirm_click}
                    >
                        { confirm_label }
                    </button>
                </div>
            </div>
        </div>
    }
}
