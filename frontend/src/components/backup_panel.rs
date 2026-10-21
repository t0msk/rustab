//! `BackupPanel` - backup list view (§7), rows are `BackupListItem`.
//! Renders from the real `list_backups` result (T12); `AppShell` owns the
//! fetch, this component is purely presentational.
//!
//! **Fallback notice (T39, `docs/UI_DESIGN.md` §7):** when
//! `TargetInfoDto.real_target_backup_fallback_dir` is `Some`, this panel
//! shows a persistent banner above the list (or empty state) rather than
//! only in a tooltip, because the fact it states is something a user needs
//! to be able to find *in a rescue scenario*, not just on first notice. The
//! banner's exact wording is built by [`fallback_notice_text`], a plain
//! function kept separate from the `html!` so its content can be unit
//! tested without a DOM.
//!
//! T40 note: the banner's second fact - that each save's automatic backup is
//! still written beside the real `/etc/fstab` - was in doubt for exactly as
//! long as it took to find out *why* a sandboxed save could not reach that
//! file (the elevated helper was being started with a bare `pkexec`, which
//! changes a process's UID but not its mount namespace, so it stayed inside
//! the sandbox). The backend now starts it with `flatpak-spawn --host pkexec`,
//! which makes it a real host root process, so the statement is true and
//! verified rather than assumed. The wording is unchanged apart from saying
//! "can't list the contents of /etc/" instead of "can't manage files in
//! /etc/", which is the accurate description of what a sandboxed session
//! cannot do: it *can* have `/etc/fstab` written for it, it just cannot
//! enumerate the directory.

use wasm_bindgen::JsValue;
use yew::prelude::*;

use crate::components::empty_state::EmptyState;
use crate::dto::BackupInfoDto;

/// The fallback-notice banner's text, or `None` when there is nothing to
/// show (`fallback_dir` is `None` - the ordinary, colocated case). Kept as
/// a plain function, separate from `BackupPanel`'s `html!`, so its content
/// is unit-testable without rendering a DOM (`docs/API_CONTRACT.md` §6
/// documents the two facts this must state).
pub fn fallback_notice_text(fallback_dir: Option<&str>) -> Option<String> {
    fallback_dir.map(|dir| {
        format!(
            "Backups you create here are being kept in {dir}, not beside the real \
             /etc/fstab, because this sandboxed install can't list the contents of \
             /etc/. Automatic backups taken during ordinary \
             saves are still written beside the real /etc/fstab and are not shown in this \
             list, but they are not lost: they're recoverable from a rescue environment \
             (e.g. booting a live/rescue disk), just not visible or manageable from inside \
             this app."
        )
    })
}

/// Formats a backup's `created_unix_secs` into `(relative, iso)` strings
/// for `BackupListItem`'s display text + hover tooltip
/// (`docs/API_CONTRACT.md` §3.6: "format the relative and ISO timestamps
/// from it in the frontend"). Uses `js_sys::Date` rather than pulling in a
/// date/time crate, since the wasm target already has it for free.
pub fn format_timestamps(created_unix_secs: Option<i64>) -> (String, String) {
    let Some(secs) = created_unix_secs else {
        return ("unknown time".to_string(), "unknown".to_string());
    };
    let date = js_sys::Date::new(&JsValue::from_f64(secs as f64 * 1000.0));
    let iso = date
        .to_iso_string()
        .as_string()
        .unwrap_or_else(|| "unknown".to_string());
    let diff_secs = ((js_sys::Date::now() / 1000.0) - secs as f64).max(0.0) as i64;
    (relative_time(diff_secs), iso)
}

fn relative_time(diff_secs: i64) -> String {
    if diff_secs < 60 {
        "just now".to_string()
    } else if diff_secs < 3600 {
        let minutes = diff_secs / 60;
        format!(
            "{minutes} minute{} ago",
            if minutes == 1 { "" } else { "s" }
        )
    } else if diff_secs < 86_400 {
        let hours = diff_secs / 3600;
        format!("{hours} hour{} ago", if hours == 1 { "" } else { "s" })
    } else {
        let days = diff_secs / 86_400;
        format!("{days} day{} ago", if days == 1 { "" } else { "s" })
    }
}

#[derive(Properties, PartialEq)]
pub struct BackupPanelProps {
    pub backups: Vec<BackupInfoDto>,
    pub on_restore: Callback<usize>,
    pub on_view_diff: Callback<usize>,
    pub on_delete: Callback<usize>,
    /// `TargetInfoDto.real_target_backup_fallback_dir` (API_CONTRACT.md §6),
    /// threaded down unchanged. `Some(dir)` shows the fallback notice above
    /// the list/empty-state regardless of whether `backups` is empty -
    /// the fact applies to the whole panel, not to any particular row.
    pub backup_fallback_dir: Option<AttrValue>,
}

#[function_component(BackupPanel)]
pub fn backup_panel(props: &BackupPanelProps) -> Html {
    let notice = fallback_notice_text(props.backup_fallback_dir.as_deref());

    html! {
        <div class="space-y-4">
            if let Some(notice) = notice {
                <div
                    class="flex items-start gap-2 rounded-md border border-amber-300 dark:border-amber-800 bg-amber-50 dark:bg-amber-900/20 text-amber-900 dark:text-amber-200 px-4 py-3 text-sm"
                    role="status"
                >
                    <p>{ notice }</p>
                </div>
            }
            <div class="bg-white dark:bg-slate-800 rounded-lg shadow-sm border border-slate-200 dark:border-slate-700 overflow-hidden">
                if props.backups.is_empty() {
                    <EmptyState
                        icon="🗄"
                        title="No backups yet"
                        description={Some(AttrValue::from(
                            "No backups yet - one will be created automatically the first time you save."
                        ))}
                    />
                } else {
                    <ul class="divide-y divide-slate-200 dark:divide-slate-700">
                        { for props.backups.iter().enumerate().map(|(idx, backup)| {
                            let on_restore = {
                                let cb = props.on_restore.clone();
                                Callback::from(move |_: MouseEvent| cb.emit(idx))
                            };
                            let on_view_diff = {
                                let cb = props.on_view_diff.clone();
                                Callback::from(move |_: MouseEvent| cb.emit(idx))
                            };
                            let on_delete = {
                                let cb = props.on_delete.clone();
                                Callback::from(move |_: MouseEvent| cb.emit(idx))
                            };
                            html! {
                                <BackupListItem
                                    key={idx}
                                    backup={backup.clone()}
                                    on_restore={on_restore}
                                    on_view_diff={on_view_diff}
                                    on_delete={on_delete}
                                />
                            }
                        }) }
                    </ul>
                }
            </div>
        </div>
    }
}

#[derive(Properties, PartialEq)]
pub struct BackupListItemProps {
    pub backup: BackupInfoDto,
    pub on_restore: Callback<MouseEvent>,
    pub on_view_diff: Callback<MouseEvent>,
    pub on_delete: Callback<MouseEvent>,
}

#[function_component(BackupListItem)]
pub fn backup_list_item(props: &BackupListItemProps) -> Html {
    let backup = &props.backup;
    let (relative, iso) = format_timestamps(backup.created_unix_secs);
    let size_kb = format!("{:.1} KB", backup.size_bytes as f64 / 1024.0);

    html! {
        <li class="flex items-center justify-between px-4 py-3 hover:bg-sky-50 dark:hover:bg-slate-700/60">
            <div>
                <p class="text-sm text-slate-900 dark:text-slate-100" title={iso}>
                    { relative }
                    if backup.is_manual {
                        <span class="inline-block rounded-full bg-sky-100 dark:bg-sky-900/40 text-sky-700 dark:text-sky-300 text-xs font-medium px-2 py-0.5 ml-2">
                            { "Manual" }
                        </span>
                    } else {
                        <span class="inline-block rounded-full bg-slate-100 dark:bg-slate-700 text-slate-600 dark:text-slate-300 text-xs font-medium px-2 py-0.5 ml-2">
                            { "Automatic" }
                        </span>
                    }
                </p>
                <p class="text-xs text-slate-500 dark:text-slate-400 font-mono break-all">
                    { "Target: " }{ backup.target_path.clone() }{ " · " }{ size_kb }
                </p>
                <p class="text-xs text-slate-500 dark:text-slate-400 font-mono break-all">
                    { "Location: " }{ backup.path.clone() }
                </p>
            </div>
            <div class="flex items-center gap-2">
                <button
                    type="button"
                    class="px-3 py-1.5 rounded-md bg-slate-100 hover:bg-slate-200 dark:bg-slate-700 dark:hover:bg-slate-600 text-slate-700 dark:text-slate-200 text-xs font-medium focus-visible:ring-2 focus-visible:ring-sky-500"
                    onclick={props.on_restore.clone()}
                >
                    { "Restore" }
                </button>
                <button
                    type="button"
                    class="px-3 py-1.5 rounded-md bg-slate-100 hover:bg-slate-200 dark:bg-slate-700 dark:hover:bg-slate-600 text-slate-700 dark:text-slate-200 text-xs font-medium focus-visible:ring-2 focus-visible:ring-sky-500"
                    onclick={props.on_view_diff.clone()}
                >
                    { "View diff" }
                </button>
                <button
                    type="button"
                    class="px-3 py-1.5 rounded-md text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-900/20 text-xs font-medium focus-visible:ring-2 focus-visible:ring-sky-500"
                    onclick={props.on_delete.clone()}
                >
                    { "Delete" }
                </button>
            </div>
        </li>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_fallback_dir_means_no_notice() {
        assert_eq!(fallback_notice_text(None), None);
    }

    #[test]
    fn a_fallback_dir_produces_a_notice_stating_both_required_facts() {
        let dir = "/home/u/.local/share/rustab/etc-fstab-backups";
        let notice = fallback_notice_text(Some(dir)).expect("Some(dir) must produce a notice");

        // (a) backups created from this panel land in the given directory,
        // not beside /etc/fstab.
        assert!(notice.contains(dir));
        assert!(notice.contains("/etc/fstab"));

        // (b) automatic backups from ordinary saves are still beside the
        // real file, not shown here, not lost, recoverable from a rescue
        // environment.
        assert!(notice.contains("not shown in this list"));
        assert!(notice.to_lowercase().contains("not lost"));
        assert!(notice.to_lowercase().contains("rescue"));
    }

    #[test]
    fn a_different_fallback_dir_is_reflected_verbatim() {
        // Guards against the message hard-coding a path rather than using
        // the one it was given.
        let notice = fallback_notice_text(Some("/some/other/path")).unwrap();
        assert!(notice.contains("/some/other/path"));
    }
}
