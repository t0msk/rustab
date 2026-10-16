//! Integration tests for `fstab-core::parser` against the synthetic fixtures
//! in `tests/fixtures/` (workspace root). See `docs/SPEC.md` §7: these
//! fixtures are hand-written, synthetic, and safe to publish — this test
//! suite must never read `dev-data/fstab.real-copy`.

use fstab_core::model::Line;
use fstab_core::parser::parse;

/// Fixtures are addressed relative to this crate's manifest directory,
/// which is `crates/fstab-core`; the shared fixture directory lives two
/// levels up, at the workspace root.
const FIXTURES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures");

fn read_fixture(name: &str) -> String {
    let path = format!("{FIXTURES_DIR}/{name}");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"))
}

const FIXTURE_NAMES: &[&str] = &[
    "uuid_label_partuuid_partlabel.fstab",
    "bind_swap_systemd_escaped.fstab",
    "network_and_malformed.fstab",
];

/// Each fixture is written in the parser's own canonical output format, so
/// parsing and immediately re-serializing must reproduce it byte-for-byte.
#[test]
fn each_fixture_round_trips_byte_for_byte() {
    for name in FIXTURE_NAMES {
        let original = read_fixture(name);
        let serialized = parse(&original).to_string();
        assert_eq!(
            serialized, original,
            "fixture {name} did not round-trip byte-for-byte"
        );
    }
}

/// Re-parsing an already-canonical serialization must be a stable fixed
/// point: parsing it again and re-serializing again changes nothing.
#[test]
fn each_fixture_is_a_stable_fixed_point() {
    for name in FIXTURE_NAMES {
        let original = read_fixture(name);
        let once = parse(&original);
        let serialized_once = once.to_string();
        let twice = parse(&serialized_once);
        assert_eq!(once, twice, "fixture {name} was not a stable fixed point");
        assert_eq!(twice.to_string(), serialized_once);
    }
}

/// The `network_and_malformed.fstab` fixture is expected to contain exactly
/// one malformed line (the intentionally short `/dev/fake1` source-only
/// line), and parsing must not drop or crash on it.
#[test]
fn malformed_fixture_line_is_preserved_not_dropped() {
    let original = read_fixture("network_and_malformed.fstab");
    let doc = parse(&original);
    assert!(doc.has_malformed_lines());
    let malformed_count = doc
        .lines
        .iter()
        .filter(|line| matches!(line, Line::Malformed(_)))
        .count();
    assert_eq!(malformed_count, 1);
    assert!(doc
        .lines
        .iter()
        .any(|line| matches!(line, Line::Malformed(raw) if raw == "/dev/fake1")));
}

/// Loading a multi-entry document, editing exactly one entry in place, and
/// re-serializing must leave every *other* line byte-identical to the
/// original — proving comment/blank-line/other-entry preservation
/// independent of whitespace-canonicalization concerns.
#[test]
fn editing_one_entry_leaves_every_other_line_byte_identical() {
    let original = read_fixture("uuid_label_partuuid_partlabel.fstab");
    let mut doc = parse(&original);

    // Find the LABEL=example-data entry and change its mount point.
    let target_index = doc
        .lines
        .iter()
        .position(|line| matches!(line, Line::Entry(e) if e.fs_type == "xfs"))
        .expect("fixture should contain the xfs entry");

    if let Line::Entry(entry) = &mut doc.lines[target_index] {
        entry.mount_point = "/srv/renamed-data".to_string();
    } else {
        unreachable!();
    }

    let edited_text = doc.to_string();

    let original_lines: Vec<&str> = original.lines().collect();
    let edited_lines: Vec<&str> = edited_text.lines().collect();

    assert_eq!(
        original_lines.len(),
        edited_lines.len(),
        "editing one entry must not add or remove lines"
    );

    for (i, (orig_line, new_line)) in original_lines.iter().zip(edited_lines.iter()).enumerate() {
        if i == target_index {
            assert_ne!(orig_line, new_line, "the edited entry's line should change");
            assert!(new_line.contains("/srv/renamed-data"));
        } else {
            assert_eq!(
                orig_line, new_line,
                "line {i} changed unexpectedly after editing a different entry"
            );
        }
    }
}
