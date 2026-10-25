//! Rustab's Tauri application crate: the command layer that wires the
//! GUI-free `fstab-core` library to a concrete target file, escalating
//! every write to the real `/etc/fstab` through a single privileged write.
//!
//! The frozen contract every command here honours (names, argument/return/
//! error shapes, target-resolution policy, privilege-escalation mechanism)
//! is documented in `docs/API_CONTRACT.md`. Change the docs and the code
//! together, never one alone.
//!
//! Module map:
//!
//! - [`target`]: which file is the active target, plus the working draft's
//!   refresh from and post-save sync with the real `/etc/fstab`.
//! - `backup_location`: whether this process can manage the real
//!   `/etc/fstab`'s backups in `/etc/` at all, and the per-user fallback
//!   directory used when it can't (a Flatpak sandbox can't, since the
//!   `/etc` it sees is the runtime's).
//! - `host`: knows this process may be inside a Flatpak sandbox, so
//!   reading the real `/etc/fstab`, listing what sits beside it and
//!   starting the elevated helper all happen *on the host*, through
//!   `flatpak-spawn --host`.
//! - `host_backups`: the real `/etc/fstab`'s own backups, listed and read
//!   through `host` - the read-only counterpart of `backups` for a session
//!   that cannot see the directory they live in. Lists and reads only;
//!   nothing there writes or unlinks.
//! - [`privileged`]: the unprivileged half of the three elevated
//!   operations (writing `/etc/fstab`, deleting a backup, creating a
//!   manual backup) - the `pkexec` re-exec and the interpretation of what
//!   came back, plus the hidden internal subcommand that dispatches into
//!   the elevated half.
//! - [`commands`]: the `#[tauri::command]` functions themselves.
//!
//! The GUI-free half lives in the `rustab-privileged` crate, which the
//! `rustab-helper` binary links too, so the elevated code can be built
//! without GTK/WebKitGTK/`tauri` (see [`privileged`]). Two of its modules
//! are re-exported here unchanged, because they are as much part of this
//! crate's own surface as of the helper's:
//!
//! - [`dto`]: serializable types crossing the Tauri boundary, and their
//!   lossless conversions to/from `fstab-core`'s domain types.
//! - [`backups`]: listing backups of the active target and resolving a
//!   frontend-supplied backup path against that list.

mod backup_location;
pub mod commands;
mod host;
// Nothing outside this module's own tests calls it yet: the command layer's
// dispatch into it (and this `allow`'s removal) is the next step.
#[allow(dead_code)]
mod host_backups;
pub mod privileged;
pub mod target;

pub use rustab_privileged::{backups, dto};

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
            commands::resync_external_change,
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
