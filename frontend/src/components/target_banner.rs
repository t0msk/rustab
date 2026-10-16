//! `TargetBanner` — the always-on-top real-vs-sandbox indicator.
//!
//! Per `docs/UI_DESIGN.md` §2, this is the single most safety-critical
//! visual element in the app: full-width, fixed above everything else,
//! deliberately more alarming in "real" mode than in "sandbox" mode.

use yew::prelude::*;

#[derive(Properties, PartialEq, Clone)]
pub struct TargetBannerProps {
    /// Full resolved path of the file this session would write to.
    pub path: AttrValue,
    /// Whether `path` is the real `/etc/fstab` (true) or a sandbox/dev
    /// target (false). Driven by a prop (rather than hardcoded) so T12 can
    /// wire this live from a Tauri "get active target" command without
    /// touching this component.
    pub is_real: bool,
}

#[function_component(TargetBanner)]
pub fn target_banner(props: &TargetBannerProps) -> Html {
    let base = "w-full h-10 shrink-0 flex items-center px-6 border-b-2 relative z-50";

    if props.is_real {
        html! {
            <div
                class={classes!(base, "bg-red-600", "border-red-800", "text-white", "font-bold", "uppercase", "tracking-wide")}
                style="background-image: repeating-linear-gradient(135deg, rgba(255,255,255,.08) 0 8px, transparent 8px 16px);"
                role="alert"
            >
                <span class="mr-2 text-base" aria-hidden="true">{ "⚠" }</span>
                <span class="truncate text-sm" title={props.path.clone()}>
                    { format!("REAL SYSTEM FILE — {}", props.path) }
                </span>
            </div>
        }
    } else {
        html! {
            <div
                class={classes!(base, "bg-emerald-600", "border-emerald-800", "text-white", "font-semibold")}
                role="status"
            >
                <span class="mr-2 text-base" aria-hidden="true">{ "🧪" }</span>
                <span class="truncate text-sm" title={props.path.clone()}>
                    { format!("Sandbox target — {}", props.path) }
                </span>
            </div>
        }
    }
}
