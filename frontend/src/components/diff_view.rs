//! `DiffView` - the pending-changes/diff confirmation modal (§6), plus its
//! `DiffSummaryList` and `DiffRawText` sub-views.
//!
//! Purely presentational: `AppShell` computes the Added/Modified/
//! Reordered/Removed grouping (`crate::diff::compute_summary_diff`) and the raw unified diff
//! (`crate::diff::compute_raw_diff`, fed by the real `render_candidate`
//! call) and hands both down as props, along with the save-in-flight/error
//! state for the confirm button (T12) and the two target flags below
//! (`writes_real_file`/`is_real`), which since T34 are not the same question:
//! see `docs/UI_DESIGN.md` §2/§6.

use yew::prelude::*;

use crate::components::loading_spinner::LoadingSpinner;
use crate::components::real_target_confirm_checkbox::{
    confirm_disabled, RealTargetConfirmCheckbox,
};
use crate::diff::{position_sentence, RawDiffLine, SummaryDiff};

#[derive(Clone, Copy, PartialEq, Eq)]
enum DiffTab {
    Summary,
    Raw,
}

#[derive(Properties, PartialEq)]
pub struct DiffViewProps {
    /// `true` when confirming here writes the real `/etc/fstab` - which,
    /// since T34, is every launch except an explicit non-real
    /// `RUSTAB_TARGET_PATH` override (`AppShell` derives it once; see
    /// `real_target_confirm_checkbox::confirm_disabled`). Drives the
    /// confirmation gate, the confirm button's color and its label: the
    /// consequence of pressing the button is identical whether the file on
    /// screen is `/etc/fstab` itself or the draft it is saved through.
    pub writes_real_file: bool,
    /// Whether `target_path` *is* the real `/etc/fstab`. Only ever used for
    /// the header badge's wording, which has to describe the file being
    /// shown, not the consequence of the button (§2/§6).
    pub is_real: bool,
    pub target_path: AttrValue,
    pub confirm_checked: bool,
    pub on_confirm_checked_change: Callback<bool>,
    pub on_cancel: Callback<()>,
    pub on_confirm: Callback<()>,
    pub summary: SummaryDiff,
    /// `None` while `render_candidate` hasn't resolved yet.
    pub raw_diff: Option<Vec<RawDiffLine>>,
    /// Set by `AppShell` while the `save` call is in flight (§6 point 4).
    #[prop_or(false)]
    pub saving: bool,
    /// The last `save` failure's message, shown inside this still-open
    /// modal (not only as a toast) per §6 point 4.
    #[prop_or_default]
    pub error: Option<AttrValue>,
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

    let confirm_disabled =
        confirm_disabled(props.writes_real_file, props.confirm_checked, props.saving);
    let confirm_classes = if props.writes_real_file {
        classes!("bg-red-600", "hover:bg-red-700", "text-white")
    } else {
        classes!("bg-sky-600", "hover:bg-sky-700", "text-white")
    };
    let confirm_label = if props.writes_real_file {
        "Write to /etc/fstab"
    } else {
        "Save changes to test file"
    };

    // Three states, matching the TargetBanner's (§2): the real file itself,
    // a draft that is saved *through* to the real file, and an isolated
    // override. The badge sits next to `target_path`, so it describes which
    // file the save lands in - which for the middle case is deliberately
    // not the path shown beside it.
    let (badge_classes, badge_label) = match (props.is_real, props.writes_real_file) {
        (true, _) => (classes!("bg-red-600", "text-white"), "REAL"),
        (false, true) => (classes!("bg-amber-600", "text-white"), "WRITES /etc/fstab"),
        (false, false) => (classes!("bg-emerald-600", "text-white"), "TEST FILE"),
    };

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
                    <div class="flex items-center gap-2">
                        <span class="text-xs font-mono text-slate-500 dark:text-slate-400 truncate max-w-xs" title={props.target_path.clone()}>
                            { props.target_path.clone() }
                        </span>
                        <span class={classes!("text-xs", "font-bold", "uppercase", "tracking-wide", "rounded-full", "px-2", "py-1", badge_classes)}>
                            { badge_label }
                        </span>
                    </div>
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
                    if let Some(error) = &props.error {
                        <div class="mb-4 rounded-md border border-red-300 dark:border-red-800 bg-red-50 dark:bg-red-900/20 text-red-800 dark:text-red-300 px-3 py-2 text-sm" role="alert">
                            { error.clone() }
                        </div>
                    }
                    if *active_tab == DiffTab::Summary {
                        <DiffSummaryList summary={props.summary.clone()} />
                    } else {
                        <DiffRawText lines={props.raw_diff.clone()} />
                    }
                </div>

                <div class="shrink-0 border-t border-slate-200 dark:border-slate-700 px-6 py-4 space-y-3">
                    if props.writes_real_file {
                        <RealTargetConfirmCheckbox
                            checked={props.confirm_checked}
                            on_change={props.on_confirm_checked_change.clone()}
                            label="I understand this modifies my system's real fstab and takes effect on next mount/boot."
                        />
                    }
                    <div class="flex justify-end gap-2">
                        <button
                            type="button"
                            disabled={props.saving}
                            class="px-4 py-2 rounded-md bg-slate-100 dark:bg-slate-700 text-slate-700 dark:text-slate-200 text-sm font-medium focus-visible:ring-2 focus-visible:ring-sky-500 disabled:opacity-60 disabled:cursor-not-allowed"
                            onclick={on_cancel_click}
                        >
                            { "Cancel" }
                        </button>
                        <button
                            type="button"
                            disabled={confirm_disabled}
                            class={classes!(
                                "px-4", "py-2", "rounded-md", "text-sm", "font-medium", "inline-flex", "items-center", "gap-2",
                                "focus-visible:ring-2", "focus-visible:ring-sky-500",
                                if confirm_disabled {
                                    classes!("bg-slate-300", "text-slate-500", "dark:bg-slate-700", "dark:text-slate-500", "cursor-not-allowed")
                                } else {
                                    confirm_classes
                                }
                            )}
                            onclick={on_confirm_click}
                        >
                            if props.saving {
                                <LoadingSpinner />
                                { "Writing changes…" }
                            } else {
                                { confirm_label }
                            }
                        </button>
                    </div>
                </div>
            </div>
        </div>
    }
}

/// `DiffSummaryList` - structured per-field Added/Modified/Reordered/
/// Removed list (§6), rendered from the real diff `AppShell` computes. The
/// `Reordered` group is T40's (§13.7); an entry that was both edited and
/// moved appears only under `Modified`, with its move as one note line.
#[derive(Properties, PartialEq)]
pub struct DiffSummaryListProps {
    pub summary: SummaryDiff,
}

#[function_component(DiffSummaryList)]
pub fn diff_summary_list(props: &DiffSummaryListProps) -> Html {
    let summary = &props.summary;
    if summary.added.is_empty()
        && summary.modified.is_empty()
        && summary.reordered.is_empty()
        && summary.removed.is_empty()
    {
        return html! {
            <p class="text-sm text-slate-500 dark:text-slate-400">{ "No changes." }</p>
        };
    }

    html! {
        <div class="space-y-6">
            if !summary.added.is_empty() {
                <section>
                    <h3 class="text-sm font-semibold text-slate-700 dark:text-slate-300 mb-2">
                        { format!("Added ({})", summary.added.len()) }
                    </h3>
                    <div class="space-y-2">
                        { for summary.added.iter().map(|line| html! {
                            <div class="rounded-md bg-emerald-50 dark:bg-emerald-900/20 px-3 py-2 text-sm font-mono text-emerald-800 dark:text-emerald-300">
                                { line.clone() }
                            </div>
                        }) }
                    </div>
                </section>
            }
            if !summary.modified.is_empty() {
                <section>
                    <h3 class="text-sm font-semibold text-slate-700 dark:text-slate-300 mb-2">
                        { format!("Modified ({})", summary.modified.len()) }
                    </h3>
                    <div class="space-y-2">
                        { for summary.modified.iter().map(|modified| html! {
                            <div class="rounded-md border border-slate-200 dark:border-slate-700 px-3 py-2 text-sm space-y-1">
                                <p class="font-mono text-slate-600 dark:text-slate-400">{ modified.summary.clone() }</p>
                                { for modified.changed_fields.iter().map(|field| html! {
                                    <p>
                                        <span class="text-slate-500 dark:text-slate-400">{ format!("{}: ", field.label) }</span>
                                        <span class="line-through text-slate-500">{ field.old.clone() }</span>
                                        { " → " }
                                        <span class="font-bold text-slate-800 dark:text-slate-100">{ field.new.clone() }</span>
                                    </p>
                                }) }
                                if let Some(note) = modified.position_note.clone() {
                                    <p class="text-slate-500 dark:text-slate-400 italic">{ note }</p>
                                }
                            </div>
                        }) }
                    </div>
                </section>
            }
            if !summary.reordered.is_empty() {
                <section>
                    <h3 class="text-sm font-semibold text-slate-700 dark:text-slate-300 mb-2">
                        { format!("Reordered ({})", summary.reordered.len()) }
                    </h3>
                    <div class="space-y-2">
                        { for summary.reordered.iter().map(|line| html! {
                            <div class="rounded-md bg-sky-50 dark:bg-sky-900/20 px-3 py-2 text-sm space-y-1">
                                <p class="font-mono text-sky-800 dark:text-sky-300">{ line.summary.clone() }</p>
                                <p class="text-slate-500 dark:text-slate-400">
                                    { position_sentence(line.old_position, line.new_position) }
                                </p>
                            </div>
                        }) }
                    </div>
                </section>
            }
            if !summary.removed.is_empty() {
                <section>
                    <h3 class="text-sm font-semibold text-slate-700 dark:text-slate-300 mb-2">
                        { format!("Removed ({})", summary.removed.len()) }
                    </h3>
                    <div class="space-y-2">
                        { for summary.removed.iter().map(|line| html! {
                            <div class="rounded-md bg-red-50 dark:bg-red-900/20 px-3 py-2 text-sm font-mono line-through text-slate-500">
                                { line.clone() }
                            </div>
                        }) }
                    </div>
                </section>
            }
        </div>
    }
}

/// `DiffRawText` - classic unified-diff rendering (§6), fed by
/// `crate::diff::compute_raw_diff`.
#[derive(Properties, PartialEq)]
pub struct DiffRawTextProps {
    /// `None` while `render_candidate` hasn't resolved yet.
    pub lines: Option<Vec<RawDiffLine>>,
}

#[function_component(DiffRawText)]
pub fn diff_raw_text(props: &DiffRawTextProps) -> Html {
    let Some(lines) = &props.lines else {
        return html! {
            <p class="text-sm text-slate-500 dark:text-slate-400">{ "Rendering the candidate file…" }</p>
        };
    };
    if lines.is_empty() {
        return html! {
            <p class="text-sm text-slate-500 dark:text-slate-400">{ "No textual differences." }</p>
        };
    }

    html! {
        <pre class="font-mono text-sm rounded-md bg-slate-100 dark:bg-slate-950/40 overflow-x-auto">
            { for lines.iter().enumerate().map(|(idx, line)| {
                let row_classes = match line.marker {
                    "-" => classes!("bg-red-50", "text-red-700", "dark:bg-red-950/40", "dark:text-red-300"),
                    "+" => classes!("bg-emerald-50", "text-emerald-700", "dark:bg-emerald-950/40", "dark:text-emerald-300"),
                    _ => classes!("text-slate-600", "dark:text-slate-400"),
                };
                html! {
                    <div class={classes!("flex", "px-3", row_classes)}>
                        <span class="w-8 shrink-0 text-right pr-2 text-slate-400 select-none">{ (idx + 1).to_string() }</span>
                        <span class="w-4 shrink-0 select-none">{ line.marker }</span>
                        <span class="whitespace-pre">{ line.text.clone() }</span>
                    </div>
                }
            }) }
        </pre>
    }
}
