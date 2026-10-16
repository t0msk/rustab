//! `MountPointField` - text input + directory-picker trigger (§4 item 3).

use web_sys::HtmlInputElement;
use yew::prelude::*;

use super::directory_picker_button::DirectoryPickerButton;
use super::validation_message::{Severity, ValidationMessage};

#[derive(Properties, PartialEq)]
pub struct MountPointFieldProps {
    pub value: AttrValue,
    pub on_change: Callback<String>,
    /// Warnings about *this* draft's mount point specifically (§5/T21.3) -
    /// i.e. already filtered and attributed by the caller, not every
    /// mount-point-related warning in the document. Rendered directly below
    /// the input.
    #[prop_or_default]
    pub warnings: Vec<AttrValue>,
    /// Hint text shown when the input is empty - set by the caller to
    /// `"none"` when the entry's source type is `None` (the conventional
    /// real-fstab value for a swap entry's mount point, e.g.
    /// `/dev/sda2 none swap sw 0 0`). Purely a visual hint, never written
    /// into `value` - the user must still type it for it to actually save.
    #[prop_or_default]
    pub placeholder: Option<AttrValue>,
    pub on_browse: Callback<()>,
}

#[function_component(MountPointField)]
pub fn mount_point_field(props: &MountPointFieldProps) -> Html {
    let on_input = {
        let cb = props.on_change.clone();
        Callback::from(move |e: InputEvent| {
            if let Some(input) = e.target_dyn_into::<HtmlInputElement>() {
                cb.emit(input.value());
            }
        })
    };

    html! {
        <div>
            <label class="block text-sm font-medium text-slate-700 dark:text-slate-300 mb-1" for="mount-point-input">
                { "Mount point" }
            </label>
            <div class="flex gap-2">
                <input
                    id="mount-point-input"
                    type="text"
                    class="flex-1 rounded-md border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-900 font-mono text-sm px-3 py-2 focus-visible:ring-2 focus-visible:ring-sky-500"
                    value={props.value.clone()}
                    placeholder={props.placeholder.clone()}
                    oninput={on_input}
                />
                <DirectoryPickerButton
                    on_click={{
                        let cb = props.on_browse.clone();
                        Callback::from(move |_: MouseEvent| cb.emit(()))
                    }}
                />
            </div>
            { for props.warnings.iter().map(|message| html! {
                <ValidationMessage message={message.clone()} severity={Severity::Warning} />
            }) }
        </div>
    }
}
