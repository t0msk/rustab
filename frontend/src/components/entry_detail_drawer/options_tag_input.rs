//! `OptionsTagInput` - well-known toggle chips + systemd/vfat value chips +
//! free tags, rendered as one ordered removable-chip list (§4 item 5).
//!
//! All state here is local, in-memory scratch state for the drawer - it
//! does not read/write the mock entries table or any app-level state.

use web_sys::HtmlInputElement;
use yew::prelude::*;

const WELL_KNOWN: &[&str] = &[
    "defaults", "noatime", "relatime", "ro", "rw", "user", "nofail", "auto", "noauto", "bind", "sw",
];

const SYSTEMD_AUTOMOUNT: &str = "x-systemd.automount";
const SYSTEMD_VALUE_KEYS: &[(&str, &str)] = &[
    ("x-systemd.idle-timeout=", "30"),
    ("x-systemd.requires-mounts-for=", "/mnt/data"),
    ("x-systemd.device-timeout=", "5s"),
];

const VFAT_VALUE_KEYS: &[(&str, &str)] = &[
    ("fmask=", "0022"),
    ("dmask=", "0022"),
    ("codepage=", "437"),
    ("iocharset=", "utf8"),
    ("shortname=", "mixed"),
];
const VFAT_ERRORS_REMOUNT_RO: &str = "errors=remount-ro";

#[derive(Properties, PartialEq)]
pub struct OptionsTagInputProps {
    pub chips: Vec<String>,
    pub on_change: Callback<Vec<String>>,
}

fn toggle_exact(
    chips: &[String],
    on_change: &Callback<Vec<String>>,
    value: &'static str,
) -> Callback<MouseEvent> {
    let chips = chips.to_vec();
    let on_change = on_change.clone();
    Callback::from(move |_: MouseEvent| {
        let mut next = chips.clone();
        if let Some(pos) = next.iter().position(|chip| chip == value) {
            next.remove(pos);
        } else {
            next.push(value.to_string());
        }
        on_change.emit(next);
    })
}

fn toggle_value_key(
    chips: &[String],
    on_change: &Callback<Vec<String>>,
    key: &'static str,
    default_value: &'static str,
) -> Callback<MouseEvent> {
    let chips = chips.to_vec();
    let on_change = on_change.clone();
    Callback::from(move |_: MouseEvent| {
        let mut next = chips.clone();
        if let Some(pos) = next.iter().position(|chip| chip.starts_with(key)) {
            next.remove(pos);
        } else {
            next.push(format!("{key}{default_value}"));
        }
        on_change.emit(next);
    })
}

fn update_value_key(
    chips: &[String],
    on_change: &Callback<Vec<String>>,
    key: &'static str,
) -> Callback<InputEvent> {
    let chips = chips.to_vec();
    let on_change = on_change.clone();
    Callback::from(move |e: InputEvent| {
        if let Some(input) = e.target_dyn_into::<HtmlInputElement>() {
            let mut next = chips.clone();
            if let Some(pos) = next.iter().position(|chip| chip.starts_with(key)) {
                next[pos] = format!("{key}{}", input.value());
            }
            on_change.emit(next);
        }
    })
}

fn remove_chip(
    chips: &[String],
    on_change: &Callback<Vec<String>>,
    value: String,
) -> Callback<MouseEvent> {
    let chips = chips.to_vec();
    let on_change = on_change.clone();
    Callback::from(move |_: MouseEvent| {
        let next: Vec<String> = chips
            .iter()
            .filter(|chip| **chip != value)
            .cloned()
            .collect();
        on_change.emit(next);
    })
}

fn chip_toggle_classes(active: bool) -> Classes {
    if active {
        classes!(
            "bg-sky-100",
            "text-sky-700",
            "dark:bg-sky-900/40",
            "dark:text-sky-300"
        )
    } else {
        classes!(
            "bg-slate-100",
            "text-slate-600",
            "dark:bg-slate-800",
            "dark:text-slate-400"
        )
    }
}

#[function_component(OptionsTagInput)]
pub fn options_tag_input(props: &OptionsTagInputProps) -> Html {
    let vfat_expanded = use_state(|| false);
    let tag_draft = use_state(String::new);

    let on_toggle_vfat = {
        let vfat_expanded = vfat_expanded.clone();
        Callback::from(move |_: MouseEvent| vfat_expanded.set(!*vfat_expanded))
    };

    let on_tag_draft_input = {
        let tag_draft = tag_draft.clone();
        Callback::from(move |e: InputEvent| {
            if let Some(input) = e.target_dyn_into::<HtmlInputElement>() {
                tag_draft.set(input.value());
            }
        })
    };

    let on_tag_draft_keydown = {
        let chips = props.chips.clone();
        let on_change = props.on_change.clone();
        let tag_draft = tag_draft.clone();
        Callback::from(move |e: KeyboardEvent| {
            if e.key() == "Enter" || e.key() == "," {
                e.prevent_default();
                let value = tag_draft.trim().trim_end_matches(',').trim().to_string();
                if !value.is_empty() && !chips.contains(&value) {
                    let mut next = chips.clone();
                    next.push(value);
                    on_change.emit(next);
                }
                tag_draft.set(String::new());
            }
        })
    };

    html! {
        <div>
            <label class="block text-sm font-medium text-slate-700 dark:text-slate-300 mb-1">{ "Options" }</label>

            <div class="flex flex-wrap gap-1.5 mb-2">
                { for WELL_KNOWN.iter().map(|option| {
                    let active = props.chips.iter().any(|chip| chip == option);
                    html! {
                        <button
                            type="button"
                            class={classes!("rounded-full", "px-3", "py-1", "text-xs", "font-medium", chip_toggle_classes(active))}
                            onclick={toggle_exact(&props.chips, &props.on_change, option)}
                        >
                            { *option }
                        </button>
                    }
                }) }
            </div>

            <div class="mb-2">
                <p class="text-xs font-medium text-slate-500 dark:text-slate-400 mb-1">{ "systemd" }</p>
                <div class="flex flex-wrap items-center gap-1.5">
                    { {
                        let active = props.chips.iter().any(|chip| chip == SYSTEMD_AUTOMOUNT);
                        html! {
                            <button
                                type="button"
                                class={classes!("rounded-full", "px-3", "py-1", "text-xs", "font-medium", chip_toggle_classes(active))}
                                onclick={toggle_exact(&props.chips, &props.on_change, SYSTEMD_AUTOMOUNT)}
                            >
                                { SYSTEMD_AUTOMOUNT }
                            </button>
                        }
                    } }
                    { for SYSTEMD_VALUE_KEYS.iter().map(|(key, default_value)| {
                        let current = props.chips.iter().find(|chip| chip.starts_with(key)).cloned();
                        let active = current.is_some();
                        html! {
                            <span class="inline-flex items-center gap-1">
                                <button
                                    type="button"
                                    class={classes!("rounded-full", "px-3", "py-1", "text-xs", "font-medium", chip_toggle_classes(active))}
                                    onclick={toggle_value_key(&props.chips, &props.on_change, key, default_value)}
                                >
                                    { *key }
                                </button>
                                if let Some(current_value) = current {
                                    <input
                                        type="text"
                                        class="w-20 rounded-md border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-900 text-xs px-2 py-1 focus-visible:ring-2 focus-visible:ring-sky-500"
                                        value={current_value[key.len()..].to_string()}
                                        oninput={update_value_key(&props.chips, &props.on_change, key)}
                                    />
                                }
                            </span>
                        }
                    }) }
                </div>
            </div>

            <div class="mb-2">
                <button
                    type="button"
                    class="text-xs font-medium text-slate-500 dark:text-slate-400 hover:text-slate-700 dark:hover:text-slate-200 mb-1"
                    onclick={on_toggle_vfat}
                >
                    { if *vfat_expanded { "▾ vfat / filesystem-specific" } else { "▸ vfat / filesystem-specific" } }
                </button>
                if *vfat_expanded {
                    <div class="flex flex-wrap items-center gap-1.5">
                        { for VFAT_VALUE_KEYS.iter().map(|(key, default_value)| {
                            let current = props.chips.iter().find(|chip| chip.starts_with(key)).cloned();
                            let active = current.is_some();
                            html! {
                                <span class="inline-flex items-center gap-1">
                                    <button
                                        type="button"
                                        class={classes!("rounded-full", "px-3", "py-1", "text-xs", "font-medium", chip_toggle_classes(active))}
                                        onclick={toggle_value_key(&props.chips, &props.on_change, key, default_value)}
                                    >
                                        { *key }
                                    </button>
                                    if let Some(current_value) = current {
                                        <input
                                            type="text"
                                            class="w-20 rounded-md border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-900 text-xs px-2 py-1 focus-visible:ring-2 focus-visible:ring-sky-500"
                                            value={current_value[key.len()..].to_string()}
                                            oninput={update_value_key(&props.chips, &props.on_change, key)}
                                        />
                                    }
                                </span>
                            }
                        }) }
                        { {
                            let active = props.chips.iter().any(|chip| chip == VFAT_ERRORS_REMOUNT_RO);
                            html! {
                                <button
                                    type="button"
                                    class={classes!("rounded-full", "px-3", "py-1", "text-xs", "font-medium", chip_toggle_classes(active))}
                                    onclick={toggle_exact(&props.chips, &props.on_change, VFAT_ERRORS_REMOUNT_RO)}
                                >
                                    { VFAT_ERRORS_REMOUNT_RO }
                                </button>
                            }
                        } }
                    </div>
                }
            </div>

            <input
                type="text"
                placeholder="Type a value, press Enter or , to add"
                class="w-full rounded-md border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-900 text-sm px-3 py-2 mb-2 focus-visible:ring-2 focus-visible:ring-sky-500"
                value={(*tag_draft).clone()}
                oninput={on_tag_draft_input}
                onkeydown={on_tag_draft_keydown}
            />

            <p class="text-xs font-medium text-slate-500 dark:text-slate-400 mb-1">{ "Resulting option order" }</p>
            <div class="flex flex-wrap gap-1.5 rounded-md border border-slate-200 dark:border-slate-700 bg-slate-50 dark:bg-slate-900/40 p-2 min-h-[2.5rem]">
                { for props.chips.iter().map(|chip| html! {
                    <span class="inline-flex items-center gap-1 rounded-full bg-sky-100 text-sky-700 dark:bg-sky-900/40 dark:text-sky-300 text-xs font-medium px-3 py-1">
                        { chip.clone() }
                        <button
                            type="button"
                            aria-label={format!("Remove {chip}")}
                            class="focus-visible:ring-2 focus-visible:ring-sky-500"
                            onclick={remove_chip(&props.chips, &props.on_change, chip.clone())}
                        >
                            { "×" }
                        </button>
                    </span>
                }) }
            </div>
        </div>
    }
}
