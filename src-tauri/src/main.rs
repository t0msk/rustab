// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // The elevated-writer check comes first, before anything else in the
    // process starts: when Rustab is re-executed through `pkexec` with the
    // internal flag (see `rustab_lib::privileged`), it must read stdin, do a
    // single atomic write, print one JSON object, and exit — with no Tauri
    // runtime, webview, plugin or logger ever initialized in a root process.
    if let Some(code) = rustab_lib::privileged::run_internal_if_requested() {
        std::process::exit(code);
    }

    rustab_lib::run();
}
