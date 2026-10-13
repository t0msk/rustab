//! `DevicePicker` — discovered-device list/combobox for the source value
//! field (§4 item 2). Backed by a tiny local mock device list for this
//! task; T11/T12 replace it with real device-discovery data (T7/T8).

use yew::prelude::*;

use crate::mock::MockDevice;

#[derive(Properties, PartialEq)]
pub struct DevicePickerProps {
    pub devices: Vec<MockDevice>,
    pub on_pick: Callback<String>,
}

#[function_component(DevicePicker)]
pub fn device_picker(props: &DevicePickerProps) -> Html {
    html! {
        <div class="mt-2 rounded-md border border-slate-200 dark:border-slate-700 bg-slate-50 dark:bg-slate-900/40 p-2">
            if props.devices.is_empty() {
                <p class="text-xs text-slate-500 dark:text-slate-400 px-1 py-1">
                    { "No devices discovered — enter a value manually" }
                </p>
            } else {
                <ul class="space-y-1">
                    { for props.devices.iter().map(|device| {
                        let value = device.path.to_string();
                        let on_pick = {
                            let cb = props.on_pick.clone();
                            Callback::from(move |_: MouseEvent| cb.emit(value.clone()))
                        };
                        html! {
                            <li>
                                <button
                                    type="button"
                                    class="w-full text-left px-2 py-1 rounded-md text-xs font-mono text-slate-700 dark:text-slate-300 hover:bg-sky-50 dark:hover:bg-slate-700/60 focus-visible:ring-2 focus-visible:ring-sky-500"
                                    onclick={on_pick}
                                >
                                    { format!("{} · UUID={} · LABEL={} · {}", device.path, device.uuid, device.label, device.fstype) }
                                </button>
                            </li>
                        }
                    }) }
                </ul>
            }
        </div>
    }
}
