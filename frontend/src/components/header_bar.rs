//! `HeaderBar` - app title, Entries/Backups tab nav, contextual action
//! button (Add Entry on Entries tab, Create backup on Backups tab).

use yew::prelude::*;

use crate::components::loading_spinner::LoadingSpinner;

/// In-memory view-state: which top-level tab is active. No router is used
/// per `docs/UI_DESIGN.md` §1 - `AppShell` owns a `UseStateHandle<Tab>`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Entries,
    Backups,
}

#[derive(Properties, PartialEq)]
pub struct HeaderBarProps {
    pub active_tab: Tab,
    pub on_tab_change: Callback<Tab>,
    pub on_add_entry: Callback<()>,
    /// Fired when the "Create backup" button is clicked. Not tied to a row -
    /// it always acts on the active target's current content
    /// (`docs/API_CONTRACT.md` §3.11).
    pub on_create_backup: Callback<()>,
    /// Set by `AppShell` while the `create_backup` call is in flight. No
    /// confirm dialog gates this action - it is treated as safe/additive/
    /// reversible (it only ever adds a file, which Delete can remove), so
    /// this is the only in-flight state the button needs.
    #[prop_or(false)]
    pub create_saving: bool,
    /// The last `create_backup` failure's message, shown inline beneath
    /// the button (no dialog).
    #[prop_or_default]
    pub create_error: Option<AttrValue>,
    /// Fired when "Open backup location" (T25, relocated into this slot by
    /// T26 per direct user request, superseding T25's own placement choice)
    /// is clicked.
    pub on_open_location: Callback<()>,
    /// The last `open_backup_location` failure's message, shown inline
    /// beneath the button (no dialog) - same treatment as `create_error`
    /// above. No loading/disabled prop: this action has no in-flight state
    /// (T25's own reasoning: the call is a near-instant `spawn()`), unchanged
    /// by this relocation.
    #[prop_or_default]
    pub open_location_error: Option<AttrValue>,
    /// Fired when the always-visible "About" trigger is clicked. Rendered
    /// regardless of `active_tab` (unlike every other button in this
    /// component).
    pub on_about_click: Callback<()>,
}

#[function_component(HeaderBar)]
pub fn header_bar(props: &HeaderBarProps) -> Html {
    let tab_class = |tab: Tab| -> Classes {
        let mut classes = classes!(
            "px-3",
            "py-1.5",
            "text-sm",
            "font-medium",
            "rounded-md",
            "focus-visible:ring-2",
            "focus-visible:ring-sky-500",
            "focus-visible:ring-offset-2"
        );
        if tab == props.active_tab {
            classes.extend(classes!(
                "bg-sky-100",
                "text-sky-700",
                "dark:bg-sky-900/40",
                "dark:text-sky-300"
            ));
        } else {
            classes.extend(classes!(
                "text-slate-500",
                "hover:text-slate-700",
                "dark:text-slate-400",
                "dark:hover:text-slate-200"
            ));
        }
        classes
    };

    let on_entries = {
        let cb = props.on_tab_change.clone();
        Callback::from(move |_: MouseEvent| cb.emit(Tab::Entries))
    };
    let on_backups = {
        let cb = props.on_tab_change.clone();
        Callback::from(move |_: MouseEvent| cb.emit(Tab::Backups))
    };
    let on_add = {
        let cb = props.on_add_entry.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };
    let on_create_click = {
        let cb = props.on_create_backup.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };
    let on_open_location_click = {
        let cb = props.on_open_location.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };
    let on_about_click = {
        let cb = props.on_about_click.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };

    html! {
        <header class="w-full shrink-0 bg-white dark:bg-slate-800 border-b border-slate-200 dark:border-slate-700 px-6 py-3 flex items-center justify-between">
            <div class="flex items-center gap-6">
                <h1 class="text-xl font-bold text-slate-900 dark:text-slate-100">{ "Rustab" }</h1>
                <nav class="flex gap-1" aria-label="Main view">
                    <button type="button" class={tab_class(Tab::Entries)} onclick={on_entries}>
                        { "Entries" }
                    </button>
                    <button type="button" class={tab_class(Tab::Backups)} onclick={on_backups}>
                        { "Backups" }
                    </button>
                </nav>
            </div>
            <div class="flex items-center gap-4">
                if props.active_tab == Tab::Entries {
                    <button
                        type="button"
                        class="px-4 py-2 rounded-md bg-sky-600 hover:bg-sky-700 text-white text-sm font-medium focus-visible:ring-2 focus-visible:ring-sky-500 focus-visible:ring-offset-2"
                        onclick={on_add}
                    >
                        { "+ Add Entry" }
                    </button>
                } else {
                    <div class="flex items-center gap-2">
                        <div>
                            <button
                                type="button"
                                disabled={props.create_saving}
                                class="px-4 py-2 rounded-md bg-sky-600 hover:bg-sky-700 text-white text-sm font-medium inline-flex items-center gap-2 focus-visible:ring-2 focus-visible:ring-sky-500 focus-visible:ring-offset-2 disabled:opacity-60 disabled:cursor-not-allowed"
                                onclick={on_create_click}
                            >
                                if props.create_saving {
                                    <LoadingSpinner />
                                    { "Creating…" }
                                } else {
                                    { "Create backup" }
                                }
                            </button>
                            if let Some(error) = &props.create_error {
                                <p class="mt-2 text-sm text-red-600 dark:text-red-400" role="alert">
                                    { error.clone() }
                                </p>
                            }
                        </div>
                        <div>
                            <button
                                type="button"
                                class="px-3 py-1.5 rounded-md bg-slate-100 hover:bg-slate-200 dark:bg-slate-700 dark:hover:bg-slate-600 text-slate-700 dark:text-slate-200 text-xs font-medium focus-visible:ring-2 focus-visible:ring-sky-500"
                                onclick={on_open_location_click}
                            >
                                { "Open backup location" }
                            </button>
                            if let Some(error) = &props.open_location_error {
                                <p class="mt-2 text-sm text-red-600 dark:text-red-400" role="alert">
                                    { error.clone() }
                                </p>
                            }
                        </div>
                    </div>
                }
                <button
                    type="button"
                    class="text-xs font-medium text-slate-500 hover:text-slate-700 dark:text-slate-400 dark:hover:text-slate-200 rounded-md px-2 py-1 focus-visible:ring-2 focus-visible:ring-sky-500"
                    onclick={on_about_click}
                >
                    { "About" }
                </button>
            </div>
        </header>
    }
}
