//! `DevicePicker` — discovered-device list/combobox for the source value
//! field (§4 item 2). Backed by `discover_devices` (T11); respects the
//! soft-failure contract (`docs/API_CONTRACT.md` §3.8, `docs/SPEC.md` §8):
//! when `lsblk` is unavailable the picker says so plainly rather than
//! claiming "no devices found", and manual text entry in the parent field
//! stays usable either way.

use yew::prelude::*;

use crate::dto::DeviceInfoDto;

#[derive(Properties, PartialEq)]
pub struct DevicePickerProps {
    pub devices: Vec<DeviceInfoDto>,
    pub lsblk_available: bool,
    /// Emits the full picked device; the caller decides which field
    /// (`path`/`uuid`/`label`/...) to pull the source value from, since
    /// that depends on the currently selected source *type*, which this
    /// component does not know about.
    pub on_pick: Callback<DeviceInfoDto>,
}

fn device_label(device: &DeviceInfoDto) -> String {
    let mut parts = vec![device.path.clone()];
    if let Some(uuid) = &device.uuid {
        parts.push(format!("UUID={uuid}"));
    }
    if let Some(label) = &device.label {
        parts.push(format!("LABEL={label}"));
    }
    if let Some(partuuid) = &device.partuuid {
        parts.push(format!("PARTUUID={partuuid}"));
    }
    if let Some(partlabel) = &device.partlabel {
        parts.push(format!("PARTLABEL={partlabel}"));
    }
    if let Some(fs_type) = &device.fs_type {
        parts.push(fs_type.clone());
    }
    parts.join(" · ")
}

#[function_component(DevicePicker)]
pub fn device_picker(props: &DevicePickerProps) -> Html {
    html! {
        <div class="mt-2 rounded-md border border-slate-200 dark:border-slate-700 bg-slate-50 dark:bg-slate-900/40 p-2">
            if !props.lsblk_available {
                <p class="text-xs text-slate-500 dark:text-slate-400 px-1 py-1">
                    { "Device discovery unavailable — enter a value manually" }
                </p>
            } else if props.devices.is_empty() {
                <p class="text-xs text-slate-500 dark:text-slate-400 px-1 py-1">
                    { "No devices discovered — enter a value manually" }
                </p>
            } else {
                <ul class="space-y-1">
                    { for props.devices.iter().map(|device| {
                        let device_value = device.clone();
                        let on_pick = {
                            let cb = props.on_pick.clone();
                            Callback::from(move |_: MouseEvent| cb.emit(device_value.clone()))
                        };
                        html! {
                            <li>
                                <button
                                    type="button"
                                    class="w-full text-left px-2 py-1 rounded-md text-xs font-mono text-slate-700 dark:text-slate-300 hover:bg-sky-50 dark:hover:bg-slate-700/60 focus-visible:ring-2 focus-visible:ring-sky-500"
                                    onclick={on_pick}
                                >
                                    { device_label(device) }
                                </button>
                            </li>
                        }
                    }) }
                </ul>
            }
        </div>
    }
}
