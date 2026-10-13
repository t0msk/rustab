//! Minimal Yew application entry point for Rustab.
//!
//! This is a scaffold-only shell (task T1): it renders a single heading to
//! prove the Trunk + Yew + Tailwind build pipeline works end to end. The
//! real component tree (entries table, add/edit form, diff view, etc.) is
//! built in later tasks (T9/T10/T11/T12) per `docs/SPEC.md` §9.

use yew::prelude::*;

#[function_component(App)]
fn app() -> Html {
    html! {
        <h1 class="text-3xl font-bold text-slate-100">{ "Rustab" }</h1>
    }
}

fn main() {
    yew::Renderer::<App>::new().render();
}
