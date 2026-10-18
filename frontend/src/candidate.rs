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

    /// Stages a positional swap of the lines at `from` and `to`
    /// (`docs/UI_DESIGN.md` §13.4). A no-op if either index is out of
    /// range, the same defensive contract shape as `stage_delete`.
    ///
    /// This is deliberately a plain two-line swap and nothing more: it does
    /// **not** re-derive "the nearest row-kind neighbor" itself, because the
    /// caller (`EntriesTable`) already knows both indices from its own
    /// `rows()`-derived render order, so exactly one place in the codebase
    /// decides adjacency. Any freestanding comment/blank line that sat
    /// between the two swapped lines keeps its exact position (§13.4).
    pub fn stage_move(&mut self, from: usize, to: usize) {
        if from < self.lines.len() && to < self.lines.len() {
            self.lines.swap(from, to);
        }
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

        candidate.stage_move(0, 2);

        assert_eq!(mount_points(&candidate), vec!["/c", "/b", "/a"]);
        assert_eq!(candidate.lines[0].original_index, Some(2));
        assert_eq!(candidate.lines[2].original_index, Some(0));
    }

    #[test]
    fn stage_move_of_adjacent_indices_swaps_exactly_those_two() {
        let original = document(vec![entry_line("/a"), entry_line("/b"), entry_line("/c")]);
        let mut candidate = CandidateDocument::from_document(&original);

        candidate.stage_move(1, 2);

        assert_eq!(mount_points(&candidate), vec!["/a", "/c", "/b"]);
    }

    #[test]
    fn stage_move_leaves_a_freestanding_comment_between_the_two_slots_in_place() {
        // §13.4's landing rule: only the two row-kind lines swap; anything
        // between them keeps its exact position.
        let original = document(vec![
            entry_line("/a"),
            LineDto::Comment {
                text: " Network mounts".to_string(),
            },
            entry_line("/b"),
        ]);
        let mut candidate = CandidateDocument::from_document(&original);

        candidate.stage_move(2, 0);

        assert_eq!(
            mount_points(&candidate),
            vec!["/b", "# Network mounts", "/a"]
        );
    }

    #[test]
    fn stage_move_with_an_out_of_range_index_is_a_no_op() {
        let original = document(vec![entry_line("/a"), entry_line("/b")]);
        let before = CandidateDocument::from_document(&original);

        let mut from_out_of_range = before.clone();
        from_out_of_range.stage_move(5, 1);
        assert_eq!(from_out_of_range, before);

        let mut to_out_of_range = before.clone();
        to_out_of_range.stage_move(0, 5);
        assert_eq!(to_out_of_range, before);
    }

    #[test]
    fn stage_move_of_an_index_onto_itself_changes_nothing() {
        let original = document(vec![entry_line("/a"), entry_line("/b")]);
        let before = CandidateDocument::from_document(&original);

        let mut candidate = before.clone();
        candidate.stage_move(1, 1);

        assert_eq!(candidate, before);
    }

    #[test]
    fn a_pure_reorder_is_detected_and_counted() {
        // The gap this category exists to close (§13.7): before it, a swap
        // changed no line's content at its own original index, so nothing
        // was reported at all.
        let original = document(vec![entry_line("/a"), entry_line("/b")]);
        let mut candidate = CandidateDocument::from_document(&original);
        candidate.stage_move(0, 1);

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
        candidate.stage_move(0, 1);

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
        candidate.stage_move(0, 1);
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
        candidate.stage_move(0, 1);
        let changes = candidate.position_changes(&original);
        assert_eq!(changes.len(), 2);
        assert_eq!((changes[0].original_index, changes[0].new_rank), (2, 0));
        assert_eq!((changes[1].original_index, changes[1].new_rank), (1, 1));
        assert_eq!(candidate.pending_count(&original), 4);
    }
}
