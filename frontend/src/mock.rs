//! Remaining hardcoded mock data, trimmed down after T11.
//!
//! T11 replaced the entries/devices/target mock data with real Tauri-backed
//! data (see `api`, `dto`, `candidate`). `BackupPanel` is still mock-backed
//! here — wiring it to `list_backups`/`restore_backup` is T12's job.

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
