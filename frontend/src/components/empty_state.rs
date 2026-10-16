//! `EmptyState` - generic empty-list placeholder, reused by `EntriesTable`
//! and `BackupPanel` (§3, §7).

use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct EmptyStateProps {
    pub icon: AttrValue,
    pub title: AttrValue,
    #[prop_or_default]
    pub description: Option<AttrValue>,
    #[prop_or_default]
    pub action: Option<Html>,
}

#[function_component(EmptyState)]
pub fn empty_state(props: &EmptyStateProps) -> Html {
    html! {
        <div class="flex flex-col items-center justify-center gap-3 py-16 px-6 text-center">
            <span class="text-4xl" aria-hidden="true">{ props.icon.clone() }</span>
            <p class="text-base font-medium text-slate-700 dark:text-slate-200">{ props.title.clone() }</p>
            if let Some(description) = &props.description {
                <p class="text-sm text-slate-500 dark:text-slate-400 max-w-sm">{ description.clone() }</p>
            }
            if let Some(action) = &props.action {
                { action.clone() }
            }
        </div>
    }
}
