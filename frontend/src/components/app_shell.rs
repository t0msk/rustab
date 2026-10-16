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
//! command except the diff-confirm/save and restore flows below.
//! `original_document`/`original_text` keep the document exactly as loaded
//! (replaced wholesale only after a successful `save`/`restore_backup`, or
//! after a failed one that can't guarantee the target is unchanged — see
//! `on_diff_confirm`/`on_restore_confirm`), so `CandidateDocument`'s own
//! comparison helpers and `crate::diff` can build the diff view without any
//! further bookkeeping here.

use yew::prelude::*;

use crate::api;
use crate::candidate::CandidateDocument;
use crate::components::about_dialog::AboutDialog;
use crate::components::backup_delete_confirm_dialog::BackupDeleteConfirmDialog;
use crate::components::backup_diff_view::BackupDiffView;
use crate::components::backup_panel::{format_timestamps, BackupPanel};
use crate::components::backup_restore_confirm_dialog::BackupRestoreConfirmDialog;
use crate::components::delete_confirm_dialog::DeleteConfirmDialog;
use crate::components::diff_view::DiffView;
use crate::components::entries_table::{source_badge, EntriesTable, RowEntry};
use crate::components::entry_detail_drawer::{DrawerMode, DrawerSaveResult, EntryDetailDrawer};
use crate::components::header_bar::{HeaderBar, Tab};
use crate::components::pending_changes_bar::PendingChangesBar;
use crate::components::status_toast::{StatusToast, ToastKind};
use crate::components::target_banner::TargetBanner;
use crate::diff::compute_summary_diff;
use crate::dto::{BackupInfoDto, DocumentDto, LineDto, TargetInfoDto};

/// Reloads `original_document`/`original_text`/`candidate` from
/// `list_entries` — "reload after every save" (API_CONTRACT.md §11) — used
/// after a successful write and after a failed one that can't guarantee
/// the target is unchanged.
async fn reload_entries(
    candidate: UseStateHandle<CandidateDocument>,
    original_document: UseStateHandle<Option<DocumentDto>>,
    original_text: UseStateHandle<String>,
    list_error: UseStateHandle<Option<String>>,
) {
    match api::list_entries().await {
        Ok(view) => {
            candidate.set(CandidateDocument::from_document(&view.document));
            original_text.set(view.text);
            original_document.set(Some(view.document));
            list_error.set(None);
        }
        Err(message) => list_error.set(Some(message)),
    }
}

/// Reloads the backup list — called after mount and after every successful
/// write, since a save prunes old backups and adds a new one.
async fn reload_backups(
    backups: UseStateHandle<Vec<BackupInfoDto>>,
    backups_error: UseStateHandle<Option<String>>,
) {
    match api::list_backups().await {
        Ok(list) => {
            backups.set(list);
            backups_error.set(None);
        }
        Err(message) => backups_error.set(Some(message)),
    }
}

#[function_component(AppShell)]
pub fn app_shell() -> Html {
    let active_tab = use_state(|| Tab::Entries);

    let backups: UseStateHandle<Vec<BackupInfoDto>> = use_state(Vec::new);
    let backups_error: UseStateHandle<Option<String>> = use_state(|| None);

    // Live target info (§2), loaded from `get_target_info` on mount —
    // replaces T10's `MOCK_TARGET` constant.
    let target: UseStateHandle<Option<TargetInfoDto>> = use_state(|| None);
    let target_error: UseStateHandle<Option<String>> = use_state(|| None);

    // The document exactly as `list_entries` returned it (never mutated
    // after load except by a reload) and the in-memory candidate derived
    // from it (mutated by every staged add/edit/delete). See the module
    // doc comment. `original_text` is the document's raw text, needed by
    // the diff view's raw-text tab (`docs/API_CONTRACT.md` §5).
    let original_document: UseStateHandle<Option<DocumentDto>> = use_state(|| None);
    let original_text: UseStateHandle<String> = use_state(String::new);
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

    // Backup-delete flow state, a structural sibling of the restore flow
    // above (`docs/UI_DESIGN.md` §7).
    let backup_delete_target: UseStateHandle<Option<usize>> = use_state(|| None);
    let backup_delete_confirm_checked = use_state(|| false);
    let backup_delete_saving = use_state(|| false);
    let backup_delete_error: UseStateHandle<Option<String>> = use_state(|| None);

    // Backup-create flow state (T21.1): not tied to a row and has no
    // confirm step — the button itself is the whole flow, so there's no
    // target/confirm-checked pair to mirror from the delete flow above.
    let backup_create_saving = use_state(|| false);
    let backup_create_error: UseStateHandle<Option<String>> = use_state(|| None);

    // Open-backup-location flow state (T25): no loading/disabled state —
    // the call is a near-instant `spawn()`, not a slow operation — and no
    // success toast/reload either, since it doesn't change the backup list
    // or the target; only a failure needs to be surfaced, inline.
    let open_location_error: UseStateHandle<Option<String>> = use_state(|| None);

    // About-dialog flow state (T26): purely static/informational content,
    // no fetch, so a single open/closed flag is the whole of its state.
    let about_open: UseStateHandle<bool> = use_state(|| false);

    // View-diff flow state: which backup's diff overlay is open, its
    // fetched raw text (`None` = loading or not open), and the last
    // `read_backup` failure, if any (`docs/UI_DESIGN.md` §7).
    let view_diff_target: UseStateHandle<Option<usize>> = use_state(|| None);
    let view_diff_text: UseStateHandle<Option<String>> = use_state(|| None);
    let view_diff_error: UseStateHandle<Option<String>> = use_state(|| None);

    // `render_candidate`'s result for the currently open diff view's raw
    // tab, and the save/restore in-flight + error state for their
    // respective confirm buttons (`docs/UI_DESIGN.md` §6/§7).
    let rendered_text: UseStateHandle<Option<String>> = use_state(|| None);
    let diff_saving = use_state(|| false);
    let diff_error: UseStateHandle<Option<String>> = use_state(|| None);
    let restore_saving = use_state(|| false);
    let restore_error: UseStateHandle<Option<String>> = use_state(|| None);

    // Transient success notification (§6 point 4 / §7's "same ... success
    // ... pattern"); auto-dismissed a few seconds after it's shown. A
    // monotonically increasing request id guards against an older timeout
    // clearing a toast a newer one just set.
    let toast: UseStateHandle<Option<(ToastKind, String)>> = use_state(|| None);
    let toast_request = use_mut_ref(|| 0u64);

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
        let candidate = candidate.clone();
        let original_document = original_document.clone();
        let original_text = original_text.clone();
        let list_error = list_error.clone();
        use_effect_with((), move |_| {
            wasm_bindgen_futures::spawn_local(reload_entries(
                candidate,
                original_document,
                original_text,
                list_error,
            ));
            || ()
        });
    }
    {
        let backups = backups.clone();
        let backups_error = backups_error.clone();
        use_effect_with((), move |_| {
            wasm_bindgen_futures::spawn_local(reload_backups(backups, backups_error));
            || ()
        });
    }
    // Renders the exact "after" text for the raw-diff tab whenever the
    // diff view opens (API_CONTRACT.md §3.4) — the candidate can't change
    // while the modal is open (it's a blocking overlay), so this only
    // needs to run once per open, not per keystroke.
    {
        let rendered_text = rendered_text.clone();
        let candidate = candidate.clone();
        use_effect_with(*diff_view_open, move |open| {
            if *open {
                let document = candidate.to_document();
                let rendered_text = rendered_text.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    if let Ok(text) = api::render_candidate(&document).await {
                        rendered_text.set(Some(text));
                    }
                });
            } else {
                rendered_text.set(None);
            }
            || ()
        });
    }

    let show_toast = {
        let toast = toast.clone();
        let toast_request = toast_request.clone();
        Callback::from(move |(kind, message): (ToastKind, String)| {
            let request_id = {
                let mut counter = toast_request.borrow_mut();
                *counter += 1;
                *counter
            };
            toast.set(Some((kind, message)));
            let toast = toast.clone();
            let toast_request = toast_request.clone();
            wasm_bindgen_futures::spawn_local(async move {
                gloo_timers::future::TimeoutFuture::new(4000).await;
                if *toast_request.borrow() == request_id {
                    toast.set(None);
                }
            });
        })
    };

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
    // the diff-confirm flow below runs `save`.
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
        let diff_error = diff_error.clone();
        Callback::from(move |()| {
            diff_error.set(None);
            diff_view_open.set(true);
        })
    };
    let on_diff_cancel = {
        let diff_view_open = diff_view_open.clone();
        let diff_confirm_checked = diff_confirm_checked.clone();
        let diff_error = diff_error.clone();
        let diff_saving = diff_saving.clone();
        Callback::from(move |()| {
            // Not reachable while `*diff_saving` is true (the Cancel button
            // is disabled then — see the `DiffView` render below), but
            // guard anyway rather than ever closing the modal out from
            // under an in-flight save.
            if *diff_saving {
                return;
            }
            diff_view_open.set(false);
            diff_confirm_checked.set(false);
            diff_error.set(None);
        })
    };
    let on_diff_confirm = {
        let candidate = candidate.clone();
        let original_document = original_document.clone();
        let original_text = original_text.clone();
        let list_error = list_error.clone();
        let backups = backups.clone();
        let backups_error = backups_error.clone();
        let diff_view_open = diff_view_open.clone();
        let diff_confirm_checked = diff_confirm_checked.clone();
        let diff_saving = diff_saving.clone();
        let diff_error = diff_error.clone();
        let show_toast = show_toast.clone();
        Callback::from(move |()| {
            if *diff_saving {
                return;
            }
            let document = candidate.to_document();
            let candidate = candidate.clone();
            let original_document = original_document.clone();
            let original_text = original_text.clone();
            let list_error = list_error.clone();
            let backups = backups.clone();
            let backups_error = backups_error.clone();
            let diff_view_open = diff_view_open.clone();
            let diff_confirm_checked = diff_confirm_checked.clone();
            let diff_saving = diff_saving.clone();
            let diff_error = diff_error.clone();
            let show_toast = show_toast.clone();
            diff_saving.set(true);
            diff_error.set(None);
            wasm_bindgen_futures::spawn_local(async move {
                match api::save(&document).await {
                    Ok(_outcome) => {
                        reload_entries(candidate, original_document, original_text, list_error)
                            .await;
                        reload_backups(backups, backups_error).await;
                        diff_saving.set(false);
                        diff_confirm_checked.set(false);
                        diff_view_open.set(false);
                        show_toast.emit((ToastKind::Success, "Changes saved.".to_string()));
                    }
                    Err(write_error) => {
                        // API_CONTRACT.md §8: "target_unchanged is the
                        // field to branch on" — `true` keeps the user's
                        // pending changes in place and offers retry;
                        // `false` means the write's guarantee can't be
                        // made, so reload what's actually on disk instead.
                        let unchanged = write_error.target_unchanged();
                        diff_error.set(Some(write_error.message()));
                        if !unchanged {
                            reload_entries(candidate, original_document, original_text, list_error)
                                .await;
                        }
                        diff_saving.set(false);
                    }
                }
            });
        })
    };

    let on_restore = {
        let restore_target = restore_target.clone();
        let restore_error = restore_error.clone();
        Callback::from(move |idx: usize| {
            restore_error.set(None);
            restore_target.set(Some(idx));
        })
    };
    let on_restore_cancel = {
        let restore_target = restore_target.clone();
        let restore_confirm_checked = restore_confirm_checked.clone();
        let restore_error = restore_error.clone();
        let restore_saving = restore_saving.clone();
        Callback::from(move |()| {
            if *restore_saving {
                return;
            }
            restore_target.set(None);
            restore_confirm_checked.set(false);
            restore_error.set(None);
        })
    };
    let on_restore_confirm = {
        let restore_target = restore_target.clone();
        let restore_confirm_checked = restore_confirm_checked.clone();
        let restore_saving = restore_saving.clone();
        let restore_error = restore_error.clone();
        let backups = backups.clone();
        let backups_error = backups_error.clone();
        let candidate = candidate.clone();
        let original_document = original_document.clone();
        let original_text = original_text.clone();
        let list_error = list_error.clone();
        let show_toast = show_toast.clone();
        Callback::from(move |()| {
            if *restore_saving {
                return;
            }
            let Some(idx) = *restore_target else {
                return;
            };
            let Some(backup_path) = backups.get(idx).map(|b| b.path.clone()) else {
                return;
            };
            let restore_target = restore_target.clone();
            let restore_confirm_checked = restore_confirm_checked.clone();
            let restore_saving = restore_saving.clone();
            let restore_error = restore_error.clone();
            let backups = backups.clone();
            let backups_error = backups_error.clone();
            let candidate = candidate.clone();
            let original_document = original_document.clone();
            let original_text = original_text.clone();
            let list_error = list_error.clone();
            let show_toast = show_toast.clone();
            restore_saving.set(true);
            restore_error.set(None);
            wasm_bindgen_futures::spawn_local(async move {
                match api::restore_backup(&backup_path).await {
                    Ok(_outcome) => {
                        reload_entries(candidate, original_document, original_text, list_error)
                            .await;
                        reload_backups(backups, backups_error).await;
                        restore_saving.set(false);
                        restore_target.set(None);
                        restore_confirm_checked.set(false);
                        show_toast.emit((ToastKind::Success, "Backup restored.".to_string()));
                    }
                    Err(write_error) => {
                        let unchanged = write_error.target_unchanged();
                        restore_error.set(Some(write_error.message()));
                        if !unchanged {
                            reload_entries(candidate, original_document, original_text, list_error)
                                .await;
                        }
                        restore_saving.set(false);
                    }
                }
            });
        })
    };

    let on_backup_delete = {
        let backup_delete_target = backup_delete_target.clone();
        let backup_delete_error = backup_delete_error.clone();
        Callback::from(move |idx: usize| {
            backup_delete_error.set(None);
            backup_delete_target.set(Some(idx));
        })
    };
    let on_backup_delete_cancel = {
        let backup_delete_target = backup_delete_target.clone();
        let backup_delete_confirm_checked = backup_delete_confirm_checked.clone();
        let backup_delete_error = backup_delete_error.clone();
        let backup_delete_saving = backup_delete_saving.clone();
        Callback::from(move |()| {
            if *backup_delete_saving {
                return;
            }
            backup_delete_target.set(None);
            backup_delete_confirm_checked.set(false);
            backup_delete_error.set(None);
        })
    };
    let on_backup_delete_confirm = {
        let backup_delete_target = backup_delete_target.clone();
        let backup_delete_confirm_checked = backup_delete_confirm_checked.clone();
        let backup_delete_saving = backup_delete_saving.clone();
        let backup_delete_error = backup_delete_error.clone();
        let backups = backups.clone();
        let backups_error = backups_error.clone();
        let show_toast = show_toast.clone();
        Callback::from(move |()| {
            if *backup_delete_saving {
                return;
            }
            let Some(idx) = *backup_delete_target else {
                return;
            };
            let Some(backup_path) = backups.get(idx).map(|b| b.path.clone()) else {
                return;
            };
            let backup_delete_target = backup_delete_target.clone();
            let backup_delete_confirm_checked = backup_delete_confirm_checked.clone();
            let backup_delete_saving = backup_delete_saving.clone();
            let backup_delete_error = backup_delete_error.clone();
            let backups = backups.clone();
            let backups_error = backups_error.clone();
            let show_toast = show_toast.clone();
            backup_delete_saving.set(true);
            backup_delete_error.set(None);
            wasm_bindgen_futures::spawn_local(async move {
                match api::delete_backup(&backup_path).await {
                    Ok(()) => {
                        // A backup deletion never touches the live target,
                        // so `reload_entries` is deliberately not called
                        // here — only the backup list changed.
                        reload_backups(backups, backups_error).await;
                        backup_delete_saving.set(false);
                        backup_delete_target.set(None);
                        backup_delete_confirm_checked.set(false);
                        show_toast.emit((ToastKind::Success, "Backup deleted.".to_string()));
                    }
                    Err(delete_error) => {
                        // No `target_unchanged` to branch on here (§8): a
                        // `delete_backup` failure never touches the
                        // target, so `reload_entries` is never called on
                        // failure either.
                        backup_delete_error.set(Some(delete_error.message()));
                        backup_delete_saving.set(false);
                    }
                }
            });
        })
    };

    let on_create_backup = {
        let backup_create_saving = backup_create_saving.clone();
        let backup_create_error = backup_create_error.clone();
        let backups = backups.clone();
        let backups_error = backups_error.clone();
        let show_toast = show_toast.clone();
        Callback::from(move |()| {
            if *backup_create_saving {
                return;
            }
            let backup_create_saving = backup_create_saving.clone();
            let backup_create_error = backup_create_error.clone();
            let backups = backups.clone();
            let backups_error = backups_error.clone();
            let show_toast = show_toast.clone();
            backup_create_saving.set(true);
            backup_create_error.set(None);
            wasm_bindgen_futures::spawn_local(async move {
                match api::create_backup().await {
                    Ok(_backup) => {
                        // A backup creation never touches the live target,
                        // so `reload_entries` is deliberately not called
                        // here — only the backup list changed.
                        reload_backups(backups, backups_error).await;
                        backup_create_saving.set(false);
                        show_toast.emit((ToastKind::Success, "Backup created.".to_string()));
                    }
                    Err(create_error) => {
                        backup_create_error.set(Some(create_error.message()));
                        backup_create_saving.set(false);
                    }
                }
            });
        })
    };

    let on_open_location = {
        let open_location_error = open_location_error.clone();
        Callback::from(move |()| {
            let open_location_error = open_location_error.clone();
            open_location_error.set(None);
            wasm_bindgen_futures::spawn_local(async move {
                if let Err(message) = api::open_backup_location().await {
                    open_location_error.set(Some(message));
                }
            });
        })
    };

    let on_about_click = {
        let about_open = about_open.clone();
        Callback::from(move |()| about_open.set(true))
    };
    let on_about_close = {
        let about_open = about_open.clone();
        Callback::from(move |()| about_open.set(false))
    };

    let on_view_diff = {
        let view_diff_target = view_diff_target.clone();
        let view_diff_text = view_diff_text.clone();
        let view_diff_error = view_diff_error.clone();
        let backups = backups.clone();
        Callback::from(move |idx: usize| {
            view_diff_target.set(Some(idx));
            view_diff_text.set(None);
            view_diff_error.set(None);
            let Some(backup_path) = backups.get(idx).map(|b| b.path.clone()) else {
                return;
            };
            let view_diff_text = view_diff_text.clone();
            let view_diff_error = view_diff_error.clone();
            wasm_bindgen_futures::spawn_local(async move {
                match api::read_backup(&backup_path).await {
                    Ok(text) => view_diff_text.set(Some(text)),
                    Err(message) => view_diff_error.set(Some(message)),
                }
            });
        })
    };
    let on_view_diff_close = {
        let view_diff_target = view_diff_target.clone();
        let view_diff_text = view_diff_text.clone();
        let view_diff_error = view_diff_error.clone();
        Callback::from(move |()| {
            view_diff_target.set(None);
            view_diff_text.set(None);
            view_diff_error.set(None);
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

    let summary_diff = original_document
        .as_ref()
        .map(|doc| compute_summary_diff(doc, &candidate))
        .unwrap_or_default();
    let raw_diff = rendered_text
        .as_ref()
        .map(|new_text| crate::diff::compute_raw_diff(original_text.as_str(), new_text));

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
            let (relative, _iso) = format_timestamps(b.created_unix_secs);
            (
                AttrValue::from(relative),
                AttrValue::from(b.target_path.clone()),
            )
        })
    });

    let backup_delete_dialog_data: Option<AttrValue> = (*backup_delete_target).and_then(|idx| {
        backups.get(idx).map(|b| {
            let (relative, _iso) = format_timestamps(b.created_unix_secs);
            AttrValue::from(relative)
        })
    });

    let view_diff_dialog_data: Option<(AttrValue, AttrValue)> =
        (*view_diff_target).and_then(|idx| {
            backups.get(idx).map(|b| {
                let (relative, _iso) = format_timestamps(b.created_unix_secs);
                (
                    AttrValue::from(relative),
                    AttrValue::from(b.target_path.clone()),
                )
            })
        });
    let view_diff_lines: Option<Vec<crate::diff::RawDiffLine>> = (*view_diff_text)
        .as_ref()
        .map(|old| crate::diff::compute_raw_diff(old, original_text.as_str()));

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
            <HeaderBar
                active_tab={*active_tab}
                on_tab_change={on_tab_change}
                on_add_entry={on_add_entry.clone()}
                on_create_backup={on_create_backup.clone()}
                create_saving={*backup_create_saving}
                create_error={(*backup_create_error).clone().map(AttrValue::from)}
                on_open_location={on_open_location.clone()}
                open_location_error={(*open_location_error).clone().map(AttrValue::from)}
                on_about_click={on_about_click.clone()}
            />
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
                } else if let Some(err) = &*backups_error {
                    <div class="rounded-md border border-red-300 dark:border-red-800 bg-red-50 dark:bg-red-900/20 text-red-800 dark:text-red-300 px-4 py-3 text-sm" role="alert">
                        { format!("Could not load backups: {err}") }
                    </div>
                } else {
                    <BackupPanel
                        backups={(*backups).clone()}
                        on_restore={on_restore}
                        on_view_diff={on_view_diff}
                        on_delete={on_backup_delete}
                    />
                }
            </main>

            if let Some(mode) = (*drawer_mode).clone() {
                <EntryDetailDrawer mode={mode} other_lines={other_lines} on_close={on_drawer_close} on_save={on_drawer_save} show_toast={show_toast.clone()} />
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
                    summary={summary_diff}
                    raw_diff={raw_diff}
                    saving={*diff_saving}
                    error={(*diff_error).clone().map(AttrValue::from)}
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
                    saving={*restore_saving}
                    error={(*restore_error).clone().map(AttrValue::from)}
                />
            }

            if let Some(backup_timestamp) = backup_delete_dialog_data {
                <BackupDeleteConfirmDialog
                    backup_timestamp={backup_timestamp}
                    is_real={is_real}
                    confirm_checked={*backup_delete_confirm_checked}
                    on_confirm_checked_change={{
                        let backup_delete_confirm_checked = backup_delete_confirm_checked.clone();
                        Callback::from(move |checked: bool| backup_delete_confirm_checked.set(checked))
                    }}
                    on_cancel={on_backup_delete_cancel}
                    on_confirm={on_backup_delete_confirm}
                    saving={*backup_delete_saving}
                    error={(*backup_delete_error).clone().map(AttrValue::from)}
                />
            }

            if let Some((backup_timestamp, backup_target_path)) = view_diff_dialog_data {
                <BackupDiffView
                    backup_timestamp={backup_timestamp}
                    target_path={backup_target_path}
                    lines={view_diff_lines}
                    error={(*view_diff_error).clone().map(AttrValue::from)}
                    on_close={on_view_diff_close}
                />
            }

            if *about_open {
                <AboutDialog on_close={on_about_close} />
            }

            if let Some((kind, message)) = (*toast).clone() {
                <StatusToast kind={kind} message={AttrValue::from(message)} />
            }
        </div>
    }
}
