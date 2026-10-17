//! `RealTargetConfirmCheckbox` - the extra "I understand..." gate shown
//! before any write that reaches the real `/etc/fstab`, reused by
//! `DiffView` (§6) and `BackupRestoreConfirmDialog` (§7).

use web_sys::HtmlInputElement;
use yew::prelude::*;

/// Whether a write/restore confirm button should be disabled, per
/// `docs/UI_DESIGN.md` §6/§7: a write the user must explicitly acknowledge
/// requires the checkbox above to be checked, and any write is disabled
/// while another one is already in flight.
///
/// `requires_confirmation` is deliberately **not** "the active target is
/// literally `/etc/fstab`" (`TargetInfoDto::is_real`), which is what it
/// used to be before T34. Since T34 a save from a default, unconfigured
/// launch also writes the real `/etc/fstab` - through the elevated path,
/// mirroring into the local working draft afterwards - even though the
/// resolved target is that draft and `is_real` is therefore `false`
/// (`src-tauri/src/commands.rs::save_document_with`). The caller passes
/// "this action writes the real `/etc/fstab`" (`AppShell` derives it once,
/// as `is_real || source == SandboxDefault`), so the only launch that skips
/// this gate is the one that genuinely writes some other file: an explicit
/// non-real `RUSTAB_TARGET_PATH` override.
///
/// Both `DiffView` and `BackupRestoreConfirmDialog` call this rather than
/// each inlining their own copy of the gating rule, so the one safety gate
/// can't drift between the two call sites.
pub fn confirm_disabled(requires_confirmation: bool, confirm_checked: bool, saving: bool) -> bool {
    saving || (requires_confirmation && !confirm_checked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_reaching_the_real_file_requires_the_checkbox() {
        // Both the explicit `RUSTAB_TARGET_PATH=/etc/fstab` case and the
        // default launch (which writes `/etc/fstab` and mirrors into the
        // draft) arrive here as `requires_confirmation: true`.
        assert!(confirm_disabled(true, false, false));
        assert!(!confirm_disabled(true, true, false));
    }

    #[test]
    fn an_isolated_target_never_requires_the_checkbox() {
        // The one remaining case: an explicit non-real `RUSTAB_TARGET_PATH`
        // override, which writes only that file.
        assert!(!confirm_disabled(false, false, false));
        assert!(!confirm_disabled(false, true, false));
    }

    #[test]
    fn a_save_already_in_flight_always_disables_the_button() {
        assert!(confirm_disabled(false, true, true));
        assert!(confirm_disabled(true, true, true));
    }
}

#[derive(Properties, PartialEq)]
pub struct RealTargetConfirmCheckboxProps {
    pub checked: bool,
    pub on_change: Callback<bool>,
    /// Copy varies slightly between the diff-save flow and the restore
    /// flow, so the caller supplies the exact sentence (§6, §7).
    pub label: AttrValue,
}

#[function_component(RealTargetConfirmCheckbox)]
pub fn real_target_confirm_checkbox(props: &RealTargetConfirmCheckboxProps) -> Html {
    let on_change = {
        let cb = props.on_change.clone();
        Callback::from(move |e: Event| {
            let checked = e
                .target_dyn_into::<HtmlInputElement>()
                .is_some_and(|el| el.checked());
            cb.emit(checked);
        })
    };

    html! {
        <label class="flex items-start gap-2 text-sm text-slate-700 dark:text-slate-300 bg-red-50 dark:bg-red-900/20 border border-red-300 dark:border-red-800 rounded-md px-3 py-2">
            <input
                type="checkbox"
                class="mt-0.5 h-4 w-4 rounded border-slate-300 text-red-600 focus-visible:ring-2 focus-visible:ring-sky-500"
                checked={props.checked}
                onchange={on_change}
            />
            <span>{ props.label.clone() }</span>
        </label>
    }
}
