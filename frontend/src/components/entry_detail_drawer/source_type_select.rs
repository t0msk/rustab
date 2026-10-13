//! `SourceTypeSelect` — source-type dropdown (§4 item 1).

use web_sys::HtmlSelectElement;
use yew::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SourceType {
    DevicePath,
    Uuid,
    Label,
    PartUuid,
    PartLabel,
    Network,
    None,
}

impl SourceType {
    pub const ALL: [SourceType; 7] = [
        SourceType::DevicePath,
        SourceType::Uuid,
        SourceType::Label,
        SourceType::PartUuid,
        SourceType::PartLabel,
        SourceType::Network,
        SourceType::None,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SourceType::DevicePath => "Device path",
            SourceType::Uuid => "UUID",
            SourceType::Label => "LABEL",
            SourceType::PartUuid => "PARTUUID",
            SourceType::PartLabel => "PARTLABEL",
            SourceType::Network => "Network (NFS/CIFS)",
            SourceType::None => "None",
        }
    }

    pub fn value(self) -> &'static str {
        match self {
            SourceType::DevicePath => "device-path",
            SourceType::Uuid => "uuid",
            SourceType::Label => "label",
            SourceType::PartUuid => "partuuid",
            SourceType::PartLabel => "partlabel",
            SourceType::Network => "network",
            SourceType::None => "none",
        }
    }

    pub fn from_value(value: &str) -> SourceType {
        match value {
            "uuid" => SourceType::Uuid,
            "label" => SourceType::Label,
            "partuuid" => SourceType::PartUuid,
            "partlabel" => SourceType::PartLabel,
            "network" => SourceType::Network,
            "none" => SourceType::None,
            _ => SourceType::DevicePath,
        }
    }

    /// True for the identifier types that use a discovered-device combobox
    /// (`DevicePicker`) rather than a plain free-text + Browse… field.
    pub fn is_identifier(self) -> bool {
        matches!(
            self,
            SourceType::Uuid | SourceType::Label | SourceType::PartUuid | SourceType::PartLabel
        )
    }
}

#[derive(Properties, PartialEq)]
pub struct SourceTypeSelectProps {
    pub value: SourceType,
    pub on_change: Callback<SourceType>,
}

#[function_component(SourceTypeSelect)]
pub fn source_type_select(props: &SourceTypeSelectProps) -> Html {
    let on_change = {
        let cb = props.on_change.clone();
        Callback::from(move |e: Event| {
            if let Some(select) = e.target_dyn_into::<HtmlSelectElement>() {
                cb.emit(SourceType::from_value(&select.value()));
            }
        })
    };

    html! {
        <div>
            <label class="block text-sm font-medium text-slate-700 dark:text-slate-300 mb-1" for="source-type-select">
                { "Source type" }
            </label>
            <select
                id="source-type-select"
                class="w-full rounded-md border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-900 text-sm px-3 py-2 focus-visible:ring-2 focus-visible:ring-sky-500"
                onchange={on_change}
            >
                { for SourceType::ALL.iter().map(|option| html! {
                    <option value={option.value()} selected={*option == props.value}>{ option.label() }</option>
                }) }
            </select>
        </div>
    }
}
