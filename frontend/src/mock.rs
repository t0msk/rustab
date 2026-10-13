//! Hardcoded mock data for the T10 UI shell.
//!
//! Everything in this module is a placeholder. None of it is backed by a
//! real `/etc/fstab` read, a Tauri command, or `fstab-core` types (the
//! `frontend` crate intentionally does not depend on `fstab-core` — see
//! PROJECT_PLAN.md T10). T11 replaces the entries/backups data with a real
//! Tauri-backed data layer; T12 replaces the target info with a live value.

/// Placeholder "active target" info. In T11/T12 this will come from a Tauri
/// command exposed by the backend (see `docs/UI_DESIGN.md` §2) instead of
/// being hardcoded here.
#[derive(Clone, PartialEq)]
pub struct MockTarget {
    pub path: &'static str,
    pub is_real: bool,
}

pub const MOCK_TARGET: MockTarget = MockTarget {
    path: "/home/tomsk/Projects/Apps/rustab/dev-data/fstab.sandbox",
    is_real: false,
};

/// Shape of a single successfully-parsed fstab line, trimmed down to just
/// what `EntriesTable`/`EntryRow` need to display (§3). This is a plain
/// local struct, not `fstab_core::Entry` — the frontend crate has no
/// dependency on `fstab-core`.
#[derive(Clone, PartialEq)]
pub struct MockParsedEntry {
    pub source_type: &'static str,
    pub source_value: &'static str,
    pub mount_point: &'static str,
    pub fs_type: &'static str,
    pub options: &'static [&'static str],
    pub dump: u8,
    pub pass: u8,
}

/// One row of the entries table: either a successfully-parsed entry or a
/// line the (future) parser couldn't fully understand, kept verbatim.
#[derive(Clone, PartialEq)]
pub enum MockEntry {
    Parsed(MockParsedEntry),
    Unparsed { raw_line: &'static str },
}

/// Three mock rows: one normal entry, one entry with several options (to
/// exercise the "+N more" overflow), and one malformed/unparsed line.
pub fn mock_entries() -> Vec<MockEntry> {
    vec![
        MockEntry::Parsed(MockParsedEntry {
            source_type: "UUID",
            source_value: "4f3a9c2e-1234-4b9a-8d0f-abc123456789",
            mount_point: "/",
            fs_type: "ext4",
            options: &["defaults", "noatime"],
            dump: 0,
            pass: 1,
        }),
        MockEntry::Parsed(MockParsedEntry {
            source_type: "LABEL",
            source_value: "DATA",
            mount_point: "/mnt/data",
            fs_type: "btrfs",
            options: &["rw", "noatime", "compress=zstd", "x-systemd.automount"],
            dump: 0,
            pass: 2,
        }),
        MockEntry::Unparsed {
            raw_line: "/dev/sdX1  /mnt/broken   autofs   defaults,,  0  0",
        },
    ]
}

/// Shape of a single auto-backup entry, trimmed down to what
/// `BackupPanel`/`BackupListItem` need to display (§7).
#[derive(Clone, PartialEq)]
pub struct MockBackup {
    pub relative_time: &'static str,
    pub iso_timestamp: &'static str,
    pub size_bytes: u64,
    pub target_path: &'static str,
}

/// Two or three mock backups with fake timestamps, newest first.
pub fn mock_backups() -> Vec<MockBackup> {
    vec![
        MockBackup {
            relative_time: "2 hours ago",
            iso_timestamp: "2026-10-13T08:15:00Z",
            size_bytes: 1842,
            target_path: "/home/tomsk/Projects/Apps/rustab/dev-data/fstab.sandbox",
        },
        MockBackup {
            relative_time: "Yesterday",
            iso_timestamp: "2026-10-12T19:42:00Z",
            size_bytes: 1790,
            target_path: "/home/tomsk/Projects/Apps/rustab/dev-data/fstab.sandbox",
        },
        MockBackup {
            relative_time: "3 days ago",
            iso_timestamp: "2026-10-10T11:03:00Z",
            size_bytes: 1655,
            target_path: "/home/tomsk/Projects/Apps/rustab/dev-data/fstab.sandbox",
        },
    ]
}

/// A tiny mock discovered-device record for `DevicePicker`. In T11/T12 this
/// comes from the device-discovery command (T7/T8), not hardcoded here.
#[derive(Clone, PartialEq)]
pub struct MockDevice {
    pub path: &'static str,
    pub uuid: &'static str,
    pub label: &'static str,
    pub fstype: &'static str,
}

pub fn mock_devices() -> Vec<MockDevice> {
    vec![
        MockDevice {
            path: "/dev/sda1",
            uuid: "4f3a9c2e-1234-4b9a-8d0f-abc123456789",
            label: "ROOT",
            fstype: "ext4",
        },
        MockDevice {
            path: "/dev/sdb1",
            uuid: "9c7e2b10-aaaa-4bbb-8ccc-def012345678",
            label: "DATA",
            fstype: "btrfs",
        },
    ]
}
