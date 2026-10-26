//! Block-device / UUID / LABEL discovery helpers - see `docs/SPEC.md` §8.
//!
//! ## Why `lsblk` alone
//!
//! `lsblk -J -O` ("all columns") already reports `PATH`/`UUID`/`LABEL`/
//! `PARTUUID`/`PARTLABEL`/`FSTYPE` from the same udev/blkid metadata
//! `blkid` would report, so shelling out to `blkid` too would just
//! duplicate data for no gain. `lsblk -J` emits one JSON tree (whole disks
//! at top level, partitions nested under `children`), which this module
//! flattens into a `Vec<DeviceInfo>`.
//!
//! `findmnt` is not used here either: SPEC §8 wants a device/UUID/LABEL
//! *picker*, not live mount state (that's §6's `findmnt --verify` step, in
//! `validate.rs`).
//!
//! ## Soft-failure contract (per SPEC §8)
//!
//! `lsblk` may be missing, exit non-zero, or print something unparseable.
//! None of that may panic or hard-error: [`discover_devices`] always
//! returns a [`DiscoveryResult`] by value, never `Result`/`Option`.
//! [`DiscoveryResult::lsblk_available`] tells the caller whether `devices`
//! reflects a real run (including legitimately zero devices) or the tool
//! not running at all; callers fall back to manual text entry in the
//! latter case. Unparseable JSON from a successful run counts as
//! "available, zero devices" - see [`parse_lsblk_json`].
//!
//! ## Which process runs `lsblk`
//!
//! `lsblk` reports what *its own* process can see of `/dev` and udev, so a
//! process containerized away from the system being edited would get a
//! clean exit and an empty device list - a silent wrong answer rather than
//! a failure. [`discover_devices_with`] therefore takes the
//! [`crate::exec::CommandRunner`] that decides where `lsblk` runs, and
//! [`discover_devices`] is that with [`crate::exec::DirectRunner`]
//! supplied. See [`crate::exec`] for the full reasoning.

use std::io;

use serde_json::Value;

use crate::exec::{CommandRunner, DirectRunner};

/// One block device or partition discovered via `lsblk`, carrying whichever
/// device-identifier fields fstab entries can reference as a source.
///
/// Every field besides `path` is `Option` because `lsblk` legitimately
/// reports `null` for most of these (e.g. an unformatted partition has no
/// `fs_type`; a whole-disk entry usually has no `uuid`/`label` of its own).
/// `None` means "lsblk reported no value", not "parse error" - invocation
/// failures are modeled at the [`DiscoveryResult`] level, not per-field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// The device's path, e.g. `/dev/sda1`. Falls back to `/dev/<name>` if
    /// `lsblk`'s `path` column was `null`.
    pub path: String,
    /// Filesystem UUID (`lsblk`'s `uuid` column) - what fstab's `UUID=` references.
    pub uuid: Option<String>,
    /// Filesystem label (`lsblk`'s `label` column) - what fstab's `LABEL=` references.
    pub label: Option<String>,
    /// Partition-table UUID (`lsblk`'s `partuuid` column) - distinct from
    /// `uuid`, which identifies the filesystem rather than the partition entry.
    pub partuuid: Option<String>,
    /// Partition label (`lsblk`'s `partlabel` column) - the GPT partition
    /// name, distinct from the filesystem `label`.
    pub partlabel: Option<String>,
    /// Filesystem type (`lsblk`'s `fstype` column), e.g. `ext4`, `vfat`,
    /// `swap`. `None` for devices with no filesystem `lsblk` recognizes.
    pub fs_type: Option<String>,
}

/// The outcome of a device-discovery attempt (see [`discover_devices`]).
///
/// Not a `Result<Vec<DeviceInfo>, E>` - there's no error variant to match
/// on or propagate. `devices` is always usable directly; `lsblk_available`
/// is just a UI hint for whether to fall back to manual text entry.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiscoveryResult {
    /// Devices discovered, in `lsblk`'s order (disks then their
    /// partitions). Empty if `lsblk` is unavailable, failed, produced
    /// unparseable output, or the system legitimately has none.
    pub devices: Vec<DeviceInfo>,
    /// `true` if `lsblk` ran and exited successfully, regardless of how
    /// many devices were parsed. `false` if the binary couldn't be
    /// launched or exited non-zero - callers fall back to manual text
    /// entry per SPEC §8, not treat it as an error to surface loudly.
    pub lsblk_available: bool,
}

/// Runs `lsblk -J -O` through `runner` and returns its raw stdout.
///
/// Returns `Err` if the binary couldn't be launched or exited non-zero (the
/// `io::Error` message carries the exit status, not stderr). Kept separate
/// from [`parse_lsblk_json`] so the parsing logic can be unit-tested with
/// canned input, without `lsblk` installed. No input is staged: `lsblk`
/// reads the system, not a file.
fn run_lsblk(runner: &dyn CommandRunner) -> io::Result<String> {
    let output = runner.run("lsblk", &["-J", "-O"], None)?;
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
/// Pure function, no I/O. Never panics: invalid JSON, an unexpected shape,
/// or a missing/non-array `blockdevices` field all yield an empty `Vec`
/// (see the module's soft-failure contract) - there's no way to report
/// *why* parsing failed, since "fall back to manual entry" is the same
/// caller action regardless.
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

/// Converts one `lsblk` device JSON object into a [`DeviceInfo`] (pushed
/// onto `out`) and recurses into its `children`, flattening partitions into
/// the same list as their parent disk.
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

/// Builds a [`DeviceInfo`] from one `lsblk` device JSON object. Returns
/// `None` if `device` isn't an object, or has neither `path` nor `name` to
/// derive one from.
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
/// `null` and `""` as absent (`lsblk` has used both for unset columns).
fn non_empty_string(value: Option<&Value>) -> Option<String> {
    let s = value?.as_str()?;
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Discovers block devices on this system via `lsblk` (see module docs for
/// why `lsblk` alone is sufficient).
///
/// Never panics and never returns an error the caller must handle - see
/// [`DiscoveryResult`] for how soft failure is modeled.
///
/// Runs `lsblk` in this process. A caller that must run it somewhere else
/// (see the module docs) calls [`discover_devices_with`] instead.
pub fn discover_devices() -> DiscoveryResult {
    discover_devices_with(&DirectRunner)
}

/// [`discover_devices`], with the process that runs `lsblk` chosen by the
/// caller.
///
/// Identical in every other respect, including the soft-failure contract:
/// `runner` decides only *where* `lsblk` runs, never how its output is
/// read.
pub fn discover_devices_with(runner: &dyn CommandRunner) -> DiscoveryResult {
    match run_lsblk(runner) {
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

    /// Synthetic `lsblk -J -O` sample: one disk with two partitions, plus a
    /// second disk that's itself a filesystem (no partition table).
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
        // Doesn't assume lsblk is installed in CI - only that an unavailable
        // tool is reported together with an empty device list.
        let result = discover_devices();
        if !result.lsblk_available {
            assert!(result.devices.is_empty());
        }
    }

    // --- The command-runner seam (see `crate::exec`) ---

    /// The exact command discovery is built from, and that a successful run
    /// is parsed into devices through the seam rather than from whatever
    /// this machine's own `lsblk` reports.
    #[test]
    fn discovery_runs_lsblk_with_json_and_all_columns_and_parses_its_output() {
        let runner = crate::exec::RecordingRunner::answering(0, SAMPLE_LSBLK_JSON);

        let result = discover_devices_with(&runner);

        let runs = runner.runs();
        assert_eq!(runs.len(), 1, "exactly one command per discovery");
        assert_eq!(runs[0].program, "lsblk");
        assert_eq!(runs[0].args, vec!["-J", "-O"]);
        assert_eq!(
            runs[0].input, None,
            "lsblk reads the system, so it must be given no staged input"
        );
        assert!(result.lsblk_available);
        let paths: Vec<&str> = result.devices.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, ["/dev/sda", "/dev/sda1", "/dev/sda2", "/dev/sdb"]);
    }

    /// A non-zero exit from the seam is the soft failure SPEC §8 wants, not
    /// a panic and not a partial list.
    #[test]
    fn a_non_zero_lsblk_exit_from_the_runner_is_reported_as_unavailable() {
        let runner = crate::exec::RecordingRunner::answering(1, SAMPLE_LSBLK_JSON);

        let result = discover_devices_with(&runner);

        assert!(!result.lsblk_available);
        assert!(result.devices.is_empty());
    }
}
