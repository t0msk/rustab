//! Yew application entry point for Rustab.
//!
//! T1 scaffolded a single "Rustab" heading just to prove the Trunk + Yew +
//! Tailwind build pipeline. This task (T10) builds the real component tree
//! on top of that scaffold: `AppShell` and everything under it, rendered
//! with mock/static data only — zero Tauri calls. Real data wiring for the
//! entries table / add-edit form is T11; diff view, backup panel, and the
//! live target indicator are T12 (see `docs/UI_DESIGN.md` §9).

mod components;
mod mock;

use components::app_shell::AppShell;

fn main() {
    yew::Renderer::<AppShell>::new().render();
}
