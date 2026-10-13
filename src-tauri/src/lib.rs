//! Rustab's Tauri application crate: the command layer that wires the
//! GUI-free `fstab-core` library to a concrete target file and, when that
//! target is the real `/etc/fstab`, to a single privilege-escalated write.
//!
//! The frozen contract every command here honours — names, argument shapes,
//! return shapes, error shapes, the target-resolution policy, and the
//! privilege-escalation mechanism — is documented in `docs/API_CONTRACT.md`.
//! Change the docs and the code together, never one alone.
//!
//! Module map:
//!
//! - [`target`]: which file is the active target (the only place that
//!   decides).
//! - [`dto`]: the serializable types crossing the Tauri boundary, and their
//!   lossless conversions to and from `fstab-core`'s domain types.
//! - [`backups`]: listing backups of the active target and resolving a
//!   frontend-supplied backup path against that list.
//! - [`privileged`]: both halves of the elevated write — the `pkexec`
//!   re-exec on the unprivileged side and the hidden internal subcommand on
//!   the elevated side.
//! - [`commands`]: the `#[tauri::command]` functions themselves.

pub mod backups;
pub mod commands;
pub mod dto;
pub mod privileged;
pub mod target;

#[cfg(test)]
mod testutil;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_target_info,
            commands::list_entries,
            commands::validate_candidate,
            commands::render_candidate,
            commands::save,
            commands::list_backups,
            commands::restore_backup,
            commands::discover_devices,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
