//! The in-memory "candidate document" - T11's central state design.
//!
//! `AppShell` owns one `CandidateDocument`, seeded from `list_entries`'s
//! `DocumentDto` on load. Every staged add/edit/delete mutates it directly
//! in memory; nothing here ever calls a write command (`save` is T12's
//! job). `EntriesTable` renders from it, `EntryDetailDrawer` stages into
//! it, and `validate_candidate` is called against its full
//! `to_document()` so duplicate-mount-point-style checks see the whole
//! file, not just the one entry being edited.
//!
//! Each line remembers the index it had in the *original* `list_entries`
//! response (`original_index`), or `None` if it was added this session.
//! That is deliberately enough information for T12 to compute the
//! Added/Modified/Removed grouping its diff view needs - by comparing each
//! candidate line's `original_index`/content against the original
//! document's lines - without this task having to build that comparison
//! itself:
//! - `original_index: None` → Added.
//! - `original_index: Some(i)` and `line != original.lines[i]` → Modified.
//! - an original index present in `original.lines` but absent from every
//!   candidate line's `original_index` → Removed.
//!
//! That content-only comparison is position-blind, which T40's entry
//! reordering made insufficient on its own: a pure swap changes no line's
//! content at its own `original_index`. [`CandidateDocument::
//! position_changes`] adds the missing positional half (`docs/UI_DESIGN.md`
//! §13.7), and is the one shared implementation both `pending_count` and
//! `diff.rs` use for it.
//!
//! Comments and blank lines round-trip untouched (never surfaced as table
//! rows, never dropped - `docs/UI_DESIGN.md` §3) because they are seeded
//! the same way as entry/malformed lines and nothing in this task's UI
//! offers a way to touch them.

use crate::dto::{DocumentDto, EntryDto, LineDto};

/// Whether a line is one of the kinds the entries table renders as a row
/// (and every line-oriented view therefore counts): comments and blank
/// lines are deliberately excluded everywhere (`docs/UI_DESIGN.md` §3).
fn is_row(line: &LineDto) -> bool {
    matches!(line, LineDto::Entry { .. } | LineDto::Malformed { .. })
}

/// Where the two lines involved in a [`CandidateDocument::stage_move`]
/// actually ended up (`docs/UI_DESIGN.md` §13.4 step 3).
///
/// T40's `stage_move` was a bare `Vec::swap`, so a caller could assume the
/// line at `from` landed at `to` and vice versa. Once each entry can carry
/// an attached comment block of its own length (§13.4), that assumption is
/// wrong whenever the two blocks differ in length - so the computation that
/// already has to rearrange the lines reports its own answer rather than
/// leaving a caller to re-derive it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MoveOutcome {
    /// New index of the line that was at `from`.
    pub from_new_index: usize,
    /// New index of the line that was at `to`.
    pub to_new_index: usize,
}

/// Where the two row-kind lines of a move land, given each one's attached
/// comment block length (`docs/UI_DESIGN.md` §13.4 step 3).
///
/// `upper`/`lower` are the two lines' current indices by *document
/// position* (not by which button was clicked), `upper_len`/`lower_len`
/// their [`CandidateDocument::attached_block_len`] values. Returns
/// `(new_upper, new_lower)`.
///
/// The affected range runs from `upper - upper_len` through `lower`
/// inclusive and is rewritten in place as `lower`'s span, the unchanged
/// freestanding middle, then `upper`'s span - so:
///
/// - `upper`'s own line is the last line of the last span placed, and the
///   range still ends at `lower`, so it always lands exactly at `lower`,
///   whatever either block's length.
/// - `lower`'s own line is the last line of the *first* span placed, which
///   starts at `upper - upper_len`, so it lands at
///   `upper - upper_len + lower_len` - *not* simply `upper`, whenever the
///   two blocks differ in length.
///
/// This is the single place either fact is computed: `stage_move` uses it
/// to report its [`MoveOutcome`], and `EntriesTable` uses the same function
/// to keep its position-keyed `expanded` set in sync (§13.8), so the two
/// cannot quietly disagree.
pub fn resolve_moved_indices(
    upper: usize,
    upper_len: usize,
    lower: usize,
    lower_len: usize,
) -> (usize, usize) {
    (lower, upper.saturating_sub(upper_len) + lower_len)
}

/// One still-present line whose *position* changed relative to the
/// originally loaded document - the raw material behind both
/// `pending_count`'s reorder contribution and `DiffView`'s `Reordered`
/// group (`docs/UI_DESIGN.md` §13.7).
///
/// `content_modified` carries §13.7's precedence rule to both call sites
/// from one place: a line whose content *also* changed is reported once,
/// under `Modified` (with the position appended as a note), never twice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PositionChange {
    /// Index of this line in the originally loaded document - the key both
    /// call sites match a line on.
    pub original_index: usize,
    /// 0-based rank among the comparable set in the *original* document.
    pub old_rank: usize,
    /// 0-based rank among the comparable set in the *current* candidate.
    pub new_rank: usize,
    /// Whether this line's content also differs from its original.
    pub content_modified: bool,
}

/// One line of the candidate document, tagged with where (if anywhere) it
/// came from in the originally loaded document.
#[derive(Clone, Debug, PartialEq)]
pub struct CandidateLine {
    pub original_index: Option<usize>,
    pub line: LineDto,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CandidateDocument {
    pub lines: Vec<CandidateLine>,
}

impl CandidateDocument {
    /// Seeds a candidate from a freshly loaded `DocumentDto`: every line
    /// maps 1:1 to its own index, in order.
    pub fn from_document(document: &DocumentDto) -> Self {
        Self {
            lines: document
                .lines
                .iter()
                .enumerate()
                .map(|(i, line)| CandidateLine {
                    original_index: Some(i),
                    line: line.clone(),
                })
                .collect(),
        }
    }

    /// Renders the candidate back to a `DocumentDto`, in its current
    /// order - used by T12's `save` and `render_candidate` calls.
    pub fn to_document(&self) -> DocumentDto {
        DocumentDto {
            lines: self.lines.iter().map(|cl| cl.line.clone()).collect(),
        }
    }

    /// Every candidate line except the one at `lines[index]`, as a plain
    /// line list - used to build the "rest of the document" context a
    /// drawer edit/add is validated against (see `EntryDetailDrawer`'s
    /// `other_lines` prop).
    pub fn lines_excluding(&self, index: Option<usize>) -> Vec<LineDto> {
        self.lines
            .iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != index)
            .map(|(_, cl)| cl.line.clone())
            .collect()
    }

    /// Stages a new entry, appended at the end of the document.
    pub fn stage_add(&mut self, entry: EntryDto) {
        self.lines.push(CandidateLine {
            original_index: None,
            line: LineDto::Entry { entry },
        });
    }

    /// Stages an edit of the structured entry at `index`, in place.
    pub fn stage_edit_entry(&mut self, index: usize, entry: EntryDto) {
        if let Some(candidate_line) = self.lines.get_mut(index) {
            candidate_line.line = LineDto::Entry { entry };
        }
    }

    /// Stages an edit of the raw text of a malformed line at `index`, in
    /// place. The line stays `Malformed` - the frontend has no parser of
    /// its own to promote it to a structured entry (API_CONTRACT.md §3.2).
    pub fn stage_edit_raw(&mut self, index: usize, raw: String) {
        if let Some(candidate_line) = self.lines.get_mut(index) {
            candidate_line.line = LineDto::Malformed { raw };
        }
    }

    /// Stages a removal of the line at `index`.
    pub fn stage_delete(&mut self, index: usize) {
        if index < self.lines.len() {
            self.lines.remove(index);
        }
    }

    /// Length of the attached comment block sitting directly above the
    /// row-kind line at `row_index` - the lines that move *with* it
    /// (`docs/UI_DESIGN.md` §13.4). `0` means it has none.
    ///
    /// An ordinary `#` comment written above an entry is the common fstab
    /// convention for describing that entry, so it must travel with the
    /// entry when it moves; T40 wrongly left every comment behind. (As of
    /// T43, the backend parser itself absorbs any ordinary comment block
    /// sitting directly above an `Entry` line into that entry's own
    /// `description` field - and a same-line trailing comment is absorbed
    /// into `trailing_comment` the same way - so neither ever reaches the
    /// frontend as a standalone `LineDto::Comment` to begin with, and
    /// neither is involved here at all. This walk only ever sees comment
    /// blocks the backend did *not* absorb: ones above a `Malformed` line
    /// (the backend's rule is scoped to `Entry` lines only) or a block at
    /// true end-of-file with nothing below it.)
    ///
    /// The backward walk, exactly as §13.4 specifies it:
    ///
    /// 1. Skip any number of consecutive `Blank` lines immediately above
    ///    `row_index`. Blank lines alone never break attachment.
    /// 2. If the first non-blank line reached is not a `Comment` (a
    ///    row-kind line, or the start of the document), there is no
    ///    attached block - return `0`.
    /// 3. Otherwise keep walking back *without* skipping blanks, while the
    ///    lines stay strictly contiguous `Comment`s; a `Blank`, a row-kind
    ///    line or the start of the document ends the run. The attached
    ///    block is everything from there down to (but excluding)
    ///    `row_index` itself - i.e. the comment run *and* any blank lines
    ///    step 1 skipped, so that the block plus its row is one contiguous
    ///    span that can be relocated as a unit.
    ///
    /// A second, separate comment block further back is never reached (the
    /// walk stops at the nearer one), and a comment block at true
    /// end-of-file is never visited by any walk at all - both stay
    /// freestanding and pinned in place, exactly as T40 had them.
    pub fn attached_block_len(&self, row_index: usize) -> usize {
        if row_index >= self.lines.len() {
            return 0;
        }

        // Step 1: skip blanks immediately above.
        let mut cursor = row_index;
        while cursor > 0 && matches!(self.lines[cursor - 1].line, LineDto::Blank) {
            cursor -= 1;
        }

        // Step 2: the first non-blank line above must be a comment.
        if cursor == 0 || !matches!(self.lines[cursor - 1].line, LineDto::Comment { .. }) {
            return 0;
        }

        // Step 3: the contiguous comment run this block is made of.
        let mut block_start = cursor;
        while block_start > 0 && matches!(self.lines[block_start - 1].line, LineDto::Comment { .. })
        {
            block_start -= 1;
        }

        row_index - block_start
    }

    /// Stages a reorder of the row-kind lines at `from` and `to`, each
    /// moving together with its own attached comment block
    /// (`docs/UI_DESIGN.md` §13.4).
    ///
    /// A no-op (`None`, document untouched) if either index is out of range
    /// or `from == to` - the same defensive contract shape as
    /// `stage_delete`. Otherwise the whole range from the upper line's
    /// block start through the lower line inclusive is rewritten as the
    /// lower line's full span, the unchanged freestanding middle, then the
    /// upper line's full span. That range's length is unchanged, so nothing
    /// outside it shifts by even one index.
    ///
    /// This does **not** re-derive "the nearest row-kind neighbor" itself:
    /// the caller (`EntriesTable`) already knows both indices from its own
    /// `rows()`-derived render order, so exactly one place in the codebase
    /// decides adjacency. Freestanding comment/blank lines - ones no
    /// entry's attachment walk claims - never travel with either entry:
    /// they stay in the middle, in their original order, and keep their
    /// exact index whenever the two spans are the same length (the common
    /// case). When the spans differ in length the middle necessarily
    /// shifts by that difference, since the whole range is rewritten in
    /// place and the middle still sits between the two spans.
    ///
    /// Returns the *actual* new indices of the two lines, via
    /// [`resolve_moved_indices`]; they are not simply `to`/`from` once the
    /// two attached blocks differ in length.
    pub fn stage_move(&mut self, from: usize, to: usize) -> Option<MoveOutcome> {
        if from >= self.lines.len() || to >= self.lines.len() || from == to {
            return None;
        }

        let upper = from.min(to);
        let lower = from.max(to);
        let upper_len = self.attached_block_len(upper);
        // The lower line's block can never reach across the upper line when
        // both are row-kind lines (the walk stops at a row-kind line), but
        // clamp anyway so a non-row index from a future caller can't make
        // the two spans overlap.
        let lower_len = self.attached_block_len(lower).min(lower - upper - 1);

        let range_start = upper - upper_len;
        let mut rearranged: Vec<CandidateLine> = Vec::with_capacity(lower - range_start + 1);
        // Lower's full span, then the untouched freestanding middle, then
        // upper's full span (§13.4 steps 1-2).
        rearranged.extend_from_slice(&self.lines[lower - lower_len..=lower]);
        rearranged.extend_from_slice(&self.lines[upper + 1..lower - lower_len]);
        rearranged.extend_from_slice(&self.lines[range_start..=upper]);
        self.lines.splice(range_start..=lower, rearranged);

        let (new_upper, new_lower) = resolve_moved_indices(upper, upper_len, lower, lower_len);
        let (from_new_index, to_new_index) = if from == upper {
            (new_upper, new_lower)
        } else {
            (new_lower, new_upper)
        };
        Some(MoveOutcome {
            from_new_index,
            to_new_index,
        })
    }

    /// Every still-present row-kind line whose position changed relative to
    /// `original` (`docs/UI_DESIGN.md` §13.7).
    ///
    /// This is the single shared implementation behind both
    /// [`Self::pending_count`]'s reorder contribution and `diff.rs`'s
    /// `Reordered` group - §13.7 makes "both call sites use the *same*
    /// logic" a hard requirement, so neither computes ranks for itself.
    ///
    /// The algorithm compares final state to original state directly (not a
    /// log of individual moves), so it is correct for any combination of
    /// moves, edits, additions and deletions made in any order:
    ///
    /// 1. The comparable set is every row-kind candidate line that still
    ///    carries an `original_index` pointing at a row-kind original line
    ///    (so deletions drop out, and additions - which have no original
    ///    rank to compare against - never enter it).
    /// 2. Rank that set twice, 0-based: once in original-document order,
    ///    once in current-candidate order.
    /// 3. Any line whose two ranks differ changed position.
    pub fn position_changes(&self, original: &DocumentDto) -> Vec<PositionChange> {
        // Step 1: the comparable set, in current candidate order. Pairs are
        // `(original_index, content_modified)`.
        let comparable: Vec<(usize, bool)> = self
            .lines
            .iter()
            .filter_map(|cl| {
                if !is_row(&cl.line) {
                    return None;
                }
                let index = cl.original_index?;
                let original_line = original.lines.get(index)?;
                if !is_row(original_line) {
                    return None;
                }
                Some((index, original_line != &cl.line))
            })
            .collect();

        // Step 2a: rank by original-document order, over the same set.
        let mut original_order: Vec<usize> = comparable.iter().map(|(index, _)| *index).collect();
        original_order.sort_unstable();

        // Step 2b/3: current rank is simply the position in `comparable`.
        comparable
            .iter()
            .enumerate()
            .filter_map(|(new_rank, (original_index, content_modified))| {
                let old_rank = original_order
                    .iter()
                    .position(|index| index == original_index)?;
                (old_rank != new_rank).then_some(PositionChange {
                    original_index: *original_index,
                    old_rank,
                    new_rank,
                    content_modified: *content_modified,
                })
            })
            .collect()
    }

    /// Row-kind (entry/malformed) candidate lines only, each paired with
    /// its index into `self.lines` - the identity `EntriesTable` reports
    /// back on edit/delete. Comments/blanks are preserved in `lines` but
    /// intentionally excluded from this view (`docs/UI_DESIGN.md` §3).
    pub fn rows(&self) -> Vec<(usize, &CandidateLine)> {
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, cl)| matches!(cl.line, LineDto::Entry { .. } | LineDto::Malformed { .. }))
            .collect()
    }

    /// Count of staged changes, for `PendingChangesBar`'s "N entries
    /// changed" (`docs/UI_DESIGN.md` §6): added + modified + reordered +
    /// removed row-kind lines, compared against the originally loaded
    /// document.
    ///
    /// The reordered contribution (§13.7) counts only lines whose content
    /// is unchanged - a line that was both edited and moved is already
    /// counted once as modified, matching the diff view's own precedence
    /// rule, so neither view can report a different total than the other.
    pub fn pending_count(&self, original: &DocumentDto) -> usize {
        let added_or_modified = self
            .lines
            .iter()
            .filter(|cl| {
                if !is_row(&cl.line) {
                    return false;
                }
                match cl.original_index {
                    None => true,
                    Some(i) => original.lines.get(i) != Some(&cl.line),
                }
            })
            .count();

        let still_present: Vec<usize> = self
            .lines
            .iter()
            .filter_map(|cl| cl.original_index)
            .collect();
        let removed = original
            .lines
            .iter()
            .enumerate()
            .filter(|(i, line)| is_row(line) && !still_present.contains(i))
            .count();

        let reordered = self
            .position_changes(original)
            .iter()
            .filter(|change| !change.content_modified)
            .count();

        added_or_modified + reordered + removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::{OptionDto, SourceDto, SourceKind};

    fn entry(mount_point: &str) -> EntryDto {
        EntryDto {
            source: SourceDto {
                kind: SourceKind::Uuid,
                value: "1111".to_string(),
            },
            mount_point: mount_point.to_string(),
            fs_type: "ext4".to_string(),
            options: vec![OptionDto {
                key: "defaults".to_string(),
                value: None,
            }],
            dump: 0,
            pass: 2,
            trailing_comment: None,
            description: None,
        }
    }

    fn entry_line(mount_point: &str) -> LineDto {
        LineDto::Entry {
            entry: entry(mount_point),
        }
    }

    fn document(lines: Vec<LineDto>) -> DocumentDto {
        DocumentDto { lines }
    }

    fn mount_points(candidate: &CandidateDocument) -> Vec<String> {
        candidate
            .lines
            .iter()
            .map(|cl| match &cl.line {
                LineDto::Entry { entry } => entry.mount_point.clone(),
                LineDto::Malformed { raw } => raw.clone(),
                LineDto::Comment { text } => format!("#{text}"),
                LineDto::Blank => String::new(),
            })
            .collect()
    }

    #[test]
    fn stage_move_swaps_two_lines_and_their_original_indices_travel_with_them() {
        let original = document(vec![entry_line("/a"), entry_line("/b"), entry_line("/c")]);
        let mut candidate = CandidateDocument::from_document(&original);

        let _ = candidate.stage_move(0, 2);

        assert_eq!(mount_points(&candidate), vec!["/c", "/b", "/a"]);
        assert_eq!(candidate.lines[0].original_index, Some(2));
        assert_eq!(candidate.lines[2].original_index, Some(0));
    }

    #[test]
    fn stage_move_of_adjacent_indices_swaps_exactly_those_two() {
        let original = document(vec![entry_line("/a"), entry_line("/b"), entry_line("/c")]);
        let mut candidate = CandidateDocument::from_document(&original);

        let _ = candidate.stage_move(1, 2);

        assert_eq!(mount_points(&candidate), vec!["/a", "/c", "/b"]);
    }

    #[test]
    fn stage_move_leaves_a_freestanding_blank_line_between_the_two_slots_in_place() {
        // §13.4's landing rule for genuinely freestanding content: a blank
        // line is never itself an attached block, so it keeps its exact
        // position while the two spans around it swap. (T40 asserted the
        // same for an ordinary comment here, which §13.4 now corrects -
        // see `a_single_comment_directly_above_an_entry_is_attached`.)
        let original = document(vec![entry_line("/a"), LineDto::Blank, entry_line("/b")]);
        let mut candidate = CandidateDocument::from_document(&original);

        let _ = candidate.stage_move(2, 0);

        assert_eq!(mount_points(&candidate), vec!["/b", "", "/a"]);
    }

    #[test]
    fn stage_move_with_an_out_of_range_index_is_a_no_op() {
        let original = document(vec![entry_line("/a"), entry_line("/b")]);
        let before = CandidateDocument::from_document(&original);

        let mut from_out_of_range = before.clone();
        assert_eq!(from_out_of_range.stage_move(5, 1), None);
        assert_eq!(from_out_of_range, before);

        let mut to_out_of_range = before.clone();
        assert_eq!(to_out_of_range.stage_move(0, 5), None);
        assert_eq!(to_out_of_range, before);
    }

    #[test]
    fn stage_move_of_an_index_onto_itself_changes_nothing() {
        let original = document(vec![entry_line("/a"), entry_line("/b")]);
        let before = CandidateDocument::from_document(&original);

        let mut candidate = before.clone();
        assert_eq!(candidate.stage_move(1, 1), None);

        assert_eq!(candidate, before);
    }

    #[test]
    fn a_pure_reorder_is_detected_and_counted() {
        // The gap this category exists to close (§13.7): before it, a swap
        // changed no line's content at its own original index, so nothing
        // was reported at all.
        let original = document(vec![entry_line("/a"), entry_line("/b")]);
        let mut candidate = CandidateDocument::from_document(&original);
        let _ = candidate.stage_move(0, 1);

        let changes = candidate.position_changes(&original);
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].original_index, 1);
        assert_eq!((changes[0].old_rank, changes[0].new_rank), (1, 0));
        assert!(!changes[0].content_modified);
        assert_eq!(changes[1].original_index, 0);
        assert_eq!((changes[1].old_rank, changes[1].new_rank), (0, 1));

        assert_eq!(candidate.pending_count(&original), 2);
    }

    #[test]
    fn an_entry_that_did_not_move_is_never_reported_as_reordered() {
        let original = document(vec![entry_line("/a"), entry_line("/b"), entry_line("/c")]);
        let mut candidate = CandidateDocument::from_document(&original);
        let _ = candidate.stage_move(0, 1);

        let moved: Vec<usize> = candidate
            .position_changes(&original)
            .iter()
            .map(|change| change.original_index)
            .collect();
        assert_eq!(moved, vec![1, 0]);
        assert_eq!(candidate.pending_count(&original), 2);
    }

    #[test]
    fn an_untouched_document_reports_no_position_changes() {
        let original = document(vec![
            entry_line("/a"),
            LineDto::Blank,
            entry_line("/b"),
            LineDto::Comment {
                text: " note".to_string(),
            },
        ]);
        let candidate = CandidateDocument::from_document(&original);

        assert!(candidate.position_changes(&original).is_empty());
        assert_eq!(candidate.pending_count(&original), 0);
    }

    #[test]
    fn a_line_that_was_both_moved_and_edited_is_flagged_as_content_modified() {
        // §13.7's precedence rule lives on this flag: `pending_count` must
        // count such a line once (as modified), not twice.
        let original = document(vec![entry_line("/a"), entry_line("/b")]);
        let mut candidate = CandidateDocument::from_document(&original);
        let _ = candidate.stage_move(0, 1);
        // `/a` now sits at index 1; edit it there.
        candidate.stage_edit_entry(1, entry("/a-renamed"));

        let changes = candidate.position_changes(&original);
        assert_eq!(changes.len(), 2);
        let edited = changes
            .iter()
            .find(|change| change.original_index == 0)
            .expect("the edited line still moved");
        assert!(edited.content_modified);
        let untouched = changes
            .iter()
            .find(|change| change.original_index == 1)
            .expect("the other line moved too");
        assert!(!untouched.content_modified);

        // 1 modified + 1 reordered, not 3: the moved-and-edited line is
        // counted once.
        assert_eq!(candidate.pending_count(&original), 2);
    }

    #[test]
    fn ranks_ignore_removed_and_added_lines() {
        // §13.7 step 2: a deleted line must not shift everyone else's rank,
        // and an added line has no original rank to compare against.
        let original = document(vec![entry_line("/a"), entry_line("/b"), entry_line("/c")]);
        let mut candidate = CandidateDocument::from_document(&original);
        candidate.stage_delete(0); // drop /a - /b and /c keep their order
        candidate.stage_add(entry("/new")); // appended, no original rank

        assert!(
            candidate.position_changes(&original).is_empty(),
            "deleting the first entry must not count as moving the rest"
        );
        // 1 added + 1 removed, no reorder.
        assert_eq!(candidate.pending_count(&original), 2);

        // Now actually swap the two survivors: they rank among themselves.
        let _ = candidate.stage_move(0, 1);
        let changes = candidate.position_changes(&original);
        assert_eq!(changes.len(), 2);
        assert_eq!((changes[0].original_index, changes[0].new_rank), (2, 0));
        assert_eq!((changes[1].original_index, changes[1].new_rank), (1, 1));
        assert_eq!(candidate.pending_count(&original), 4);
    }

    // --- T41: attached comment blocks (`docs/UI_DESIGN.md` §13.4/§13.10) ---

    fn comment(text: &str) -> LineDto {
        LineDto::Comment {
            text: format!(" {text}"),
        }
    }

    #[test]
    fn a_single_comment_directly_above_an_entry_is_attached_and_moves_with_it() {
        // §13.10 case 1 - the core bug T40 shipped: an ordinary `#` comment
        // describing an entry was left behind when the entry moved.
        let original = document(vec![
            entry_line("/a"),
            comment("the /b mount"),
            entry_line("/b"),
        ]);
        let mut candidate = CandidateDocument::from_document(&original);

        assert_eq!(candidate.attached_block_len(2), 1);
        assert_eq!(candidate.attached_block_len(0), 0);

        let outcome = candidate.stage_move(2, 0).expect("a real move");

        assert_eq!(
            mount_points(&candidate),
            vec!["# the /b mount", "/b", "/a"],
            "the comment must travel with its entry, not stay behind"
        );
        assert_eq!(outcome.from_new_index, 1);
        assert_eq!(outcome.to_new_index, 2);
    }

    #[test]
    fn a_multi_line_comment_block_moves_as_one_intact_unit() {
        // §13.10 case 2 - never split, never partially left behind.
        let original = document(vec![
            entry_line("/a"),
            comment("block line 1"),
            comment("block line 2"),
            comment("block line 3"),
            entry_line("/b"),
        ]);
        let mut candidate = CandidateDocument::from_document(&original);

        assert_eq!(candidate.attached_block_len(4), 3);

        let outcome = candidate.stage_move(0, 4).expect("a real move");

        assert_eq!(
            mount_points(&candidate),
            vec![
                "# block line 1",
                "# block line 2",
                "# block line 3",
                "/b",
                "/a",
            ]
        );
        assert_eq!(outcome.from_new_index, 4); // `/a`
        assert_eq!(outcome.to_new_index, 3); // `/b`
    }

    #[test]
    fn multiple_blank_lines_do_not_break_attachment() {
        // §13.10 case 3 - blank lines alone never break attachment, so the
        // attached span reaches past them to the comment (and carries them
        // along, since a span is contiguous).
        let original = document(vec![
            entry_line("/a"),
            comment("the /b mount"),
            LineDto::Blank,
            LineDto::Blank,
            entry_line("/b"),
        ]);
        let mut candidate = CandidateDocument::from_document(&original);

        assert_eq!(candidate.attached_block_len(4), 3);

        let _ = candidate.stage_move(4, 0).expect("a real move");

        assert_eq!(
            mount_points(&candidate),
            vec!["# the /b mount", "", "", "/b", "/a"]
        );
    }

    #[test]
    fn a_second_comment_block_further_back_stays_freestanding() {
        // §13.10 case 4 - the walk stops at the nearest block, so the one
        // above it belongs to nobody and keeps its exact position. Both
        // spans are one comment + one entry here, so "keeps its position"
        // is literal, index for index.
        let original = document(vec![
            comment("the /a mount"),
            entry_line("/a"),
            comment("Section: network mounts"),
            LineDto::Blank,
            comment("the /b mount"),
            entry_line("/b"),
        ]);
        let mut candidate = CandidateDocument::from_document(&original);

        assert_eq!(candidate.attached_block_len(1), 1);
        assert_eq!(
            candidate.attached_block_len(5),
            1,
            "only the nearest block attaches - the section header does not"
        );

        let _ = candidate.stage_move(5, 1).expect("a real move");

        assert_eq!(
            mount_points(&candidate),
            vec![
                "# the /b mount",
                "/b",
                "# Section: network mounts",
                "",
                "# the /a mount",
                "/a",
            ]
        );
    }

    #[test]
    fn a_comment_block_at_end_of_file_belongs_to_nobody_and_never_moves() {
        // §13.10 case 5 - the walk only ever runs backward from a row, so a
        // block with no row below it is never visited by any walk.
        let original = document(vec![
            entry_line("/a"),
            entry_line("/b"),
            comment("end of file note"),
            comment("second line of it"),
        ]);
        let mut candidate = CandidateDocument::from_document(&original);

        assert_eq!(candidate.attached_block_len(0), 0);
        assert_eq!(candidate.attached_block_len(1), 0);

        let _ = candidate.stage_move(0, 1).expect("a real move");

        assert_eq!(
            mount_points(&candidate),
            vec!["/b", "/a", "# end of file note", "# second line of it"]
        );
    }

    #[test]
    fn an_absorbed_description_comment_is_never_part_of_an_attached_block() {
        // §13.10 case 6 - an ordinary `#` comment block directly above an
        // entry is absorbed into that entry's `description` by the backend
        // parser (`fstab-core::parser::absorb_description_comments`, T43)
        // and never reaches the frontend as a `LineDto::Comment`, so it
        // cannot enter the attachment walk at all. It travels with its
        // entry via the `description` field, exactly as it did before T40.
        let mut described = entry("/a");
        described.description = Some("Root filesystem".to_string());
        let original = document(vec![LineDto::Entry { entry: described }, entry_line("/b")]);
        let candidate_before = CandidateDocument::from_document(&original);

        assert!(
            !candidate_before
                .lines
                .iter()
                .any(|cl| matches!(cl.line, LineDto::Comment { .. })),
            "an absorbed description comment is not a standalone comment line in the frontend"
        );
        assert_eq!(candidate_before.attached_block_len(0), 0);
        assert_eq!(candidate_before.attached_block_len(1), 0);

        let mut candidate = candidate_before;
        let _ = candidate.stage_move(0, 1).expect("a real move");

        assert_eq!(mount_points(&candidate), vec!["/b", "/a"]);
        let LineDto::Entry { entry: moved } = &candidate.lines[1].line else {
            panic!("the described entry is still an entry");
        };
        assert_eq!(moved.description.as_deref(), Some("Root filesystem"));
    }

    #[test]
    fn a_comment_block_above_a_malformed_row_is_still_attached() {
        // T43 scoped the backend's comment-absorption rule to `Entry` lines
        // only, so a comment block sitting directly above a `Malformed` row
        // is never absorbed and still reaches the frontend as standalone
        // `LineDto::Comment` values - this walk remains the only mechanism
        // that attaches it to its row.
        let original = document(vec![
            entry_line("/a"),
            comment("can't parse this one"),
            LineDto::Malformed {
                raw: "garbage line".to_string(),
            },
        ]);
        let mut candidate = CandidateDocument::from_document(&original);

        assert_eq!(candidate.attached_block_len(2), 1);

        let _ = candidate.stage_move(2, 0).expect("a real move");

        assert_eq!(
            mount_points(&candidate),
            vec!["# can't parse this one", "garbage line", "/a"],
            "the comment must travel with the malformed row, not stay behind"
        );
    }

    /// The §13.10 case 7/8 scenario, built once so both tests provably use
    /// the same asymmetric-block document.
    fn asymmetric_block_document() -> DocumentDto {
        document(vec![
            comment("block line 1"),
            comment("block line 2"),
            comment("block line 3"),
            entry_line("/a"),
            LineDto::Blank,
            entry_line("/b"),
        ])
    }

    #[test]
    fn asymmetric_attached_blocks_relocate_correctly_and_report_real_indices() {
        // §13.10 case 7: `/a` carries a 3-line block, `/b` carries none, so
        // neither one lands where a plain `to`/`from` swap would assume.
        let original = asymmetric_block_document();
        let mut candidate = CandidateDocument::from_document(&original);

        assert_eq!(candidate.attached_block_len(3), 3);
        assert_eq!(candidate.attached_block_len(5), 0);

        let outcome = candidate.stage_move(3, 5).expect("a real move");

        assert_eq!(
            mount_points(&candidate),
            vec![
                "/b",
                "",
                "# block line 1",
                "# block line 2",
                "# block line 3",
                "/a"
            ]
        );
        assert_eq!(outcome.from_new_index, 5, "`/a` really ended up at 5");
        assert_eq!(
            outcome.to_new_index, 0,
            "`/b` ended up at 0, not at the naive `from` of 3"
        );

        // The same move requested in the other direction is the same
        // rearrangement, with the two reported indices simply swapped.
        let mut other_direction = CandidateDocument::from_document(&original);
        let reverse = other_direction.stage_move(5, 3).expect("a real move");
        assert_eq!(other_direction, candidate);
        assert_eq!(reverse.from_new_index, 0);
        assert_eq!(reverse.to_new_index, 5);
    }

    #[test]
    fn resolve_moved_indices_agrees_with_what_stage_move_actually_does() {
        // §13.10 case 8: one shared function, not two computations that can
        // drift - checked against the real rearrangement, not restated.
        let original = asymmetric_block_document();
        let mut candidate = CandidateDocument::from_document(&original);
        let upper_len = candidate.attached_block_len(3);
        let lower_len = candidate.attached_block_len(5);

        let (new_upper, new_lower) = resolve_moved_indices(3, upper_len, 5, lower_len);
        assert_eq!((new_upper, new_lower), (5, 0));

        let outcome = candidate.stage_move(3, 5).expect("a real move");
        assert_eq!(outcome.from_new_index, new_upper);
        assert_eq!(outcome.to_new_index, new_lower);

        // And the indices it predicts really are where those lines are now.
        assert_eq!(candidate.lines[new_upper].original_index, Some(3));
        assert_eq!(candidate.lines[new_lower].original_index, Some(5));
    }

    #[test]
    fn equal_length_blocks_reduce_to_a_plain_swap_of_the_two_indices() {
        // The common case (including zero/zero) must behave exactly as T40
        // did, so this fix only changes behavior where T40 was wrong.
        let original = document(vec![entry_line("/a"), entry_line("/b")]);
        let mut candidate = CandidateDocument::from_document(&original);

        let outcome = candidate.stage_move(0, 1).expect("a real move");

        assert_eq!(outcome.from_new_index, 1);
        assert_eq!(outcome.to_new_index, 0);
        assert_eq!(resolve_moved_indices(0, 0, 1, 0), (1, 0));
    }
}
