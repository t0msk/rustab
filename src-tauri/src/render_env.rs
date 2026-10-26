//! Linux-only startup workaround for a known WebKitGTK rendering bug: on
//! some GPU/driver combinations WebKitGTK's hardware-compositing/DMA-BUF
//! renderer either aborts the WebKit child process outright or renders a
//! silently blank window instead of the UI - see `README.md`'s and
//! `docs/FLATPAK.md`'s "known WebKitGTK rendering issue" sections for the
//! user-facing symptom, which used to require setting a couple of
//! environment variables by hand before launching.
//!
//! [`apply`] sets the same variables automatically, but only on machines
//! that match a known-bad combination, so GPU-accelerated rendering stays
//! on for everyone else - on bare-metal AMD/Intel, which is the common
//! case, the hardware path works fine and is noticeably faster than
//! software rendering (the exact tradeoff `docs/FLATPAK.md` §1.5 already
//! documents for why this was previously opt-in only).
//!
//! Every variable here is skipped if it's already set
//! (`var_os(...).is_none()`), so a user who knows better can always
//! override a single one by exporting it themselves before launching.
//! `RUSTAB_SKIP_RENDER_DETECTION=1` skips this module entirely, and
//! `RUSTAB_FORCE_SOFTWARE_RENDER=1` applies the workaround unconditionally
//! regardless of what detection finds.

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

/// The variables [`apply`] itself set (not ones already present before it
/// ran) - tracked so [`strip_from`] can keep them off the processes this
/// app spawns for the *user's* sake (`xdg-open`, and whatever it hands off
/// to: a browser, a mail client, a file manager). Those are unrelated
/// programs with their own GPU/WebKit situation; forcing a browser into
/// software-rendered WebGL/video just because Rustab's own WebKitGTK
/// instance needed it would be a regression for the user, not a fix.
static OWN_RENDER_ENV: OnceLock<Vec<&'static str>> = OnceLock::new();

/// Applied once, before the Tauri runtime - and the WebKit child process it
/// spawns - start up (see [`crate::run`]). No-op unless detection, or an
/// explicit override, says this machine needs it.
pub fn apply() {
    if std::env::var_os("RUSTAB_SKIP_RENDER_DETECTION").is_some() {
        return;
    }
    let forced = std::env::var_os("RUSTAB_FORCE_SOFTWARE_RENDER").is_some();
    if !forced && !needs_software_render(nvidia_driver_present(), likely_running_in_a_vm()) {
        return;
    }

    let mut own: Vec<&'static str> = Vec::new();
    set_if_unset("WEBKIT_DISABLE_HARDWARE_ACCELERATION", "1", &mut own);
    set_if_unset("WEBKIT_DISABLE_DMABUF_RENDERER", "1", &mut own);
    set_if_unset("WEBKIT_DISABLE_COMPOSITING_MODE", "1", &mut own);
    set_if_unset("LIBGL_ALWAYS_SOFTWARE", "1", &mut own);
    if session_is_wayland() {
        // Nvidia's EGL/Wayland path is the one most consistently reported as
        // broken (see README.md); X11/XWayland sidesteps it entirely, at the
        // cost of XWayland's fuzzier HiDPI scaling.
        set_if_unset("GDK_BACKEND", "x11", &mut own);
    }
    let _ = OWN_RENDER_ENV.set(own);
}

/// Removes whatever [`apply`] itself set from `cmd`'s environment - call
/// this on every `Command` this app spawns on the user's behalf (currently
/// `xdg-open`, see `commands.rs`) before `.spawn()`. A no-op if [`apply`]
/// never ran or set nothing.
pub fn strip_from(cmd: &mut Command) {
    if let Some(keys) = OWN_RENDER_ENV.get() {
        for key in keys {
            cmd.env_remove(key);
        }
    }
}

fn set_if_unset(key: &'static str, value: &str, own: &mut Vec<&'static str>) {
    if std::env::var_os(key).is_none() {
        std::env::set_var(key, value);
        own.push(key);
    }
}

fn session_is_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v == "wayland")
}

/// The pure decision: given what the two probes below found, does this
/// machine match a known-bad combination? Kept separate from the probes
/// themselves so it's testable without touching the real filesystem.
fn needs_software_render(nvidia_driver_present: bool, likely_vm: bool) -> bool {
    nvidia_driver_present || likely_vm
}

/// Nvidia's proprietary driver is the GPU/driver combination `README.md`
/// and `docs/FLATPAK.md` report most consistently for this bug - unlike the
/// open Mesa stack, which WebKitGTK's hardware path handles fine on the
/// same bug reports upstream (WebKit #258834, tauri-apps/tauri#9304).
/// `/proc/driver/nvidia/version` only exists while the proprietary kernel
/// module is loaded; the open `nouveau` driver (which doesn't hit this bug)
/// never creates it.
fn nvidia_driver_present() -> bool {
    Path::new("/proc/driver/nvidia/version").exists()
}

/// A best-effort VM check via the `hypervisor` CPU flag Linux exposes in
/// `/proc/cpuinfo` under any hypervisor (KVM/QEMU, VMware, VirtualBox,
/// Hyper-V) - the same flag `systemd-detect-virt`/`lscpu` key off, and one
/// that needs no extra binary or sandbox permission beyond `/proc` itself.
/// A false result either way only costs a performance tradeoff, never
/// correctness - see [`needs_software_render`].
fn likely_running_in_a_vm() -> bool {
    std::fs::read_to_string("/proc/cpuinfo")
        .map(|contents| {
            contents
                .lines()
                .any(|line| line.starts_with("flags") && line.contains("hypervisor"))
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neither_signal_means_hardware_rendering_stays_on() {
        assert!(!needs_software_render(false, false));
    }

    #[test]
    fn nvidia_alone_forces_software_render() {
        assert!(needs_software_render(true, false));
    }

    #[test]
    fn vm_alone_forces_software_render() {
        assert!(needs_software_render(false, true));
    }

    #[test]
    fn both_signals_still_force_software_render() {
        assert!(needs_software_render(true, true));
    }
}
