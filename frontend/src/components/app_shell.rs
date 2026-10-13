//! `AppShell` — top-level layout: `TargetBanner` + `HeaderBar` + tab content
//! area (§1). Owns all in-memory view-state for this task (which tab is
//! active, which overlay is open) using Yew `use_state` — no router, per
//! `docs/UI_DESIGN.md` §1.
//!
//! **State design (T11, load-bearing for T12):** `AppShell` owns the one
//! `CandidateDocument` for the session (see `crate::candidate`), seeded
//! from `list_entries` on mount. `EntriesTable` renders it, deletes/edits
//! mutate it directly, and `EntryDetailDrawer` stages adds/edits into it
//! via `DrawerSaveResult` — nothing below `AppShell` ever calls a write
//! command. `original_document` keeps the document exactly as loaded (never
//! mutated after load, only replaced wholesale after a future `save` —
//! T12), so `CandidateDocument::pending_count` can compare "what's staged"
//! against "what's actually on disk" without any diffing logic living in
//! this file. T12's diff view is expected to use the same two pieces of
//! state (`original_document`, `candidate`) to build its Added/Modified/
//! Removed grouping and its `render_candidate` call — no rewrite of this
//! state shape should be needed.

use yew::prelude::*;

use crate::api;
use crate::candidate::CandidateDocument;
use crate::components::backup_panel::BackupPanel;
use crate::components::backup_restore_confirm_dialog::BackupRestoreConfirmDialog;
use crate::components::delete_confirm_dialog::DeleteConfirmDialog;
use crate::components::diff_view::DiffView;
use crate::components::entries_table::{source_badge, EntriesTable, RowEntry};
use crate::components::entry_detail_drawer::{DrawerMode, DrawerSaveResult, EntryDetailDrawer};
use crate::components::header_bar::{HeaderBar, Tab};
use crate::components::pending_changes_bar::PendingChangesBar;
use crate::components::target_banner::TargetBanner;
use crate::dto::{DocumentDto, LineDto, TargetInfoDto};
use crate::mock::mock_backups;

#[function_component(AppShell)]
pub fn app_shell() -> Html {
    let active_tab = use_state(|| Tab::Entries);
    let backups = use_state(mock_backups);

    // Live target info (§2), loaded from `get_target_info` on mount —
    // replaces T10's `MOCK_TARGET` constant.
    let target: UseStateHandle<Option<TargetInfoDto>> = use_state(|| None);
    let target_error: UseStateHandle<Option<String>> = use_state(|| None);

    // The document exactly as `list_entries` returned it (never mutated
    // after load) and the in-memory candidate derived from it (mutated by
    // every staged add/edit/delete). See the module doc comment.
    let original_document: UseStateHandle<Option<DocumentDto>> = use_state(|| None);
    let candidate = use_state(CandidateDocument::default);
    let list_error: UseStateHandle<Option<String>> = use_state(|| None);

    let drawer_mode: UseStateHandle<Option<DrawerMode>> = use_state(|| None);
    // The candidate-document index of the line being edited (`None` means
    // the drawer is in Add mode, appending rather than replacing).
    let editing_index: UseStateHandle<Option<usize>> = use_state(|| None);
    let delete_target: UseStateHandle<Option<usize>> = use_state(|| None);
    let restore_target: UseStateHandle<Option<usize>> = use_state(|| None);
    let diff_view_open = use_state(|| false);
    let diff_confirm_checked = use_state(|| false);
    let restore_confirm_checked = use_state(|| false);

    {
        let target = target.clone();
        let target_error = target_error.clone();
        use_effect_with((), move |_| {
            wasm_bindgen_futures::spawn_local(async move {
                match api::get_target_info().await {
                    Ok(info) => target.set(Some(info)),
                    Err(message) => target_error.set(Some(message)),
                }
            });
            || ()
        });
    }
    {
        let original_document = original_document.clone();
        let candidate = candidate.clone();
        let list_error = list_error.clone();
        use_effect_with((), move |_| {
            wasm_bindgen_futures::spawn_local(async move {
                match api::list_entries().await {
                    Ok(view) => {
                        candidate.set(CandidateDocument::from_document(&view.document));
                        original_document.set(Some(view.document));
                    }
                    Err(message) => list_error.set(Some(message)),
                }
            });
            || ()
        });
    }

    let on_tab_change = {
        let active_tab = active_tab.clone();
        Callback::from(move |tab: Tab| active_tab.set(tab))
    };

    let on_add_entry = {
        let drawer_mode = drawer_mode.clone();
        let editing_index = editing_index.clone();
        Callback::from(move |()| {
            editing_index.set(None);
            drawer_mode.set(Some(DrawerMode::Add));
        })
    };

    let on_edit_entry = {
        let drawer_mode = drawer_mode.clone();
        let editing_index = editing_index.clone();
        let candidate = candidate.clone();
        Callback::from(move |candidate_index: usize| {
            if let Some(candidate_line) = candidate.lines.get(candidate_index) {
                let mode = match &candidate_line.line {
                    LineDto::Entry { entry } => Some(DrawerMode::EditStructured(entry.clone())),
                    LineDto::Malformed { raw } => {
                        Some(DrawerMode::EditRaw(AttrValue::from(raw.clone())))
                    }
                    LineDto::Comment { .. } | LineDto::Blank => None,
                };
                if let Some(mode) = mode {
                    editing_index.set(Some(candidate_index));
                    drawer_mode.set(Some(mode));
                }
            }
        })
    };

    let on_delete_entry = {
        let delete_target = delete_target.clone();
        Callback::from(move |candidate_index: usize| delete_target.set(Some(candidate_index)))
    };

    let on_drawer_close = {
        let drawer_mode = drawer_mode.clone();
        let editing_index = editing_index.clone();
        Callback::from(move |()| {
            drawer_mode.set(None);
            editing_index.set(None);
        })
    };

    // Stages the drawer's result into the in-memory candidate document.
    // Never calls a write command — the actual file is untouched until
    // T12's diff-confirm flow runs `save`.
    let on_drawer_save = {
        let drawer_mode = drawer_mode.clone();
        let editing_index = editing_index.clone();
        let candidate = candidate.clone();
        Callback::from(move |result: DrawerSaveResult| {
            let mut next = (*candidate).clone();
            match result {
                DrawerSaveResult::Entry(entry) => match *editing_index {
                    Some(idx) => next.stage_edit_entry(idx, entry),
                    None => next.stage_add(entry),
                },
                DrawerSaveResult::Raw(raw) => {
                    if let Some(idx) = *editing_index {
                        next.stage_edit_raw(idx, raw);
                    }
                }
            }
            candidate.set(next);
            drawer_mode.set(None);
            editing_index.set(None);
        })
    };

    let on_delete_cancel = {
        let delete_target = delete_target.clone();
        Callback::from(move |()| delete_target.set(None))
    };
    let on_delete_confirm = {
        let delete_target = delete_target.clone();
        let candidate = candidate.clone();
        Callback::from(move |()| {
            if let Some(idx) = *delete_target {
                let mut next = (*candidate).clone();
                next.stage_delete(idx);
                candidate.set(next);
            }
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
        // Stub only: building the real diff and calling `save` is T12's
        // job (`docs/UI_DESIGN.md` §6's "no save action that skips the
        // diff view" rule means this still must not short-circuit into a
        // write). This only closes the modal; staged changes remain
        // staged, matching "Cancel" — proven here as the show/hide round
        // trip, same as T10.
        let diff_view_open = diff_view_open.clone();
        Callback::from(move |()| diff_view_open.set(false))
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
        // Stub only: no real restore happens in T11 (T12's job).
        let restore_target = restore_target.clone();
        let restore_confirm_checked = restore_confirm_checked.clone();
        Callback::from(move |()| {
            restore_target.set(None);
            restore_confirm_checked.set(false);
        })
    };

    let rows: Vec<(usize, RowEntry)> = candidate
        .rows()
        .into_iter()
        .filter_map(|(idx, candidate_line)| match &candidate_line.line {
            LineDto::Entry { entry } => Some((idx, RowEntry::Parsed(entry.clone()))),
            LineDto::Malformed { raw } => Some((idx, RowEntry::Unparsed(raw.clone()))),
            LineDto::Comment { .. } | LineDto::Blank => None,
        })
        .collect();

    let other_lines = candidate.lines_excluding(*editing_index);

    let pending_count = original_document
        .as_ref()
        .map(|doc| candidate.pending_count(doc))
        .unwrap_or(0);

    let delete_dialog_data: Option<(AttrValue, AttrValue, AttrValue)> =
        (*delete_target).and_then(|idx| {
            candidate
                .lines
                .get(idx)
                .map(|candidate_line| match &candidate_line.line {
                    LineDto::Entry { entry } => (
                        AttrValue::from(format!(
                            "{} {}",
                            source_badge(entry.source.kind),
                            entry.source.value
                        )),
                        AttrValue::from(entry.mount_point.clone()),
                        AttrValue::from(entry.fs_type.clone()),
                    ),
                    LineDto::Malformed { raw } => (
                        AttrValue::from("(unparsed line)"),
                        AttrValue::from(raw.clone()),
                        AttrValue::from("-"),
                    ),
                    LineDto::Comment { .. } | LineDto::Blank => (
                        AttrValue::from(""),
                        AttrValue::from(""),
                        AttrValue::from(""),
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

    let is_real = target.as_ref().map(|t| t.is_real).unwrap_or(false);
    let target_path = target.as_ref().map(|t| t.path.clone()).unwrap_or_default();

    html! {
        <div class="h-screen w-screen flex flex-col bg-slate-50 dark:bg-slate-900 text-slate-900 dark:text-slate-100">
            if let Some(info) = &*target {
                <TargetBanner path={AttrValue::from(info.path.clone())} is_real={info.is_real} />
            } else if let Some(err) = &*target_error {
                <div class="w-full h-10 shrink-0 flex items-center px-6 bg-amber-600 text-white text-sm font-semibold" role="alert">
                    { format!("Could not determine the active target: {err}") }
                </div>
            } else {
                <div class="w-full h-10 shrink-0 flex items-center px-6 bg-slate-600 text-white text-sm" role="status">
                    { "Loading target…" }
                </div>
            }
            <HeaderBar active_tab={*active_tab} on_tab_change={on_tab_change} on_add_entry={on_add_entry.clone()} />
            <PendingChangesBar count={pending_count} on_review={on_review} />

            <main class="flex-1 overflow-y-auto px-6 py-6">
                if *active_tab == Tab::Entries {
                    if let Some(err) = &*list_error {
                        <div class="rounded-md border border-red-300 dark:border-red-800 bg-red-50 dark:bg-red-900/20 text-red-800 dark:text-red-300 px-4 py-3 text-sm" role="alert">
                            { format!("Could not load entries: {err}") }
                        </div>
                    } else {
                        <EntriesTable
                            rows={rows}
                            on_edit={on_edit_entry}
                            on_delete={on_delete_entry}
                            on_add_entry={on_add_entry}
                        />
                    }
                } else {
                    <BackupPanel backups={(*backups).clone()} on_restore={on_restore} />
                }
            </main>

            if let Some(mode) = (*drawer_mode).clone() {
                <EntryDetailDrawer mode={mode} other_lines={other_lines} on_close={on_drawer_close} on_save={on_drawer_save} />
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
                    is_real={is_real}
                    target_path={AttrValue::from(target_path.clone())}
                    confirm_checked={*diff_confirm_checked}
                    on_confirm_checked_change={{
                        let diff_confirm_checked = diff_confirm_checked.clone();
                        Callback::from(move |checked: bool| diff_confirm_checked.set(checked))
                    }}
                    on_cancel={on_diff_cancel}
                    on_confirm={on_diff_confirm}
                />
            }

            if let Some((backup_timestamp, backup_target_path)) = restore_dialog_data {
                <BackupRestoreConfirmDialog
                    backup_timestamp={backup_timestamp}
                    target_path={backup_target_path}
                    is_real={is_real}
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
