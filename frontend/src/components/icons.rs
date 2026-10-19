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
//!
//! `ChevronRightIcon`, `CopyIcon`, `CheckIcon` and `FolderOpenIcon` were
//! added for the entries-table row-expand panel (T32): the expand/collapse
//! disclosure chevron, and the Mount point value's copy-to-clipboard /
//! open-in-file-manager buttons (the latter swaps to `CheckIcon` briefly
//! after a successful copy). `ChevronUpIcon`/`ChevronDownIcon` were added
//! for the entries-table Move up/Move down buttons (T40), and
//! `GripVerticalIcon` for the drag-and-drop reorder handle (T48).

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

/// A single chevron, pointing right at rest; the caller rotates it 90°
/// (`rotate-90` utility) to indicate the expanded state rather than
/// swapping in a second icon.
#[function_component(ChevronRightIcon)]
pub fn chevron_right_icon() -> Html {
    icon_svg!(html! {
        <path d="m9 18 6-6-6-6" />
    })
}

/// Entry-reordering "Move up" glyph (T40, `docs/UI_DESIGN.md` §13.2).
/// Deliberately its own path rather than a CSS-rotated `ChevronRightIcon`:
/// that icon's rotation represents one glyph's two states (collapsed/
/// expanded), while Up and Down are two distinct, permanently-meaningful
/// actions - so, like `EditIcon`/`DeleteIcon`, each gets its own path.
#[function_component(ChevronUpIcon)]
pub fn chevron_up_icon() -> Html {
    icon_svg!(html! {
        <path d="m18 15-6-6-6 6" />
    })
}

/// Entry-reordering "Move down" glyph - see [`ChevronUpIcon`].
#[function_component(ChevronDownIcon)]
pub fn chevron_down_icon() -> Html {
    icon_svg!(html! {
        <path d="m6 9 6 6 6-6" />
    })
}

/// Drag-handle glyph for entry reordering (T48, `docs/UI_DESIGN.md`
/// §13.11) - Lucide's `grip-vertical`, six dots in two columns of three.
/// The conventional drag-handle glyph across design systems, and (unlike
/// the chevrons above) it names a *grab affordance* rather than a discrete
/// action, which is why it is its own path and not a reuse of either
/// chevron.
///
/// The six `r="1"` circles are deliberately left to the shared wrapper's
/// `fill="none"`/`stroke="currentColor"`/`stroke-width="2"` attributes,
/// with no per-icon override: a 1-unit circle stroked 2 units wide renders
/// as a solid 4-unit dot, which is exactly how Lucide's own published
/// markup for this icon draws them.
#[function_component(GripVerticalIcon)]
pub fn grip_vertical_icon() -> Html {
    icon_svg!(html! {
        <>
            <circle cx="9" cy="5" r="1" />
            <circle cx="9" cy="12" r="1" />
            <circle cx="9" cy="19" r="1" />
            <circle cx="15" cy="5" r="1" />
            <circle cx="15" cy="12" r="1" />
            <circle cx="15" cy="19" r="1" />
        </>
    })
}

#[function_component(CopyIcon)]
pub fn copy_icon() -> Html {
    icon_svg!(html! {
        <>
            <rect width="14" height="14" x="8" y="8" rx="2" ry="2" />
            <path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2" />
        </>
    })
}

#[function_component(CheckIcon)]
pub fn check_icon() -> Html {
    icon_svg!(html! {
        <path d="M20 6 9 17l-5-5" />
    })
}

#[function_component(FolderOpenIcon)]
pub fn folder_open_icon() -> Html {
    icon_svg!(html! {
        <path d="m6 14 1.45-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.55 6a2 2 0 0 1-1.94 1.5H4a2 2 0 0 1-2-2V5c0-1.1.9-2 2-2h3.93a2 2 0 0 1 1.66.9l.82 1.2a2 2 0 0 0 1.66.9H18a2 2 0 0 1 2 2v2" />
    })
}
