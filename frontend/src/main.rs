//! Yew application entry point for Rustab.
//!
//! Renders the `AppShell` component tree, which drives the entries
//! table, add/edit/delete form, diff view, and backup/restore panel
//! against the real Tauri commands in `docs/API_CONTRACT.md` via
//! `api`/`dto`/`candidate` (see `docs/UI_DESIGN.md` §9).

mod api;
mod candidate;
mod components;
mod diff;
mod dto;

use components::app_shell::AppShell;

fn main() {
    yew::Renderer::<AppShell>::new().render();
}
