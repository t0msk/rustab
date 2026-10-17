//! `TargetBanner` - the always-on-top "which file does a save actually
//! write" indicator.
//!
//! Per `docs/UI_DESIGN.md` §2, this is the single most safety-critical
//! visual element in the app: full-width, fixed above everything else, and
//! deliberately asymmetric in alarm level - calm only for the one launch
//! mode that genuinely cannot touch the real `/etc/fstab`.
//!
//! Since T34 that is three states, not two (`docs/SPEC.md` §7,
//! `src-tauri/src/commands.rs::save_document_with`): a save from a default,
//! unconfigured launch writes the real `/etc/fstab` too, even though the
//! file being *edited* is the local working draft. A calm green "sandbox"
//! banner for that case - which is what this component showed before T34 -
//! would now be telling the user something false.

use yew::prelude::*;

use crate::dto::TargetSource;

#[derive(Properties, PartialEq, Clone)]
pub struct TargetBannerProps {
    /// Full resolved path of the file this session edits - the real
    /// `/etc/fstab`, the local working draft, or an explicit override.
    pub path: AttrValue,
    /// Whether `path` *is* the real `/etc/fstab`.
    pub is_real: bool,
    /// How `path` was resolved. Needed alongside `is_real` because the two
    /// non-real cases are the opposite of each other in consequence and
    /// `is_real` alone cannot tell them apart: a
    /// [`TargetSource::SandboxDefault`] draft is still saved *through* to
    /// the real `/etc/fstab`, while a [`TargetSource::Environment`]
    /// override is the one target whose save stops at itself.
    pub source: TargetSource,
}

/// The three states this banner has to distinguish, derived from
/// `is_real` + `source` so the mapping lives in one place.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BannerState {
    /// Explicit `RUSTAB_TARGET_PATH=/etc/fstab`: the file on screen is the
    /// real one. Most alarming treatment; unchanged by T34.
    RealFile,
    /// Default launch: editing the local working draft, but every save
    /// writes the real `/etc/fstab` and mirrors back into the draft.
    DraftWritingReal,
    /// Explicit non-real `RUSTAB_TARGET_PATH`: the only launch mode that
    /// never touches `/etc/fstab`, and the only one allowed to look calm.
    IsolatedOverride,
}

#[function_component(TargetBanner)]
pub fn target_banner(props: &TargetBannerProps) -> Html {
    let state = if props.is_real {
        BannerState::RealFile
    } else if props.source == TargetSource::SandboxDefault {
        BannerState::DraftWritingReal
    } else {
        BannerState::IsolatedOverride
    };

    let base = "w-full h-10 shrink-0 flex items-center px-6 border-b-2 relative z-50";
    // Shared by the two states a save writes `/etc/fstab` from, so the
    // hazard texture reads as "a save from here is real" rather than as
    // "this path is /etc/fstab" (§2).
    let hazard_stripes =
        "background-image: repeating-linear-gradient(135deg, rgba(255,255,255,.08) 0 8px, \
         transparent 8px 16px);";

    match state {
        BannerState::RealFile => html! {
            <div
                class={classes!(base, "bg-red-600", "border-red-800", "text-white", "font-bold", "uppercase", "tracking-wide")}
                style={hazard_stripes}
                role="alert"
            >
                <span class="mr-2 text-base" aria-hidden="true">{ "⚠" }</span>
                <span class="truncate text-sm" title={props.path.clone()}>
                    { format!("REAL SYSTEM FILE - {}", props.path) }
                </span>
            </div>
        },
        BannerState::DraftWritingReal => html! {
            <div
                class={classes!(base, "bg-amber-600", "border-amber-800", "text-white", "font-semibold")}
                style={hazard_stripes}
                role="alert"
            >
                <span class="mr-2 text-base" aria-hidden="true">{ "⚠" }</span>
                <span class="truncate text-sm" title={props.path.clone()}>
                    <span class="font-bold uppercase tracking-wide">
                        { "Saving writes the real /etc/fstab" }
                    </span>
                    { format!(" - editing working draft {}", props.path) }
                </span>
            </div>
        },
        BannerState::IsolatedOverride => html! {
            <div
                class={classes!(base, "bg-emerald-600", "border-emerald-800", "text-white", "font-semibold")}
                role="status"
            >
                <span class="mr-2 text-base" aria-hidden="true">{ "🧪" }</span>
                <span class="truncate text-sm" title={props.path.clone()}>
                    { format!("Test target - {} · your real /etc/fstab is never written", props.path) }
                </span>
            </div>
        },
    }
}
