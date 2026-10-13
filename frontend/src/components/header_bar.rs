//! `HeaderBar` — app title, Entries/Backups tab nav, Add Entry button.

use yew::prelude::*;

/// In-memory view-state: which top-level tab is active. No router is used
/// per `docs/UI_DESIGN.md` §1 — `AppShell` owns a `UseStateHandle<Tab>`.
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
            <button
                type="button"
                class="px-4 py-2 rounded-md bg-sky-600 hover:bg-sky-700 text-white text-sm font-medium focus-visible:ring-2 focus-visible:ring-sky-500 focus-visible:ring-offset-2"
                onclick={on_add}
            >
                { "+ Add Entry" }
            </button>
        </header>
    }
}
