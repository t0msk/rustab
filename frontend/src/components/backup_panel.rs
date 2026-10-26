//! `BackupPanel` - backup list view (§7), rows are `BackupListItem`.
//! Renders from the real `list_backups` result; `AppShell` owns the
//! fetch, this component is purely presentational.
//!
//! **Fallback notice** (`docs/UI_DESIGN.md` §7): when
//! `TargetInfoDto.real_target_backup_fallback_dir` is `Some`, this panel
//! shows a persistent banner above the list (or empty state) rather than
//! only a tooltip, since the fact it states needs to be findable in a
//! rescue scenario, not just on first notice. The banner's wording is
//! built by [`fallback_notice_text`], kept separate from the `html!` so
//! it can be unit tested without a DOM.
//!
//! In fallback mode, the list returned by `list_backups` also includes the
//! real `/etc/fstab`'s own automatic backups from the host's `/etc/`, each
//! flagged `deletable == false` (`BackupInfoDto::deletable`): this sandboxed
//! install can restore one and show its diff, but cannot delete it, so
//! `BackupListItem` renders a non-interactive pill in place of the `Delete`
//! button for those rows.

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
            "Backups created here are kept in {dir} because this sandbox can't manage files \
             in /etc/; /etc/fstab's automatic backups are listed too and can be restored, but \
             not deleted, from here."
        )
    })
}

/// Tooltip for the pill `BackupListItem` shows in place of `Delete` when
/// `BackupInfoDto::deletable` is `false`. Kept as a plain function, same
/// reasoning as [`fallback_notice_text`], so the exact wording is unit
/// tested without a DOM.
fn non_deletable_pill_title() -> &'static str {
    "This backup sits beside the real /etc/fstab. This sandboxed install can restore it and \
     show its diff, but cannot delete it."
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
                if backup.deletable {
                    <button
                        type="button"
                        class="px-3 py-1.5 rounded-md text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-900/20 text-xs font-medium focus-visible:ring-2 focus-visible:ring-sky-500"
                        onclick={props.on_delete.clone()}
                    >
                        { "Delete" }
                    </button>
                } else {
                    <span
                        class="inline-block rounded-full bg-violet-100 dark:bg-violet-900/40 text-violet-700 dark:text-violet-300 text-xs font-medium px-2 py-0.5"
                        title={non_deletable_pill_title()}
                    >
                        { "Beside /etc/fstab" }
                    </span>
                }
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

        // (b) /etc/fstab's own automatic backups are listed too, and can be
        // restored but not deleted from here.
        assert!(notice.to_lowercase().contains("restored"));
        assert!(notice.to_lowercase().contains("not deleted"));

        // (c) exactly one sentence - not a bare period count, since `dir`
        // may itself contain a literal '.' (e.g. `.local`).
        assert!(notice.ends_with('.'));
        assert!(!notice.contains(". "));
    }

    #[test]
    fn a_different_fallback_dir_is_reflected_verbatim() {
        // Guards against the message hard-coding a path rather than using
        // the one it was given.
        let notice = fallback_notice_text(Some("/some/other/path")).unwrap();
        assert!(notice.contains("/some/other/path"));
    }

    #[test]
    fn non_deletable_pill_title_names_what_is_and_is_not_possible() {
        let title = non_deletable_pill_title();
        assert!(title.contains("/etc/fstab"));
        assert!(title.to_lowercase().contains("restore"));
        assert!(title.to_lowercase().contains("diff"));
        assert!(title.to_lowercase().contains("cannot delete"));
    }
}
