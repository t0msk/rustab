//! `TargetBanner` - the always-on-top "which file am I looking at right
//! now" indicator.
//!
//! Per `docs/UI_DESIGN.md` §2, this is the single most safety-critical
//! visual element in the app: full-width, fixed above everything else, and
//! the one state that is *not* calm (`RealFile`) deliberately stands out
//! from the other two.
//!
//! Since T34 this distinguishes three states, not two (`docs/SPEC.md` §7,
//! `src-tauri/src/commands.rs::save_document_with`): a save from a default,
//! unconfigured launch writes the real `/etc/fstab` too, even though the
//! file being *edited* is the local working draft. T35 (per the repo
//! owner's own call, recorded in `PROJECT_PLAN.md`) reverted that state's
//! ambient treatment back to calm: this banner reports *what you are
//! currently looking at* (a local draft, nothing written yet), not *what
//! a future save will do*. The Review Changes and backup-delete/restore
//! confirmation dialogs already carry an explicit checkbox at the one
//! moment a real write is actually about to happen
//! (`crate::components::real_target_confirm_checkbox::confirm_disabled`,
//! gated on the same `writes_real_file` predicate) - duplicating that as a
//! second, permanent, high-alarm ambient banner would just be noise, not
//! an extra safety signal.

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
    /// real one. Most alarming treatment; unchanged since T34.
    RealFile,
    /// Default launch: editing the local working draft, which every save
    /// writes through to the real `/etc/fstab` (then mirrors back into the
    /// draft). Calm treatment as of T35: this banner describes the *current*
    /// state (a local draft, nothing written yet), not the consequence of a
    /// future save - that risk is communicated explicitly, with a checkbox,
    /// at the one moment it actually applies (Review Changes, backup
    /// delete/restore), not as a second permanent ambient alarm here.
    DraftWritingReal,
    /// Explicit non-real `RUSTAB_TARGET_PATH`: the only launch mode that
    /// never touches `/etc/fstab`.
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
    // `RealFile` only, as of T35: the hazard texture now means "the path on
    // screen *is* `/etc/fstab`," not "a save from here is real" - the latter
    // is no longer something this ambient banner signals at all (§2).
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
                class={classes!(base, "bg-emerald-600", "border-emerald-800", "text-white", "font-semibold")}
                role="status"
            >
                <span class="mr-2 text-base" aria-hidden="true">{ "🧪" }</span>
                <span class="truncate text-sm" title={props.path.clone()}>
                    { format!("Working draft - {}", props.path) }
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
