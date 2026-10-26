//! Startup workaround for a known WebKitGTK rendering bug: on some
//! GPU/driver combinations WebKitGTK's hardware-compositing/DMA-BUF
//! renderer crashes or renders a blank window instead of the UI - see
//! `README.md`'s "Troubleshooting: blank/white window on launch" section
//! for the user-facing symptom.
//!
//! An earlier version of this module tried to *predict* this from the GPU
//! alone (Nvidia's proprietary driver present, or running in a VM) and
//! forced software rendering preemptively whenever either was true. That
//! was wrong in practice: Nvidia alone is a bad predictor - most Nvidia
//! users never hit this bug - so it cost everyone on Nvidia real
//! scrolling/repaint performance for nothing.
//!
//! [`relaunch_supervised`] instead *tries hardware rendering first, every
//! launch*, and only falls back if it actually fails on this machine. The
//! one concrete failure reproduced during this fix (Nvidia proprietary
//! driver + Wayland session, current driver as of writing): the process
//! reliably dies about 2.5-2.8s after launch with
//! `Gdk-Message: Error 71 (Protocol error) dispatching to Wayland
//! display.` and exit code 1 - not a silent blank window, an observable
//! crash. [`PROBE_WINDOW`] is sized with margin over that.
//!
//! Every variable [`software_render_vars`] lists is only ever added where
//! the user hasn't already set it themselves, so an explicit value the
//! user set always wins. `RUSTAB_FORCE_SOFTWARE_RENDER=1` applies the
//! workaround unconditionally, skipping the probe (for a machine whose
//! failure mode isn't an early crash, so probing wouldn't catch it
//! anyway). `RUSTAB_SKIP_RENDER_DETECTION=1` disables this module
//! entirely.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

/// How long the hardware-render probe child is given to prove itself
/// before [`relaunch_supervised`] accepts it as the real app and stops
/// watching it. Margin over the ~2.5-2.8s the one reproduced failure took
/// across several runs (see the module doc) - long enough to catch that
/// crash, short enough that it costs nothing for a working hardware path,
/// since that child *is* the real, already-visible app for the whole
/// window, not an invisible delay bolted on before it.
const PROBE_WINDOW: Duration = Duration::from_millis(4000);

/// Set on every child this module spawns, so that child's own call to
/// [`crate::run`] skips straight to building the real Tauri app instead of
/// supervising *itself* - without it, each relaunch would spawn another.
pub(crate) const SUPERVISED_MARKER: &str = "RUSTAB_RENDER_SUPERVISED";

/// Tries hardware rendering first by re-executing this same binary as a
/// watched child, falling back to software rendering exactly once if that
/// child fails within [`PROBE_WINDOW`] - see the module doc for why and
/// how. Call this at the very top of [`crate::run`], before
/// `SUPERVISED_MARKER` could possibly already be set (i.e. guard the call
/// site on `var_os(SUPERVISED_MARKER).is_none()`), or every relaunch would
/// re-enter this and spawn another child forever.
///
/// Returns `Some(exit_code)` once the real app - whichever child actually
/// ran it - has finished; the caller should `std::process::exit` with it
/// and build nothing in *this* process. Returns `None` when the app should
/// instead be built and run directly, in *this* process:
/// `RUSTAB_SKIP_RENDER_DETECTION=1` asked for no probing at all,
/// `RUSTAB_FORCE_SOFTWARE_RENDER=1` asked for the workaround
/// unconditionally (nothing to probe for), or re-executing this binary
/// failed outright (no `current_exe()`, exec permissions, ...) - in which
/// case probing is skipped rather than failing the whole app over it.
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
            Ok(Some(status)) if status.success() => {
                // Exited on its own, cleanly, within the probe window -
                // e.g. the user closed it immediately. Nothing to recover
                // from; this is just how the run ended.
                return Some(exit_code(status));
            }
            Ok(Some(_failure)) => return Some(relaunch_with_software_render(&exe, &args)),
            Ok(None) if Instant::now() >= deadline => {
                // Survived the probe window: hardware rendering works
                // here. Let this same child keep running as the real app;
                // a later crash, however it happens, is a different
                // problem, not this bug - don't second-guess it.
                return Some(exit_code(child.wait().ok()?));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return Some(relaunch_with_software_render(&exe, &args)),
        }
    }
}

/// The one-time fallback: relaunch with the software-render workaround
/// forced, and run that to completion. Only ever reached once per launch -
/// this child is never itself probed.
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

/// Where [`apply_software_render_vars`] writes each variable - either into
/// *this* process's own environment (the `RUSTAB_FORCE_SOFTWARE_RENDER=1`
/// path, which never spawns a child) or onto a [`Command`] about to be
/// spawned (the probe-failed fallback path). Both skip a key the user
/// already set themselves - see [`apply_software_render_vars`].
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

/// Applies the WebKitGTK software-render workaround to `sink`, skipping
/// any variable the user already set themselves - see
/// [`software_render_vars`] for the list and why `GDK_BACKEND` is
/// conditional.
fn apply_software_render_vars(sink: &mut dyn VarSink) {
    for (key, value) in software_render_vars(session_is_wayland()) {
        if !sink.is_already_set(key) {
            sink.set(key, value);
        }
    }
}

/// The WebKitGTK software-render workaround itself - see README.md's
/// "Troubleshooting: blank/white window on launch" section. Pure function
/// of whether the session is Wayland, so it's testable without touching
/// real environment variables.
fn software_render_vars(wayland: bool) -> Vec<(&'static str, &'static str)> {
    let mut vars = vec![
        ("WEBKIT_DISABLE_HARDWARE_ACCELERATION", "1"),
        ("WEBKIT_DISABLE_DMABUF_RENDERER", "1"),
        ("WEBKIT_DISABLE_COMPOSITING_MODE", "1"),
        ("LIBGL_ALWAYS_SOFTWARE", "1"),
    ];
    if wayland {
        // Nvidia's EGL/Wayland path is the one reproduced broken (see the
        // module doc); X11/XWayland sidesteps it entirely, at the cost of
        // XWayland's fuzzier HiDPI scaling - no reason to force it on an
        // X11 session that was never affected.
        vars.push(("GDK_BACKEND", "x11"));
    }
    vars
}

fn session_is_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v == "wayland")
}

/// The keys [`software_render_vars`] can set, stripped unconditionally from
/// every `Command` this app spawns on the user's behalf (currently
/// `xdg-open`, see `commands.rs`) before it runs: they're specifically a
/// WebKitGTK/Mesa workaround for *this* app's own renderer, and forcing an
/// unrelated program - a browser, a file manager - into software rendering
/// or X11 just because this process needed it would be a regression for
/// the user, not a fix. Unconditional (not just "what this process itself
/// set") because by the time a command is spawned, this process can no
/// longer tell its own fallback-relaunch env apart from something the user
/// set independently - and either way, neither belongs on an unrelated
/// program.
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

    /// A stand-in [`VarSink`] that records what was asked of it, with a
    /// settable "already set" response per key - so
    /// [`apply_software_render_vars`]'s "skip what the user already set"
    /// rule is tested without touching the real process environment.
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
            software_render_vars(sink_is_wayland()).len()
        );
    }

    /// Matches whatever the real `session_is_wayland()` would see in this
    /// test process, so the count in the test above stays correct
    /// regardless of which session type runs the test suite.
    fn sink_is_wayland() -> bool {
        session_is_wayland()
    }
}
