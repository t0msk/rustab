//! Integration tests for `fstab-core::parser` against the synthetic fixtures
//! in `tests/fixtures/` (workspace root). See `docs/SPEC.md` §7: these
//! fixtures are hand-written, synthetic, and safe to publish - this test
//! suite must never read `dev-data/fstab.real-copy`. The per-fixture
//! "everything in here is fake" disclosure is on [`FIXTURE_NAMES`] below.

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

/// Every committed fixture is synthetic and safe to publish: **all values in
/// all of them - UUIDs, PARTUUIDs, labels, hostnames, share names, device
/// nodes and paths - are fake placeholders invented for testing only**, and
/// none of them describes any real machine.
///
/// That disclosure used to live in a two-line `#rustab synthetic fixture: ...`
/// header comment at the top of each file. T43 moved it here: since any
/// ordinary comment directly above an entry is now that entry's description,
/// such a header would be absorbed into the *first* entry's description (see
/// `parser.rs`'s "Description block absorption"), which is both wrong and
/// would stop the fixtures being byte-for-byte fixed points. The disclosure
/// belongs in test code, where it has no file-structural consequences.
///
/// Each fixture is written in the parser's canonical shape: an entry's
/// comment block sits directly above it, with blank lines only *between*
/// those blocks, which is what makes the byte-for-byte assertions below hold.
const FIXTURE_NAMES: &[&str] = &[
    "uuid_label_partuuid_partlabel.fstab",
    "bind_swap_systemd_escaped.fstab",
    "network_and_malformed.fstab",
    "description_blocks.fstab",
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
/// re-serializing must leave every *other* physical line byte-identical to the
/// original - proving comment/blank-line/other-entry preservation
/// independent of whitespace-canonicalization concerns.
///
/// The changed line is located by content rather than by index: an entry's
/// description lines are absorbed into the entry itself, so a `Document`'s
/// `lines` index is no longer the same thing as a physical line number (the
/// xfs entry here is `lines[4]` but physical line 7). Comparing the two texts
/// line by line and demanding exactly one difference is both stricter and
/// independent of that mapping.
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

    let changed: Vec<usize> = original_lines
        .iter()
        .zip(edited_lines.iter())
        .enumerate()
        .filter(|(_, (orig, new))| orig != new)
        .map(|(i, _)| i)
        .collect();

    assert_eq!(
        changed.len(),
        1,
        "exactly one physical line may change, but these did: {changed:?}"
    );
    let changed_line = edited_lines[changed[0]];
    assert!(
        changed_line.contains("/srv/renamed-data"),
        "the changed line must be the edited entry's own line, got {changed_line:?}"
    );
    assert!(
        original_lines[changed[0]].contains("LABEL=example-data"),
        "the changed line must be where the xfs entry already was"
    );
}
