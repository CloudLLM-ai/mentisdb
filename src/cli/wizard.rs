use crate::integrations::detect::{detect_integrations_with_environment, DetectionStatus};
use crate::integrations::plan::{
    build_detected_setup_catalog, build_setup_plan_for_integration, SetupCatalogPlan, SetupPlan,
};
use crate::integrations::IntegrationKind;
use crate::paths::{HostPlatform, PathEnvironment};
use std::io::{self, BufRead, IsTerminal, Write};

use super::args::{default_url, parse_integration};
use super::prompt::{
    boxed_apply_summary, boxed_selection_prompt, boxed_skip_notice, boxed_text_prompt,
    boxed_yn_prompt,
};
use super::setup::{ensure_prerequisites, PrerequisiteStatus};
use super::{render_setup_plan, WizardCommand};

pub(super) fn run_wizard(
    command: &WizardCommand,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> io::Result<()> {
    let env = PathEnvironment::capture();
    let platform = HostPlatform::current();
    let report = detect_integrations_with_environment(platform, env.clone());
    let catalog = build_detected_setup_catalog(report);

    writeln!(out, "MentisDB setup wizard")?;
    writeln!(out)?;

    let selected = if command.assume_yes {
        render_catalog_summary(&catalog, out)?;
        writeln!(out)?;
        default_selections(&catalog)
    } else if std::io::stdin().is_terminal() {
        interactive_checkbox_select(&catalog, out)?
    } else {
        render_catalog_summary(&catalog, out)?;
        writeln!(out)?;
        let entered = boxed_selection_prompt(out, input)?;
        resolve_selections(&catalog, &entered)?
    };

    if selected.is_empty() {
        writeln!(out, "\nNothing selected.")?;
        return Ok(());
    }

    let url_override = if let Some(url) = &command.url {
        Some(url.clone())
    } else {
        let entered = boxed_text_prompt(
            out,
            "Override the default MentisDB URL for all selected integrations?\n(Leave blank to use per-integration defaults)",
            input,
        )?;
        (!entered.trim().is_empty()).then_some(entered.trim().to_string())
    };

    let mut planned = Vec::new();
    for integration in selected {
        let url = url_override
            .clone()
            .unwrap_or_else(|| default_url(integration).to_string());
        let Some(plan) = build_setup_plan_for_integration(integration, url, platform, &env) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported integration target",
            ));
        };
        planned.push(plan);
    }

    let mut final_plans = Vec::new();
    for plan in planned {
        if plan.detection_status == DetectionStatus::Configured {
            if command.assume_yes {
                boxed_skip_notice(out, plan.integration.display_name())?;
                continue;
            }

            let question = format!(
                "{} already has a mentisdb integration.\nOverwrite it with a fresh setup?",
                plan.integration.display_name()
            );
            let decision = boxed_yn_prompt(out, &question, false, input)?;
            if !decision.eq_ignore_ascii_case("y") && !decision.eq_ignore_ascii_case("yes") {
                continue;
            }
        }
        write!(out, "{}", render_setup_plan(&plan))?;
        final_plans.push(plan);
    }

    if final_plans.is_empty() {
        return Ok(());
    }

    let default_yes = !command.assume_yes;
    let apply_items: Vec<(String, String)> = final_plans
        .iter()
        .map(|plan| {
            (
                plan.integration.display_name().to_owned(),
                plan.spec.config_target.path.display().to_string(),
            )
        })
        .collect();

    if !command.assume_yes {
        let response = boxed_apply_summary(out, &apply_items, default_yes, input)?;
        if response.eq_ignore_ascii_case("n") || response.eq_ignore_ascii_case("no") {
            writeln!(out, "\nCancelled.")?;
            return Ok(());
        }
    }

    writeln!(out)?;
    let mut had_errors = false;
    for plan in final_plans {
        match ensure_prerequisites(plan.integration, command.assume_yes, out, input) {
            Ok(PrerequisiteStatus::Ok) | Ok(PrerequisiteStatus::Warning(_)) => {}
            Ok(PrerequisiteStatus::Skipped) => {
                writeln!(
                    out,
                    "Skipping {} — mcp-remote installation declined.",
                    plan.integration.display_name()
                )?;
                continue;
            }
            Err(e) => {
                writeln!(
                    out,
                    "Skipping {} — prerequisite check failed: {e}",
                    plan.integration.display_name()
                )?;
                had_errors = true;
                continue;
            }
        }
        match crate::integrations::apply::apply_setup_with_environment(
            plan.integration,
            plan.url.clone(),
            platform,
            &env,
        ) {
            Ok(result) => {
                writeln!(
                    out,
                    "{} -> {} ({})",
                    plan.integration.display_name(),
                    result.path.display(),
                    if result.changed {
                        "updated"
                    } else {
                        "unchanged"
                    }
                )?;
            }
            Err(e) => {
                writeln!(
                    out,
                    "Skipping {} — apply failed: {e}",
                    plan.integration.display_name()
                )?;
                had_errors = true;
            }
        }
    }

    if had_errors {
        writeln!(
            out,
            "\nSome integrations could not be configured. See warnings above."
        )?;
    }
    Ok(())
}

/// Interactive checkbox selector using raw terminal mode.
///
/// Displays each integration with a `[x]`/`[ ]` checkbox and a `>` cursor.
/// The user navigates with ↑/↓ (or k/j), toggles with Space, selects all
/// with `a`, deselects all with `n`, and confirms with Enter.
fn interactive_checkbox_select(
    catalog: &SetupCatalogPlan,
    out: &mut dyn Write,
) -> io::Result<Vec<IntegrationKind>> {
    use crossterm::{
        event::{self, Event, KeyCode, KeyModifiers},
        terminal,
    };

    let integrations: Vec<&SetupPlan> = catalog.integrations.iter().collect();
    if integrations.is_empty() {
        return Ok(Vec::new());
    }

    let mut checked: Vec<bool> = integrations
        .iter()
        .map(|p| p.detection_status == DetectionStatus::InstalledOrUsed)
        .collect();
    let mut cursor_idx: usize = 0;
    let n = integrations.len();

    writeln!(out, "Select integrations to configure:")?;
    writeln!(out)?;
    out.flush()?;

    terminal::enable_raw_mode()?;
    let _ = write!(out, "\x1b[?25l"); // hide cursor

    let result: io::Result<Vec<IntegrationKind>> = (|| {
        draw_checkbox_list(out, &integrations, &checked, cursor_idx)?;

        loop {
            if let Event::Key(key) = event::read()? {
                match (key.code, key.modifiers) {
                    (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                        return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
                    }
                    (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                        cursor_idx = cursor_idx.saturating_sub(1);
                    }
                    (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                        if cursor_idx + 1 < n {
                            cursor_idx += 1;
                        }
                    }
                    (KeyCode::Char(' '), _) => {
                        checked[cursor_idx] = !checked[cursor_idx];
                    }
                    (KeyCode::Char('a' | 'A'), _) => {
                        checked.iter_mut().for_each(|c| *c = true);
                    }
                    (KeyCode::Char('n' | 'N'), _) => {
                        checked.iter_mut().for_each(|c| *c = false);
                    }
                    (KeyCode::Enter, _) => break,
                    (KeyCode::Esc, _) => {
                        // Restore defaults on Escape
                        for (i, p) in integrations.iter().enumerate() {
                            checked[i] = p.detection_status == DetectionStatus::InstalledOrUsed;
                        }
                        break;
                    }
                    _ => continue,
                }
                // Redraw: n integration rows + 1 hint row
                write!(out, "\x1b[{}A", n + 1)?; // move cursor up
                draw_checkbox_list(out, &integrations, &checked, cursor_idx)?;
            }
        }

        Ok(integrations
            .iter()
            .zip(checked.iter())
            .filter(|(_, &c)| c)
            .map(|(p, _)| p.integration)
            .collect())
    })();

    let _ = write!(out, "\x1b[?25h"); // show cursor
    let _ = terminal::disable_raw_mode();
    writeln!(out)?;

    result
}

/// Render the checkbox list in raw-mode style (`\r\n` line endings).
fn draw_checkbox_list(
    out: &mut dyn Write,
    integrations: &[&SetupPlan],
    checked: &[bool],
    cursor_idx: usize,
) -> io::Result<()> {
    const BOLD: &str = "\x1b[1m";
    const DIM: &str = "\x1b[2m";
    const RESET: &str = "\x1b[0m";

    for (i, (plan, &is_checked)) in integrations.iter().zip(checked.iter()).enumerate() {
        let arrow = if i == cursor_idx { ">" } else { " " };
        let checkbox = if is_checked { "[x]" } else { "[ ]" };
        let (pre, post) = if i == cursor_idx {
            (BOLD, RESET)
        } else {
            ("", "")
        };
        write!(
            out,
            "  {}{} {}  {:<20} {:<18} {}{}\r\n",
            pre,
            arrow,
            checkbox,
            plan.integration.display_name(),
            plan.detection_status.as_str(),
            plan.spec.config_target.path.display(),
            post,
        )?;
    }
    write!(
        out,
        "{}  ↑/↓: move   Space: toggle   a: all   n: none   Enter: confirm{}\r\n",
        DIM, RESET
    )?;
    out.flush()
}

fn render_catalog_summary(catalog: &SetupCatalogPlan, out: &mut dyn Write) -> io::Result<()> {
    writeln!(out, "Detected integrations:")?;
    for plan in &catalog.integrations {
        let checkbox = if plan.detection_status == DetectionStatus::InstalledOrUsed {
            "[x]"
        } else {
            "[ ]"
        };
        writeln!(
            out,
            "  {}  {:<20} {:<18} {}",
            checkbox,
            plan.integration.display_name(),
            plan.detection_status.as_str(),
            plan.spec.config_target.path.display()
        )?;
    }
    Ok(())
}

fn default_selections(catalog: &SetupCatalogPlan) -> Vec<IntegrationKind> {
    catalog
        .integrations
        .iter()
        .filter(|plan| plan.detection_status == DetectionStatus::InstalledOrUsed)
        .map(|plan| plan.integration)
        .collect()
}

fn resolve_selections(catalog: &SetupCatalogPlan, raw: &str) -> io::Result<Vec<IntegrationKind>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(default_selections(catalog));
    }
    if trimmed.eq_ignore_ascii_case("none") {
        return Ok(Vec::new());
    }
    if trimmed.eq_ignore_ascii_case("all") {
        return Ok(catalog
            .integrations
            .iter()
            .map(|plan| plan.integration)
            .collect());
    }

    let mut selections = Vec::new();
    for part in trimmed.split(',') {
        let Some(selection) = parse_selection(part.trim()) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid selection '{}'", part.trim()),
            ));
        };
        if !selections.contains(&selection) {
            selections.push(selection);
        }
    }
    Ok(selections)
}

fn parse_selection(value: &str) -> Option<IntegrationKind> {
    if let Ok(index) = value.trim().parse::<usize>() {
        if index == 0 {
            return None;
        }
        return IntegrationKind::ALL.get(index - 1).copied();
    }

    parse_integration(value)
}
