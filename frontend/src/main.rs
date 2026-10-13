//! Yew application entry point for Rustab.
//!
//! T1 scaffolded a single "Rustab" heading just to prove the Trunk + Yew +
//! Tailwind build pipeline. T10 built the real component tree on top of
//! that scaffold with mock/static data only. T11 (this task) wires the
//! entries table and add/edit/delete form to the real Tauri commands in
//! `docs/API_CONTRACT.md` via `api`/`dto`/`candidate`. The diff view,
//! backup panel, and live target-aware save/restore flow are T12 (see
//! `docs/UI_DESIGN.md` §9).

mod api;
mod candidate;
mod components;
mod dto;
mod mock;

use components::app_shell::AppShell;

fn main() {
    yew::Renderer::<AppShell>::new().render();
}
