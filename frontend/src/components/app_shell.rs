//! `AppShell` — top-level layout: `TargetBanner` + `HeaderBar` + tab content
//! area (§1). Owns all in-memory view-state for this task (which tab is
//! active, which overlay is open) using Yew `use_state` — no router, per
//! `docs/UI_DESIGN.md` §1.

use yew::prelude::*;

use crate::components::backup_panel::BackupPanel;
use crate::components::backup_restore_confirm_dialog::BackupRestoreConfirmDialog;
use crate::components::delete_confirm_dialog::DeleteConfirmDialog;
use crate::components::diff_view::DiffView;
use crate::components::entries_table::EntriesTable;
use crate::components::entry_detail_drawer::{DrawerMode, EntryDetailDrawer};
use crate::components::header_bar::{HeaderBar, Tab};
use crate::components::pending_changes_bar::PendingChangesBar;
use crate::components::target_banner::TargetBanner;
use crate::mock::{mock_backups, mock_entries, MockEntry, MOCK_TARGET};

#[function_component(AppShell)]
pub fn app_shell() -> Html {
    let active_tab = use_state(|| Tab::Entries);
    let entries = use_state(mock_entries);
    let backups = use_state(mock_backups);

    // Mock-only: defaults to 0 purely to prove PendingChangesBar's
    // show/hide logic. Real pending-change tracking is T11's job.
    let pending_count = use_state(|| 0usize);

    let drawer_mode: UseStateHandle<Option<DrawerMode>> = use_state(|| None);
    let delete_target: UseStateHandle<Option<usize>> = use_state(|| None);
    let restore_target: UseStateHandle<Option<usize>> = use_state(|| None);
    let diff_view_open = use_state(|| false);
    let diff_confirm_checked = use_state(|| false);
    let restore_confirm_checked = use_state(|| false);

    let on_tab_change = {
        let active_tab = active_tab.clone();
        Callback::from(move |tab: Tab| active_tab.set(tab))
    };

    let on_add_entry = {
        let drawer_mode = drawer_mode.clone();
        Callback::from(move |()| drawer_mode.set(Some(DrawerMode::Add)))
    };

    let on_edit_entry = {
        let drawer_mode = drawer_mode.clone();
        let entries = entries.clone();
        Callback::from(move |idx: usize| {
            if let Some(entry) = entries.get(idx) {
                let mode = match entry {
                    MockEntry::Parsed(_) => DrawerMode::EditStructured,
                    MockEntry::Unparsed { raw_line } => {
                        DrawerMode::EditRaw(AttrValue::from(*raw_line))
                    }
                };
                drawer_mode.set(Some(mode));
            }
        })
    };

    let on_delete_entry = {
        let delete_target = delete_target.clone();
        Callback::from(move |idx: usize| delete_target.set(Some(idx)))
    };

    let on_drawer_close = {
        let drawer_mode = drawer_mode.clone();
        Callback::from(move |()| drawer_mode.set(None))
    };

    // Stub save: no real diffing/persistence happens here (T11). Bumping
    // the mock pending count just gives PendingChangesBar/DiffView a
    // reachable trigger to demonstrate their show/hide and modal chrome.
    let on_drawer_save = {
        let drawer_mode = drawer_mode.clone();
        let pending_count = pending_count.clone();
        Callback::from(move |()| {
            pending_count.set(*pending_count + 1);
            drawer_mode.set(None);
        })
    };

    let on_delete_cancel = {
        let delete_target = delete_target.clone();
        Callback::from(move |()| delete_target.set(None))
    };
    let on_delete_confirm = {
        let delete_target = delete_target.clone();
        let pending_count = pending_count.clone();
        Callback::from(move |()| {
            pending_count.set(*pending_count + 1);
            delete_target.set(None);
        })
    };

    let on_review = {
        let diff_view_open = diff_view_open.clone();
        Callback::from(move |()| diff_view_open.set(true))
    };
    let on_diff_cancel = {
        let diff_view_open = diff_view_open.clone();
        Callback::from(move |()| diff_view_open.set(false))
    };
    let on_diff_confirm = {
        let diff_view_open = diff_view_open.clone();
        let pending_count = pending_count.clone();
        let diff_confirm_checked = diff_confirm_checked.clone();
        Callback::from(move |()| {
            // Stub only: no real write happens in T10 (T12's job). Closing
            // the modal and clearing the mock count proves the confirm ->
            // close -> bar-hides round trip.
            diff_view_open.set(false);
            diff_confirm_checked.set(false);
            pending_count.set(0);
        })
    };

    let on_restore = {
        let restore_target = restore_target.clone();
        Callback::from(move |idx: usize| restore_target.set(Some(idx)))
    };
    let on_restore_cancel = {
        let restore_target = restore_target.clone();
        let restore_confirm_checked = restore_confirm_checked.clone();
        Callback::from(move |()| {
            restore_target.set(None);
            restore_confirm_checked.set(false);
        })
    };
    let on_restore_confirm = {
        let restore_target = restore_target.clone();
        let restore_confirm_checked = restore_confirm_checked.clone();
        Callback::from(move |()| {
            // Stub only: no real restore happens in T10 (T12's job).
            restore_target.set(None);
            restore_confirm_checked.set(false);
        })
    };

    let delete_dialog_data: Option<(AttrValue, AttrValue, AttrValue)> =
        (*delete_target).and_then(|idx| {
            entries.get(idx).map(|entry| match entry {
                MockEntry::Parsed(e) => (
                    AttrValue::from(format!("{} {}", e.source_type, e.source_value)),
                    AttrValue::from(e.mount_point),
                    AttrValue::from(e.fs_type),
                ),
                MockEntry::Unparsed { raw_line } => (
                    AttrValue::from("(unparsed line)"),
                    AttrValue::from(*raw_line),
                    AttrValue::from("-"),
                ),
            })
        });

    let restore_dialog_data: Option<(AttrValue, AttrValue)> = (*restore_target).and_then(|idx| {
        backups.get(idx).map(|b| {
            (
                AttrValue::from(b.iso_timestamp),
                AttrValue::from(b.target_path),
            )
        })
    });

    html! {
        <div class="h-screen w-screen flex flex-col bg-slate-50 dark:bg-slate-900 text-slate-900 dark:text-slate-100">
            <TargetBanner path={AttrValue::from(MOCK_TARGET.path)} is_real={MOCK_TARGET.is_real} />
            <HeaderBar active_tab={*active_tab} on_tab_change={on_tab_change} on_add_entry={on_add_entry.clone()} />
            <PendingChangesBar count={*pending_count} on_review={on_review} />

            <main class="flex-1 overflow-y-auto px-6 py-6">
                if *active_tab == Tab::Entries {
                    <EntriesTable
                        entries={(*entries).clone()}
                        on_edit={on_edit_entry}
                        on_delete={on_delete_entry}
                        on_add_entry={on_add_entry}
                    />
                } else {
                    <BackupPanel backups={(*backups).clone()} on_restore={on_restore} />
                }
            </main>

            if let Some(mode) = (*drawer_mode).clone() {
                <EntryDetailDrawer mode={mode} on_close={on_drawer_close} on_save={on_drawer_save} />
            }

            if let Some((source_summary, mount_point, fs_type)) = delete_dialog_data {
                <DeleteConfirmDialog
                    source_summary={source_summary}
                    mount_point={mount_point}
                    fs_type={fs_type}
                    on_cancel={on_delete_cancel}
                    on_confirm={on_delete_confirm}
                />
            }

            if *diff_view_open {
                <DiffView
                    is_real={MOCK_TARGET.is_real}
                    target_path={AttrValue::from(MOCK_TARGET.path)}
                    confirm_checked={*diff_confirm_checked}
                    on_confirm_checked_change={{
                        let diff_confirm_checked = diff_confirm_checked.clone();
                        Callback::from(move |checked: bool| diff_confirm_checked.set(checked))
                    }}
                    on_cancel={on_diff_cancel}
                    on_confirm={on_diff_confirm}
                />
            }

            if let Some((backup_timestamp, target_path)) = restore_dialog_data {
                <BackupRestoreConfirmDialog
                    backup_timestamp={backup_timestamp}
                    target_path={target_path}
                    is_real={MOCK_TARGET.is_real}
                    confirm_checked={*restore_confirm_checked}
                    on_confirm_checked_change={{
                        let restore_confirm_checked = restore_confirm_checked.clone();
                        Callback::from(move |checked: bool| restore_confirm_checked.set(checked))
                    }}
                    on_cancel={on_restore_cancel}
                    on_confirm={on_restore_confirm}
                />
            }
        </div>
    }
}
