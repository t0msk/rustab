//! `ExternalChangeDialog` - T36's warning/diff modal for a `save`/
//! `restore_backup` call that rejected with `kind: "external_change"`
//! (`docs/API_CONTRACT.md` §8): `/etc/fstab` changed outside Rustab since
//! this session last synced with it, so nothing was written.
//!
//! Shared by both flows (`DiffView`'s save and `BackupRestoreConfirmDialog`'s
//! restore) exactly like `RealTargetConfirmCheckbox` already is - `AppShell`
//! instantiates one per flow, each wired to its own retry callback.
//!
//! Same non-dismissible-by-backdrop treatment as `DiffView`/
//! `BackupRestoreConfirmDialog` (no `onclick` on the backdrop, `stop_propagation`
//! on the inner panel): this is also a real-write gate, one step further
//! down the same path. Reuses `crate::diff::compute_raw_diff` (via the
//! `lines` prop `AppShell` computes, mirroring `BackupDiffView`'s own
//! already-computed-upstream pattern) and `DiffRawText` to render it, and
//! `RealTargetConfirmCheckbox` for the second, more specific confirmation -
//! "write anyway" is a separate, explicit gate layered on top of whichever
//! `RealTargetConfirmCheckbox` the user already passed to reach this point,
//! not a replacement for it.
//!
//! **T37** adds a third action, "Reload and re-sync", between `Cancel` and
//! `Write anyway`: it calls `resync_external_change`, which never writes to
//! the real file, so it needs no confirmation checkbox - only its own
//! `resyncing` loading flag, kept separate from `saving` so neither button's
//! label is misrepresented by the other call's state. All three buttons are
//! disabled while *either* flag is `true`, so the user can't fire two of
//! these calls at once.

use yew::prelude::*;

use crate::components::diff_view::DiffRawText;
use crate::components::loading_spinner::LoadingSpinner;
use crate::components::real_target_confirm_checkbox::{
    confirm_disabled, RealTargetConfirmCheckbox,
};
use crate::diff::RawDiffLine;

#[derive(Properties, PartialEq)]
pub struct ExternalChangeDialogProps {
    /// The real `/etc/fstab` path the external change was detected on
    /// (`WriteErrorDto::path`), shown in the header.
    pub target_path: AttrValue,
    /// `compute_raw_diff(last_known_content, current_content)`, already
    /// computed by `AppShell` from the error's `external_change` payload -
    /// both sides are present as soon as this dialog opens, unlike
    /// `DiffView`/`BackupDiffView`'s own `Option` props, which cover an
    /// async fetch this dialog never needs.
    pub lines: Vec<RawDiffLine>,
    pub confirm_checked: bool,
    pub on_confirm_checked_change: Callback<bool>,
    /// Nothing is written; the caller's documented choice is to close the
    /// whole flow back to the pending-changes state (see `app_shell.rs`).
    pub on_cancel: Callback<()>,
    /// Repeats the original call with `allow_external_change: true`.
    pub on_write_anyway: Callback<()>,
    /// Set by `AppShell` while the retried `save`/`restore_backup` call is
    /// in flight, same treatment as `DiffView`/`BackupRestoreConfirmDialog`.
    #[prop_or(false)]
    pub saving: bool,
    /// An unrelated failure from the retried call itself (e.g. validation,
    /// another write error, or even another drift), shown inside this
    /// still-open dialog - same pattern as `DiffView`/
    /// `BackupRestoreConfirmDialog`.
    #[prop_or_default]
    pub error: Option<AttrValue>,
    /// T37: calls `resync_external_change`, adopting the real file's current
    /// content as the new baseline without touching the user's pending
    /// edits (save flow) or writing anything (restore flow).
    pub on_reload: Callback<()>,
    /// Set by `AppShell` while the `resync_external_change` call is in
    /// flight - its own flag, deliberately kept separate from `saving` so
    /// neither button's loading state is misrepresented by the other one
    /// being in flight.
    #[prop_or(false)]
    pub resyncing: bool,
}

#[function_component(ExternalChangeDialog)]
pub fn external_change_dialog(props: &ExternalChangeDialogProps) -> Html {
    let on_cancel_click = {
        let cb = props.on_cancel.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };
    let on_write_anyway_click = {
        let cb = props.on_write_anyway.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };
    let on_reload_click = {
        let cb = props.on_reload.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };
    let stop_propagation = Callback::from(|e: MouseEvent| e.stop_propagation());

    // Mutual exclusion: none of the three buttons may be clicked while
    // *either* retried call is in flight, so a user can never fire two of
    // them at once.
    let any_in_flight = props.saving || props.resyncing;

    // "Write anyway" is gated exactly like the save/restore confirm button
    // it sits behind: always `requires_confirmation: true` here, since this
    // dialog only ever appears on a path that already writes the real
    // `/etc/fstab` (the external-change check only runs for the
    // `SandboxDefault` dispatch - API_CONTRACT.md §3.5).
    let write_anyway_disabled = confirm_disabled(true, props.confirm_checked, any_in_flight);
    // "Reload and re-sync" writes nothing to the real file, so it needs no
    // confirmation checkbox - only the same in-flight gate as the other two.
    let reload_disabled = any_in_flight;

    html! {
        // Intentionally no `onclick` here, same as `DiffView`/
        // `BackupRestoreConfirmDialog`: this is also a real-write gate, not
        // dismissible by a stray backdrop click.
        <div class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4">
            <div
                class="w-full max-w-4xl max-h-[85vh] flex flex-col rounded-lg bg-white dark:bg-slate-800 shadow-lg"
                onclick={stop_propagation}
                role="dialog"
                aria-modal="true"
                aria-label="External change detected"
            >
                <div class="shrink-0 px-6 py-4 border-b border-slate-200 dark:border-slate-700">
                    <h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100 mb-1">
                        { "This file changed outside Rustab" }
                    </h2>
                    <p class="text-sm text-slate-600 dark:text-slate-300 break-all">
                        {
                            format!(
                                "{} changed outside Rustab since this session last synced with it. \
                                 Nothing was written. Review what changed below before deciding.",
                                props.target_path
                            )
                        }
                    </p>
                </div>

                <div class="flex-1 overflow-y-auto px-6 py-4">
                    if let Some(error) = &props.error {
                        <div class="mb-4 rounded-md border border-red-300 dark:border-red-800 bg-red-50 dark:bg-red-900/20 text-red-800 dark:text-red-300 px-3 py-2 text-sm" role="alert">
                            { error.clone() }
                        </div>
                    }
                    <DiffRawText lines={Some(props.lines.clone())} />
                </div>

                <div class="shrink-0 border-t border-slate-200 dark:border-slate-700 px-6 py-4 space-y-3">
                    <RealTargetConfirmCheckbox
                        checked={props.confirm_checked}
                        on_change={props.on_confirm_checked_change.clone()}
                        label="I understand this overwrites a change that happened outside Rustab, discarding what is on disk right now."
                    />
                    <div class="flex justify-end gap-2">
                        <button
                            type="button"
                            disabled={any_in_flight}
                            class="px-4 py-2 rounded-md bg-slate-100 dark:bg-slate-700 text-slate-700 dark:text-slate-200 text-sm font-medium focus-visible:ring-2 focus-visible:ring-sky-500 disabled:opacity-60 disabled:cursor-not-allowed"
                            onclick={on_cancel_click}
                        >
                            { "Cancel" }
                        </button>
                        <button
                            type="button"
                            disabled={reload_disabled}
                            class="px-4 py-2 rounded-md bg-sky-600 hover:bg-sky-700 text-white text-sm font-medium inline-flex items-center gap-2 focus-visible:ring-2 focus-visible:ring-sky-500 disabled:opacity-60 disabled:cursor-not-allowed"
                            onclick={on_reload_click}
                        >
                            if props.resyncing {
                                <LoadingSpinner />
                                { "Reloading…" }
                            } else {
                                { "Reload and re-sync" }
                            }
                        </button>
                        <button
                            type="button"
                            disabled={write_anyway_disabled}
                            class={classes!(
                                "px-4", "py-2", "rounded-md", "text-sm", "font-medium", "inline-flex", "items-center", "gap-2",
                                "focus-visible:ring-2", "focus-visible:ring-sky-500",
                                if write_anyway_disabled {
                                    classes!("bg-slate-300", "text-slate-500", "dark:bg-slate-700", "dark:text-slate-500", "cursor-not-allowed")
                                } else {
                                    classes!("bg-red-600", "hover:bg-red-700", "text-white")
                                }
                            )}
                            onclick={on_write_anyway_click}
                        >
                            if props.saving {
                                <LoadingSpinner />
                                { "Writing changes…" }
                            } else {
                                { "Write anyway" }
                            }
                        </button>
                    </div>
                </div>
            </div>
        </div>
    }
}
