//! `EntryDetailDrawer` — add/edit form container (§4).
//!
//! Slides in from the right, fixed width 520px, backdrop dismissible by
//! click (unlike `DiffView`/`BackupRestoreConfirmDialog`), since this only
//! ever stages a non-destructive in-progress edit.
//!
//! Field *values* here are local, in-memory scratch state only: this
//! component does not read from or write to the mock entries table, the
//! `PendingChangesBar` count, or any backend — that data wiring is T11's
//! job. The interactivity that does exist (source-type switching, options
//! chip toggles, etc.) exists purely to prove each field's visual
//! structure and behavior per §4/§8, not to persist anything.

mod device_picker;
mod directory_picker_button;
mod dump_pass_fields;
mod form_action_bar;
mod fs_type_field;
mod mount_point_field;
mod options_tag_input;
mod source_type_select;
mod validation_message;
mod validation_summary;

pub use form_action_bar::FormActionBar;
pub use source_type_select::SourceType;
pub use validation_summary::ValidationSummary;

// `validation_message` (`ValidationMessage`/`Severity`) is intentionally not
// re-exported yet: nothing in this T10 shell has real validation results to
// render per-field (that's T11's job, see docs/UI_DESIGN.md §5). The module
// itself carries `#![allow(dead_code)]` for the same reason.

use web_sys::{HtmlInputElement, HtmlTextAreaElement};
use yew::prelude::*;

use device_picker::DevicePicker;
use dump_pass_fields::DumpPassFields;
use fs_type_field::FsTypeField;
use mount_point_field::MountPointField;
use options_tag_input::OptionsTagInput;
use source_type_select::SourceTypeSelect;

use crate::mock::mock_devices;

/// Which mode the drawer is open in. Owned by `AppShell`.
#[derive(Clone, PartialEq)]
pub enum DrawerMode {
    Add,
    /// Edit an existing, successfully-parsed entry.
    EditStructured,
    /// Edit a line the parser couldn't fully understand: a single raw-text
    /// field instead of the normal structured form (§3).
    EditRaw(AttrValue),
}

#[derive(Properties, PartialEq)]
pub struct EntryDetailDrawerProps {
    pub mode: DrawerMode,
    pub on_close: Callback<()>,
    pub on_save: Callback<()>,
}

#[function_component(EntryDetailDrawer)]
pub fn entry_detail_drawer(props: &EntryDetailDrawerProps) -> Html {
    let source_type = use_state(|| SourceType::Uuid);
    let source_value = use_state(String::new);
    let mount_point = use_state(String::new);
    let fs_type = use_state(String::new);
    let chips = use_state(|| vec!["defaults".to_string(), "noatime".to_string()]);
    let dump = use_state(|| 0u8);
    let pass = use_state(|| 0u8);
    let raw_text = use_state(|| match &props.mode {
        DrawerMode::EditRaw(raw) => raw.to_string(),
        _ => String::new(),
    });

    let title = match &props.mode {
        DrawerMode::Add => "Add entry",
        DrawerMode::EditStructured => "Edit entry",
        DrawerMode::EditRaw(_) => "Edit raw entry",
    };

    let on_backdrop_click = {
        let cb = props.on_close.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };
    let stop_propagation = Callback::from(|e: MouseEvent| e.stop_propagation());

    html! {
        <div class="fixed inset-0 z-50 flex justify-end bg-black/40" onclick={on_backdrop_click}>
            <div
                class="h-full w-full sm:w-[520px] shrink-0 flex flex-col bg-white dark:bg-slate-800 shadow-lg"
                onclick={stop_propagation}
                role="dialog"
                aria-modal="true"
                aria-label={title}
            >
                <div class="shrink-0 flex items-center justify-between px-6 py-4 border-b border-slate-200 dark:border-slate-700">
                    <h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100">{ title }</h2>
                    <button
                        type="button"
                        aria-label="Close"
                        class="p-2 rounded-md hover:bg-slate-100 dark:hover:bg-slate-700 focus-visible:ring-2 focus-visible:ring-sky-500"
                        onclick={{
                            let cb = props.on_close.clone();
                            Callback::from(move |_: MouseEvent| cb.emit(()))
                        }}
                    >
                        <span aria-hidden="true">{ "×" }</span>
                    </button>
                </div>

                <div class="flex-1 overflow-y-auto px-6 py-6">
                    <ValidationSummary errors={Vec::<AttrValue>::new()} />

                    if let DrawerMode::EditRaw(_) = &props.mode {
                        <div>
                            <label class="block text-sm font-medium text-slate-700 dark:text-slate-300 mb-1" for="raw-line-textarea">
                                { "Raw line" }
                            </label>
                            <p class="text-xs text-slate-500 dark:text-slate-400 mb-2">
                                { "Structured fields can't be offered for a line the parser couldn't decompose — edit the raw text directly." }
                            </p>
                            <textarea
                                id="raw-line-textarea"
                                rows="4"
                                class="w-full rounded-md border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-900 font-mono text-sm px-3 py-2 focus-visible:ring-2 focus-visible:ring-sky-500"
                                value={(*raw_text).clone()}
                                oninput={{
                                    let raw_text = raw_text.clone();
                                    Callback::from(move |e: InputEvent| {
                                        if let Some(textarea) = e.target_dyn_into::<HtmlTextAreaElement>() {
                                            raw_text.set(textarea.value());
                                        }
                                    })
                                }}
                            ></textarea>
                        </div>
                    } else {
                        <div class="space-y-4">
                            <SourceTypeSelect
                                value={*source_type}
                                on_change={{
                                    let source_type = source_type.clone();
                                    Callback::from(move |value: SourceType| source_type.set(value))
                                }}
                            />

                            { render_source_value_field(&source_type, &source_value) }

                            <MountPointField
                                value={AttrValue::from((*mount_point).clone())}
                                on_change={{
                                    let mount_point = mount_point.clone();
                                    Callback::from(move |value: String| mount_point.set(value))
                                }}
                            />

                            <FsTypeField
                                value={AttrValue::from((*fs_type).clone())}
                                on_change={{
                                    let fs_type = fs_type.clone();
                                    Callback::from(move |value: String| fs_type.set(value))
                                }}
                            />

                            <OptionsTagInput
                                chips={(*chips).clone()}
                                on_change={{
                                    let chips = chips.clone();
                                    Callback::from(move |value: Vec<String>| chips.set(value))
                                }}
                            />

                            <DumpPassFields
                                dump={*dump}
                                pass={*pass}
                                on_dump_change={{
                                    let dump = dump.clone();
                                    Callback::from(move |value: u8| dump.set(value))
                                }}
                                on_pass_change={{
                                    let pass = pass.clone();
                                    Callback::from(move |value: u8| pass.set(value))
                                }}
                            />
                        </div>
                    }
                </div>

                <FormActionBar on_cancel={props.on_close.clone()} on_save={props.on_save.clone()} />
            </div>
        </div>
    }
}

fn render_source_value_field(
    source_type: &UseStateHandle<SourceType>,
    source_value: &UseStateHandle<String>,
) -> Html {
    let on_input = {
        let source_value = source_value.clone();
        Callback::from(move |e: InputEvent| {
            if let Some(input) = e.target_dyn_into::<HtmlInputElement>() {
                source_value.set(input.value());
            }
        })
    };

    if **source_type == SourceType::None {
        return html! {
            <div>
                <label class="block text-sm font-medium text-slate-700 dark:text-slate-300 mb-1">{ "Source value" }</label>
                <input
                    type="text"
                    disabled=true
                    placeholder="none"
                    class="w-full rounded-md border border-slate-300 dark:border-slate-600 bg-slate-100 dark:bg-slate-900 text-sm px-3 py-2 text-slate-400"
                />
                <p class="text-xs text-slate-500 dark:text-slate-400 mt-1">
                    { "Used for pseudo-filesystems (tmpfs, proc) or swap entries without a discrete backing device." }
                </p>
            </div>
        };
    }

    let label = if source_type.is_identifier() {
        "Source value (pick a discovered device or type manually)"
    } else {
        "Source value"
    };

    html! {
        <div>
            <label class="block text-sm font-medium text-slate-700 dark:text-slate-300 mb-1" for="source-value-input">
                { label }
            </label>
            <div class="flex gap-2">
                <input
                    id="source-value-input"
                    type="text"
                    class="flex-1 rounded-md border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-900 font-mono text-sm px-3 py-2 focus-visible:ring-2 focus-visible:ring-sky-500"
                    value={(**source_value).clone()}
                    oninput={on_input}
                />
            </div>
            <DevicePicker
                devices={mock_devices()}
                on_pick={{
                    let source_value = source_value.clone();
                    Callback::from(move |value: String| source_value.set(value))
                }}
            />
        </div>
    }
}
