//! Small reusable inline-SVG icon components, replacing ad hoc Unicode
//! glyphs (`✎`, `🗑`) across the app so icon rendering is consistent across
//! platforms/fonts instead of depending on whatever glyphs the system font
//! happens to ship - the same reasoning `AboutDialog`'s own inline-SVG
//! icons were built on (T26).
//!
//! Deliberately not an icon-library dependency (`yew-lucide` and similar
//! were considered): this app only ever needs a handful of icons, so a
//! whole icon-set crate would be unused-icon bloat in the compiled wasm
//! bundle for no benefit a few hand-copied `<path>`s don't already give.
//! The path data below is copied from Lucide (https://lucide.dev, ISC
//! license - permissive, explicitly allows this), picked for its
//! consistent, modern stroke-based style; nothing here pulls in Lucide as
//! an actual dependency.

use yew::prelude::*;

/// Shared size/stroke attributes every icon in this file uses, so adding a
/// new one only means pasting in a new `<path>`/`<line>` set, not
/// re-deriving the wrapper attributes each time.
macro_rules! icon_svg {
    ($body:expr) => {
        html! {
            <svg
                viewBox="0 0 24 24"
                width="16"
                height="16"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
                stroke-linejoin="round"
                class="shrink-0"
                aria-hidden="true"
            >
                { $body }
            </svg>
        }
    };
}

#[function_component(EditIcon)]
pub fn edit_icon() -> Html {
    icon_svg!(html! {
        <>
            <path d="M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z" />
            <path d="m15 5 4 4" />
        </>
    })
}

#[function_component(DeleteIcon)]
pub fn delete_icon() -> Html {
    icon_svg!(html! {
        <>
            <path d="M3 6h18" />
            <path d="M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6" />
            <path d="M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2" />
            <line x1="10" x2="10" y1="11" y2="17" />
            <line x1="14" x2="14" y1="11" y2="17" />
        </>
    })
}
