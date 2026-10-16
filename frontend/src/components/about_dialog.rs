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

/// Inline SVGs for the link buttons below, rather than external
/// `img.shields.io` badge images: those render at whatever width their own
/// text needs, which made the four buttons visually uneven widths; a fixed
/// Tailwind width (`w-52`) on each button only works cleanly with an
/// in-markup icon, not an externally-sized image. This also means the About
/// dialog no longer needs network access to render correctly.
#[function_component(GithubMarkIcon)]
fn github_mark_icon() -> Html {
    html! {
        <svg viewBox="0 0 16 16" width="16" height="16" class="shrink-0 fill-current" aria-hidden="true">
            <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0 0 16 8c0-4.42-3.58-8-8-8Z" />
        </svg>
    }
}

#[function_component(HeartIcon)]
fn heart_icon() -> Html {
    html! {
        <svg viewBox="0 0 16 16" width="16" height="16" class="shrink-0 fill-current" aria-hidden="true">
            <path d="M8 14.25c-.2 0-.39-.07-.54-.2C4.3 11.57 2 9.37 2 6.5 2 4.57 3.57 3 5.5 3c1.15 0 2.17.56 2.5 1.42C8.33 3.56 9.35 3 10.5 3 12.43 3 14 4.57 14 6.5c0 2.87-2.3 5.07-5.46 7.55-.15.13-.34.2-.54.2Z" />
        </svg>
    }
}

#[function_component(ScaleIcon)]
fn scale_icon() -> Html {
    html! {
        <svg viewBox="0 0 16 16" width="16" height="16" class="shrink-0 fill-current" aria-hidden="true">
            <path d="M8 1a.75.75 0 0 1 .75.75V2h2.5a.75.75 0 0 1 0 1.5h-.214l1.714 3.5a.75.75 0 0 1-.169.868c-.47.42-1.182.632-1.831.632s-1.36-.212-1.831-.632a.75.75 0 0 1-.169-.868L9.464 3.5H8.75v9.25h2.5a.75.75 0 0 1 0 1.5h-6.5a.75.75 0 0 1 0-1.5h2.5V3.5h-.714l1.714 3.5a.75.75 0 0 1-.169.868c-.47.42-1.182.632-1.831.632s-1.36-.212-1.831-.632a.75.75 0 0 1-.169-.868L5.964 3.5H5.75a.75.75 0 0 1 0-1.5h2.5v-.25A.75.75 0 0 1 8 1ZM3.106 7.236 4.25 4.857l1.144 2.379a1.98 1.98 0 0 1-1.144.332 1.98 1.98 0 0 1-1.144-.332Zm7.5 0L11.75 4.857l1.144 2.379a1.98 1.98 0 0 1-1.144.332 1.98 1.98 0 0 1-1.144-.332Z" />
        </svg>
    }
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
                class="w-full max-w-sm rounded-lg bg-white dark:bg-slate-800 shadow-lg p-8 flex flex-col items-center text-center gap-4"
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
                <div class="flex flex-col items-center gap-6 mt-1">
                    <button
                        type="button"
                        class="w-52 flex items-center gap-2 px-3 py-2 rounded-md bg-slate-700 hover:bg-slate-600 text-white text-sm font-medium focus-visible:ring-2 focus-visible:ring-sky-500"
                        onclick={on_repo_click}
                    >
                        <GithubMarkIcon />
                        { "GitHub Repository" }
                    </button>
                    <button
                        type="button"
                        class="w-52 flex items-center gap-2 px-3 py-2 rounded-md bg-slate-700 hover:bg-slate-600 text-white text-sm font-medium focus-visible:ring-2 focus-visible:ring-sky-500"
                        onclick={on_issues_click}
                    >
                        <GithubMarkIcon />
                        { "GitHub Issues" }
                    </button>
                    <button
                        type="button"
                        class="w-52 flex items-center gap-2 px-3 py-2 rounded-md bg-pink-600 hover:bg-pink-700 text-white text-sm font-medium focus-visible:ring-2 focus-visible:ring-sky-500"
                        onclick={on_sponsors_click}
                    >
                        <HeartIcon />
                        { "GitHub Sponsors" }
                    </button>
                    <button
                        type="button"
                        class="w-52 flex items-center gap-2 px-3 py-2 rounded-md bg-sky-600 hover:bg-sky-700 text-white text-sm font-medium focus-visible:ring-2 focus-visible:ring-sky-500"
                        onclick={on_license_click}
                    >
                        <ScaleIcon />
                        { "License (MIT/Apache)" }
                    </button>
                </div>
                <p class="text-xs text-slate-500 dark:text-slate-400 mt-2">
                    { "Author: Tomáš \"tomsk\" Sabo · " }
                    <button type="button" class="hover:underline" onclick={on_email_click}>
                        { "opensource@tomsk.dev" }
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
