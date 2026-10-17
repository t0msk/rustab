//! Rustab's Tauri application crate: the command layer that wires the
//! GUI-free `fstab-core` library to a concrete target file and, for every
//! write that reaches the real `/etc/fstab` - which since T34 is every save
//! of the default target - to a single privilege-escalated write.
//!
//! The frozen contract every command here honours - names, argument shapes,
//! return shapes, error shapes, the target-resolution policy, and the
//! privilege-escalation mechanism - is documented in `docs/API_CONTRACT.md`.
//! Change the docs and the code together, never one alone.
//!
//! Module map:
//!
//! - [`target`]: which file is the active target (the only place that
//!   decides), plus the working draft's conditional refresh from - and
//!   post-save sync with - the real `/etc/fstab`.
//! - [`dto`]: the serializable types crossing the Tauri boundary, and their
//!   lossless conversions to and from `fstab-core`'s domain types.
//! - [`backups`]: listing backups of the active target and resolving a
//!   frontend-supplied backup path against that list.
//! - [`privileged`]: both halves of the three elevated operations (writing the
//!   real `/etc/fstab`, deleting one of its backups, and creating a manual
//!   backup of it) - the `pkexec` re-exec on the unprivileged side and the
//!   single hidden internal subcommand on the elevated side.
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
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::get_target_info,
            commands::list_entries,
            commands::validate_candidate,
            commands::render_candidate,
            commands::save,
            commands::list_backups,
            commands::restore_backup,
            commands::read_backup,
            commands::delete_backup,
            commands::create_backup,
            commands::discover_devices,
            commands::open_backup_location,
            commands::open_external_link,
            commands::open_path_in_file_manager,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
