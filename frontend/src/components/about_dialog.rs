//! `AboutDialog` - static, read-only "About Rustab" overlay (T26),
//! triggered by `HeaderBar`'s always-visible "About" button.
//!
//! Reuses `BackupDiffView`'s exact modal shell (backdrop `onclick` dismiss,
//! inner `stop_propagation`, `role="dialog"` `aria-modal="true"`): this
//! dialog is purely informational, like `BackupDiffView`, so it is
//! dismissible by clicking the backdrop, unlike the non-dismissible
//! write/delete confirm dialogs.

use yew::prelude::*;

use crate::api;
use crate::dto::ExternalLink;

#[derive(Properties, PartialEq)]
pub struct AboutDialogProps {
    pub on_close: Callback<()>,
}

#[function_component(AboutDialog)]
pub fn about_dialog(props: &AboutDialogProps) -> Html {
    let on_close_click = {
        let cb = props.on_close.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };
    let on_backdrop_click = {
        let cb = props.on_close.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };
    let stop_propagation = Callback::from(|e: MouseEvent| e.stop_propagation());

    let on_repo_click = Callback::from(|_: MouseEvent| {
        wasm_bindgen_futures::spawn_local(async move {
            let _ = api::open_external_link(ExternalLink::GithubRepo).await;
        });
    });
    let on_sponsors_click = Callback::from(|_: MouseEvent| {
        wasm_bindgen_futures::spawn_local(async move {
            let _ = api::open_external_link(ExternalLink::GithubSponsors).await;
        });
    });
    let on_issues_click = Callback::from(|_: MouseEvent| {
        wasm_bindgen_futures::spawn_local(async move {
            let _ = api::open_external_link(ExternalLink::Issues).await;
        });
    });
    // Repo root, not a specific license file - Rustab is dual MIT/Apache-2.0
    // (`docs/SPEC.md` §10), and the repo root is where GitHub itself
    // surfaces both rather than this link arbitrarily picking one.
    let on_license_click = Callback::from(|_: MouseEvent| {
        wasm_bindgen_futures::spawn_local(async move {
            let _ = api::open_external_link(ExternalLink::License).await;
        });
    });
    // Deliberately routed through `open_external_link` rather than a plain
    // `<a href="mailto:...">`: an in-webview anchor navigation to a
    // non-`http(s)` scheme is not reliably handled by the host webview
    // (WebKitGTK shows "The URL can't be shown" for `mailto:` instead of
    // falling back to the OS's own handler) - see API_CONTRACT.md §3.13.
    let on_email_click = Callback::from(|_: MouseEvent| {
        wasm_bindgen_futures::spawn_local(async move {
            let _ = api::open_external_link(ExternalLink::Email).await;
        });
    });

    html! {
        // Dismissible by backdrop click, unlike the write/delete dialogs:
        // this is read-only, so there is nothing a stray click could lose.
        <div
            class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
            onclick={on_backdrop_click}
        >
            <div
                class="w-full max-w-sm rounded-lg bg-white dark:bg-slate-800 shadow-lg p-6 flex flex-col items-center text-center gap-3"
                onclick={stop_propagation}
                role="dialog"
                aria-modal="true"
                aria-label="About Rustab"
            >
                <img src="128x128.png" alt="Rustab logo" class="w-16 h-16 rounded-md" />
                <h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100">{ "Rustab" }</h2>
                <p class="text-sm text-slate-500 dark:text-slate-400">
                    { format!("Version {}", env!("CARGO_PKG_VERSION")) }
                </p>
                <div class="flex flex-wrap items-center justify-center gap-2 mt-1">
                    <button type="button" onclick={on_repo_click} aria-label="GitHub repository">
                        <img
                            src="https://img.shields.io/badge/GitHub-Repository-181717?logo=github&logoColor=white"
                            alt="GitHub repository"
                            class="h-5"
                        />
                    </button>
                    <button type="button" onclick={on_issues_click} aria-label="GitHub issues">
                        <img
                            src="https://img.shields.io/badge/GitHub-Issues-181717?logo=github&logoColor=white"
                            alt="GitHub issues"
                            class="h-5"
                        />
                    </button>
                    <button type="button" onclick={on_sponsors_click} aria-label="Sponsor on GitHub Sponsors">
                        <img
                            src="https://img.shields.io/badge/Sponsor-%E2%9D%A4-db61a2?logo=githubsponsors&logoColor=white"
                            alt="Sponsor on GitHub Sponsors"
                            class="h-5"
                        />
                    </button>
                    <button type="button" onclick={on_license_click} aria-label="License (MIT or Apache-2.0)">
                        <img
                            src="https://img.shields.io/badge/License-MIT%2FApache--2.0-blue"
                            alt="License: MIT or Apache-2.0"
                            class="h-5"
                        />
                    </button>
                </div>
                <p class="text-xs text-slate-500 dark:text-slate-400 mt-2">
                    { "Author: Tomsk · " }
                    <button type="button" class="hover:underline" onclick={on_email_click}>
                        { "rustab@tomsk.dev" }
                    </button>
                </p>
                <button
                    type="button"
                    class="px-4 py-2 rounded-md bg-slate-100 dark:bg-slate-700 text-slate-700 dark:text-slate-200 text-sm font-medium focus-visible:ring-2 focus-visible:ring-sky-500"
                    onclick={on_close_click}
                >
                    { "Close" }
                </button>
            </div>
        </div>
    }
}
