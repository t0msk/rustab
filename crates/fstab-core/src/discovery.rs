//! Block-device / UUID / LABEL discovery helpers - see `docs/SPEC.md` §8.
//!
//! ## Which system tool(s) this module shells out to, and why
//!
//! This module shells out to `lsblk` only (`lsblk -J -O`), not `blkid` or
//! `findmnt`. `lsblk -O` ("all columns") already reports the
//! `PATH`/`UUID`/`LABEL`/`PARTUUID`/`PARTLABEL`/`FSTYPE` columns - on modern
//! util-linux these columns are themselves sourced from the same udev/blkid
//! metadata `blkid` would report - so a second shell-out to `blkid` and a
//! manual cross-reference by device path would duplicate data `lsblk`
//! already gives us in one process invocation and one parse, for no
//! additional information. `lsblk -J` emits a single JSON document
//! describing the whole device tree (whole disks as top-level entries,
//! partitions nested under a `children` array), which this module flattens
//! into one `Vec<DeviceInfo>`.
//!
//! `findmnt` is intentionally not used here: `lsblk -O` already includes
//! `mountpoint`/`mountpoints` columns, and SPEC §8's ask is a device/UUID/
//! LABEL *picker*, not a live mount-state view (that belongs to §6's
//! `findmnt --verify` semantic-validation step, a different concern that
//! lives in `validate.rs`). If a future task needs richer live-mount-state
//! info than `lsblk` reports, that's a reason to add a dedicated
//! `findmnt`-based helper then, not a reason to add one speculatively now.
//!
//! ## Soft-failure contract (critical, per SPEC §8)
//!
//! `lsblk` may be entirely absent (minimal/sandboxed systems), may exit
//! non-zero, or may print something this module's parser doesn't
//! recognize. None of those cases may panic or propagate a hard error that
//! blocks the rest of the app: [`discover_devices`] always returns a
//! [`DiscoveryResult`] by value, never a `Result`/`Option`, which makes
//! "just unwrap it" structurally the only thing a caller can do - there is
//! no error variant to forget to handle. [`DiscoveryResult::lsblk_available`]
//! is the caller's signal for whether `devices` reflects "the tool ran and
//! reported however many devices it reported" (including legitimately
//! zero) or "the tool itself could not be run at all" (binary missing or
//! non-zero exit); callers should fall back to manual text entry for the
//! source field in the latter case, per SPEC §8. A successful run that
//! produces JSON this module fails to parse is treated as "available, zero
//! devices" rather than "unavailable", since the tool did run - see
//! [`parse_lsblk_json`].

use std::io;
use std::process::Command;

use serde_json::Value;

/// One block device or partition discovered via `lsblk`, carrying whichever
/// device-identifier fields fstab entries can reference as a source.
///
/// Every field besides `path` is `Option` because `lsblk` legitimately
/// reports `null` for most of these on many devices (e.g. an unformatted
/// partition has no `fs_type`; a whole-disk entry usually has no `uuid` or
/// `label` of its own, only its partitions do). A `None` here means "lsblk
/// reported no value for this device", not "unknown due to a parse error" -
/// parse errors on the whole `lsblk` invocation are modeled at the
/// [`DiscoveryResult`] level instead, not per-field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// The device's path, e.g. `/dev/sda1`. Taken from `lsblk`'s `path`
    /// column when present; falls back to `/dev/<name>` (built from the
    /// `name` column) if `path` itself was `null`, since every block device
    /// `lsblk` knows about does live under `/dev`.
    pub path: String,
    /// The filesystem UUID (`lsblk`'s `uuid` column), matching what an
    /// fstab `UUID=` source would reference.
    pub uuid: Option<String>,
    /// The filesystem label (`lsblk`'s `label` column), matching what an
    /// fstab `LABEL=` source would reference.
    pub label: Option<String>,
    /// The partition-table UUID (`lsblk`'s `partuuid` column), matching
    /// what an fstab `PARTUUID=` source would reference. Distinct from
    /// `uuid`: this identifies the partition table entry, not the
    /// filesystem inside it.
    pub partuuid: Option<String>,
    /// The partition label (`lsblk`'s `partlabel` column), matching what an
    /// fstab `PARTLABEL=` source would reference. Distinct from `label`:
    /// this is the GPT partition name, not the filesystem label.
    pub partlabel: Option<String>,
    /// The filesystem type (`lsblk`'s `fstype` column), e.g. `ext4`,
    /// `vfat`, `swap`, `crypto_LUKS`. `None` if the device has no
    /// filesystem `lsblk` recognizes (e.g. an empty whole-disk entry that
    /// is purely a partition-table container).
    pub fs_type: Option<String>,
}

/// The outcome of a device-discovery attempt (see [`discover_devices`]).
///
/// Deliberately not a `Result<Vec<DeviceInfo>, E>`: there is no error
/// variant for a caller to match on and no way to `?`-propagate a discovery
/// failure into a hard error, by construction. A caller can always use
/// `devices` directly; `lsblk_available` is purely an optional UI hint
/// (e.g. "device picker unavailable, use manual text entry") rather than
/// something that must be checked before `devices` can be trusted.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiscoveryResult {
    /// The devices discovered, in the order `lsblk` reported them
    /// (top-level block devices followed by their partitions). Empty if
    /// `lsblk` is unavailable, failed, produced unparseable output, or
    /// (legitimately) if the system has no block devices `lsblk` can see.
    pub devices: Vec<DeviceInfo>,
    /// `true` if the `lsblk` invocation itself ran and exited successfully
    /// (regardless of how many devices, if any, were parsed out of its
    /// output). `false` if the `lsblk` binary could not be launched at all
    /// (not installed / not on `PATH`) or it exited with a non-zero status.
    /// Callers should treat `false` as "the device picker has no data
    /// source on this system" and fall back to manual text entry, per
    /// SPEC §8 - not as an error to surface loudly.
    pub lsblk_available: bool,
}

/// Runs `lsblk -J -O` and returns its raw stdout as a `String`.
///
/// Returns `Err` if the binary could not be launched (e.g. not installed)
/// or if it exited with a non-zero status; the `io::Error` in the latter
/// case carries the exit status in its message, not `lsblk`'s stderr
/// output, since this crate has no logging dependency to report it through.
/// Kept separate from [`parse_lsblk_json`] specifically so the pure parsing
/// logic can be unit-tested with canned input, without the test suite
/// needing `lsblk` installed or needing to touch real hardware.
fn run_lsblk() -> io::Result<String> {
    let output = Command::new("lsblk").args(["-J", "-O"]).output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "lsblk exited with status {}",
            output.status
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parses the JSON text produced by `lsblk -J -O` into a flat list of
/// [`DeviceInfo`], recursing into each device's `children` (partitions).
///
/// This is a pure function with no I/O, so tests can feed it synthetic
/// sample strings directly. Never panics: invalid JSON, a JSON value that
/// isn't the expected object shape, or a missing/non-array `blockdevices`
/// field all simply yield an empty `Vec`, matching this module's
/// soft-failure contract (see the module-level docs) - this function has no
/// way to report *why* parsing failed, by design, since "fall back to
/// manual entry" is the same caller action regardless of the reason.
pub fn parse_lsblk_json(raw: &str) -> Vec<DeviceInfo> {
    let root: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let Some(top_level) = root.get("blockdevices").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut devices = Vec::new();
    for device in top_level {
        flatten_device(device, &mut devices);
    }
    devices
}

/// Converts one `lsblk` device JSON object into a [`DeviceInfo`] (pushing it
/// onto `out`) and recurses into its `children` array, if any, so partitions
/// nested under a whole-disk entry are flattened into the same list as their
/// parent.
fn flatten_device(device: &Value, out: &mut Vec<DeviceInfo>) {
    if let Some(info) = device_info_from_json(device) {
        out.push(info);
    }

    if let Some(children) = device.get("children").and_then(Value::as_array) {
        for child in children {
            flatten_device(child, out);
        }
    }
}

/// Builds a single [`DeviceInfo`] from one `lsblk` device JSON object.
/// Returns `None` if `device` isn't a JSON object, or if it has neither a
/// usable `path` nor a `name` field to derive a path from - such an entry
/// carries no identifying information at all and would be useless to a
/// device picker.
fn device_info_from_json(device: &Value) -> Option<DeviceInfo> {
    let obj = device.as_object()?;

    let path = non_empty_string(obj.get("path"))
        .or_else(|| non_empty_string(obj.get("name")).map(|name| format!("/dev/{name}")))?;

    Some(DeviceInfo {
        path,
        uuid: non_empty_string(obj.get("uuid")),
        label: non_empty_string(obj.get("label")),
        partuuid: non_empty_string(obj.get("partuuid")),
        partlabel: non_empty_string(obj.get("partlabel")),
        fs_type: non_empty_string(obj.get("fstype")),
    })
}

/// Extracts a non-empty string from an optional JSON value, treating both
/// JSON `null` and an empty string as "absent" - `lsblk` uses `null` for
/// most unset columns but has historically emitted empty strings for some,
/// and this module does not need to distinguish the two.
fn non_empty_string(value: Option<&Value>) -> Option<String> {
    let s = value?.as_str()?;
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Discovers block devices available on this system by shelling out to
/// `lsblk` (see the module-level docs for why `lsblk` alone is sufficient).
///
/// Never panics and never returns an error type the caller must handle -
/// see [`DiscoveryResult`]'s docs for how soft failure is modeled. Intended
/// to be called from a `#[tauri::command]` wrapper in a later task so the
/// GUI can offer a device picker when adding/editing an entry's source,
/// falling back to manual text entry when `lsblk_available` is `false`.
pub fn discover_devices() -> DiscoveryResult {
    match run_lsblk() {
        Ok(raw) => DiscoveryResult {
            devices: parse_lsblk_json(&raw),
            lsblk_available: true,
        },
        Err(_) => DiscoveryResult {
            devices: Vec::new(),
            lsblk_available: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic (invented, not copied from any real machine) `lsblk -J
    /// -O` sample: one whole disk with no filesystem of its own and two
    /// partitions nested under it, plus a second whole disk that is itself
    /// a filesystem (no partition table), covering the common shapes a
    /// device picker needs to handle.
    const SAMPLE_LSBLK_JSON: &str = r#"
    {
       "blockdevices": [
          {
             "name": "sda",
             "path": "/dev/sda",
             "fstype": null,
             "label": null,
             "uuid": null,
             "partuuid": null,
             "partlabel": null,
             "children": [
                {
                   "name": "sda1",
                   "path": "/dev/sda1",
                   "fstype": "vfat",
                   "label": "ESP",
                   "uuid": "AAAA-BBBB",
                   "partuuid": "11111111-0101-0101-0101-111111111111",
                   "partlabel": "EFI System Partition"
                },
                {
                   "name": "sda2",
                   "path": "/dev/sda2",
                   "fstype": "ext4",
                   "label": null,
                   "uuid": "22222222-3333-4444-5555-666666666666",
                   "partuuid": "22222222-0202-0202-0202-222222222222",
                   "partlabel": null
                }
             ]
          },
          {
             "name": "sdb",
             "path": "/dev/sdb",
             "fstype": "swap",
             "label": "swapdisk",
             "uuid": "77777777-8888-9999-aaaa-bbbbbbbbbbbb",
             "partuuid": null,
             "partlabel": null
          }
       ]
    }
    "#;

    #[test]
    fn parses_nested_children_into_flat_list() {
        let devices = parse_lsblk_json(SAMPLE_LSBLK_JSON);
        assert_eq!(devices.len(), 4);

        let paths: Vec<&str> = devices.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, ["/dev/sda", "/dev/sda1", "/dev/sda2", "/dev/sdb"]);
    }

    #[test]
    fn whole_disk_with_no_filesystem_has_all_none_fields() {
        let devices = parse_lsblk_json(SAMPLE_LSBLK_JSON);
        let sda = devices.iter().find(|d| d.path == "/dev/sda").unwrap();
        assert_eq!(sda.fs_type, None);
        assert_eq!(sda.uuid, None);
        assert_eq!(sda.label, None);
        assert_eq!(sda.partuuid, None);
        assert_eq!(sda.partlabel, None);
    }

    #[test]
    fn partition_fields_are_parsed() {
        let devices = parse_lsblk_json(SAMPLE_LSBLK_JSON);
        let esp = devices.iter().find(|d| d.path == "/dev/sda1").unwrap();
        assert_eq!(esp.fs_type, Some("vfat".to_string()));
        assert_eq!(esp.label, Some("ESP".to_string()));
        assert_eq!(esp.uuid, Some("AAAA-BBBB".to_string()));
        assert_eq!(
            esp.partuuid,
            Some("11111111-0101-0101-0101-111111111111".to_string())
        );
        assert_eq!(esp.partlabel, Some("EFI System Partition".to_string()));
    }

    #[test]
    fn whole_disk_filesystem_without_partition_table_is_parsed() {
        let devices = parse_lsblk_json(SAMPLE_LSBLK_JSON);
        let sdb = devices.iter().find(|d| d.path == "/dev/sdb").unwrap();
        assert_eq!(sdb.fs_type, Some("swap".to_string()));
        assert_eq!(sdb.label, Some("swapdisk".to_string()));
        assert_eq!(sdb.partuuid, None);
    }

    #[test]
    fn path_falls_back_to_dev_slash_name_when_path_column_missing() {
        let raw = r#"{"blockdevices":[{"name":"sdc1","fstype":"xfs"}]}"#;
        let devices = parse_lsblk_json(raw);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].path, "/dev/sdc1");
        assert_eq!(devices[0].fs_type, Some("xfs".to_string()));
    }

    #[test]
    fn device_with_neither_path_nor_name_is_skipped_not_panicked() {
        let raw = r#"{"blockdevices":[{"fstype":"ext4"}]}"#;
        let devices = parse_lsblk_json(raw);
        assert_eq!(devices, Vec::new());
    }

    // --- Soft-failure path: see module docs and SPEC §8. ---

    #[test]
    fn empty_input_yields_empty_list_not_panic() {
        assert_eq!(parse_lsblk_json(""), Vec::new());
    }

    #[test]
    fn garbage_non_json_input_yields_empty_list_not_panic() {
        assert_eq!(parse_lsblk_json("this is not json at all {{{"), Vec::new());
    }

    #[test]
    fn valid_json_with_missing_blockdevices_key_yields_empty_list() {
        assert_eq!(parse_lsblk_json("{}"), Vec::new());
    }

    #[test]
    fn valid_json_with_wrong_shaped_blockdevices_yields_empty_list() {
        // "blockdevices" present but not an array: must not panic.
        assert_eq!(
            parse_lsblk_json(r#"{"blockdevices": "unexpected-string"}"#),
            Vec::new()
        );
        assert_eq!(parse_lsblk_json(r#"{"blockdevices": null}"#), Vec::new());
    }

    #[test]
    fn top_level_json_array_instead_of_object_yields_empty_list() {
        assert_eq!(parse_lsblk_json("[1, 2, 3]"), Vec::new());
    }

    #[test]
    fn discover_devices_never_panics_and_is_internally_consistent() {
        // This exercises the real `run_lsblk()` path. It deliberately makes
        // no assertion about whether `lsblk` is installed in the CI/test
        // environment (per SPEC §8, that must never be a hard dependency)
        // - only that the function returns normally and that an
        // unavailable tool is reported together with an empty device list.
        let result = discover_devices();
        if !result.lsblk_available {
            assert!(result.devices.is_empty());
        }
    }
}
