//! `ValidationSummary` — drawer-top aggregate error banner (§5).
//!
//! Renders nothing when there are no errors. Like `ValidationMessage`, real
//! validation results arrive in T11 — this task only needs the rendering
//! logic to exist and to correctly hide itself when the error list is
//! empty (which it always is in this task's static mock form).

use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct ValidationSummaryProps {
    pub errors: Vec<AttrValue>,
}

#[function_component(ValidationSummary)]
pub fn validation_summary(props: &ValidationSummaryProps) -> Html {
    if props.errors.is_empty() {
        return html! {};
    }

    html! {
        <div class="rounded-md border border-red-300 dark:border-red-800 bg-red-50 dark:bg-red-900/20 text-red-800 dark:text-red-300 px-4 py-3 mt-3 mb-4">
            <p class="text-sm font-medium mb-1">
                { format!("{} issues must be fixed before saving", props.errors.len()) }
            </p>
            <ul class="list-disc list-inside text-xs space-y-0.5">
                { for props.errors.iter().map(|error| html! { <li>{ error.clone() }</li> }) }
            </ul>
        </div>
    }
}
