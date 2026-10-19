//! `EntriesTable` - main entry list (§3), rows are `EntryRow` (parsed) or
//! `UnparsedEntryRow` (malformed). Renders from the app's in-memory
//! candidate document (see `crate::candidate`) rather than mock data (T11).
//!
//! **Row expansion (T32):** clicking anywhere on an `EntryRow`'s own `<tr>`
//! toggles an inline detail panel showing every one of that entry's fields,
//! untruncated - the deliberate replacement for the fact that there was
//! previously no way to see an entry's description without opening its
//! drawer. Expansion state is a `HashSet<usize>` of expanded
//! `candidate_index` values owned by `EntriesTable` itself, so any number of
//! rows can be open at once (not an accordion). `UnparsedEntryRow` is not
//! affected - it already shows its full raw text untruncated.
//!
//! **Reordering (T40, revised by T41 - `docs/UI_DESIGN.md` §13):** every
//! row carries Move up/Move down buttons that emit `on_move((from, to))`
//! with two *candidate-document* indices. This component resolves `to`
//! itself from the adjacent entry in its own `rows` list, so exactly one
//! place decides adjacency (§13.4), and it mirrors the move onto its own
//! `expanded` set at the same time, because that set is keyed by position,
//! not by entry identity (§13.8).
//!
//! An entry moves together with its attached comment block (§13.4), so the
//! two entries do not simply trade indices once their blocks differ in
//! length - this component predicts where each one really lands with the
//! same `resolve_moved_indices` `stage_move` uses, fed by the
//! `attached_block_len` prop.
//!
//! **Drag-and-drop (T48, §13.11)** is layered on top of that, additively:
//! every row grows a drag handle whose `pointerdown`/`pointermove`/
//! `pointerup` gesture commits *the very same* `on_move((from, to))` calls
//! a Move-button click makes - one adjacent step per row boundary the
//! pointer crosses, and **at most one emission per pointer event**. That
//! last rule is not a style preference: `AppShell`'s handler reads a
//! `UseStateHandle` snapshot of the candidate document and calls `.set()`,
//! so a second `emit` in the same JS callback would stage onto the
//! pre-`.set()` document and the two steps would not compose. A fast drag
//! that outruns one step per event simply catches up on the next one.
//!
//! **Why the drag renders rows out of document order.** A move changes
//! only the *contents* of this table's rows, never their count or their
//! keys' order (rows are keyed by `candidate_index`, rendered ascending, so
//! Yew's keyed diff sees `[0, 1, ..., N-1]` before and after and never
//! physically moves a `<tr>`). Nothing in the DOM changes position, so
//! there is nothing for a CSS `transition: transform` to animate - a
//! "just add a transition" drag would silently produce zero motion. The fix
//! is to freeze the layout for the duration of the gesture: while a drag is
//! in progress this component renders the rows in the order they had at
//! `pointerdown` (each keyed by the `candidate_index` it had *then* - an
//! ephemeral, gesture-scoped `key=` identity, nothing more) and expresses
//! the committed order purely as a `translateY` per row. Each row's DOM
//! node therefore keeps its own entry for the whole gesture, and every
//! committed step changes those `translateY` *values*, which is precisely
//! what a CSS transform transition animates. On `pointerup` the frozen
//! order and the transforms are dropped together; the document order they
//! were standing in for is already live, so nothing moves on screen.
//!
//! Nothing here introduces an app-level per-entry identity: `expanded`,
//! `on_move` and `candidate_index` keep their exact position-based
//! meaning, and the frozen keys live and die with one gesture.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use wasm_bindgen::JsCast;
use yew::prelude::*;

use crate::api;
use crate::candidate::resolve_moved_indices;
use crate::components::empty_state::EmptyState;
use crate::components::icons::{
    CheckIcon, ChevronDownIcon, ChevronRightIcon, ChevronUpIcon, CopyIcon, DeleteIcon, EditIcon,
    FolderOpenIcon, GripVerticalIcon,
};
use crate::components::status_toast::ToastKind;
use crate::dto::{EntryDto, OptionDto, SourceKind};

/// One table row's content, independent of `candidate::CandidateDocument`
/// so this component only depends on the plain DTOs it needs to render.
#[derive(Clone, PartialEq)]
pub enum RowEntry {
    Parsed(EntryDto),
    Unparsed(String),
}

#[derive(Properties, PartialEq)]
pub struct EntriesTableProps {
    /// `(candidate_index, row)` pairs, in document order. `candidate_index`
    /// is the row's identity in the app's `CandidateDocument` - the same
    /// value is reported back via `on_edit`/`on_delete`.
    pub rows: Vec<(usize, RowEntry)>,
    /// Document-wide warning findings keyed by `candidate_index`, from a
    /// `validate_candidate` run against the whole document (not just the
    /// row being edited) - lets a row visibly flag "this has a warning"
    /// before anyone opens its own drawer. A row with no entry here has no
    /// known warning. Full message text is shown as a native tooltip, not
    /// inline, to avoid repeating the mistake the mount-point note-glyph
    /// made (taking up row space) - this is a border accent only.
    #[prop_or_default]
    pub row_warnings: HashMap<usize, Vec<AttrValue>>,
    /// Each row's own attached comment block length, keyed by
    /// `candidate_index` - computed by `AppShell` once per render from
    /// `CandidateDocument::attached_block_len`, the same whole-document
    /// cadence as `row_warnings` (`docs/UI_DESIGN.md` §13.4). A row missing
    /// from the map has no attached block. Used for the `expanded`-set
    /// remap (§13.8).
    #[prop_or_default]
    pub attached_block_len: HashMap<usize, usize>,
    pub on_edit: Callback<usize>,
    pub on_delete: Callback<usize>,
    /// `(from, to)` candidate indices for a Move up/Move down click (T40,
    /// `docs/UI_DESIGN.md` §13.8). `to` is resolved here, from the
    /// neighboring element of `rows` - comments/blank lines are already
    /// filtered out of that list, so the neighbor is always the next
    /// *entry*, whatever sits between them in the file.
    pub on_move: Callback<(usize, usize)>,
    pub on_add_entry: Callback<()>,
    /// Surfaces a transient notification from the Mount point copy/open
    /// buttons inside a row's expanded detail panel (T32) - threaded down
    /// the same way `EntryDetailDrawer` already receives its own handle
    /// from `AppShell`.
    pub show_toast: Callback<(ToastKind, String)>,
}

/// Also reused by `AppShell` for `DeleteConfirmDialog`'s read-only summary.
pub(crate) fn source_badge(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Path => "PATH",
        SourceKind::Uuid => "UUID",
        SourceKind::Label => "LABEL",
        SourceKind::PartUuid => "PARTUUID",
        SourceKind::PartLabel => "PARTLABEL",
        SourceKind::Network => "NET",
        SourceKind::None => "NONE",
        SourceKind::Other => "OTHER",
    }
}

fn format_option(option: &OptionDto) -> String {
    match &option.value {
        Some(value) => format!("{}={}", option.key, value),
        None => option.key.clone(),
    }
}

/// Whether the row at render position `row_position` can move up - i.e.
/// whether it has a row above it (`docs/UI_DESIGN.md` §13.2). The first
/// row's Move up button is rendered `disabled`, never hidden.
fn can_move_up(row_position: usize) -> bool {
    row_position > 0
}

/// Whether the row at render position `row_position`, in a table of
/// `row_count` rows, can move down. A one-row table disables both
/// directions (§13.2).
fn can_move_down(row_position: usize, row_count: usize) -> bool {
    row_position + 1 < row_count
}

/// One Move up/Move down button (`docs/UI_DESIGN.md` §13.2).
///
/// `handler` being `None` *is* the boundary state: the button is still
/// rendered, in its constant position in the Actions cell, as a real
/// `disabled` element carrying the `title` that explains why - never
/// hidden, so nothing shifts around as a row becomes or stops being the
/// first/last one.
fn move_button(
    handler: &Option<Callback<MouseEvent>>,
    label: &'static str,
    disabled_title: &'static str,
    hover_class: &'static str,
    icon: Html,
) -> Html {
    let enabled = handler.is_some();
    let class = classes!(
        "p-2",
        "rounded-md",
        "focus-visible:ring-2",
        "focus-visible:ring-sky-500",
        enabled.then_some(hover_class),
        (!enabled).then_some("opacity-40"),
        (!enabled).then_some("cursor-not-allowed"),
    );
    html! {
        <button
            type="button"
            aria-label={label}
            class={class}
            disabled={!enabled}
            title={(!enabled).then_some(AttrValue::from(disabled_title))}
            onclick={handler.clone().unwrap_or_else(|| Callback::from(|_: MouseEvent| ()))}
        >
            { icon }
        </button>
    }
}

/// Mirrors a `from`/`to` move onto the set of expanded candidate indices
/// (`docs/UI_DESIGN.md` §13.8).
///
/// `expanded` is keyed by *position*, not by entry identity, so without
/// this an expanded row would stay open over whichever entry moved into
/// its slot instead of following the entry the user actually expanded.
///
/// T40 did this as a plain membership swap between `from` and `to`, which
/// is wrong once each entry moves together with an attached comment block
/// of its own length (§13.4): the entry at `from` does not, in general,
/// end up at `to`. Rather than route the real answer back from `AppShell`
/// (there is no such channel, and inventing one would be a far bigger
/// change than this needs), this predicts it synchronously at click time
/// with the very same [`resolve_moved_indices`] `stage_move` runs
/// internally, fed by the `attached_block_len` prop computed from the same
/// document this table is currently showing - so the prediction agreeing
/// with the staged result is a guarantee, not a coincidence.
///
/// Only `from`/`to` need remapping: `to` is always the *adjacent row* in
/// the render order, so no other row-kind line sits between them, and
/// comment/blank lines are never in `expanded` to begin with.
///
/// With two equal-length blocks (including the usual none/none case) this
/// reduces exactly to T40's swap.
fn remap_expanded(
    expanded: &HashSet<usize>,
    from: usize,
    to: usize,
    attached_block_len: &HashMap<usize, usize>,
) -> HashSet<usize> {
    let upper = from.min(to);
    let lower = from.max(to);
    let block_len = |index: usize| attached_block_len.get(&index).copied().unwrap_or(0);
    let (new_upper, new_lower) =
        resolve_moved_indices(upper, block_len(upper), lower, block_len(lower));
    let (from_new, to_new) = if from == upper {
        (new_upper, new_lower)
    } else {
        (new_lower, new_upper)
    };

    let from_was_expanded = expanded.contains(&from);
    let to_was_expanded = expanded.contains(&to);
    let mut next = expanded.clone();
    next.remove(&from);
    next.remove(&to);
    if from_was_expanded {
        next.insert(from_new);
    }
    if to_was_expanded {
        next.insert(to_new);
    }
    next
}

/// One adjacent reorder step, in render-order terms - the only kind of
/// move either a Move button or a drag ever performs (T40 §13.8, T48
/// §13.11).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Step {
    Up,
    Down,
}

/// Which edge of a row the drop-insertion line is drawn on (§13.11).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InsertionEdge {
    Top,
    Bottom,
}

/// One row's measured geometry, captured once at `pointerdown` and then
/// held fixed for the whole gesture (see the module docs on why the layout
/// is frozen). `height` covers the row's *whole* vertical extent - for an
/// expanded `EntryRow` that is its main `<tr>` plus its detail `<tr>`,
/// which move together.
#[derive(Clone, PartialEq, Debug)]
struct RowMetrics {
    top: f64,
    height: f64,
}

/// Everything one in-progress drag gesture needs to know. Held in a
/// `use_state` so each `pointermove` re-renders the table with fresh
/// transforms.
#[derive(Clone, PartialEq, Debug)]
struct DragState {
    /// The pointer this gesture captured; events from any other pointer
    /// (a second finger) are ignored.
    pointer_id: i32,
    /// The dragged row's render position at `pointerdown`. Doubles as its
    /// *gesture slot*: the frozen layout slot its DOM node keeps for the
    /// whole gesture.
    start_position: usize,
    /// The dragged row's render position right now, after the steps
    /// committed so far.
    current_position: usize,
    start_y: f64,
    current_y: f64,
    /// Frozen per-render-position geometry, indexed by gesture slot.
    metrics: Vec<RowMetrics>,
    /// Each gesture slot's `candidate_index` as of `pointerdown` - used
    /// only as Yew's `key=` for the gesture's duration (module docs).
    keys: Vec<usize>,
}

/// The candidate index one adjacent step away from `row_position` in this
/// table's own render order, or `None` at that boundary.
///
/// Deliberately the single place both paths resolve a move's `to` index:
/// the Move buttons call it to build their click handlers, and a drag's
/// committed step calls it with the dragged row's current render position,
/// so a drag provably cannot produce a different `(from, to)` pair than
/// the equivalent click (§13.12 item 4). It keeps `on_move`'s contract
/// exactly as T40 wrote it - "`EntriesTable` resolves `to` itself from the
/// adjacent element of `rows`" - for both.
fn neighbor_target(candidate_indices: &[usize], row_position: usize, step: Step) -> Option<usize> {
    match step {
        Step::Up => can_move_up(row_position).then(|| candidate_indices[row_position - 1]),
        Step::Down => can_move_down(row_position, candidate_indices.len())
            .then(|| candidate_indices[row_position + 1]),
    }
}

/// Where gesture slot `slot` currently sits in the live render order.
///
/// A drag only ever rotates one row through its neighbors, so the whole
/// permutation is described by just the dragged row's start and current
/// positions: it sits at `current`, and everything it has passed shifted
/// one slot the other way. Identity while `current == start`.
fn permuted_position(start: usize, current: usize, slot: usize) -> usize {
    if slot == start {
        current
    } else if start < slot && slot <= current {
        slot - 1
    } else if current <= slot && slot < start {
        slot + 1
    } else {
        slot
    }
}

/// Inverse of [`permuted_position`]: which gesture slot's row is currently
/// rendered at render position `position`.
fn gesture_slot_at(start: usize, current: usize, position: usize) -> usize {
    if position == current {
        start
    } else if start <= position && position < current {
        position + 1
    } else if current < position && position <= start {
        position - 1
    } else {
        position
    }
}

/// The row heights in the *current* document order (the frozen heights,
/// permuted), i.e. `heights[p]` is the height of whatever row now sits at
/// render position `p`.
fn heights_in_current_order(metrics: &[RowMetrics], start: usize, current: usize) -> Vec<f64> {
    (0..metrics.len())
        .map(|position| metrics[gesture_slot_at(start, current, position)].height)
        .collect()
}

/// Where each render position's top edge *would* be, had the table
/// re-laid itself out after the steps committed so far. The frozen layout
/// deliberately has not moved, so these are the targets every non-dragged
/// row is translated onto.
fn target_tops(metrics: &[RowMetrics], start: usize, current: usize) -> Vec<f64> {
    let origin = metrics.first().map(|m| m.top).unwrap_or(0.0);
    let mut tops = Vec::with_capacity(metrics.len());
    let mut cursor = origin;
    for height in heights_in_current_order(metrics, start, current) {
        tops.push(cursor);
        cursor += height;
    }
    tops
}

/// The dragged row's own offset: the raw pointer delta, clamped so the row
/// can never be dragged outside the table it belongs to. Its frozen slot
/// never moves, so the pointer delta *is* the offset - which is also why
/// the dragged row needs no transition to track the pointer 1:1 (§13.11).
fn clamp_drag_offset(metrics: &[RowMetrics], start: usize, pointer_delta: f64) -> f64 {
    let (Some(first), Some(last), Some(dragged)) =
        (metrics.first(), metrics.last(), metrics.get(start))
    else {
        return 0.0;
    };
    let min = first.top - dragged.top;
    let max = (last.top + last.height - dragged.height) - dragged.top;
    pointer_delta.clamp(min, max.max(min))
}

/// Every gesture slot's `translateY`, in pixels, for the current drag
/// state (§13.12 item 2 - the "which rows slide, and by how much"
/// computation, independent of whether a live browser animates it).
///
/// The dragged row gets the clamped pointer delta; every other row gets
/// the distance from its frozen slot to the slot it now occupies - zero
/// for the rows the drag has not passed, one row height for the ones it
/// has. Those values changing between renders is what the displaced rows'
/// `transition: transform` animates.
///
/// A row the drag has not passed is short-circuited to an exact `0.0`
/// rather than going through [`target_tops`]' prefix sum: measured row
/// boxes can carry sub-pixel gaps that the prefix sum would turn into a
/// fraction-of-a-pixel nudge on rows that are supposed to be standing
/// still.
fn drag_offsets(
    metrics: &[RowMetrics],
    start: usize,
    current: usize,
    pointer_delta: f64,
) -> Vec<f64> {
    let tops = target_tops(metrics, start, current);
    (0..metrics.len())
        .map(|slot| {
            let position = permuted_position(start, current, slot);
            if slot == start {
                clamp_drag_offset(metrics, start, pointer_delta)
            } else if position == slot {
                0.0
            } else {
                tops[position] - metrics[slot].top
            }
        })
        .collect()
}

/// How much of the neighbouring row a drag has to cover before its step
/// commits, as a fraction of that neighbour's own height.
///
/// Deliberately *more* than half. The two directions' thresholds are
/// mirror images of each other, so a plain 50% rule is exactly
/// self-reversing: the instant a step commits, the pointer is standing
/// precisely on the threshold for stepping straight back, and the rows
/// would then swap back and forth on every single pointer event without
/// the user moving at all. The 15 percentage points above half are the
/// resulting dead zone - about 13px for this table's 44px rows - i.e.
/// pointer travel the user has to deliberately undo before a committed
/// step reverses. Kept well below 100% so that the last row remains
/// reachable even for a dragged row much taller than the ones it passes
/// (see [`clamp_drag_offset`], which stops it at the table's own edge).
const STEP_THRESHOLD: f64 = 0.65;

/// Whether this pointer position has crossed the next row boundary, and in
/// which direction (§13.12 item 1).
///
/// The threshold is [`STEP_THRESHOLD`] of the *neighbour's* height, taken
/// from the dragged row's own resting top edge, which keeps the step
/// honest for the mixed row heights this table actually has (a tall
/// `UnparsedEntryRow`, an expanded row).
///
/// Returns **at most one** step, by construction - no loop, nothing
/// accumulated - which is the whole of §13.11's "never more than one
/// `on_move` emission per pointer event" rule: a pointer that jumped
/// several boundaries in one event takes one step now and the rest on the
/// following events, by which time `AppShell` has re-rendered this table
/// with the staged document (§13.12 item 3).
fn plan_step(
    metrics: &[RowMetrics],
    start: usize,
    current: usize,
    pointer_delta: f64,
) -> Option<Step> {
    let tops = target_tops(metrics, start, current);
    let heights = heights_in_current_order(metrics, start, current);
    let dragged_top = metrics.get(start)?.top + clamp_drag_offset(metrics, start, pointer_delta);
    let delta = dragged_top - tops.get(current)?;

    if delta > 0.0 {
        let below = *heights.get(current + 1)?;
        (delta >= below * STEP_THRESHOLD).then_some(Step::Down)
    } else if delta < 0.0 {
        let above = *heights.get(current.checked_sub(1)?)?;
        (-delta >= above * STEP_THRESHOLD).then_some(Step::Up)
    } else {
        None
    }
}

/// Which row carries the drop-insertion line, and on which edge (§13.11).
///
/// Because every boundary crossing is committed the moment it happens, the
/// prospective landing slot *is* the gap the dragged row left behind at
/// `current_position`. The line is drawn on the gap's lower edge - the top
/// border of the row below it - falling back to the gap's upper edge when
/// the drag is already at the last position and there is no row below.
fn insertion_line(row_count: usize, current_position: usize) -> Option<(usize, InsertionEdge)> {
    if row_count < 2 {
        return None;
    }
    if current_position + 1 < row_count {
        Some((current_position + 1, InsertionEdge::Top))
    } else {
        Some((current_position.checked_sub(1)?, InsertionEdge::Bottom))
    }
}

/// Folds the raw `(render position, top, bottom)` triples read off the DOM
/// at `pointerdown` into one [`RowMetrics`] per row, merging an expanded
/// row's two `<tr>` elements into the single block they visually are.
///
/// `None` if any row is missing a measurement, which aborts the gesture
/// rather than dragging against geometry that doesn't describe the table.
fn group_row_metrics(raw: &[(usize, f64, f64)], row_count: usize) -> Option<Vec<RowMetrics>> {
    if row_count == 0 {
        return None;
    }
    let mut bounds: Vec<Option<(f64, f64)>> = vec![None; row_count];
    for &(position, top, bottom) in raw {
        let slot = bounds.get_mut(position)?;
        *slot = Some(match *slot {
            Some((known_top, known_bottom)) => (known_top.min(top), known_bottom.max(bottom)),
            None => (top, bottom),
        });
    }
    bounds
        .into_iter()
        .map(|bound| {
            bound.map(|(top, bottom)| RowMetrics {
                top,
                height: bottom - top,
            })
        })
        .collect()
}

/// Marks the drag handle in the DOM so a pointer event landing on the icon
/// inside it still resolves to the handle itself.
const DRAG_HANDLE_SELECTOR: &str = "[data-drag-handle]";

/// The handle element a pointer event belongs to, if any.
fn handle_element(event: &PointerEvent) -> Option<web_sys::Element> {
    event
        .target()?
        .dyn_into::<web_sys::Element>()
        .ok()?
        .closest(DRAG_HANDLE_SELECTOR)
        .ok()
        .flatten()
}

/// Reads every row's current geometry out of the live DOM, once, at
/// `pointerdown` - the only DOM measurement this feature makes.
fn measure_rows(tbody: &web_sys::Element, row_count: usize) -> Option<Vec<RowMetrics>> {
    let nodes = tbody.query_selector_all("tr[data-row-position]").ok()?;
    let mut raw: Vec<(usize, f64, f64)> = Vec::with_capacity(nodes.length() as usize);
    for index in 0..nodes.length() {
        let element = nodes.item(index)?.dyn_into::<web_sys::Element>().ok()?;
        let position = element
            .get_attribute("data-row-position")?
            .parse::<usize>()
            .ok()?;
        let rect = element.get_bounding_client_rect();
        raw.push((position, rect.top(), rect.bottom()));
    }
    group_row_metrics(&raw, row_count)
}

#[function_component(EntriesTable)]
pub fn entries_table(props: &EntriesTableProps) -> Html {
    // Hooks must run unconditionally on every render (Rules of Hooks) - this
    // has to come before the empty-rows early return below, not after it,
    // or the hook count differs between an empty-then-populated transition
    // (the normal case on startup, before `list_entries` resolves) and Yew
    // panics with "Hooks are called conditionally".
    let expanded: UseStateHandle<HashSet<usize>> = use_state(HashSet::new);
    let drag: UseStateHandle<Option<DragState>> = use_state(|| None);

    // The grabbing cursor has to be document-level for the gesture's
    // duration (§13.11), or it flickers back to the default as the pointer
    // crosses other rows - and it has to be an *inline* style on `<body>`,
    // because `styles/input.css` sets `body { cursor: default }` outside
    // any cascade layer, which outranks every utility class. Running it as
    // an effect (rather than setting/clearing it in the pointer handlers)
    // means the cleanup also covers an unmount mid-gesture.
    {
        let dragging = drag.is_some();
        use_effect_with(dragging, move |dragging: &bool| {
            let body = web_sys::window()
                .and_then(|window| window.document())
                .and_then(|document| document.body());
            if *dragging {
                if let Some(body) = &body {
                    let style = body.style();
                    let _ = style.set_property("cursor", "grabbing");
                    let _ = style.set_property("user-select", "none");
                }
            }
            move || {
                if let Some(body) = body {
                    let style = body.style();
                    let _ = style.remove_property("cursor");
                    let _ = style.remove_property("user-select");
                }
            }
        });
    }

    if props.rows.is_empty() {
        let on_add = {
            let cb = props.on_add_entry.clone();
            Callback::from(move |_: MouseEvent| cb.emit(()))
        };
        return html! {
            <div class="bg-white dark:bg-slate-800 rounded-lg shadow-sm border border-slate-200 dark:border-slate-700">
                <EmptyState
                    icon="📄"
                    title="No entries yet"
                    action={Some(html! {
                        <button
                            type="button"
                            class="px-4 py-2 rounded-md bg-sky-600 hover:bg-sky-700 text-white text-sm font-medium focus-visible:ring-2 focus-visible:ring-sky-500 focus-visible:ring-offset-2"
                            onclick={on_add}
                        >
                            { "Add Entry" }
                        </button>
                    })}
                />
            </div>
        };
    }

    // Every row's candidate index in document order - the one list both
    // the Move buttons and a drag resolve their `(from, to)` pair from.
    // `Rc` only so the per-row closures below share one allocation.
    let candidate_indices: Rc<Vec<usize>> =
        Rc::new(props.rows.iter().map(|(index, _)| *index).collect());

    // A gesture's frozen geometry only describes the table it was measured
    // against. Row membership cannot change mid-gesture (the pointer is
    // captured and every other control is out of reach), but if it somehow
    // did, fall back to plain document-order rendering rather than drag
    // against stale geometry.
    let drag_state: Option<DragState> = (*drag).clone().filter(|state| {
        state.metrics.len() == props.rows.len() && state.keys.len() == props.rows.len()
    });
    let row_offsets: Option<Vec<f64>> = drag_state.as_ref().map(|state| {
        drag_offsets(
            &state.metrics,
            state.start_position,
            state.current_position,
            state.current_y - state.start_y,
        )
    });
    let insertion = drag_state
        .as_ref()
        .and_then(|state| insertion_line(props.rows.len(), state.current_position));

    // Render order: document order normally, the gesture's own frozen
    // order (keyed by the candidate indices it started with) while a drag
    // is in progress - see the module docs for why.
    let render_order: Vec<(usize, usize)> = match &drag_state {
        Some(state) => (0..props.rows.len())
            .map(|slot| {
                (
                    state.keys[slot],
                    permuted_position(state.start_position, state.current_position, slot),
                )
            })
            .collect(),
        None => props
            .rows
            .iter()
            .enumerate()
            .map(|(position, (index, _))| (*index, position))
            .collect(),
    };

    // `pointermove`: advance the gesture and, if this event crossed the
    // next boundary, commit exactly one adjacent step - the identical
    // `on_move` call the neighbouring Move button would have made, plus
    // the identical `expanded` remap its click handler does (§13.8). One
    // `if let`, never a loop: see the module docs on the staleness trap.
    let on_drag_move = {
        let drag = drag.clone();
        let expanded = expanded.clone();
        let on_move = props.on_move.clone();
        let attached_block_len = props.attached_block_len.clone();
        let candidate_indices = candidate_indices.clone();
        Callback::from(move |event: PointerEvent| {
            let Some(state) = (*drag).clone() else {
                return;
            };
            if event.pointer_id() != state.pointer_id {
                return;
            }
            if state.metrics.len() != candidate_indices.len() {
                drag.set(None);
                return;
            }
            let mut next = state;
            next.current_y = f64::from(event.client_y());
            let pointer_delta = next.current_y - next.start_y;
            if let Some(step) = plan_step(
                &next.metrics,
                next.start_position,
                next.current_position,
                pointer_delta,
            ) {
                let from = candidate_indices[next.current_position];
                if let Some(to) = neighbor_target(&candidate_indices, next.current_position, step) {
                    expanded.set(remap_expanded(&expanded, from, to, &attached_block_len));
                    next.current_position = match step {
                        Step::Up => next.current_position - 1,
                        Step::Down => next.current_position + 1,
                    };
                    on_move.emit((from, to));
                }
            }
            drag.set(Some(next));
        })
    };
    // `pointerup`/`pointercancel`/`lostpointercapture` all end the gesture
    // the same way: release the capture and drop every drag-only visual.
    // The committed document order is already live, so there is nothing to
    // apply here and nothing moves on screen.
    let on_drag_end = {
        let drag = drag.clone();
        Callback::from(move |event: PointerEvent| {
            if drag.is_none() {
                return;
            }
            if let Some(handle) = handle_element(&event) {
                if handle.has_pointer_capture(event.pointer_id()) {
                    let _ = handle.release_pointer_capture(event.pointer_id());
                }
            }
            drag.set(None);
        })
    };

    html! {
        <div class="bg-white dark:bg-slate-800 rounded-lg shadow-sm border border-slate-200 dark:border-slate-700 overflow-hidden">
            <table class="w-full text-sm">
                <thead class="bg-slate-50 dark:bg-slate-900/40 text-left text-xs font-medium text-slate-500 dark:text-slate-400 uppercase tracking-wide">
                    <tr>
                        <th class="px-2 py-2 w-8"><span class="sr-only">{ "Expand row" }</span></th>
                        <th class="px-4 py-2">{ "Source" }</th>
                        <th class="px-4 py-2">{ "Mount point" }</th>
                        <th class="px-4 py-2">{ "FS type" }</th>
                        <th class="px-4 py-2">{ "Options" }</th>
                        <th class="px-2 py-2">{ "Dump" }</th>
                        <th class="px-2 py-2">{ "Pass" }</th>
                        <th class="px-2 py-2 text-center">{ "Actions" }</th>
                    </tr>
                </thead>
                <tbody>
                    { for render_order.iter().enumerate().map(|(gesture_slot, &(key, row_position))| {
                        let (candidate_index, row) = &props.rows[row_position];
                        let candidate_index = *candidate_index;
                        let up_target = neighbor_target(&candidate_indices, row_position, Step::Up);
                        let down_target = neighbor_target(&candidate_indices, row_position, Step::Down);
                        // Both Move buttons stage the move *and* mirror it
                        // onto `expanded` (§13.8) - the set is keyed by
                        // position, so expansion would otherwise stay with
                        // the slot rather than with the entry.
                        let make_move = |target: Option<usize>| {
                            let cb = props.on_move.clone();
                            let expanded = expanded.clone();
                            let attached_block_len = props.attached_block_len.clone();
                            target.map(|to| Callback::from(move |e: MouseEvent| {
                                e.stop_propagation();
                                expanded.set(remap_expanded(
                                    &expanded,
                                    candidate_index,
                                    to,
                                    &attached_block_len,
                                ));
                                cb.emit((candidate_index, to));
                            }))
                        };
                        let on_move_up = make_move(up_target);
                        let on_move_down = make_move(down_target);
                        let on_edit = {
                            let cb = props.on_edit.clone();
                            Callback::from(move |e: MouseEvent| {
                                e.stop_propagation();
                                cb.emit(candidate_index);
                            })
                        };
                        let on_delete = {
                            let cb = props.on_delete.clone();
                            Callback::from(move |e: MouseEvent| {
                                e.stop_propagation();
                                cb.emit(candidate_index);
                            })
                        };
                        // `pointerdown` on this row's handle: capture the
                        // pointer, freeze the table's geometry, and start
                        // the gesture at this row's *current* render
                        // position (no drag is in progress yet, so render
                        // order is still document order and the gesture
                        // slot equals the render position).
                        let on_drag_start = {
                            let drag = drag.clone();
                            let candidate_indices = candidate_indices.clone();
                            Callback::from(move |event: PointerEvent| {
                                // Only the primary button/contact drags: a
                                // right-click or a second finger on the
                                // handle is not a reorder gesture.
                                if event.button() != 0 || !event.is_primary() {
                                    return;
                                }
                                // The row's own `<tr>` carries the T32
                                // expand/collapse click (§13.11's whole
                                // reason for a dedicated handle) - keep the
                                // gesture off it, and off the browser's
                                // text selection.
                                event.stop_propagation();
                                event.prevent_default();
                                let Some(handle) = handle_element(&event) else {
                                    return;
                                };
                                let Some(tbody) = handle.closest("tbody").ok().flatten() else {
                                    return;
                                };
                                let Some(metrics) = measure_rows(&tbody, candidate_indices.len())
                                else {
                                    return;
                                };
                                let _ = handle.set_pointer_capture(event.pointer_id());
                                let pointer_y = f64::from(event.client_y());
                                drag.set(Some(DragState {
                                    pointer_id: event.pointer_id(),
                                    start_position: row_position,
                                    current_position: row_position,
                                    start_y: pointer_y,
                                    current_y: pointer_y,
                                    metrics,
                                    keys: (*candidate_indices).clone(),
                                }));
                            })
                        };
                        let drag_offset = row_offsets.as_ref().map(|offsets| offsets[gesture_slot]);
                        let dragging = drag_state
                            .as_ref()
                            .is_some_and(|state| state.start_position == gesture_slot);
                        let insertion_edge = insertion.and_then(|(position, edge)| {
                            (position == row_position).then_some(edge)
                        });
                        let row_html = match row {
                            RowEntry::Parsed(entry) => {
                                let warnings = props
                                    .row_warnings
                                    .get(&candidate_index)
                                    .cloned()
                                    .unwrap_or_default();
                                let is_expanded = expanded.contains(&candidate_index);
                                let on_toggle = {
                                    let expanded = expanded.clone();
                                    Callback::from(move |_: MouseEvent| {
                                        let mut next = (*expanded).clone();
                                        if !next.remove(&candidate_index) {
                                            next.insert(candidate_index);
                                        }
                                        expanded.set(next);
                                    })
                                };
                                html! {
                                    <EntryRow
                                        key={key}
                                        entry={entry.clone()}
                                        warnings={warnings}
                                        expanded={is_expanded}
                                        row_position={row_position}
                                        on_toggle={on_toggle}
                                        on_move_up={on_move_up}
                                        on_move_down={on_move_down}
                                        on_edit={on_edit}
                                        on_delete={on_delete}
                                        on_drag_start={on_drag_start}
                                        on_drag_move={on_drag_move.clone()}
                                        on_drag_end={on_drag_end.clone()}
                                        drag_offset={drag_offset}
                                        dragging={dragging}
                                        insertion_edge={insertion_edge}
                                        show_toast={props.show_toast.clone()}
                                    />
                                }
                            }
                            RowEntry::Unparsed(raw_line) => html! {
                                <UnparsedEntryRow
                                    key={key}
                                    raw_line={AttrValue::from(raw_line.clone())}
                                    row_position={row_position}
                                    on_move_up={on_move_up}
                                    on_move_down={on_move_down}
                                    on_edit={on_edit}
                                    on_delete={on_delete}
                                    on_drag_start={on_drag_start}
                                    on_drag_move={on_drag_move.clone()}
                                    on_drag_end={on_drag_end.clone()}
                                    drag_offset={drag_offset}
                                    dragging={dragging}
                                    insertion_edge={insertion_edge}
                                />
                            },
                        };
                        row_html
                    }) }
                </tbody>
            </table>
        </div>
    }
}

#[derive(Properties, PartialEq)]
pub struct EntryRowProps {
    pub entry: EntryDto,
    /// Document-wide warning messages about this row specifically (see
    /// `EntriesTableProps::row_warnings`). Empty means no known warning.
    #[prop_or_default]
    pub warnings: Vec<AttrValue>,
    /// Whether this row's detail panel is currently open (T32) - owned by
    /// `EntriesTable`, not this component, so several rows can be expanded
    /// at once.
    pub expanded: bool,
    /// Toggles `expanded` for this row's own `candidate_index`. Clicking
    /// anywhere on the row's `<tr>` fires this; Edit/Delete call
    /// `stop_propagation` first so neither of them also toggles the row.
    pub on_toggle: Callback<MouseEvent>,
    /// This row's position in the table's render order, mirrored into a
    /// `data-row-position` attribute on both of this row's `<tr>`s so one
    /// `pointerdown` can measure the whole table's geometry in a single
    /// DOM query (T48).
    pub row_position: usize,
    /// Moves this entry one position up/down (T40, §13.2). `None` means
    /// this row is at that boundary of the table, which renders a real
    /// `disabled` button rather than hiding it.
    pub on_move_up: Option<Callback<MouseEvent>>,
    pub on_move_down: Option<Callback<MouseEvent>>,
    pub on_edit: Callback<MouseEvent>,
    pub on_delete: Callback<MouseEvent>,
    /// Drag-handle pointer wiring (T48, §13.11) - the gesture's whole
    /// state machine lives in `EntriesTable`, which owns the document
    /// order; this row only forwards its handle's pointer events.
    pub on_drag_start: Callback<PointerEvent>,
    pub on_drag_move: Callback<PointerEvent>,
    pub on_drag_end: Callback<PointerEvent>,
    /// This row's `translateY`, in pixels, while a drag is in progress;
    /// `None` when no drag is running (no transform, no transition).
    #[prop_or_default]
    pub drag_offset: Option<f64>,
    /// Whether *this* is the row being dragged - lifted, and glued to the
    /// pointer with no transition of its own (§13.11).
    #[prop_or_default]
    pub dragging: bool,
    /// Draws the drop-insertion line on this row's top/bottom edge.
    #[prop_or_default]
    pub insertion_edge: Option<InsertionEdge>,
    /// Surfaces a copy/open-in-file-manager failure from the expanded
    /// panel's Mount point buttons.
    pub show_toast: Callback<(ToastKind, String)>,
}

/// The `transition: transform 150ms ease-out` §13.11 specifies for
/// displaced rows, reusing the drawer's own motion token - and §8's
/// `prefers-reduced-motion` rule applied the only way that makes sense for
/// a pure position change: the row snaps straight to its new place with no
/// transition at all (an opacity fade would say nothing about position).
const ROW_SLIDE_CLASS: &str =
    "transition-transform duration-150 ease-out motion-reduce:transition-none";

/// The drop-insertion line (§13.11): full row width, at the boundary the
/// dragged row lands on if released now. `border-t-sky-500` rather than a
/// bare `border-sky-500` so it cannot repaint a warning row's amber left
/// border.
const INSERTION_TOP_CLASS: &str = "border-t-2 border-t-sky-500 dark:border-t-sky-400";
const INSERTION_BOTTOM_CLASS: &str = "border-b-2 border-b-sky-500 dark:border-b-sky-400";

/// The drag handle (T48, §13.11).
///
/// Deliberately **not** a `<button>`: it is `aria-hidden`, carries no
/// `tabindex` and never takes focus, because there is no sensible keyboard
/// activation for "start a continuous pointer drag" and Move up/Move down
/// already are this table's fully keyboard-operable reorder path. The
/// `title` keeps it discoverable for a sighted mouse user.
///
/// The `onclick` that stops propagation is load-bearing, not defensive
/// tidiness: `stop_propagation` on `pointerdown` does not stop the `click`
/// the browser synthesises afterwards, which would otherwise bubble to the
/// row's own `<tr>` and toggle the T32 expansion panel on every grab.
fn drag_handle(
    title: &'static str,
    hover_class: &'static str,
    on_drag_start: &Callback<PointerEvent>,
    on_drag_move: &Callback<PointerEvent>,
    on_drag_end: &Callback<PointerEvent>,
) -> Html {
    html! {
        <span
            data-drag-handle="true"
            class={classes!(
                "inline-flex",
                "items-center",
                "p-2",
                "rounded-md",
                "text-slate-400",
                "dark:text-slate-500",
                "cursor-grab",
                "active:cursor-grabbing",
                "touch-none",
                "select-none",
                hover_class,
            )}
            title={title}
            aria-hidden="true"
            onpointerdown={on_drag_start.clone()}
            onpointermove={on_drag_move.clone()}
            onpointerup={on_drag_end.clone()}
            onpointercancel={on_drag_end.clone()}
            onlostpointercapture={on_drag_end.clone()}
            onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}
        >
            <GripVerticalIcon />
        </span>
    }
}

/// The inline `transform` a row carries while a drag is in progress.
/// Always written out explicitly (including `0px`) so every committed step
/// changes a *value* the browser can transition from, rather than toggling
/// between `none` and a transform.
fn drag_transform_style(drag_offset: Option<f64>) -> Option<AttrValue> {
    drag_offset.map(|offset| AttrValue::from(format!("transform: translateY({offset:.2}px)")))
}

/// Builds a "copy `value` to the clipboard" click handler, flashing
/// `just_copied` for ~1.5s on success. Shared by Source and Mount point's
/// copy buttons in the row detail panel (T32).
fn make_copy_callback(
    value: String,
    just_copied: UseStateHandle<bool>,
    show_toast: Callback<(ToastKind, String)>,
    error_message: &'static str,
) -> Callback<MouseEvent> {
    Callback::from(move |e: MouseEvent| {
        e.stop_propagation();
        let just_copied = just_copied.clone();
        let show_toast = show_toast.clone();
        // The clipboard-write call itself must happen synchronously in this
        // handler, before any `.await`, to stay inside the user-gesture
        // context the webview requires for clipboard permission - only the
        // *result* is awaited inside the spawned future below.
        let Some(window) = web_sys::window() else {
            show_toast.emit((ToastKind::Error, error_message.to_string()));
            return;
        };
        let promise = window.navigator().clipboard().write_text(&value);
        wasm_bindgen_futures::spawn_local(async move {
            match wasm_bindgen_futures::JsFuture::from(promise).await {
                Ok(_) => {
                    just_copied.set(true);
                    gloo_timers::future::TimeoutFuture::new(1500).await;
                    just_copied.set(false);
                }
                Err(_) => {
                    show_toast.emit((ToastKind::Error, error_message.to_string()));
                }
            }
        });
    })
}

/// Builds an "open `path` in the system file manager" click handler. Shared
/// by Source (only when its kind is `Path`) and Mount point's open-folder
/// buttons in the row detail panel (T32).
fn make_open_folder_callback(
    path: String,
    show_toast: Callback<(ToastKind, String)>,
) -> Callback<MouseEvent> {
    Callback::from(move |e: MouseEvent| {
        e.stop_propagation();
        let path = path.clone();
        let show_toast = show_toast.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(message) = api::open_path_in_file_manager(&path).await {
                show_toast.emit((ToastKind::Error, message));
            }
        });
    })
}

#[function_component(EntryRow)]
pub fn entry_row(props: &EntryRowProps) -> Html {
    let entry = &props.entry;
    let options: Vec<String> = entry.options.iter().map(format_option).collect();
    let shown_options: Vec<&String> = options.iter().take(3).collect();
    let overflow = options.len().saturating_sub(3);
    let full_list = options.join(", ");

    let has_warnings = !props.warnings.is_empty();
    // Drag-only visuals (T48, §13.11). The dragged row is lifted, kept
    // close to fully opaque ("the real row, just lifted", not a ghost) and
    // explicitly *not* transitioned, so it tracks the pointer 1:1; only
    // the rows it displaces animate. The insertion line's own edge
    // replaces the row's default 1px bottom border rather than layering on
    // top of it, so the sky line's width cannot depend on the order
    // Tailwind happens to emit two `border-b` widths in.
    let dragging = props.dragging;
    let in_drag = props.drag_offset.is_some();
    let sliding = !dragging && in_drag;
    let insertion_bottom = props.insertion_edge == Some(InsertionEdge::Bottom) && !props.expanded;
    let row_class = classes!(
        "h-11",
        // Every row drops its own click cursor for the gesture's duration,
        // dragged or not, so the grabbing cursor `EntriesTable` puts on
        // `<body>` shows through instead of flickering back to a click
        // cursor as the pointer crosses other rows (§13.11).
        (!in_drag).then_some("cursor-pointer"),
        in_drag.then_some("cursor-grabbing"),
        "odd:bg-slate-50",
        "dark:odd:bg-slate-800/60",
        "hover:bg-sky-50",
        "dark:hover:bg-slate-700/60",
        (!insertion_bottom).then_some("border-b"),
        "border-slate-200",
        "dark:border-slate-700",
        has_warnings.then_some("border-l-4"),
        has_warnings.then_some("border-l-amber-400"),
        has_warnings.then_some("dark:border-l-amber-500"),
        dragging.then_some("relative z-10 shadow-lg opacity-90"),
        sliding.then_some(ROW_SLIDE_CLASS),
        (props.insertion_edge == Some(InsertionEdge::Top)).then_some(INSERTION_TOP_CLASS),
        insertion_bottom.then_some(INSERTION_BOTTOM_CLASS),
    );
    let drag_style = drag_transform_style(props.drag_offset);
    let row_position = AttrValue::from(props.row_position.to_string());
    // The expanded detail row is part of the same visual block as its own
    // row: it carries the same transform (so it travels with it) and the
    // same `data-row-position` (so `pointerdown`'s measurement counts its
    // height as part of that row).
    let detail_class = classes!(
        "bg-slate-50",
        "dark:bg-slate-900/40",
        "border-l-4",
        "border-l-sky-400",
        "dark:border-l-sky-500",
        (props.insertion_edge != Some(InsertionEdge::Bottom)).then_some("border-b"),
        "border-slate-200",
        "dark:border-slate-700",
        dragging.then_some("relative z-10 shadow-lg opacity-90"),
        sliding.then_some(ROW_SLIDE_CLASS),
        (props.insertion_edge == Some(InsertionEdge::Bottom)).then_some(INSERTION_BOTTOM_CLASS),
    );
    let row_title: Option<AttrValue> = has_warnings.then(|| {
        AttrValue::from(
            props
                .warnings
                .iter()
                .map(|w| w.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        )
    });

    let chevron_class = classes!(
        "inline-flex",
        "transition-transform",
        "duration-150",
        props.expanded.then_some("rotate-90"),
    );

    // Clipboard "just copied" flash, local to this row (T32 §6 of the
    // task): flips to `true` right after a successful `write_text`, then
    // back to `false` ~1.5s later, swapping the copy icon for a checkmark
    // in between. Source and Mount point each get their own flash state so
    // copying one doesn't flash the other's button.
    let source_just_copied = use_state(|| false);
    let mount_just_copied = use_state(|| false);
    let on_copy_source = make_copy_callback(
        entry.source.value.clone(),
        source_just_copied.clone(),
        props.show_toast.clone(),
        "Could not copy the source to the clipboard.",
    );
    let on_copy_mount = make_copy_callback(
        entry.mount_point.clone(),
        mount_just_copied.clone(),
        props.show_toast.clone(),
        "Could not copy the mount point to the clipboard.",
    );

    // Only a `Path` source names something that can actually be opened in a
    // file manager (e.g. a bind mount's source directory) - a UUID/LABEL/
    // network source etc. isn't a path on this filesystem at all, so those
    // get a copy button only.
    let on_open_folder_source = (entry.source.kind == SourceKind::Path)
        .then(|| make_open_folder_callback(entry.source.value.clone(), props.show_toast.clone()));
    let on_open_folder_mount =
        make_open_folder_callback(entry.mount_point.clone(), props.show_toast.clone());

    let description = entry.description.clone();
    let trailing_comment = entry.trailing_comment.clone();

    html! {
        <>
            <tr
                class={row_class}
                style={drag_style.clone()}
                data-row-position={row_position.clone()}
                title={row_title}
                onclick={props.on_toggle.clone()}
            >
                <td class="px-2 py-2 text-slate-400 dark:text-slate-500">
                    <span class={chevron_class} aria-hidden="true">
                        <ChevronRightIcon />
                    </span>
                </td>
                <td class="px-4 py-2 font-mono text-sm max-w-[16rem]">
                    <div class="flex items-center gap-2 min-w-0">
                        <span class="shrink-0 inline-block rounded-full bg-slate-100 dark:bg-slate-700 text-slate-600 dark:text-slate-300 text-xs font-medium px-2 py-0.5">
                            { source_badge(entry.source.kind) }
                        </span>
                        <span class="truncate min-w-0" title={entry.source.value.clone()}>
                            { entry.source.value.clone() }
                        </span>
                    </div>
                </td>
                <td class="px-4 py-2 font-mono text-sm max-w-[14rem]">
                    <span class="block truncate" title={entry.mount_point.clone()}>
                        { entry.mount_point.clone() }
                    </span>
                </td>
                <td class="px-4 py-2 text-sm">
                    <span class="rounded-md bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-slate-600 dark:text-slate-300">
                        { entry.fs_type.clone() }
                    </span>
                </td>
                <td class="px-4 py-2 text-sm" title={full_list}>
                    { for shown_options.iter().map(|opt| html! {
                        <span class="inline-block rounded-full bg-sky-100 text-sky-700 dark:bg-sky-900/40 dark:text-sky-300 text-xs font-medium px-3 py-1 mr-1">
                            { (*opt).clone() }
                        </span>
                    }) }
                    if overflow > 0 {
                        <span class="text-xs text-slate-500 dark:text-slate-400">{ format!("+{overflow} more") }</span>
                    }
                </td>
                <td class="px-2 py-2 text-sm">{ entry.dump.to_string() }</td>
                <td class="px-2 py-2 text-sm">{ entry.pass.to_string() }</td>
                <td class="px-2 py-2 text-center whitespace-nowrap">
                    <div class="inline-flex items-center gap-1">
                        { drag_handle(
                            "Drag to reorder",
                            "hover:bg-slate-100 dark:hover:bg-slate-700",
                            &props.on_drag_start,
                            &props.on_drag_move,
                            &props.on_drag_end,
                        ) }
                        { move_button(
                            &props.on_move_up,
                            "Move entry up",
                            "Already the first entry",
                            "hover:bg-slate-100 dark:hover:bg-slate-700",
                            html! { <ChevronUpIcon /> },
                        ) }
                        { move_button(
                            &props.on_move_down,
                            "Move entry down",
                            "Already the last entry",
                            "hover:bg-slate-100 dark:hover:bg-slate-700",
                            html! { <ChevronDownIcon /> },
                        ) }
                        <button
                            type="button"
                            aria-label="Edit entry"
                            class="p-2 rounded-md hover:bg-slate-100 dark:hover:bg-slate-700 focus-visible:ring-2 focus-visible:ring-sky-500"
                            onclick={props.on_edit.clone()}
                        >
                            <EditIcon />
                        </button>
                        <button
                            type="button"
                            aria-label="Delete entry"
                            class="p-2 rounded-md text-slate-600 dark:text-slate-400 hover:bg-red-50 hover:text-red-600 dark:hover:bg-red-900/30 focus-visible:ring-2 focus-visible:ring-sky-500"
                            onclick={props.on_delete.clone()}
                        >
                            <DeleteIcon />
                        </button>
                    </div>
                </td>
            </tr>
            if props.expanded {
                <tr class={detail_class} style={drag_style} data-row-position={row_position}>
                    <td colspan="8" class="px-6 py-5">
                        <div class="grid grid-cols-[minmax(0,1fr)_minmax(200px,auto)] gap-x-8 gap-y-5">
                            <div>
                                <p class="text-xs font-medium text-slate-500 dark:text-slate-400 uppercase tracking-wide mb-1">
                                    { "Source" }
                                </p>
                                <div class="flex items-center gap-1.5 font-mono text-sm">
                                    <span class="shrink-0 inline-block rounded-full bg-slate-100 dark:bg-slate-700 text-slate-600 dark:text-slate-300 text-xs font-medium px-2 py-0.5">
                                        { source_badge(entry.source.kind) }
                                    </span>
                                    <span class="break-all">{ entry.source.value.clone() }</span>
                                    <button
                                        type="button"
                                        aria-label="Copy source to clipboard"
                                        class="p-1.5 rounded-md text-slate-500 dark:text-slate-400 hover:bg-slate-200 dark:hover:bg-slate-700 focus-visible:ring-2 focus-visible:ring-sky-500 shrink-0"
                                        onclick={on_copy_source}
                                    >
                                        if *source_just_copied {
                                            <CheckIcon />
                                        } else {
                                            <CopyIcon />
                                        }
                                    </button>
                                    if let Some(on_open_folder_source) = on_open_folder_source {
                                        <button
                                            type="button"
                                            aria-label="Open source in file manager"
                                            class="p-1.5 rounded-md text-slate-500 dark:text-slate-400 hover:bg-slate-200 dark:hover:bg-slate-700 focus-visible:ring-2 focus-visible:ring-sky-500 shrink-0"
                                            onclick={on_open_folder_source}
                                        >
                                            <FolderOpenIcon />
                                        </button>
                                    }
                                </div>
                            </div>
                            <div>
                                <p class="text-xs font-medium text-slate-500 dark:text-slate-400 uppercase tracking-wide mb-1">
                                    { "FS type" }
                                </p>
                                <span class="rounded-md bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-sm text-slate-600 dark:text-slate-300">
                                    { entry.fs_type.clone() }
                                </span>
                            </div>
                            <div>
                                <p class="text-xs font-medium text-slate-500 dark:text-slate-400 uppercase tracking-wide mb-1">
                                    { "Mount point" }
                                </p>
                                <div class="flex items-center gap-1.5 font-mono text-sm">
                                    <span class="break-all">{ entry.mount_point.clone() }</span>
                                    <button
                                        type="button"
                                        aria-label="Copy mount point to clipboard"
                                        class="p-1.5 rounded-md text-slate-500 dark:text-slate-400 hover:bg-slate-200 dark:hover:bg-slate-700 focus-visible:ring-2 focus-visible:ring-sky-500 shrink-0"
                                        onclick={on_copy_mount}
                                    >
                                        if *mount_just_copied {
                                            <CheckIcon />
                                        } else {
                                            <CopyIcon />
                                        }
                                    </button>
                                    <button
                                        type="button"
                                        aria-label="Open mount point in file manager"
                                        class="p-1.5 rounded-md text-slate-500 dark:text-slate-400 hover:bg-slate-200 dark:hover:bg-slate-700 focus-visible:ring-2 focus-visible:ring-sky-500 shrink-0"
                                        onclick={on_open_folder_mount}
                                    >
                                        <FolderOpenIcon />
                                    </button>
                                </div>
                            </div>
                            <div>
                                <p class="text-xs font-medium text-slate-500 dark:text-slate-400 uppercase tracking-wide mb-1">
                                    { "Dump / Pass" }
                                </p>
                                <span class="text-sm">{ format!("{} / {}", entry.dump, entry.pass) }</span>
                            </div>
                            <div class="col-span-2">
                                <p class="text-xs font-medium text-slate-500 dark:text-slate-400 uppercase tracking-wide mb-1">
                                    { "Options" }
                                </p>
                                <div>
                                    { for options.iter().map(|opt| html! {
                                        <span class="inline-block rounded-full bg-sky-100 text-sky-700 dark:bg-sky-900/40 dark:text-sky-300 text-xs font-medium px-3 py-1 mr-1 mb-1">
                                            { opt.clone() }
                                        </span>
                                    }) }
                                </div>
                            </div>
                            if let Some(description) = description {
                                <div class="col-span-2">
                                    <p class="text-xs font-medium text-slate-500 dark:text-slate-400 uppercase tracking-wide mb-1">
                                        { "Description" }
                                    </p>
                                    <p class="text-sm">{ description }</p>
                                </div>
                            }
                            if let Some(trailing_comment) = trailing_comment {
                                <div class="col-span-2">
                                    <p class="text-xs font-medium text-slate-500 dark:text-slate-400 uppercase tracking-wide mb-1">
                                        { "Trailing comment" }
                                    </p>
                                    <p class="text-sm font-mono">{ trailing_comment }</p>
                                </div>
                            }
                        </div>
                    </td>
                </tr>
            }
        </>
    }
}

#[derive(Properties, PartialEq)]
pub struct UnparsedEntryRowProps {
    pub raw_line: AttrValue,
    /// See `EntryRowProps::row_position` (T48).
    pub row_position: usize,
    /// Same contract as `EntryRowProps`' own pair: `None` is the boundary
    /// state and renders a real `disabled` button (§13.2).
    pub on_move_up: Option<Callback<MouseEvent>>,
    pub on_move_down: Option<Callback<MouseEvent>>,
    pub on_edit: Callback<MouseEvent>,
    pub on_delete: Callback<MouseEvent>,
    /// Same drag wiring and drag-only visuals as `EntryRowProps` - a raw
    /// row is reordered exactly like a parsed one (§13.11).
    pub on_drag_start: Callback<PointerEvent>,
    pub on_drag_move: Callback<PointerEvent>,
    pub on_drag_end: Callback<PointerEvent>,
    #[prop_or_default]
    pub drag_offset: Option<f64>,
    #[prop_or_default]
    pub dragging: bool,
    #[prop_or_default]
    pub insertion_edge: Option<InsertionEdge>,
}

#[function_component(UnparsedEntryRow)]
pub fn unparsed_entry_row(props: &UnparsedEntryRowProps) -> Html {
    let dragging = props.dragging;
    let in_drag = props.drag_offset.is_some();
    let sliding = !dragging && in_drag;
    let insertion_bottom = props.insertion_edge == Some(InsertionEdge::Bottom);
    let row_class = classes!(
        "bg-amber-50",
        "dark:bg-amber-900/20",
        "border-l-4",
        "border-amber-500",
        (!insertion_bottom).then_some("border-b"),
        "border-slate-200",
        "dark:border-slate-700",
        // A raw row has no click-to-expand cursor of its own, so it only
        // needs the grabbing one for the gesture's duration (§13.11).
        in_drag.then_some("cursor-grabbing"),
        dragging.then_some("relative z-10 shadow-lg opacity-90"),
        sliding.then_some(ROW_SLIDE_CLASS),
        (props.insertion_edge == Some(InsertionEdge::Top)).then_some(INSERTION_TOP_CLASS),
        insertion_bottom.then_some(INSERTION_BOTTOM_CLASS),
    );
    html! {
        <tr
            class={row_class}
            style={drag_transform_style(props.drag_offset)}
            data-row-position={props.row_position.to_string()}
        >
            <td colspan="8" class="px-4 py-3">
                <div class="flex items-start gap-3">
                    <span class="text-amber-600 dark:text-amber-400 mt-0.5" aria-hidden="true">{ "⚠" }</span>
                    <div class="flex-1">
                        <p class="text-sm font-medium text-amber-800 dark:text-amber-300 mb-1">
                            { "Couldn't fully parse this line - shown as raw text" }
                        </p>
                        <pre class="font-mono text-sm bg-slate-900/5 dark:bg-slate-950/40 rounded-md px-3 py-2 whitespace-pre-wrap">
                            { props.raw_line.clone() }
                        </pre>
                    </div>
                    <div class="flex gap-1 shrink-0">
                        { drag_handle(
                            "Drag to reorder raw entry",
                            "hover:bg-amber-100 dark:hover:bg-amber-900/40",
                            &props.on_drag_start,
                            &props.on_drag_move,
                            &props.on_drag_end,
                        ) }
                        { move_button(
                            &props.on_move_up,
                            "Move raw entry up",
                            "Already the first entry",
                            "hover:bg-amber-100 dark:hover:bg-amber-900/40",
                            html! { <ChevronUpIcon /> },
                        ) }
                        { move_button(
                            &props.on_move_down,
                            "Move raw entry down",
                            "Already the last entry",
                            "hover:bg-amber-100 dark:hover:bg-amber-900/40",
                            html! { <ChevronDownIcon /> },
                        ) }
                        <button
                            type="button"
                            aria-label="Edit raw entry"
                            class="p-2 rounded-md hover:bg-amber-100 dark:hover:bg-amber-900/40 focus-visible:ring-2 focus-visible:ring-sky-500"
                            onclick={props.on_edit.clone()}
                        >
                            <EditIcon />
                        </button>
                        <button
                            type="button"
                            aria-label="Delete entry"
                            class="p-2 rounded-md text-slate-600 dark:text-slate-400 hover:bg-red-50 hover:text-red-600 dark:hover:bg-red-900/30 focus-visible:ring-2 focus-visible:ring-sky-500"
                            onclick={props.on_delete.clone()}
                        >
                            <DeleteIcon />
                        </button>
                    </div>
                </div>
            </td>
        </tr>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_row_cannot_move_up_and_the_last_cannot_move_down() {
        let row_count = 3;
        assert!(!can_move_up(0));
        assert!(can_move_down(0, row_count));
        assert!(can_move_up(1));
        assert!(can_move_down(1, row_count));
        assert!(can_move_up(2));
        assert!(!can_move_down(2, row_count));
    }

    #[test]
    fn a_single_row_table_disables_both_directions() {
        assert!(!can_move_up(0));
        assert!(!can_move_down(0, 1));
    }

    /// No row has an attached comment block - the T40 baseline, and still
    /// the common case.
    fn no_blocks() -> HashMap<usize, usize> {
        HashMap::new()
    }

    #[test]
    fn expansion_follows_the_moved_entry_not_the_slot_it_left() {
        // §13.8: `expanded` is keyed by position, so a move has to carry
        // the entry's expansion with it.
        let expanded: HashSet<usize> = [2].into_iter().collect();

        // The expanded row itself moves down into slot 4.
        let after_moving_it = remap_expanded(&expanded, 2, 4, &no_blocks());
        assert_eq!(after_moving_it, [4].into_iter().collect::<HashSet<usize>>());

        // A collapsed row moves into the expanded row's slot: the
        // expansion goes with the entry that left, not with the slot.
        let after_moving_the_other = remap_expanded(&expanded, 4, 2, &no_blocks());
        assert_eq!(
            after_moving_the_other,
            [4].into_iter().collect::<HashSet<usize>>()
        );
    }

    #[test]
    fn swapping_two_rows_that_are_both_or_neither_expanded_leaves_the_set_alone() {
        let both: HashSet<usize> = [1, 2, 9].into_iter().collect();
        assert_eq!(remap_expanded(&both, 1, 2, &no_blocks()), both);

        let neither: HashSet<usize> = [9].into_iter().collect();
        assert_eq!(remap_expanded(&neither, 1, 2, &no_blocks()), neither);
    }

    #[test]
    fn swapping_an_expanded_row_with_itself_keeps_it_expanded() {
        let expanded: HashSet<usize> = [3].into_iter().collect();
        assert_eq!(remap_expanded(&expanded, 3, 3, &no_blocks()), expanded);
    }

    #[test]
    fn expansion_follows_the_entry_to_its_real_index_when_blocks_differ_in_length() {
        // §13.10 case 9, the direct regression test for the index-shift
        // bug. Same document as `candidate.rs`'s asymmetric-block tests:
        // row 3 (`/a`) carries a 3-line block, row 5 (`/b`) carries none,
        // so `/a` really lands at 5 and `/b` at 0 - T40's naive swap would
        // have moved `/b`'s expansion to 3, over a comment line.
        let attached: HashMap<usize, usize> = [(3, 3), (5, 0)].into_iter().collect();

        let a_expanded: HashSet<usize> = [3].into_iter().collect();
        assert_eq!(
            remap_expanded(&a_expanded, 3, 5, &attached),
            [5].into_iter().collect::<HashSet<usize>>()
        );

        let b_expanded: HashSet<usize> = [5].into_iter().collect();
        assert_eq!(
            remap_expanded(&b_expanded, 3, 5, &attached),
            [0].into_iter().collect::<HashSet<usize>>(),
            "`/b` lands at 0, not at the naive `from` of 3"
        );

        // Clicking Move up on `/b` instead of Move down on `/a` is the
        // same move, so it must produce the same mapping.
        assert_eq!(
            remap_expanded(&b_expanded, 5, 3, &attached),
            [0].into_iter().collect::<HashSet<usize>>()
        );

        // Both expanded: both follow their own entry, and an unrelated
        // expanded row outside the affected range is untouched.
        let both: HashSet<usize> = [3, 5, 9].into_iter().collect();
        assert_eq!(
            remap_expanded(&both, 3, 5, &attached),
            [0, 5, 9].into_iter().collect::<HashSet<usize>>()
        );
    }

    // ---------------------------------------------------------------
    // Drag-and-drop (T48, `docs/UI_DESIGN.md` §13.11/§13.12)
    //
    // Everything §13.12 lists as unit-testable is pure geometry/
    // bookkeeping, deliberately extracted out of the pointer handlers the
    // same way `can_move_up`/`remap_expanded` already are, so it is
    // testable without a DOM. The wiring those functions feed (real
    // pointer events, the actual CSS transition, focus order) is §13.12's
    // own manual/code-review list.
    // ---------------------------------------------------------------

    /// `count` rows of equal `height`, stacked from y=0 - the common case
    /// (every `EntryRow` is `h-11`, i.e. 44px, when collapsed).
    fn uniform_metrics(count: usize, height: f64) -> Vec<RowMetrics> {
        (0..count)
            .map(|position| RowMetrics {
                top: position as f64 * height,
                height,
            })
            .collect()
    }

    /// Stacks the given heights from y=0 - for the mixed-height cases this
    /// table really has (a tall `UnparsedEntryRow`, an expanded row).
    fn metrics_from_heights(heights: &[f64]) -> Vec<RowMetrics> {
        let mut top = 0.0;
        heights
            .iter()
            .map(|&height| {
                let metric = RowMetrics { top, height };
                top += height;
                metric
            })
            .collect()
    }

    #[test]
    fn the_gesture_permutation_is_a_rotation_and_round_trips() {
        // Nothing has been committed yet: identity, both ways.
        for slot in 0..4 {
            assert_eq!(permuted_position(2, 2, slot), slot);
            assert_eq!(gesture_slot_at(2, 2, slot), slot);
        }

        // The row that started at slot 0 has been dragged down to
        // position 2: the two rows it passed each shifted up one.
        assert_eq!(permuted_position(0, 2, 0), 2);
        assert_eq!(permuted_position(0, 2, 1), 0);
        assert_eq!(permuted_position(0, 2, 2), 1);
        assert_eq!(permuted_position(0, 2, 3), 3);

        // Dragged upward instead: the rows it passed shift down one.
        assert_eq!(permuted_position(3, 1, 3), 1);
        assert_eq!(permuted_position(3, 1, 1), 2);
        assert_eq!(permuted_position(3, 1, 2), 3);
        assert_eq!(permuted_position(3, 1, 0), 0);

        // `gesture_slot_at` is its exact inverse, which is what makes
        // "whose row is at render position p" and "where is slot g now"
        // one consistent story rather than two hand-derived ones.
        for (start, current) in [(0, 2), (3, 1), (1, 1), (0, 3)] {
            for slot in 0..4 {
                assert_eq!(
                    gesture_slot_at(start, current, permuted_position(start, current, slot)),
                    slot,
                    "start {start}, current {current}, slot {slot}"
                );
            }
        }
    }

    #[test]
    fn nothing_is_offset_before_the_drag_crosses_a_boundary() {
        // §13.12 item 2, the baseline: the dragged row follows the
        // pointer, every other row sits exactly where it already is (an
        // explicit 0, so the next committed step transitions from a real
        // value rather than from `none`).
        let metrics = uniform_metrics(4, 44.0);
        assert_eq!(
            drag_offsets(&metrics, 1, 1, 12.0),
            vec![0.0, 12.0, 0.0, 0.0]
        );
    }

    #[test]
    fn only_the_rows_the_drag_passed_slide_and_only_by_their_own_height() {
        // §13.12 item 2: one step down - the single row that was passed
        // slides up by exactly one row height, nothing else moves, and the
        // dragged row still tracks the pointer.
        let metrics = uniform_metrics(4, 44.0);
        assert_eq!(
            drag_offsets(&metrics, 1, 2, 50.0),
            vec![0.0, 50.0, -44.0, 0.0]
        );

        // Two steps down: both passed rows slide up one height each -
        // never two, since each only moved one slot. 88px is also exactly
        // where this row's travel stops (the last row's bottom edge), so
        // the dragged row's own offset is the clamp, not the raw pointer.
        assert_eq!(
            drag_offsets(&metrics, 1, 3, 150.0),
            vec![0.0, 88.0, -44.0, -44.0]
        );

        // Upward: the passed rows slide *down* instead.
        assert_eq!(
            drag_offsets(&metrics, 3, 1, -100.0),
            vec![0.0, 44.0, 44.0, -100.0]
        );
    }

    #[test]
    fn a_displaced_row_slides_by_the_dragged_rows_height_not_its_own() {
        // Mixed heights: a 120px raw row dragged past two 44px rows. Each
        // displaced row moves by the height of the row that passed it,
        // and the dragged row's own slide (when it is finally released)
        // is the sum of theirs - i.e. the offsets describe one consistent
        // final layout, not per-row guesses.
        let metrics = metrics_from_heights(&[120.0, 44.0, 44.0]);
        let offsets = drag_offsets(&metrics, 0, 2, 70.0);
        assert_eq!(offsets[1], -120.0);
        assert_eq!(offsets[2], -120.0);
        assert_eq!(offsets[0], 70.0);

        // And the reverse direction, dragging a short row above a tall one.
        let offsets = drag_offsets(&metrics, 2, 0, -150.0);
        assert_eq!(offsets[0], 44.0);
        assert_eq!(offsets[1], 44.0);
    }

    #[test]
    fn the_dragged_row_cannot_be_dragged_outside_the_table() {
        let metrics = uniform_metrics(3, 44.0);
        // Row 0 can't go above the first row's own top edge...
        assert_eq!(clamp_drag_offset(&metrics, 0, -500.0), 0.0);
        // ...nor row 2 below the last row's bottom edge.
        assert_eq!(clamp_drag_offset(&metrics, 2, 500.0), 0.0);
        // Row 1's travel is one row height either way.
        assert_eq!(clamp_drag_offset(&metrics, 1, -500.0), -44.0);
        assert_eq!(clamp_drag_offset(&metrics, 1, 500.0), 44.0);
        // Anything in range is passed through untouched (1:1 tracking).
        assert_eq!(clamp_drag_offset(&metrics, 1, 13.5), 13.5);
    }

    #[test]
    fn a_step_is_committed_only_once_the_pointer_covers_most_of_the_next_row() {
        // §13.12 item 1 - the pure threshold-crossing decision. 65% of a
        // 44px row is ~28.6px.
        let metrics = uniform_metrics(3, 44.0);
        assert_eq!(plan_step(&metrics, 1, 1, 0.0), None);
        assert_eq!(plan_step(&metrics, 1, 1, 28.0), None);
        assert_eq!(plan_step(&metrics, 1, 1, 29.0), Some(Step::Down));
        assert_eq!(plan_step(&metrics, 1, 1, -28.0), None);
        assert_eq!(plan_step(&metrics, 1, 1, -29.0), Some(Step::Up));

        // The threshold is a fraction of the *neighbour's* height, so a
        // tall row below has to be covered further than a short one.
        let metrics = metrics_from_heights(&[44.0, 44.0, 120.0]);
        assert_eq!(plan_step(&metrics, 1, 1, 29.0), None);
        assert_eq!(plan_step(&metrics, 1, 1, 80.0), Some(Step::Down));
    }

    #[test]
    fn a_committed_step_never_immediately_reverses_itself() {
        // The regression test for the one genuinely subtle thing about
        // committing steps live: both directions' thresholds are mirror
        // images, so an exactly-half rule would leave the drag standing on
        // the reverse threshold the moment a step commits, and the pair
        // would swap back and forth on every pointer event with the
        // pointer held still. Walk the pointer to each direction's own
        // trigger point and confirm the opposite step is not planned from
        // the position the step just moved to.
        let metrics = uniform_metrics(3, 44.0);

        let mut down_trigger = 0.0;
        while plan_step(&metrics, 1, 1, down_trigger).is_none() {
            down_trigger += 0.25;
            assert!(
                down_trigger <= 44.0,
                "a step has to commit within one row of travel to stay reachable"
            );
        }
        assert_eq!(plan_step(&metrics, 1, 1, down_trigger), Some(Step::Down));
        assert_eq!(
            plan_step(&metrics, 1, 2, down_trigger),
            None,
            "the pointer has not moved - the step must not bounce back"
        );

        let mut up_trigger = 0.0;
        while plan_step(&metrics, 1, 1, up_trigger).is_none() {
            up_trigger -= 0.25;
            assert!(up_trigger >= -44.0);
        }
        assert_eq!(plan_step(&metrics, 1, 1, up_trigger), Some(Step::Up));
        assert_eq!(plan_step(&metrics, 1, 0, up_trigger), None);
    }

    #[test]
    fn a_tall_row_can_still_be_dragged_all_the_way_to_the_end() {
        // The clamp keeps the dragged row inside its own table, so the
        // threshold has to stay within reach of it even when the dragged
        // row is far taller than the rows it passes - otherwise a raw
        // (tall) row could not be dropped last at all.
        let metrics = metrics_from_heights(&[120.0, 44.0, 44.0]);
        assert_eq!(plan_step(&metrics, 0, 0, 1000.0), Some(Step::Down));
        assert_eq!(plan_step(&metrics, 0, 1, 1000.0), Some(Step::Down));
        assert_eq!(plan_step(&metrics, 0, 2, 1000.0), None);

        // ...and the same dragging a tall row back up from the end.
        let metrics = metrics_from_heights(&[44.0, 44.0, 120.0]);
        assert_eq!(plan_step(&metrics, 2, 2, -1000.0), Some(Step::Up));
        assert_eq!(plan_step(&metrics, 2, 1, -1000.0), Some(Step::Up));
        assert_eq!(plan_step(&metrics, 2, 0, -1000.0), None);
    }

    #[test]
    fn the_boundary_rows_have_no_step_to_commit() {
        // The same boundary state the Move buttons render as `disabled`
        // (§13.2) - a drag past the end of the table commits nothing,
        // however far the pointer travels.
        let metrics = uniform_metrics(3, 44.0);
        assert_eq!(plan_step(&metrics, 0, 0, -500.0), None);
        assert_eq!(plan_step(&metrics, 2, 2, 500.0), None);
        // A one-row table has nowhere to go in either direction.
        let metrics = uniform_metrics(1, 44.0);
        assert_eq!(plan_step(&metrics, 0, 0, 500.0), None);
        assert_eq!(plan_step(&metrics, 0, 0, -500.0), None);
    }

    #[test]
    fn one_pointer_event_can_never_commit_more_than_one_step() {
        // §13.12 item 3, the direct test for the rule the closure-
        // staleness trap makes binding (see the module docs): a pointer
        // that jumps clean across three row boundaries in a single event
        // still yields exactly one step, and the rest are picked up by the
        // following events - by which time `AppShell` has re-rendered this
        // table with the step already staged.
        let metrics = uniform_metrics(5, 44.0);
        let jump = 3.0 * 44.0;

        let mut current = 0usize;
        let mut steps = Vec::new();
        for _event in 0..5 {
            // `plan_step` returns an `Option`, not a list: one event can
            // only ever describe one step, whatever the pointer did.
            if let Some(step) = plan_step(&metrics, 0, current, jump) {
                steps.push(step);
                current = match step {
                    Step::Up => current - 1,
                    Step::Down => current + 1,
                };
            }
        }
        assert_eq!(steps, vec![Step::Down, Step::Down, Step::Down]);
        assert_eq!(current, 3, "the drag catches up over the next few events");

        // And once it has caught up with the pointer, further events of
        // the same gesture commit nothing at all.
        assert_eq!(plan_step(&metrics, 0, current, jump), None);
    }

    #[test]
    fn a_drags_committed_step_is_the_move_buttons_own_from_to_pair() {
        // §13.12 item 4: a drag resolves `(from, to)` through the very
        // same `neighbor_target` the Move buttons' click handlers are
        // built from, against the same render-order list - so it cannot
        // produce a different pair, and needs no move logic of its own.
        // Non-contiguous candidate indices on purpose (comments/blanks sit
        // between these rows in the document).
        let candidate_indices = vec![0, 2, 3, 7];
        let metrics = uniform_metrics(4, 44.0);

        // The row at render position 1 is dragged down far enough to
        // cross the boundary below it.
        let step = plan_step(&metrics, 1, 1, 30.0).expect("half of the next row is covered");
        assert_eq!(step, Step::Down);
        let drag_pair = (
            candidate_indices[1],
            neighbor_target(&candidate_indices, 1, step),
        );

        // What the Move down button on that same row emits (the render
        // loop's own `down_target`, then `(candidate_index, to)`).
        let button_pair = (
            candidate_indices[1],
            neighbor_target(&candidate_indices, 1, Step::Down),
        );
        assert_eq!(drag_pair, button_pair);
        assert_eq!(drag_pair, (2, Some(3)));

        // Dragging the lower row up across the same boundary is the same
        // move the Move up button on that row makes, both ways round.
        let step = plan_step(&metrics, 2, 2, -30.0).expect("half of the row above is covered");
        assert_eq!(step, Step::Up);
        assert_eq!(
            (
                candidate_indices[2],
                neighbor_target(&candidate_indices, 2, step)
            ),
            (3, Some(2))
        );

        // Boundary rows resolve to no target at all, exactly as the
        // buttons' `disabled` state does.
        assert_eq!(neighbor_target(&candidate_indices, 0, Step::Up), None);
        assert_eq!(neighbor_target(&candidate_indices, 3, Step::Down), None);
    }

    #[test]
    fn the_insertion_line_marks_the_gap_the_dragged_row_would_land_in() {
        // The gap is the dragged row's current slot, so the line goes on
        // the top edge of the row below it - and on the bottom edge of the
        // row above when the drag is already at the last position.
        assert_eq!(insertion_line(4, 0), Some((1, InsertionEdge::Top)));
        assert_eq!(insertion_line(4, 2), Some((3, InsertionEdge::Top)));
        assert_eq!(insertion_line(4, 3), Some((2, InsertionEdge::Bottom)));
        // A one-row table has no boundary to point at.
        assert_eq!(insertion_line(1, 0), None);
    }

    #[test]
    fn an_expanded_rows_two_table_rows_are_measured_as_one_block() {
        // `pointerdown` reads every `<tr>` carrying a `data-row-position`;
        // an expanded `EntryRow` contributes two of them and has to come
        // back as the single block it visually is, since the whole block
        // moves (and is dragged) together.
        let raw = vec![
            (0, 0.0, 44.0),
            (1, 44.0, 88.0),
            (1, 88.0, 200.0),
            (2, 200.0, 244.0),
        ];
        let metrics = group_row_metrics(&raw, 3).expect("every row measured");
        assert_eq!(
            metrics,
            vec![
                RowMetrics {
                    top: 0.0,
                    height: 44.0
                },
                RowMetrics {
                    top: 44.0,
                    height: 156.0
                },
                RowMetrics {
                    top: 200.0,
                    height: 44.0
                },
            ]
        );
    }

    #[test]
    fn a_half_measured_table_aborts_the_gesture_instead_of_guessing() {
        // Better no drag at all than a drag against geometry that doesn't
        // describe the table (the caller treats `None` as "don't start").
        assert_eq!(group_row_metrics(&[(0, 0.0, 44.0)], 2), None);
        assert_eq!(group_row_metrics(&[], 1), None);
        assert_eq!(group_row_metrics(&[(0, 0.0, 44.0)], 0), None);
        // An out-of-range measurement is rejected rather than ignored.
        assert_eq!(
            group_row_metrics(&[(0, 0.0, 44.0), (5, 0.0, 44.0)], 1),
            None
        );
    }
}
