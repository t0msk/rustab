//! Startup workaround for a known WebKitGTK rendering bug on some GPU/
//! driver combinations: the hardware-compositing/DMA-BUF renderer crashes
//! or blanks the window instead of showing the UI - see README.md's
//! "Troubleshooting: blank/white window on launch". Related upstream
//! reports: WebKit #258834, tauri-apps/tauri#9304 (a Mesa ≥24 instance of
//! this same WebKitGTK compositing fragility on Wayland, on GNOME 46+/
//! Plasma 6/Fedora 40+); this app's own reproduction was Nvidia's
//! proprietary driver on Wayland, a different GPU/driver pairing hitting
//! the same general class of bug.
//!
//! [`relaunch_supervised`] tries hardware rendering first on every launch
//! and only falls back if it actually fails here, rather than guessing
//! from the GPU - most users on any given GPU/driver never hit this bug.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

/// How long the hardware-render probe child is watched before
/// [`relaunch_supervised`] accepts it as the real app. Margin over the
/// ~2.5-2.8s the one reproduced crash took (Nvidia + Wayland: a GDK
/// Wayland protocol-error abort, exit code 1).
const PROBE_WINDOW: Duration = Duration::from_millis(4000);

/// Set on every child this module spawns, so that child skips supervising
/// itself and builds the real app directly.
pub(crate) const SUPERVISED_MARKER: &str = "RUSTAB_RENDER_SUPERVISED";

/// Tries hardware rendering first via a watched re-exec of this binary,
/// falling back to software rendering exactly once if that child fails
/// within [`PROBE_WINDOW`]. Call at the top of [`crate::run`], guarded on
/// `var_os(SUPERVISED_MARKER).is_none()`.
///
/// Returns `Some(exit_code)` once the real app has finished - the caller
/// should exit with it. Returns `None` to run directly in this process
/// instead: `RUSTAB_SKIP_RENDER_DETECTION=1` skipped probing,
/// `RUSTAB_FORCE_SOFTWARE_RENDER=1` applied the workaround unconditionally,
/// or re-executing this binary failed outright.
pub fn relaunch_supervised() -> Option<i32> {
    if std::env::var_os("RUSTAB_SKIP_RENDER_DETECTION").is_some() {
        return None;
    }
    if std::env::var_os("RUSTAB_FORCE_SOFTWARE_RENDER").is_some() {
        apply_software_render_vars(&mut EnvSetter);
        return None;
    }

    let exe = std::env::current_exe().ok()?;
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();

    let mut probe = Command::new(&exe);
    probe.args(&args).env(SUPERVISED_MARKER, "1");
    let mut child = probe.spawn().ok()?;

    let deadline = Instant::now() + PROBE_WINDOW;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Some(exit_code(status)),
            Ok(Some(_failure)) => return Some(relaunch_with_software_render(&exe, &args)),
            // Survived the probe window: let this same child keep running
            // as the real app. A later crash is a different problem.
            Ok(None) if Instant::now() >= deadline => {
                return Some(exit_code(child.wait().ok()?));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return Some(relaunch_with_software_render(&exe, &args)),
        }
    }
}

/// The one-time fallback: relaunch with software rendering forced, and run
/// that to completion. Never itself probed.
fn relaunch_with_software_render(exe: &Path, args: &[OsString]) -> i32 {
    let mut fallback = Command::new(exe);
    fallback.args(args).env(SUPERVISED_MARKER, "1");
    apply_software_render_vars(&mut CommandSetter(&mut fallback));
    match fallback.spawn().and_then(|mut child| child.wait()) {
        Ok(status) => exit_code(status),
        Err(_) => 1,
    }
}

fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

/// Where [`apply_software_render_vars`] writes a variable: this process's
/// own environment, or a [`Command`] about to be spawned.
trait VarSink {
    fn is_already_set(&self, key: &str) -> bool;
    fn set(&mut self, key: &'static str, value: &'static str);
}

struct EnvSetter;

impl VarSink for EnvSetter {
    fn is_already_set(&self, key: &str) -> bool {
        std::env::var_os(key).is_some()
    }
    fn set(&mut self, key: &'static str, value: &'static str) {
        std::env::set_var(key, value);
    }
}

struct CommandSetter<'a>(&'a mut Command);

impl VarSink for CommandSetter<'_> {
    fn is_already_set(&self, key: &str) -> bool {
        std::env::var_os(key).is_some()
    }
    fn set(&mut self, key: &'static str, value: &'static str) {
        self.0.env(key, value);
    }
}

/// Applies the software-render workaround to `sink`, skipping anything the
/// user already set themselves.
fn apply_software_render_vars(sink: &mut dyn VarSink) {
    for (key, value) in software_render_vars(session_is_wayland()) {
        if !sink.is_already_set(key) {
            sink.set(key, value);
        }
    }
}

/// The WebKitGTK software-render workaround itself - see README.md.
/// `GDK_BACKEND` only on Wayland (the reproduced failure); pure function of
/// session type so it's testable without touching real env vars.
fn software_render_vars(wayland: bool) -> Vec<(&'static str, &'static str)> {
    let mut vars = vec![
        ("WEBKIT_DISABLE_HARDWARE_ACCELERATION", "1"),
        ("WEBKIT_DISABLE_DMABUF_RENDERER", "1"),
        ("WEBKIT_DISABLE_COMPOSITING_MODE", "1"),
        ("LIBGL_ALWAYS_SOFTWARE", "1"),
    ];
    if wayland {
        vars.push(("GDK_BACKEND", "x11"));
    }
    vars
}

fn session_is_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v == "wayland")
}

/// Keys [`software_render_vars`] can set, stripped unconditionally before
/// this app spawns `xdg-open` (see `commands.rs`) - a workaround for this
/// app's own renderer, not something an unrelated browser/file manager
/// should inherit.
const RENDER_ENV_KEYS: [&str; 5] = [
    "WEBKIT_DISABLE_HARDWARE_ACCELERATION",
    "WEBKIT_DISABLE_DMABUF_RENDERER",
    "WEBKIT_DISABLE_COMPOSITING_MODE",
    "LIBGL_ALWAYS_SOFTWARE",
    "GDK_BACKEND",
];

/// See [`RENDER_ENV_KEYS`].
pub fn strip_from(cmd: &mut Command) {
    for key in RENDER_ENV_KEYS {
        cmd.env_remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x11_sessions_get_no_gdk_backend_override() {
        let vars = software_render_vars(false);
        assert!(!vars.iter().any(|(k, _)| *k == "GDK_BACKEND"));
    }

    #[test]
    fn wayland_sessions_also_get_x11_forced() {
        let vars = software_render_vars(true);
        assert!(vars.iter().any(|(k, v)| *k == "GDK_BACKEND" && *v == "x11"));
    }

    #[test]
    fn both_session_types_force_the_core_webkit_and_mesa_flags() {
        for wayland in [false, true] {
            let vars = software_render_vars(wayland);
            for key in [
                "WEBKIT_DISABLE_HARDWARE_ACCELERATION",
                "WEBKIT_DISABLE_DMABUF_RENDERER",
                "WEBKIT_DISABLE_COMPOSITING_MODE",
                "LIBGL_ALWAYS_SOFTWARE",
            ] {
                assert!(vars.iter().any(|(k, v)| *k == key && *v == "1"));
            }
        }
    }

    /// A stand-in [`VarSink`] that records what it was asked to set, with a
    /// settable "already set" response per key.
    struct FakeSink {
        already_set: Vec<&'static str>,
        applied: Vec<(&'static str, &'static str)>,
    }

    impl VarSink for FakeSink {
        fn is_already_set(&self, key: &str) -> bool {
            self.already_set.contains(&key)
        }
        fn set(&mut self, key: &'static str, value: &'static str) {
            self.applied.push((key, value));
        }
    }

    #[test]
    fn a_key_the_user_already_set_is_left_untouched() {
        let mut sink = FakeSink {
            already_set: vec!["LIBGL_ALWAYS_SOFTWARE"],
            applied: Vec::new(),
        };
        apply_software_render_vars(&mut sink);
        assert!(!sink
            .applied
            .iter()
            .any(|(k, _)| *k == "LIBGL_ALWAYS_SOFTWARE"));
        assert!(sink
            .applied
            .iter()
            .any(|(k, _)| *k == "WEBKIT_DISABLE_COMPOSITING_MODE"));
    }

    #[test]
    fn nothing_already_set_applies_every_variable() {
        let mut sink = FakeSink {
            already_set: Vec::new(),
            applied: Vec::new(),
        };
        apply_software_render_vars(&mut sink);
        assert_eq!(
            sink.applied.len(),
            software_render_vars(session_is_wayland()).len()
        );
    }
}
