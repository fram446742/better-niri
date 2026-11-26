//! Human-readable summaries for the companion actions. These are intentionally
//! the same strings the old text UI printed, so previews and reports stay
//! familiar and testable.

use loom_settings::integrations::{DisplayPlan, SessionOutcome, WallpaperPlan};

pub fn pretty(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

pub fn display_plan(plan: &DisplayPlan) -> String {
    let mut text = format!(
        "{}: {} ({} action(s){})",
        plan.command,
        plan.status,
        plan.actions.len(),
        if plan.dry_run { ", dry run" } else { "" }
    );
    for action in &plan.actions {
        text.push_str("\n  ");
        text.push_str(action);
    }
    for warning in &plan.warnings {
        text.push_str("\nwarning: ");
        text.push_str(warning);
    }
    for error in &plan.errors {
        text.push_str("\nerror: ");
        text.push_str(error);
    }
    text
}

pub fn session_outcome(outcome: &SessionOutcome) -> String {
    let mut text = format!(
        "{}: {} applied, {} skipped, {} unmatched, {} profile(s){}",
        outcome.context,
        outcome.applied,
        outcome.skipped,
        outcome.unmatched,
        outcome.profiles,
        if outcome.dry_run { " [dry run]" } else { "" }
    );
    if outcome.partial {
        text.push_str(" (partial)");
    }
    if let Some(error) = &outcome.error {
        text.push_str("\nerror: ");
        text.push_str(error);
    }
    text
}

pub fn wallpaper_plan(plan: &WallpaperPlan) -> String {
    let mut text = format!(
        "{}: {} output(s){}",
        plan.backend,
        plan.images.len(),
        if plan.dry_run { " [dry run]" } else { "" }
    );
    if let Some(image) = &plan.image {
        text.push_str(&format!("\n  image: {image}"));
    }
    for (output, image) in &plan.images {
        text.push_str(&format!(
            "\n  {output}: {}",
            image.as_deref().unwrap_or("(none)")
        ));
    }
    if let Some(template) = &plan.template {
        text.push_str(&format!("\n  command template: {template}"));
    }
    text
}
