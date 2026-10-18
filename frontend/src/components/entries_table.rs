//! `EntriesTable` - main entry list (§3), rows are `EntryRow` (parsed) or
//! `UnparsedEntryRow` (malformed). Renders from the app's in-memory
//! candidate document (see `crate::candidate`) rather than mock data (T11).
//!
//! **Row expansion (T32):** clicking anywhere on an `EntryRow`'s own `<tr>`
//! toggles an inline detail panel showing every one of that entry's fields,
//! untruncated - the deliberate replacement for the fact that there was
//! previously no way to see an entry's description without opening its
//! drawer. Expansion state is a `HashSet<usize>` of expanded
//! `candidate_index` values owned by `EntriesTable` itself, so any number of
//! rows can be open at once (not an accordion). `UnparsedEntryRow` is not
//! affected - it already shows its full raw text untruncated.
//!
//! **Reordering (T40, `docs/UI_DESIGN.md` §13):** every row carries Move
//! up/Move down buttons that emit `on_move((from, to))` with two
//! *candidate-document* indices. This component resolves `to` itself from
//! the adjacent entry in its own `rows` list, so exactly one place decides
//! adjacency (§13.4), and it mirrors the swap onto its own `expanded` set
//! at the same time, because that set is keyed by position, not by entry
//! identity (§13.8). Freestanding comments/blank lines never move with an
//! entry; where one sits between two rows, a gap divider says so up front
//! (§13.5).

use std::collections::HashSet;

use yew::prelude::*;

use crate::api;
use crate::components::empty_state::EmptyState;
use crate::components::icons::{
    CheckIcon, ChevronDownIcon, ChevronRightIcon, ChevronUpIcon, CopyIcon, DeleteIcon, EditIcon,
    FolderOpenIcon,
};
use crate::components::status_toast::ToastKind;
use crate::dto::{EntryDto, OptionDto, SourceKind};

/// One table row's content, independent of `candidate::CandidateDocument`
/// so this component only depends on the plain DTOs it needs to render.
#[derive(Clone, PartialEq)]
pub enum RowEntry {
    Parsed(EntryDto),
    Unparsed(String),
}

#[derive(Properties, PartialEq)]
pub struct EntriesTableProps {
    /// `(candidate_index, row)` pairs, in document order. `candidate_index`
    /// is the row's identity in the app's `CandidateDocument` - the same
    /// value is reported back via `on_edit`/`on_delete`.
    pub rows: Vec<(usize, RowEntry)>,
    /// Document-wide warning findings keyed by `candidate_index`, from a
    /// `validate_candidate` run against the whole document (not just the
    /// row being edited) - lets a row visibly flag "this has a warning"
    /// before anyone opens its own drawer. A row with no entry here has no
    /// known warning. Full message text is shown as a native tooltip, not
    /// inline, to avoid repeating the mistake the mount-point note-glyph
    /// made (taking up row space) - this is a border accent only.
    #[prop_or_default]
    pub row_warnings: std::collections::HashMap<usize, Vec<AttrValue>>,
    pub on_edit: Callback<usize>,
    pub on_delete: Callback<usize>,
    /// `(from, to)` candidate indices for a Move up/Move down click (T40,
    /// `docs/UI_DESIGN.md` §13.8). `to` is resolved here, from the
    /// neighboring element of `rows` - comments/blank lines are already
    /// filtered out of that list, so the neighbor is always the next
    /// *entry*, whatever sits between them in the file.
    pub on_move: Callback<(usize, usize)>,
    pub on_add_entry: Callback<()>,
    /// Surfaces a transient notification from the Mount point copy/open
    /// buttons inside a row's expanded detail panel (T32) - threaded down
    /// the same way `EntryDetailDrawer` already receives its own handle
    /// from `AppShell`.
    pub show_toast: Callback<(ToastKind, String)>,
}

/// Also reused by `AppShell` for `DeleteConfirmDialog`'s read-only summary.
pub(crate) fn source_badge(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Path => "PATH",
        SourceKind::Uuid => "UUID",
        SourceKind::Label => "LABEL",
        SourceKind::PartUuid => "PARTUUID",
        SourceKind::PartLabel => "PARTLABEL",
        SourceKind::Network => "NET",
        SourceKind::None => "NONE",
        SourceKind::Other => "OTHER",
    }
}

fn format_option(option: &OptionDto) -> String {
    match &option.value {
        Some(value) => format!("{}={}", option.key, value),
        None => option.key.clone(),
    }
}

/// Whether the row at render position `row_position` can move up - i.e.
/// whether it has a row above it (`docs/UI_DESIGN.md` §13.2). The first
/// row's Move up button is rendered `disabled`, never hidden.
fn can_move_up(row_position: usize) -> bool {
    row_position > 0
}

/// Whether the row at render position `row_position`, in a table of
/// `row_count` rows, can move down. A one-row table disables both
/// directions (§13.2).
fn can_move_down(row_position: usize, row_count: usize) -> bool {
    row_position + 1 < row_count
}

/// How many non-row-kind lines (freestanding comments/blank lines) sit
/// between two adjacent table rows in the candidate document, from their
/// candidate indices alone: `rows[k+1].0 - rows[k].0 - 1`
/// (`docs/UI_DESIGN.md` §13.5). `0` means the two entries are physically
/// adjacent and no divider is rendered.
fn gap_between(upper_candidate_index: usize, lower_candidate_index: usize) -> usize {
    lower_candidate_index
        .saturating_sub(upper_candidate_index)
        .saturating_sub(1)
}

/// The gap divider's short visible label (§13.5).
fn gap_label(count: usize) -> String {
    if count == 1 {
        "1 line stays here".to_string()
    } else {
        format!("{count} lines stay here")
    }
}

/// The gap divider's `title` tooltip - the full explanation of §13.4's
/// landing rule, shown on hover rather than inline.
fn gap_tooltip(count: usize) -> String {
    let subject = if count == 1 {
        "1 comment or blank line sits".to_string()
    } else {
        format!("{count} comments or blank lines sit")
    };
    format!(
        "{subject} between these two entries in the file. They aren't shown as rows and aren't \
         attached to either entry - moving an entry across this gap does not move them; they keep \
         their exact position. Check the Raw diff tab in Review & Save to see them."
    )
}

/// One Move up/Move down button (`docs/UI_DESIGN.md` §13.2).
///
/// `handler` being `None` *is* the boundary state: the button is still
/// rendered, in its constant position in the Actions cell, as a real
/// `disabled` element carrying the `title` that explains why - never
/// hidden, so nothing shifts around as a row becomes or stops being the
/// first/last one.
fn move_button(
    handler: &Option<Callback<MouseEvent>>,
    label: &'static str,
    disabled_title: &'static str,
    hover_class: &'static str,
    icon: Html,
) -> Html {
    let enabled = handler.is_some();
    let class = classes!(
        "p-2",
        "rounded-md",
        "focus-visible:ring-2",
        "focus-visible:ring-sky-500",
        enabled.then_some(hover_class),
        (!enabled).then_some("opacity-40"),
        (!enabled).then_some("cursor-not-allowed"),
    );
    html! {
        <button
            type="button"
            aria-label={label}
            class={class}
            disabled={!enabled}
            title={(!enabled).then_some(AttrValue::from(disabled_title))}
            onclick={handler.clone().unwrap_or_else(|| Callback::from(|_: MouseEvent| ()))}
        >
            { icon }
        </button>
    }
}

/// Mirrors a `from`/`to` positional swap onto the set of expanded
/// candidate indices (`docs/UI_DESIGN.md` §13.8).
///
/// `expanded` is keyed by *position*, not by entry identity, so without
/// this an expanded row would stay open over whichever entry moved into
/// its slot instead of following the entry the user actually expanded.
fn swap_expanded(expanded: &HashSet<usize>, from: usize, to: usize) -> HashSet<usize> {
    let mut next = expanded.clone();
    let from_was_expanded = expanded.contains(&from);
    let to_was_expanded = expanded.contains(&to);
    if from_was_expanded != to_was_expanded {
        next.remove(&from);
        next.remove(&to);
        if from_was_expanded {
            next.insert(to);
        } else {
            next.insert(from);
        }
    }
    next
}

#[function_component(EntriesTable)]
pub fn entries_table(props: &EntriesTableProps) -> Html {
    // Hooks must run unconditionally on every render (Rules of Hooks) - this
    // has to come before the empty-rows early return below, not after it,
    // or the hook count differs between an empty-then-populated transition
    // (the normal case on startup, before `list_entries` resolves) and Yew
    // panics with "Hooks are called conditionally".
    let expanded: UseStateHandle<HashSet<usize>> = use_state(HashSet::new);

    if props.rows.is_empty() {
        let on_add = {
            let cb = props.on_add_entry.clone();
            Callback::from(move |_: MouseEvent| cb.emit(()))
        };
        return html! {
            <div class="bg-white dark:bg-slate-800 rounded-lg shadow-sm border border-slate-200 dark:border-slate-700">
                <EmptyState
                    icon="📄"
                    title="No entries yet"
                    action={Some(html! {
                        <button
                            type="button"
                            class="px-4 py-2 rounded-md bg-sky-600 hover:bg-sky-700 text-white text-sm font-medium focus-visible:ring-2 focus-visible:ring-sky-500 focus-visible:ring-offset-2"
                            onclick={on_add}
                        >
                            { "Add Entry" }
                        </button>
                    })}
                />
            </div>
        };
    }

    html! {
        <div class="bg-white dark:bg-slate-800 rounded-lg shadow-sm border border-slate-200 dark:border-slate-700 overflow-hidden">
            <table class="w-full text-sm">
                <thead class="bg-slate-50 dark:bg-slate-900/40 text-left text-xs font-medium text-slate-500 dark:text-slate-400 uppercase tracking-wide">
                    <tr>
                        <th class="px-2 py-2 w-8"><span class="sr-only">{ "Expand row" }</span></th>
                        <th class="px-4 py-2">{ "Source" }</th>
                        <th class="px-4 py-2">{ "Mount point" }</th>
                        <th class="px-4 py-2">{ "FS type" }</th>
                        <th class="px-4 py-2">{ "Options" }</th>
                        <th class="px-4 py-2">{ "Dump" }</th>
                        <th class="px-4 py-2">{ "Pass" }</th>
                        <th class="px-4 py-2 text-right">{ "Actions" }</th>
                    </tr>
                </thead>
                <tbody>
                    { for props.rows.iter().enumerate().flat_map(|(row_position, (candidate_index, row))| {
                        let candidate_index = *candidate_index;
                        let row_count = props.rows.len();
                        let up_target = can_move_up(row_position)
                            .then(|| props.rows[row_position - 1].0);
                        let down_target = can_move_down(row_position, row_count)
                            .then(|| props.rows[row_position + 1].0);
                        // Both Move buttons stage the swap *and* mirror it
                        // onto `expanded` (§13.8) - the set is keyed by
                        // position, so expansion would otherwise stay with
                        // the slot rather than with the entry.
                        let make_move = |target: Option<usize>| {
                            let cb = props.on_move.clone();
                            let expanded = expanded.clone();
                            target.map(|to| Callback::from(move |e: MouseEvent| {
                                e.stop_propagation();
                                expanded.set(swap_expanded(&expanded, candidate_index, to));
                                cb.emit((candidate_index, to));
                            }))
                        };
                        let on_move_up = make_move(up_target);
                        let on_move_down = make_move(down_target);
                        // The divider that follows this row, if any
                        // freestanding comment/blank line sits between it
                        // and the next one (§13.5).
                        let gap = props
                            .rows
                            .get(row_position + 1)
                            .map(|(next_index, _)| gap_between(candidate_index, *next_index))
                            .unwrap_or(0);
                        let gap_divider = (gap > 0).then(|| html! {
                            <tr key={format!("gap-{candidate_index}")} title={gap_tooltip(gap)}>
                                <td colspan="8" class="py-1">
                                    <div class="flex items-center gap-2">
                                        <span class="flex-1 border-t border-dashed border-slate-200 dark:border-slate-700"></span>
                                        <span class="text-xs text-slate-400 dark:text-slate-500">{ gap_label(gap) }</span>
                                        <span class="flex-1 border-t border-dashed border-slate-200 dark:border-slate-700"></span>
                                    </div>
                                </td>
                            </tr>
                        });
                        let on_edit = {
                            let cb = props.on_edit.clone();
                            Callback::from(move |e: MouseEvent| {
                                e.stop_propagation();
                                cb.emit(candidate_index);
                            })
                        };
                        let on_delete = {
                            let cb = props.on_delete.clone();
                            Callback::from(move |e: MouseEvent| {
                                e.stop_propagation();
                                cb.emit(candidate_index);
                            })
                        };
                        let row_html = match row {
                            RowEntry::Parsed(entry) => {
                                let warnings = props
                                    .row_warnings
                                    .get(&candidate_index)
                                    .cloned()
                                    .unwrap_or_default();
                                let is_expanded = expanded.contains(&candidate_index);
                                let on_toggle = {
                                    let expanded = expanded.clone();
                                    Callback::from(move |_: MouseEvent| {
                                        let mut next = (*expanded).clone();
                                        if !next.remove(&candidate_index) {
                                            next.insert(candidate_index);
                                        }
                                        expanded.set(next);
                                    })
                                };
                                html! {
                                    <EntryRow
                                        key={candidate_index}
                                        entry={entry.clone()}
                                        warnings={warnings}
                                        expanded={is_expanded}
                                        on_toggle={on_toggle}
                                        on_move_up={on_move_up}
                                        on_move_down={on_move_down}
                                        on_edit={on_edit}
                                        on_delete={on_delete}
                                        show_toast={props.show_toast.clone()}
                                    />
                                }
                            }
                            RowEntry::Unparsed(raw_line) => html! {
                                <UnparsedEntryRow
                                    key={candidate_index}
                                    raw_line={AttrValue::from(raw_line.clone())}
                                    on_move_up={on_move_up}
                                    on_move_down={on_move_down}
                                    on_edit={on_edit}
                                    on_delete={on_delete}
                                />
                            },
                        };
                        // Flattened rather than wrapped in a fragment so
                        // every row keeps its own key in one flat list.
                        [Some(row_html), gap_divider].into_iter().flatten()
                    }) }
                </tbody>
            </table>
        </div>
    }
}

#[derive(Properties, PartialEq)]
pub struct EntryRowProps {
    pub entry: EntryDto,
    /// Document-wide warning messages about this row specifically (see
    /// `EntriesTableProps::row_warnings`). Empty means no known warning.
    #[prop_or_default]
    pub warnings: Vec<AttrValue>,
    /// Whether this row's detail panel is currently open (T32) - owned by
    /// `EntriesTable`, not this component, so several rows can be expanded
    /// at once.
    pub expanded: bool,
    /// Toggles `expanded` for this row's own `candidate_index`. Clicking
    /// anywhere on the row's `<tr>` fires this; Edit/Delete call
    /// `stop_propagation` first so neither of them also toggles the row.
    pub on_toggle: Callback<MouseEvent>,
    /// Moves this entry one position up/down (T40, §13.2). `None` means
    /// this row is at that boundary of the table, which renders a real
    /// `disabled` button rather than hiding it.
    pub on_move_up: Option<Callback<MouseEvent>>,
    pub on_move_down: Option<Callback<MouseEvent>>,
    pub on_edit: Callback<MouseEvent>,
    pub on_delete: Callback<MouseEvent>,
    /// Surfaces a copy/open-in-file-manager failure from the expanded
    /// panel's Mount point buttons.
    pub show_toast: Callback<(ToastKind, String)>,
}

/// Builds a "copy `value` to the clipboard" click handler, flashing
/// `just_copied` for ~1.5s on success. Shared by Source and Mount point's
/// copy buttons in the row detail panel (T32).
fn make_copy_callback(
    value: String,
    just_copied: UseStateHandle<bool>,
    show_toast: Callback<(ToastKind, String)>,
    error_message: &'static str,
) -> Callback<MouseEvent> {
    Callback::from(move |e: MouseEvent| {
        e.stop_propagation();
        let just_copied = just_copied.clone();
        let show_toast = show_toast.clone();
        // The clipboard-write call itself must happen synchronously in this
        // handler, before any `.await`, to stay inside the user-gesture
        // context the webview requires for clipboard permission - only the
        // *result* is awaited inside the spawned future below.
        let Some(window) = web_sys::window() else {
            show_toast.emit((ToastKind::Error, error_message.to_string()));
            return;
        };
        let promise = window.navigator().clipboard().write_text(&value);
        wasm_bindgen_futures::spawn_local(async move {
            match wasm_bindgen_futures::JsFuture::from(promise).await {
                Ok(_) => {
                    just_copied.set(true);
                    gloo_timers::future::TimeoutFuture::new(1500).await;
                    just_copied.set(false);
                }
                Err(_) => {
                    show_toast.emit((ToastKind::Error, error_message.to_string()));
                }
            }
        });
    })
}

/// Builds an "open `path` in the system file manager" click handler. Shared
/// by Source (only when its kind is `Path`) and Mount point's open-folder
/// buttons in the row detail panel (T32).
fn make_open_folder_callback(
    path: String,
    show_toast: Callback<(ToastKind, String)>,
) -> Callback<MouseEvent> {
    Callback::from(move |e: MouseEvent| {
        e.stop_propagation();
        let path = path.clone();
        let show_toast = show_toast.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(message) = api::open_path_in_file_manager(&path).await {
                show_toast.emit((ToastKind::Error, message));
            }
        });
    })
}

#[function_component(EntryRow)]
pub fn entry_row(props: &EntryRowProps) -> Html {
    let entry = &props.entry;
    let options: Vec<String> = entry.options.iter().map(format_option).collect();
    let shown_options: Vec<&String> = options.iter().take(3).collect();
    let overflow = options.len().saturating_sub(3);
    let full_list = options.join(", ");

    let has_warnings = !props.warnings.is_empty();
    let row_class = classes!(
        "h-11",
        "cursor-pointer",
        "odd:bg-slate-50",
        "dark:odd:bg-slate-800/60",
        "hover:bg-sky-50",
        "dark:hover:bg-slate-700/60",
        "border-b",
        "border-slate-200",
        "dark:border-slate-700",
        has_warnings.then_some("border-l-4"),
        has_warnings.then_some("border-l-amber-400"),
        has_warnings.then_some("dark:border-l-amber-500"),
    );
    let row_title: Option<AttrValue> = has_warnings.then(|| {
        AttrValue::from(
            props
                .warnings
                .iter()
                .map(|w| w.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        )
    });

    let chevron_class = classes!(
        "inline-flex",
        "transition-transform",
        "duration-150",
        props.expanded.then_some("rotate-90"),
    );

    // Clipboard "just copied" flash, local to this row (T32 §6 of the
    // task): flips to `true` right after a successful `write_text`, then
    // back to `false` ~1.5s later, swapping the copy icon for a checkmark
    // in between. Source and Mount point each get their own flash state so
    // copying one doesn't flash the other's button.
    let source_just_copied = use_state(|| false);
    let mount_just_copied = use_state(|| false);
    let on_copy_source = make_copy_callback(
        entry.source.value.clone(),
        source_just_copied.clone(),
        props.show_toast.clone(),
        "Could not copy the source to the clipboard.",
    );
    let on_copy_mount = make_copy_callback(
        entry.mount_point.clone(),
        mount_just_copied.clone(),
        props.show_toast.clone(),
        "Could not copy the mount point to the clipboard.",
    );

    // Only a `Path` source names something that can actually be opened in a
    // file manager (e.g. a bind mount's source directory) - a UUID/LABEL/
    // network source etc. isn't a path on this filesystem at all, so those
    // get a copy button only.
    let on_open_folder_source = (entry.source.kind == SourceKind::Path)
        .then(|| make_open_folder_callback(entry.source.value.clone(), props.show_toast.clone()));
    let on_open_folder_mount =
        make_open_folder_callback(entry.mount_point.clone(), props.show_toast.clone());

    let description = entry.description.clone();
    let trailing_comment = entry.trailing_comment.clone();

    html! {
        <>
            <tr class={row_class} title={row_title} onclick={props.on_toggle.clone()}>
                <td class="px-2 py-2 text-slate-400 dark:text-slate-500">
                    <span class={chevron_class} aria-hidden="true">
                        <ChevronRightIcon />
                    </span>
                </td>
                <td class="px-4 py-2 font-mono text-sm max-w-[16rem]">
                    <div class="flex items-center gap-2 min-w-0">
                        <span class="shrink-0 inline-block rounded-full bg-slate-100 dark:bg-slate-700 text-slate-600 dark:text-slate-300 text-xs font-medium px-2 py-0.5">
                            { source_badge(entry.source.kind) }
                        </span>
                        <span class="truncate min-w-0" title={entry.source.value.clone()}>
                            { entry.source.value.clone() }
                        </span>
                    </div>
                </td>
                <td class="px-4 py-2 font-mono text-sm max-w-[14rem]">
                    <span class="block truncate" title={entry.mount_point.clone()}>
                        { entry.mount_point.clone() }
                    </span>
                </td>
                <td class="px-4 py-2 text-sm">
                    <span class="rounded-md bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-slate-600 dark:text-slate-300">
                        { entry.fs_type.clone() }
                    </span>
                </td>
                <td class="px-4 py-2 text-sm" title={full_list}>
                    { for shown_options.iter().map(|opt| html! {
                        <span class="inline-block rounded-full bg-sky-100 text-sky-700 dark:bg-sky-900/40 dark:text-sky-300 text-xs font-medium px-3 py-1 mr-1">
                            { (*opt).clone() }
                        </span>
                    }) }
                    if overflow > 0 {
                        <span class="text-xs text-slate-500 dark:text-slate-400">{ format!("+{overflow} more") }</span>
                    }
                </td>
                <td class="px-4 py-2 text-sm">{ entry.dump.to_string() }</td>
                <td class="px-4 py-2 text-sm">{ entry.pass.to_string() }</td>
                <td class="px-4 py-2 text-right whitespace-nowrap">
                    { move_button(
                        &props.on_move_up,
                        "Move entry up",
                        "Already the first entry",
                        "hover:bg-slate-100 dark:hover:bg-slate-700",
                        html! { <ChevronUpIcon /> },
                    ) }
                    { move_button(
                        &props.on_move_down,
                        "Move entry down",
                        "Already the last entry",
                        "hover:bg-slate-100 dark:hover:bg-slate-700",
                        html! { <ChevronDownIcon /> },
                    ) }
                    <button
                        type="button"
                        aria-label="Edit entry"
                        class="ml-2 p-2 rounded-md hover:bg-slate-100 dark:hover:bg-slate-700 focus-visible:ring-2 focus-visible:ring-sky-500"
                        onclick={props.on_edit.clone()}
                    >
                        <EditIcon />
                    </button>
                    <button
                        type="button"
                        aria-label="Delete entry"
                        class="p-2 rounded-md text-slate-600 dark:text-slate-400 hover:bg-red-50 hover:text-red-600 dark:hover:bg-red-900/30 focus-visible:ring-2 focus-visible:ring-sky-500"
                        onclick={props.on_delete.clone()}
                    >
                        <DeleteIcon />
                    </button>
                </td>
            </tr>
            if props.expanded {
                <tr class="bg-slate-50 dark:bg-slate-900/40 border-l-4 border-l-sky-400 dark:border-l-sky-500 border-b border-slate-200 dark:border-slate-700">
                    <td colspan="8" class="px-6 py-5">
                        <div class="grid grid-cols-[minmax(0,1fr)_minmax(200px,auto)] gap-x-8 gap-y-5">
                            <div>
                                <p class="text-xs font-medium text-slate-500 dark:text-slate-400 uppercase tracking-wide mb-1">
                                    { "Source" }
                                </p>
                                <div class="flex items-center gap-1.5 font-mono text-sm">
                                    <span class="shrink-0 inline-block rounded-full bg-slate-100 dark:bg-slate-700 text-slate-600 dark:text-slate-300 text-xs font-medium px-2 py-0.5">
                                        { source_badge(entry.source.kind) }
                                    </span>
                                    <span class="break-all">{ entry.source.value.clone() }</span>
                                    <button
                                        type="button"
                                        aria-label="Copy source to clipboard"
                                        class="p-1.5 rounded-md text-slate-500 dark:text-slate-400 hover:bg-slate-200 dark:hover:bg-slate-700 focus-visible:ring-2 focus-visible:ring-sky-500 shrink-0"
                                        onclick={on_copy_source}
                                    >
                                        if *source_just_copied {
                                            <CheckIcon />
                                        } else {
                                            <CopyIcon />
                                        }
                                    </button>
                                    if let Some(on_open_folder_source) = on_open_folder_source {
                                        <button
                                            type="button"
                                            aria-label="Open source in file manager"
                                            class="p-1.5 rounded-md text-slate-500 dark:text-slate-400 hover:bg-slate-200 dark:hover:bg-slate-700 focus-visible:ring-2 focus-visible:ring-sky-500 shrink-0"
                                            onclick={on_open_folder_source}
                                        >
                                            <FolderOpenIcon />
                                        </button>
                                    }
                                </div>
                            </div>
                            <div>
                                <p class="text-xs font-medium text-slate-500 dark:text-slate-400 uppercase tracking-wide mb-1">
                                    { "FS type" }
                                </p>
                                <span class="rounded-md bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-sm text-slate-600 dark:text-slate-300">
                                    { entry.fs_type.clone() }
                                </span>
                            </div>
                            <div>
                                <p class="text-xs font-medium text-slate-500 dark:text-slate-400 uppercase tracking-wide mb-1">
                                    { "Mount point" }
                                </p>
                                <div class="flex items-center gap-1.5 font-mono text-sm">
                                    <span class="break-all">{ entry.mount_point.clone() }</span>
                                    <button
                                        type="button"
                                        aria-label="Copy mount point to clipboard"
                                        class="p-1.5 rounded-md text-slate-500 dark:text-slate-400 hover:bg-slate-200 dark:hover:bg-slate-700 focus-visible:ring-2 focus-visible:ring-sky-500 shrink-0"
                                        onclick={on_copy_mount}
                                    >
                                        if *mount_just_copied {
                                            <CheckIcon />
                                        } else {
                                            <CopyIcon />
                                        }
                                    </button>
                                    <button
                                        type="button"
                                        aria-label="Open mount point in file manager"
                                        class="p-1.5 rounded-md text-slate-500 dark:text-slate-400 hover:bg-slate-200 dark:hover:bg-slate-700 focus-visible:ring-2 focus-visible:ring-sky-500 shrink-0"
                                        onclick={on_open_folder_mount}
                                    >
                                        <FolderOpenIcon />
                                    </button>
                                </div>
                            </div>
                            <div>
                                <p class="text-xs font-medium text-slate-500 dark:text-slate-400 uppercase tracking-wide mb-1">
                                    { "Dump / Pass" }
                                </p>
                                <span class="text-sm">{ format!("{} / {}", entry.dump, entry.pass) }</span>
                            </div>
                            <div class="col-span-2">
                                <p class="text-xs font-medium text-slate-500 dark:text-slate-400 uppercase tracking-wide mb-1">
                                    { "Options" }
                                </p>
                                <div>
                                    { for options.iter().map(|opt| html! {
                                        <span class="inline-block rounded-full bg-sky-100 text-sky-700 dark:bg-sky-900/40 dark:text-sky-300 text-xs font-medium px-3 py-1 mr-1 mb-1">
                                            { opt.clone() }
                                        </span>
                                    }) }
                                </div>
                            </div>
                            if let Some(description) = description {
                                <div class="col-span-2">
                                    <p class="text-xs font-medium text-slate-500 dark:text-slate-400 uppercase tracking-wide mb-1">
                                        { "Description" }
                                    </p>
                                    <p class="text-sm">{ description }</p>
                                </div>
                            }
                            if let Some(trailing_comment) = trailing_comment {
                                <div class="col-span-2">
                                    <p class="text-xs font-medium text-slate-500 dark:text-slate-400 uppercase tracking-wide mb-1">
                                        { "Trailing comment" }
                                    </p>
                                    <p class="text-sm font-mono">{ trailing_comment }</p>
                                </div>
                            }
                        </div>
                    </td>
                </tr>
            }
        </>
    }
}

#[derive(Properties, PartialEq)]
pub struct UnparsedEntryRowProps {
    pub raw_line: AttrValue,
    /// Same contract as `EntryRowProps`' own pair: `None` is the boundary
    /// state and renders a real `disabled` button (§13.2).
    pub on_move_up: Option<Callback<MouseEvent>>,
    pub on_move_down: Option<Callback<MouseEvent>>,
    pub on_edit: Callback<MouseEvent>,
    pub on_delete: Callback<MouseEvent>,
}

#[function_component(UnparsedEntryRow)]
pub fn unparsed_entry_row(props: &UnparsedEntryRowProps) -> Html {
    html! {
        <tr class="bg-amber-50 dark:bg-amber-900/20 border-l-4 border-amber-500 border-b border-slate-200 dark:border-slate-700">
            <td colspan="8" class="px-4 py-3">
                <div class="flex items-start gap-3">
                    <span class="text-amber-600 dark:text-amber-400 mt-0.5" aria-hidden="true">{ "⚠" }</span>
                    <div class="flex-1">
                        <p class="text-sm font-medium text-amber-800 dark:text-amber-300 mb-1">
                            { "Couldn't fully parse this line - shown as raw text" }
                        </p>
                        <pre class="font-mono text-sm bg-slate-900/5 dark:bg-slate-950/40 rounded-md px-3 py-2 whitespace-pre-wrap">
                            { props.raw_line.clone() }
                        </pre>
                    </div>
                    <div class="flex gap-1 shrink-0">
                        { move_button(
                            &props.on_move_up,
                            "Move raw entry up",
                            "Already the first entry",
                            "hover:bg-amber-100 dark:hover:bg-amber-900/40",
                            html! { <ChevronUpIcon /> },
                        ) }
                        { move_button(
                            &props.on_move_down,
                            "Move raw entry down",
                            "Already the last entry",
                            "hover:bg-amber-100 dark:hover:bg-amber-900/40",
                            html! { <ChevronDownIcon /> },
                        ) }
                        <button
                            type="button"
                            aria-label="Edit raw entry"
                            class="ml-2 p-2 rounded-md hover:bg-amber-100 dark:hover:bg-amber-900/40 focus-visible:ring-2 focus-visible:ring-sky-500"
                            onclick={props.on_edit.clone()}
                        >
                            <EditIcon />
                        </button>
                        <button
                            type="button"
                            aria-label="Delete entry"
                            class="p-2 rounded-md text-slate-600 dark:text-slate-400 hover:bg-red-50 hover:text-red-600 dark:hover:bg-red-900/30 focus-visible:ring-2 focus-visible:ring-sky-500"
                            onclick={props.on_delete.clone()}
                        >
                            <DeleteIcon />
                        </button>
                    </div>
                </div>
            </td>
        </tr>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_row_cannot_move_up_and_the_last_cannot_move_down() {
        let row_count = 3;
        assert!(!can_move_up(0));
        assert!(can_move_down(0, row_count));
        assert!(can_move_up(1));
        assert!(can_move_down(1, row_count));
        assert!(can_move_up(2));
        assert!(!can_move_down(2, row_count));
    }

    #[test]
    fn a_single_row_table_disables_both_directions() {
        assert!(!can_move_up(0));
        assert!(!can_move_down(0, 1));
    }

    #[test]
    fn adjacent_entries_have_no_gap_between_them() {
        assert_eq!(gap_between(0, 1), 0);
        assert_eq!(gap_between(7, 8), 0);
    }

    #[test]
    fn one_freestanding_line_between_two_entries_is_one_gap_line() {
        assert_eq!(gap_between(0, 2), 1);
        assert_eq!(gap_label(1), "1 line stays here");
        assert!(gap_tooltip(1).starts_with("1 comment or blank line sits between"));
    }

    #[test]
    fn several_freestanding_lines_between_two_entries_are_counted_and_pluralized() {
        assert_eq!(gap_between(3, 7), 3);
        assert_eq!(gap_label(3), "3 lines stay here");
        assert!(gap_tooltip(3).starts_with("3 comments or blank lines sit between"));
    }

    #[test]
    fn expansion_follows_the_moved_entry_not_the_slot_it_left() {
        // §13.8: `expanded` is keyed by position, so a move has to carry
        // the entry's expansion with it.
        let expanded: HashSet<usize> = [2].into_iter().collect();

        // The expanded row itself moves down into slot 4.
        let after_moving_it = swap_expanded(&expanded, 2, 4);
        assert_eq!(after_moving_it, [4].into_iter().collect::<HashSet<usize>>());

        // A collapsed row moves into the expanded row's slot: the
        // expansion goes with the entry that left, not with the slot.
        let after_moving_the_other = swap_expanded(&expanded, 4, 2);
        assert_eq!(
            after_moving_the_other,
            [4].into_iter().collect::<HashSet<usize>>()
        );
    }

    #[test]
    fn swapping_two_rows_that_are_both_or_neither_expanded_leaves_the_set_alone() {
        let both: HashSet<usize> = [1, 2, 9].into_iter().collect();
        assert_eq!(swap_expanded(&both, 1, 2), both);

        let neither: HashSet<usize> = [9].into_iter().collect();
        assert_eq!(swap_expanded(&neither, 1, 2), neither);
    }

    #[test]
    fn swapping_an_expanded_row_with_itself_keeps_it_expanded() {
        let expanded: HashSet<usize> = [3].into_iter().collect();
        assert_eq!(swap_expanded(&expanded, 3, 3), expanded);
    }
}
