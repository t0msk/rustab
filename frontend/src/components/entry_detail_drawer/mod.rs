//! `EntryDetailDrawer` - add/edit form container (§4).
//!
//! Slides in from the right, fixed width 520px, backdrop dismissible by
//! click (unlike `DiffView`/`BackupRestoreConfirmDialog`), since it only
//! ever stages a non-destructive in-progress edit.
//!
//! Wired (T11): prefilled from the real entry being edited, device
//! discovery for `DevicePicker`, and inline validation via
//! `validate_candidate` run against the *whole* candidate document (this
//! draft entry plus every other line already staged), per
//! `docs/API_CONTRACT.md` §3.3 - duplicate-mount-point-style checks need
//! the full document, not just this one entry. Clicking "Save entry" never
//! calls a write command: it emits the finished `EntryDto`/raw text back
//! to `AppShell`, which stages it into the in-memory candidate document
//! (see `crate::candidate`). Disk writes only ever happen through T12's
//! diff-confirm flow.

mod description_field;
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

use web_sys::{HtmlInputElement, HtmlTextAreaElement};
use yew::prelude::*;

use description_field::DescriptionField;
use device_picker::DevicePicker;
use dump_pass_fields::DumpPassFields;
use fs_type_field::FsTypeField;
use mount_point_field::MountPointField;
use options_tag_input::OptionsTagInput;
use source_type_select::SourceTypeSelect;
use validation_message::{Severity, ValidationMessage};

use crate::api;
use crate::components::status_toast::ToastKind;
use crate::dto::{
    DeviceInfoDto, DocumentDto, EntryDto, FindingSeverity, LineDto, OptionDto, SourceDto,
    ValidationField, ValidationReportDto,
};

/// Which mode the drawer is open in. Owned by `AppShell`.
#[derive(Clone, PartialEq)]
pub enum DrawerMode {
    Add,
    /// Edit an existing, successfully-parsed entry. Carries the entry so
    /// the form can be prefilled with its real current values.
    EditStructured(EntryDto),
    /// Edit a line the parser couldn't fully understand: a single raw-text
    /// field instead of the normal structured form (§3).
    EditRaw(AttrValue),
}

/// What the drawer hands back to `AppShell` once the user confirms Save
/// and validation allows proceeding. `AppShell` is the one that knows
/// *where* (which candidate-document index, or append) this belongs.
#[derive(Clone, PartialEq)]
pub enum DrawerSaveResult {
    Entry(EntryDto),
    Raw(String),
}

#[derive(Properties, PartialEq)]
pub struct EntryDetailDrawerProps {
    pub mode: DrawerMode,
    /// Every other line in the current candidate document (i.e. the whole
    /// document minus the one line this drawer is adding/editing), used to
    /// build the full candidate `validate_candidate` checks against.
    pub other_lines: Vec<LineDto>,
    pub on_close: Callback<()>,
    pub on_save: Callback<DrawerSaveResult>,
    /// Lets the drawer's own Verify action show an on-demand confirmation
    /// toast; owned/rendered by `AppShell`, same callback it already
    /// threads to other actions.
    pub show_toast: Callback<(ToastKind, String)>,
}

fn chips_to_options(chips: &[String]) -> Vec<OptionDto> {
    chips
        .iter()
        .map(|chip| match chip.find('=') {
            Some(pos) => OptionDto {
                key: chip[..pos].to_string(),
                value: Some(chip[pos + 1..].to_string()),
            },
            None => OptionDto {
                key: chip.clone(),
                value: None,
            },
        })
        .collect()
}

fn options_to_chips(options: &[OptionDto]) -> Vec<String> {
    options
        .iter()
        .map(|option| match &option.value {
            Some(value) => format!("{}={}", option.key, value),
            None => option.key.clone(),
        })
        .collect()
}

#[function_component(EntryDetailDrawer)]
pub fn entry_detail_drawer(props: &EntryDetailDrawerProps) -> Html {
    let editing_entry: Option<EntryDto> = match &props.mode {
        DrawerMode::EditStructured(entry) => Some(entry.clone()),
        _ => None,
    };

    let source_type = use_state(|| {
        editing_entry
            .as_ref()
            .map(|e| SourceType::from_dto_kind(e.source.kind))
            .unwrap_or(SourceType::Uuid)
    });
    let source_value = use_state(|| {
        editing_entry
            .as_ref()
            .map(|e| e.source.value.clone())
            .unwrap_or_default()
    });
    let mount_point = use_state(|| {
        editing_entry
            .as_ref()
            .map(|e| e.mount_point.clone())
            .unwrap_or_default()
    });
    let fs_type = use_state(|| {
        editing_entry
            .as_ref()
            .map(|e| e.fs_type.clone())
            .unwrap_or_default()
    });
    let chips = use_state(|| {
        editing_entry
            .as_ref()
            .map(|e| options_to_chips(&e.options))
            .unwrap_or_else(|| vec!["defaults".to_string(), "noatime".to_string()])
    });
    let dump = use_state(|| editing_entry.as_ref().map(|e| e.dump).unwrap_or(0));
    let pass = use_state(|| editing_entry.as_ref().map(|e| e.pass).unwrap_or(0));
    // Preserved losslessly but not user-editable in this form (no field
    // for it in `docs/UI_DESIGN.md` §4) - carried through unchanged.
    let trailing_comment = use_state(|| {
        editing_entry
            .as_ref()
            .and_then(|e| e.trailing_comment.clone())
    });
    let description = use_state(|| editing_entry.as_ref().and_then(|e| e.description.clone()));
    let raw_text = use_state(|| match &props.mode {
        DrawerMode::EditRaw(raw) => raw.to_string(),
        _ => String::new(),
    });

    let is_raw_mode = matches!(props.mode, DrawerMode::EditRaw(_));

    // Discovered devices for `DevicePicker`, loaded once per drawer open.
    // `lsblk_available` defaults to `true` so the brief window before the
    // response arrives reads as "no devices discovered yet", not a false
    // "unavailable" (API_CONTRACT.md §3.8 / docs/SPEC.md §8).
    let devices = use_state(Vec::<DeviceInfoDto>::new);
    let lsblk_available = use_state(|| true);
    {
        let devices = devices.clone();
        let lsblk_available = lsblk_available.clone();
        use_effect_with((), move |_| {
            wasm_bindgen_futures::spawn_local(async move {
                match api::discover_devices().await {
                    Ok(discovery) => {
                        devices.set(discovery.devices);
                        lsblk_available.set(discovery.lsblk_available);
                    }
                    Err(_) => lsblk_available.set(false),
                }
            });
            || ()
        });
    }

    // Inline validation (§5), debounced (API_CONTRACT.md §3.3: "findmnt is
    // an external process... don't call it per keystroke") and run against
    // the whole candidate document, not just this entry.
    let validation = use_state(|| Option::<ValidationReportDto>::None);
    let validation_request = use_mut_ref(|| 0u64);
    {
        // Hooks must run unconditionally every render (Yew tracks them by
        // call order) - `is_raw_mode` is folded into the dependency tuple
        // and checked *inside* the effect instead of guarding the
        // `use_effect_with` call itself.
        let validation = validation.clone();
        let validation_request = validation_request.clone();
        let other_lines = props.other_lines.clone();
        let draft = build_entry(
            &source_type,
            &source_value,
            &mount_point,
            &fs_type,
            &chips,
            *dump,
            *pass,
            &trailing_comment,
            &description,
        );
        use_effect_with(
            (
                is_raw_mode,
                *source_type,
                (*source_value).clone(),
                (*mount_point).clone(),
                (*fs_type).clone(),
                (*chips).clone(),
                *dump,
                *pass,
                other_lines.clone(),
                (*description).clone(),
            ),
            move |_| {
                if !is_raw_mode {
                    let request_id = {
                        let mut counter = validation_request.borrow_mut();
                        *counter += 1;
                        *counter
                    };
                    let document = DocumentDto {
                        lines: other_lines
                            .iter()
                            .cloned()
                            .chain(std::iter::once(LineDto::Entry { entry: draft }))
                            .collect(),
                    };
                    wasm_bindgen_futures::spawn_local(async move {
                        gloo_timers::future::TimeoutFuture::new(400).await;
                        if *validation_request.borrow() != request_id {
                            return;
                        }
                        if let Ok(report) = api::validate_candidate(&document).await {
                            if *validation_request.borrow() == request_id {
                                validation.set(Some(report));
                            }
                        }
                    });
                }
                || ()
            },
        );
    }

    let has_blocking_errors = validation.as_ref().map(|r| r.has_errors).unwrap_or(false);
    let saving = use_state(|| false);
    let verifying = use_state(|| false);

    let title = match &props.mode {
        DrawerMode::Add => "Add entry",
        DrawerMode::EditStructured(_) => "Edit entry",
        DrawerMode::EditRaw(_) => "Edit raw entry",
    };

    let on_backdrop_click = {
        let cb = props.on_close.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };
    let stop_propagation = Callback::from(|e: MouseEvent| e.stop_propagation());

    let on_save_click = {
        let on_save = props.on_save.clone();
        let other_lines = props.other_lines.clone();
        let source_type = source_type.clone();
        let source_value = source_value.clone();
        let mount_point = mount_point.clone();
        let fs_type = fs_type.clone();
        let chips = chips.clone();
        let dump = dump.clone();
        let pass = pass.clone();
        let trailing_comment = trailing_comment.clone();
        let description = description.clone();
        let raw_text = raw_text.clone();
        let validation = validation.clone();
        let validation_request = validation_request.clone();
        let saving = saving.clone();
        Callback::from(move |()| {
            if is_raw_mode {
                on_save.emit(DrawerSaveResult::Raw((*raw_text).clone()));
                return;
            }

            let draft = build_entry(
                &source_type,
                &source_value,
                &mount_point,
                &fs_type,
                &chips,
                *dump,
                *pass,
                &trailing_comment,
                &description,
            );
            let document = DocumentDto {
                lines: other_lines
                    .iter()
                    .cloned()
                    .chain(std::iter::once(LineDto::Entry {
                        entry: draft.clone(),
                    }))
                    .collect(),
            };

            let on_save = on_save.clone();
            let validation = validation.clone();
            let validation_request = validation_request.clone();
            let saving = saving.clone();
            saving.set(true);
            wasm_bindgen_futures::spawn_local(async move {
                // Authoritative re-check at the moment of Save click (§5:
                // "every attempted Save click"), independent of whether
                // the debounced background check has caught up yet.
                let request_id = {
                    let mut counter = validation_request.borrow_mut();
                    *counter += 1;
                    *counter
                };
                match api::validate_candidate(&document).await {
                    Ok(report) => {
                        let has_errors = report.has_errors;
                        if *validation_request.borrow() == request_id {
                            validation.set(Some(report));
                        }
                        saving.set(false);
                        if !has_errors {
                            on_save.emit(DrawerSaveResult::Entry(draft));
                        }
                    }
                    Err(message) => {
                        // A rejection here is a frontend bug per the
                        // contract ("does not reject") - fail closed
                        // rather than silently staging an unvalidated
                        // entry.
                        if *validation_request.borrow() == request_id {
                            validation.set(Some(ValidationReportDto {
                                findings: vec![crate::dto::FindingDto {
                                    severity: FindingSeverity::Error,
                                    message: format!("Could not validate this entry: {message}"),
                                    field: None,
                                    line_index: None,
                                }],
                                has_errors: true,
                            }));
                        }
                        saving.set(false);
                    }
                }
            });
        })
    };

    let on_verify_click = {
        let other_lines = props.other_lines.clone();
        let source_type = source_type.clone();
        let source_value = source_value.clone();
        let mount_point = mount_point.clone();
        let fs_type = fs_type.clone();
        let chips = chips.clone();
        let dump = dump.clone();
        let pass = pass.clone();
        let trailing_comment = trailing_comment.clone();
        let description = description.clone();
        let validation = validation.clone();
        let validation_request = validation_request.clone();
        let verifying = verifying.clone();
        let show_toast = props.show_toast.clone();
        Callback::from(move |()| {
            let draft = build_entry(
                &source_type,
                &source_value,
                &mount_point,
                &fs_type,
                &chips,
                *dump,
                *pass,
                &trailing_comment,
                &description,
            );
            let document = DocumentDto {
                lines: other_lines
                    .iter()
                    .cloned()
                    .chain(std::iter::once(LineDto::Entry { entry: draft }))
                    .collect(),
            };

            let validation = validation.clone();
            let validation_request = validation_request.clone();
            let verifying = verifying.clone();
            let show_toast = show_toast.clone();
            // Any in-flight debounced check becomes stale once this
            // immediate re-check starts - bump the shared cancellation
            // counter so its eventual result is ignored when it lands.
            let request_id = {
                let mut counter = validation_request.borrow_mut();
                *counter += 1;
                *counter
            };
            verifying.set(true);
            wasm_bindgen_futures::spawn_local(async move {
                match api::validate_candidate(&document).await {
                    Ok(report) => {
                        let error_count = report
                            .findings
                            .iter()
                            .filter(|f| f.severity == FindingSeverity::Error)
                            .count();
                        let warning_count = report
                            .findings
                            .iter()
                            .filter(|f| f.severity == FindingSeverity::Warning)
                            .count();
                        if *validation_request.borrow() == request_id {
                            validation.set(Some(report));
                        }
                        verifying.set(false);
                        if error_count == 0 && warning_count == 0 {
                            show_toast.emit((
                                ToastKind::Success,
                                "No validation issues found.".to_string(),
                            ));
                        } else {
                            show_toast.emit((
                                ToastKind::Error,
                                format!(
                                    "{error_count} error{}, {warning_count} warning{} found.",
                                    if error_count == 1 { "" } else { "s" },
                                    if warning_count == 1 { "" } else { "s" }
                                ),
                            ));
                        }
                    }
                    Err(message) => {
                        if *validation_request.borrow() == request_id {
                            validation.set(Some(ValidationReportDto {
                                findings: vec![crate::dto::FindingDto {
                                    severity: FindingSeverity::Error,
                                    message: format!("Could not validate this entry: {message}"),
                                    field: None,
                                    line_index: None,
                                }],
                                has_errors: true,
                            }));
                        }
                        verifying.set(false);
                        show_toast.emit((
                            ToastKind::Error,
                            format!("Could not verify this entry: {message}"),
                        ));
                    }
                }
            });
        })
    };

    let error_messages: Vec<AttrValue> = validation
        .as_ref()
        .map(|r| {
            r.findings
                .iter()
                .filter(|f| f.severity == FindingSeverity::Error)
                .map(|f| AttrValue::from(f.message.clone()))
                .collect()
        })
        .unwrap_or_default();
    // The draft entry being edited is always appended last to the document
    // sent to `validate_candidate` (see the `use_effect_with` above), so its
    // own findings are unambiguously those tagged with this index.
    let draft_line_index = props.other_lines.len();
    let is_own_mount_point_warning = |f: &crate::dto::FindingDto| {
        f.severity == FindingSeverity::Warning
            && f.field == Some(ValidationField::MountPoint)
            && f.line_index == Some(draft_line_index)
    };
    let mount_point_warnings: Vec<AttrValue> = validation
        .as_ref()
        .map(|r| {
            r.findings
                .iter()
                .filter(|f| is_own_mount_point_warning(f))
                .map(|f| AttrValue::from(f.message.clone()))
                .collect()
        })
        .unwrap_or_default();
    let warning_messages: Vec<AttrValue> = validation
        .as_ref()
        .map(|r| {
            r.findings
                .iter()
                .filter(|f| {
                    f.severity == FindingSeverity::Warning && !is_own_mount_point_warning(f)
                })
                .map(|f| AttrValue::from(f.message.clone()))
                .collect()
        })
        .unwrap_or_default();

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
                    if !is_raw_mode {
                        { for warning_messages.iter().map(|message| html! {
                            <ValidationMessage message={message.clone()} severity={Severity::Warning} />
                        }) }
                    }

                    if let DrawerMode::EditRaw(_) = &props.mode {
                        <div>
                            <label class="block text-sm font-medium text-slate-700 dark:text-slate-300 mb-1" for="raw-line-textarea">
                                { "Raw line" }
                            </label>
                            <p class="text-xs text-slate-500 dark:text-slate-400 mb-2">
                                { "Structured fields can't be offered for a line the parser couldn't decompose - edit the raw text directly." }
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

                            { render_source_value_field(&source_type, &source_value, &devices, *lsblk_available) }

                            <MountPointField
                                value={AttrValue::from((*mount_point).clone())}
                                on_change={{
                                    let mount_point = mount_point.clone();
                                    Callback::from(move |value: String| mount_point.set(value))
                                }}
                                warnings={mount_point_warnings.clone()}
                            />

                            <DescriptionField
                                value={AttrValue::from(
                                    description.as_deref().unwrap_or_default().to_string()
                                )}
                                on_change={{
                                    let description = description.clone();
                                    Callback::from(move |value: String| description.set(Some(value)))
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
                                    Callback::from(move |value: u32| dump.set(value))
                                }}
                                on_pass_change={{
                                    let pass = pass.clone();
                                    Callback::from(move |value: u32| pass.set(value))
                                }}
                            />
                        </div>
                    }

                    if !is_raw_mode {
                        <ValidationSummary errors={error_messages} />
                    }
                </div>

                <FormActionBar
                    save_disabled={!is_raw_mode && (has_blocking_errors || *saving)}
                    on_cancel={props.on_close.clone()}
                    on_save={on_save_click}
                    on_verify={if is_raw_mode { None } else { Some(on_verify_click) }}
                    verify_disabled={*saving || *verifying}
                    verifying={*verifying}
                />
            </div>
        </div>
    }
}

#[allow(clippy::too_many_arguments)]
fn build_entry(
    source_type: &UseStateHandle<SourceType>,
    source_value: &UseStateHandle<String>,
    mount_point: &UseStateHandle<String>,
    fs_type: &UseStateHandle<String>,
    chips: &UseStateHandle<Vec<String>>,
    dump: u32,
    pass: u32,
    trailing_comment: &UseStateHandle<Option<String>>,
    description: &UseStateHandle<Option<String>>,
) -> EntryDto {
    EntryDto {
        source: SourceDto {
            kind: source_type.to_dto_kind(),
            value: (**source_value).clone(),
        },
        mount_point: (**mount_point).clone(),
        fs_type: (**fs_type).clone(),
        options: chips_to_options(chips),
        dump,
        pass,
        trailing_comment: (**trailing_comment).clone(),
        // Mirrors the backend's own IPC-boundary normalization exactly, so
        // a save -> reload round trip is WYSIWYG (API_CONTRACT.md §5).
        description: description
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    }
}

fn render_source_value_field(
    source_type: &UseStateHandle<SourceType>,
    source_value: &UseStateHandle<String>,
    devices: &UseStateHandle<Vec<DeviceInfoDto>>,
    lsblk_available: bool,
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

    let current_type = **source_type;
    let on_pick = {
        let source_value = source_value.clone();
        Callback::from(move |device: DeviceInfoDto| {
            let picked = match current_type {
                SourceType::Uuid => device.uuid.clone(),
                SourceType::Label => device.label.clone(),
                SourceType::PartUuid => device.partuuid.clone(),
                SourceType::PartLabel => device.partlabel.clone(),
                SourceType::DevicePath | SourceType::Network => Some(device.path.clone()),
                SourceType::None => None,
            }
            // Fall back to the device path if this device doesn't carry
            // the identifier kind currently selected, so a click is never
            // silently a no-op.
            .unwrap_or(device.path);
            source_value.set(picked);
        })
    };

    html! {
        // `group`: lets `DevicePicker` stay hidden until the input or one of
        // the device buttons inside it has focus (`group-focus-within`).
        <div class="group">
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
                devices={(**devices).clone()}
                lsblk_available={lsblk_available}
                filter={(**source_value).clone()}
                on_pick={on_pick}
            />
        </div>
    }
}
