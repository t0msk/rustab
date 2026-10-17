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

use std::collections::HashSet;

use yew::prelude::*;

use crate::api;
use crate::components::empty_state::EmptyState;
use crate::components::icons::{
    CheckIcon, ChevronRightIcon, CopyIcon, DeleteIcon, EditIcon, FolderOpenIcon,
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

#[function_component(EntriesTable)]
pub fn entries_table(props: &EntriesTableProps) -> Html {
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

    let expanded: UseStateHandle<HashSet<usize>> = use_state(HashSet::new);

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
                    { for props.rows.iter().map(|(candidate_index, row)| {
                        let candidate_index = *candidate_index;
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
                        match row {
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
                                        on_edit={on_edit}
                                        on_delete={on_delete}
                                        show_toast={props.show_toast.clone()}
                                    />
                                }
                            }
                            RowEntry::Unparsed(raw_line) => html! {
                                <UnparsedEntryRow key={candidate_index} raw_line={AttrValue::from(raw_line.clone())} on_edit={on_edit} on_delete={on_delete} />
                            },
                        }
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
    pub on_edit: Callback<MouseEvent>,
    pub on_delete: Callback<MouseEvent>,
    /// Surfaces a copy/open-in-file-manager failure from the expanded
    /// panel's Mount point buttons.
    pub show_toast: Callback<(ToastKind, String)>,
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
    // in between.
    let just_copied = use_state(|| false);
    let on_copy = {
        let mount_point = entry.mount_point.clone();
        let just_copied = just_copied.clone();
        let show_toast = props.show_toast.clone();
        Callback::from(move |e: MouseEvent| {
            e.stop_propagation();
            let just_copied = just_copied.clone();
            let show_toast = show_toast.clone();
            // The clipboard-write call itself must happen synchronously in
            // this handler, before any `.await`, to stay inside the
            // user-gesture context the webview requires for clipboard
            // permission - only the *result* is awaited inside the spawned
            // future below.
            let Some(window) = web_sys::window() else {
                show_toast.emit((
                    ToastKind::Error,
                    "Could not copy the mount point to the clipboard.".to_string(),
                ));
                return;
            };
            let promise = window.navigator().clipboard().write_text(&mount_point);
            wasm_bindgen_futures::spawn_local(async move {
                match wasm_bindgen_futures::JsFuture::from(promise).await {
                    Ok(_) => {
                        just_copied.set(true);
                        gloo_timers::future::TimeoutFuture::new(1500).await;
                        just_copied.set(false);
                    }
                    Err(_) => {
                        show_toast.emit((
                            ToastKind::Error,
                            "Could not copy the mount point to the clipboard.".to_string(),
                        ));
                    }
                }
            });
        })
    };

    let on_open_folder = {
        let mount_point = entry.mount_point.clone();
        let show_toast = props.show_toast.clone();
        Callback::from(move |e: MouseEvent| {
            e.stop_propagation();
            let mount_point = mount_point.clone();
            let show_toast = show_toast.clone();
            wasm_bindgen_futures::spawn_local(async move {
                if let Err(message) = api::open_path_in_file_manager(&mount_point).await {
                    show_toast.emit((ToastKind::Error, message));
                }
            });
        })
    };

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
                    <button
                        type="button"
                        aria-label="Edit entry"
                        class="p-2 rounded-md hover:bg-slate-100 dark:hover:bg-slate-700 focus-visible:ring-2 focus-visible:ring-sky-500"
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
                        <div class="grid grid-cols-2 gap-x-8 gap-y-5">
                            <div>
                                <p class="text-xs font-medium text-slate-500 dark:text-slate-400 uppercase tracking-wide mb-1">
                                    { "Source" }
                                </p>
                                <div class="flex items-center gap-2 font-mono text-sm">
                                    <span class="shrink-0 inline-block rounded-full bg-slate-100 dark:bg-slate-700 text-slate-600 dark:text-slate-300 text-xs font-medium px-2 py-0.5">
                                        { source_badge(entry.source.kind) }
                                    </span>
                                    <span class="break-all">{ entry.source.value.clone() }</span>
                                </div>
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
                                        onclick={on_copy}
                                    >
                                        if *just_copied {
                                            <CheckIcon />
                                        } else {
                                            <CopyIcon />
                                        }
                                    </button>
                                    <button
                                        type="button"
                                        aria-label="Open mount point in file manager"
                                        class="p-1.5 rounded-md text-slate-500 dark:text-slate-400 hover:bg-slate-200 dark:hover:bg-slate-700 focus-visible:ring-2 focus-visible:ring-sky-500 shrink-0"
                                        onclick={on_open_folder}
                                    >
                                        <FolderOpenIcon />
                                    </button>
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
                        <button
                            type="button"
                            aria-label="Edit raw entry"
                            class="p-2 rounded-md hover:bg-amber-100 dark:hover:bg-amber-900/40 focus-visible:ring-2 focus-visible:ring-sky-500"
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
