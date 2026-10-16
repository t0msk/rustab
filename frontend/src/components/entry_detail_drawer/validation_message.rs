//! `ValidationMessage` - generic error/warning finding renderer (§5).
//!
//! The severity classification (which checks are hard-blocking vs.
//! advisory) is a backend/validator decision (`fstab_core::validate`,
//! surfaced via `validate_candidate`) - this component only renders
//! whatever severity it is given. Wired up in T11 to render
//! `ValidationReportDto` findings in `EntryDetailDrawer`.
//!
//! Note: `FindingDto` (API_CONTRACT.md §7) now also carries `field`/
//! `line_index` (T21.3), which `EntryDetailDrawer` uses to route a warning
//! about this draft's own mount point into `MountPointField` (rendered via
//! this component, directly below the input) instead of the flat warning
//! list. `severity_ring_classes` is still unused today - the per-field
//! `ring-2`/`ring-1` input-border highlighting `docs/UI_DESIGN.md` §5
//! describes has no caller yet, kept for API completeness.

use yew::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    // `EntryDetailDrawer` routes blocking findings through
    // `ValidationSummary` (a plain message list) instead, so nothing
    // constructs this variant yet - kept for this component's own
    // completeness as a generic two-severity renderer.
    #[allow(dead_code)]
    Error,
    Warning,
}

#[derive(Properties, PartialEq)]
pub struct ValidationMessageProps {
    pub message: AttrValue,
    pub severity: Severity,
}

#[function_component(ValidationMessage)]
pub fn validation_message(props: &ValidationMessageProps) -> Html {
    let classes = match props.severity {
        Severity::Error => classes!("text-red-700", "dark:text-red-400"),
        Severity::Warning => classes!("text-amber-600", "dark:text-amber-400"),
    };

    html! {
        <p class={classes!("text-xs", "mt-1", classes)}>
            { props.message.clone() }
        </p>
    }
}

/// The matching field-level input ring class for a given severity, for
/// callers that need to highlight the input itself (§5: `ring-2
/// ring-red-500` for errors, `ring-1 ring-amber-400` for warnings). See the
/// module doc comment: no caller applies this to an input's border yet
/// (`MountPointField`'s inline warnings render the message only).
#[allow(dead_code)]
pub fn severity_ring_classes(severity: Severity) -> Classes {
    match severity {
        Severity::Error => classes!("ring-2", "ring-red-500"),
        Severity::Warning => classes!("ring-1", "ring-amber-400"),
    }
}
