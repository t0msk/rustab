//! `DiffView` — the pending-changes/diff confirmation modal (§6), plus its
//! `DiffSummaryList` and `DiffRawText` sub-views.
//!
//! No real diff is computed yet (that's T12's job) — the structured list
//! and the raw unified diff below are static placeholder content that only
//! exist to prove the modal's chrome, tabs, and footer action buttons.

use yew::prelude::*;

use crate::components::real_target_confirm_checkbox::RealTargetConfirmCheckbox;

#[derive(Clone, Copy, PartialEq, Eq)]
enum DiffTab {
    Summary,
    Raw,
}

#[derive(Properties, PartialEq)]
pub struct DiffViewProps {
    pub is_real: bool,
    pub target_path: AttrValue,
    pub confirm_checked: bool,
    pub on_confirm_checked_change: Callback<bool>,
    pub on_cancel: Callback<()>,
    pub on_confirm: Callback<()>,
}

#[function_component(DiffView)]
pub fn diff_view(props: &DiffViewProps) -> Html {
    let active_tab = use_state(|| DiffTab::Summary);

    let on_summary_tab = {
        let active_tab = active_tab.clone();
        Callback::from(move |_: MouseEvent| active_tab.set(DiffTab::Summary))
    };
    let on_raw_tab = {
        let active_tab = active_tab.clone();
        Callback::from(move |_: MouseEvent| active_tab.set(DiffTab::Raw))
    };
    let on_cancel_click = {
        let cb = props.on_cancel.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };
    let on_confirm_click = {
        let cb = props.on_confirm.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };
    let stop_propagation = Callback::from(|e: MouseEvent| e.stop_propagation());

    let confirm_disabled = props.is_real && !props.confirm_checked;
    let confirm_classes = if props.is_real {
        classes!("bg-red-600", "hover:bg-red-700", "text-white")
    } else {
        classes!("bg-sky-600", "hover:bg-sky-700", "text-white")
    };
    let confirm_label = if props.is_real {
        "Write to /etc/fstab"
    } else {
        "Save changes to sandbox file"
    };

    let badge_classes = if props.is_real {
        classes!("bg-red-600", "text-white")
    } else {
        classes!("bg-emerald-600", "text-white")
    };
    let badge_label = if props.is_real { "REAL" } else { "SANDBOX" };

    let tab_classes = |tab: DiffTab| -> Classes {
        if tab == *active_tab {
            classes!(
                "px-3",
                "py-1.5",
                "text-sm",
                "font-medium",
                "rounded-md",
                "bg-sky-100",
                "text-sky-700",
                "dark:bg-sky-900/40",
                "dark:text-sky-300"
            )
        } else {
            classes!(
                "px-3",
                "py-1.5",
                "text-sm",
                "font-medium",
                "rounded-md",
                "text-slate-500",
                "hover:text-slate-700",
                "dark:text-slate-400",
                "dark:hover:text-slate-200"
            )
        }
    };

    html! {
        // Intentionally no `onclick` here: this is the final gate before a
        // real write (§6), so it is deliberately not dismissible by
        // clicking the backdrop.
        <div class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4">
            <div
                class="w-full max-w-4xl max-h-[85vh] flex flex-col rounded-lg bg-white dark:bg-slate-800 shadow-lg"
                onclick={stop_propagation}
                role="dialog"
                aria-modal="true"
                aria-label="Review changes before saving"
            >
                <div class="shrink-0 flex items-center justify-between px-6 py-4 border-b border-slate-200 dark:border-slate-700">
                    <h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100">
                        { "Review changes before saving" }
                    </h2>
                    <span class={classes!("text-xs", "font-bold", "uppercase", "tracking-wide", "rounded-full", "px-2", "py-1", badge_classes)}>
                        { badge_label }
                    </span>
                </div>

                <div class="shrink-0 flex gap-1 px-6 pt-3">
                    <button type="button" class={tab_classes(DiffTab::Summary)} onclick={on_summary_tab}>
                        { "Summary" }
                    </button>
                    <button type="button" class={tab_classes(DiffTab::Raw)} onclick={on_raw_tab}>
                        { "Raw diff" }
                    </button>
                </div>

                <div class="flex-1 overflow-y-auto px-6 py-4">
                    if *active_tab == DiffTab::Summary {
                        <DiffSummaryList />
                    } else {
                        <DiffRawText />
                    }
                </div>

                <div class="shrink-0 border-t border-slate-200 dark:border-slate-700 px-6 py-4 space-y-3">
                    if props.is_real {
                        <RealTargetConfirmCheckbox
                            checked={props.confirm_checked}
                            on_change={props.on_confirm_checked_change.clone()}
                            label="I understand this modifies my system's real fstab and takes effect on next mount/boot."
                        />
                    }
                    <div class="flex justify-end gap-2">
                        <button
                            type="button"
                            class="px-4 py-2 rounded-md bg-slate-100 dark:bg-slate-700 text-slate-700 dark:text-slate-200 text-sm font-medium focus-visible:ring-2 focus-visible:ring-sky-500"
                            onclick={on_cancel_click}
                        >
                            { "Cancel" }
                        </button>
                        <button
                            type="button"
                            disabled={confirm_disabled}
                            class={classes!(
                                "px-4", "py-2", "rounded-md", "text-sm", "font-medium",
                                "focus-visible:ring-2", "focus-visible:ring-sky-500",
                                if confirm_disabled {
                                    classes!("bg-slate-300", "text-slate-500", "dark:bg-slate-700", "dark:text-slate-500", "cursor-not-allowed")
                                } else {
                                    confirm_classes
                                }
                            )}
                            onclick={on_confirm_click}
                        >
                            { confirm_label }
                        </button>
                    </div>
                </div>
            </div>
        </div>
    }
}

/// `DiffSummaryList` — structured per-field Added/Modified/Removed list
/// (§6). Static placeholder groups/entries for the T10 shell.
#[function_component(DiffSummaryList)]
pub fn diff_summary_list() -> Html {
    html! {
        <div class="space-y-6">
            <section>
                <h3 class="text-sm font-semibold text-slate-700 dark:text-slate-300 mb-2">{ "Added (1)" }</h3>
                <div class="rounded-md bg-emerald-50 dark:bg-emerald-900/20 px-3 py-2 text-sm font-mono text-emerald-800 dark:text-emerald-300">
                    { "UUID=9c7e2b10-aaaa-4bbb-8ccc-def012345678  /mnt/data  btrfs  rw,noatime  0  2" }
                </div>
            </section>
            <section>
                <h3 class="text-sm font-semibold text-slate-700 dark:text-slate-300 mb-2">{ "Modified (1)" }</h3>
                <div class="rounded-md border border-slate-200 dark:border-slate-700 px-3 py-2 text-sm space-y-1">
                    <p class="font-mono text-slate-600 dark:text-slate-400">{ "/ (UUID=4f3a9c2e-…)" }</p>
                    <p>
                        <span class="text-slate-500 dark:text-slate-400">{ "options: " }</span>
                        <span class="line-through text-slate-500">{ "defaults" }</span>
                        { " → " }
                        <span class="font-bold text-slate-800 dark:text-slate-100">{ "defaults,noatime" }</span>
                    </p>
                </div>
            </section>
            <section>
                <h3 class="text-sm font-semibold text-slate-700 dark:text-slate-300 mb-2">{ "Removed (1)" }</h3>
                <div class="rounded-md bg-red-50 dark:bg-red-900/20 px-3 py-2 text-sm font-mono line-through text-slate-500">
                    { "/dev/sdX1  /mnt/broken  autofs  defaults,,  0  0" }
                </div>
            </section>
        </div>
    }
}

/// `DiffRawText` — classic unified-diff rendering (§6). Static placeholder
/// lines for the T10 shell.
#[function_component(DiffRawText)]
pub fn diff_raw_text() -> Html {
    let lines: Vec<(&str, &str)> = vec![
        (" ", "# /etc/fstab: static file system information."),
        (
            " ",
            "UUID=4f3a9c2e-1234-4b9a-8d0f-abc123456789  /  ext4  defaults  0  1",
        ),
        (
            "-",
            "UUID=4f3a9c2e-1234-4b9a-8d0f-abc123456789  /  ext4  defaults  0  1",
        ),
        (
            "+",
            "UUID=4f3a9c2e-1234-4b9a-8d0f-abc123456789  /  ext4  defaults,noatime  0  1",
        ),
        (
            "+",
            "UUID=9c7e2b10-aaaa-4bbb-8ccc-def012345678  /mnt/data  btrfs  rw,noatime  0  2",
        ),
        ("-", "/dev/sdX1  /mnt/broken  autofs  defaults,,  0  0"),
    ];

    html! {
        <pre class="font-mono text-sm rounded-md bg-slate-100 dark:bg-slate-950/40 overflow-x-auto">
            { for lines.iter().enumerate().map(|(idx, (marker, text))| {
                let row_classes = match *marker {
                    "-" => classes!("bg-red-50", "text-red-700", "dark:bg-red-950/40", "dark:text-red-300"),
                    "+" => classes!("bg-emerald-50", "text-emerald-700", "dark:bg-emerald-950/40", "dark:text-emerald-300"),
                    _ => classes!("text-slate-600", "dark:text-slate-400"),
                };
                html! {
                    <div class={classes!("flex", "px-3", row_classes)}>
                        <span class="w-8 shrink-0 text-right pr-2 text-slate-400 select-none">{ (idx + 1).to_string() }</span>
                        <span class="w-4 shrink-0 select-none">{ *marker }</span>
                        <span class="whitespace-pre">{ *text }</span>
                    </div>
                }
            }) }
        </pre>
    }
}
