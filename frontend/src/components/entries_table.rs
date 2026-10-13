//! `EntriesTable` — main entry list (§3), rows are `EntryRow` (parsed) or
//! `UnparsedEntryRow` (malformed). Renders from a small hardcoded
//! `Vec<MockEntry>` for this task; T11 replaces this with real data.

use yew::prelude::*;

use crate::components::empty_state::EmptyState;
use crate::mock::{MockEntry, MockParsedEntry};

#[derive(Properties, PartialEq)]
pub struct EntriesTableProps {
    pub entries: Vec<MockEntry>,
    /// Emits the row index to edit. The caller decides structured vs. raw
    /// edit mode by inspecting that entry's kind.
    pub on_edit: Callback<usize>,
    pub on_delete: Callback<usize>,
    pub on_add_entry: Callback<()>,
}

#[function_component(EntriesTable)]
pub fn entries_table(props: &EntriesTableProps) -> Html {
    if props.entries.is_empty() {
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
                    { for props.entries.iter().enumerate().map(|(idx, entry)| {
                        let on_edit = {
                            let cb = props.on_edit.clone();
                            Callback::from(move |_: MouseEvent| cb.emit(idx))
                        };
                        let on_delete = {
                            let cb = props.on_delete.clone();
                            Callback::from(move |_: MouseEvent| cb.emit(idx))
                        };
                        match entry {
                            MockEntry::Parsed(entry) => html! {
                                <EntryRow key={idx} entry={entry.clone()} on_edit={on_edit} on_delete={on_delete} />
                            },
                            MockEntry::Unparsed { raw_line } => html! {
                                <UnparsedEntryRow key={idx} raw_line={AttrValue::from(*raw_line)} on_edit={on_edit} on_delete={on_delete} />
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
    pub entry: MockParsedEntry,
    pub on_edit: Callback<MouseEvent>,
    pub on_delete: Callback<MouseEvent>,
}

#[function_component(EntryRow)]
pub fn entry_row(props: &EntryRowProps) -> Html {
    let entry = &props.entry;
    let shown_options: Vec<&str> = entry.options.iter().take(3).copied().collect();
    let overflow = entry.options.len().saturating_sub(3);
    let full_list = entry.options.join(", ");

    html! {
        <tr class="h-11 odd:bg-slate-50 dark:odd:bg-slate-800/60 hover:bg-sky-50 dark:hover:bg-slate-700/60 border-b border-slate-200 dark:border-slate-700">
            <td class="px-4 py-2 font-mono text-sm whitespace-nowrap">
                <span class="inline-block rounded-full bg-slate-100 dark:bg-slate-700 text-slate-600 dark:text-slate-300 text-xs font-medium px-2 py-0.5 mr-2">
                    { entry.source_type }
                </span>
                { entry.source_value }
            </td>
            <td class="px-4 py-2 font-mono text-sm">{ entry.mount_point }</td>
            <td class="px-4 py-2 text-sm">
                <span class="rounded-md bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-slate-600 dark:text-slate-300">
                    { entry.fs_type }
                </span>
            </td>
            <td class="px-4 py-2 text-sm" title={full_list}>
                { for shown_options.iter().map(|opt| html! {
                    <span class="inline-block rounded-full bg-sky-100 text-sky-700 dark:bg-sky-900/40 dark:text-sky-300 text-xs font-medium px-3 py-1 mr-1">
                        { *opt }
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
                    <span aria-hidden="true">{ "✎" }</span>
                </button>
                <button
                    type="button"
                    aria-label="Delete entry"
                    class="p-2 rounded-md text-slate-600 dark:text-slate-400 hover:bg-red-50 hover:text-red-600 dark:hover:bg-red-900/30 focus-visible:ring-2 focus-visible:ring-sky-500"
                    onclick={props.on_delete.clone()}
                >
                    <span aria-hidden="true">{ "🗑" }</span>
                </button>
            </td>
        </tr>
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
            <td colspan="7" class="px-4 py-3">
                <div class="flex items-start gap-3">
                    <span class="text-amber-600 dark:text-amber-400 mt-0.5" aria-hidden="true">{ "⚠" }</span>
                    <div class="flex-1">
                        <p class="text-sm font-medium text-amber-800 dark:text-amber-300 mb-1">
                            { "Couldn't fully parse this line — shown as raw text" }
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
                            <span aria-hidden="true">{ "✎" }</span>
                        </button>
                        <button
                            type="button"
                            aria-label="Delete entry"
                            class="p-2 rounded-md text-slate-600 dark:text-slate-400 hover:bg-red-50 hover:text-red-600 dark:hover:bg-red-900/30 focus-visible:ring-2 focus-visible:ring-sky-500"
                            onclick={props.on_delete.clone()}
                        >
                            <span aria-hidden="true">{ "🗑" }</span>
                        </button>
                    </div>
                </div>
            </td>
        </tr>
    }
}
