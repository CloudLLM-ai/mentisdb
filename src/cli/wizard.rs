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

/// Interactive checkbox selector using ratatui.
///
/// Displays each integration with a `[x]`/`[ ]` checkbox, integration name,
/// detection status, and config target path. Navigation with ↑/↓ (or k/j),
/// Space to toggle, Enter to confirm, Esc to restore defaults and continue,
/// a/all to select all, n/none to deselect all.
fn interactive_checkbox_select(
    catalog: &SetupCatalogPlan,
    out: &mut dyn Write,
) -> io::Result<Vec<IntegrationKind>> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind};
    use crossterm::terminal;
    use ratatui::{
        backend::CrosstermBackend,
        layout::{Constraint, Direction, Layout},
        style::{Color, Modifier, Style},
        text::{Line, Span},
        widgets::{Block, Borders, List, ListState, Paragraph, Scrollbar, ScrollbarOrientation},
        Terminal,
    };

    let integrations: Vec<&SetupPlan> = catalog.integrations.iter().collect();
    if integrations.is_empty() {
        return Ok(Vec::new());
    }

    let display_names: Vec<String> = integrations
        .iter()
        .map(|p| p.integration.display_name().to_string())
        .collect();
    let max_name = display_names.iter().map(|n| n.len()).max().unwrap_or(0);

    let mut checked: Vec<bool> = integrations
        .iter()
        .map(|p| p.detection_status == DetectionStatus::InstalledOrUsed)
        .collect();
    let mut list_state = ListState::default().with_selected(Some(0));

    terminal::enable_raw_mode()?;
    crossterm::execute!(io::stdout(), crossterm::terminal::EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let result: io::Result<Vec<IntegrationKind>> = (|| {
        loop {
            terminal.draw(|frame| {
                let area = frame.area();
                let narrow = area.width < 60;

                let items: Vec<Line> = integrations
                    .iter()
                    .zip(display_names.iter())
                    .zip(checked.iter())
                    .map(|((plan, name), &is_checked)| {
                        let checkbox = if is_checked { "[x]" } else { "[ ]" };
                        let padded_name =
                            format!("{:<width$}", name, width = max_name);
                        let status = plan.detection_status.as_str();
                        let status_style = match plan.detection_status {
                            DetectionStatus::Configured => Style::new().fg(Color::Green),
                            DetectionStatus::InstalledOrUsed => Style::new().fg(Color::Yellow),
                            _ => Style::new().fg(Color::Gray),
                        };
                        if narrow {
                            Line::from(vec![
                                Span::raw(checkbox),
                                Span::raw(" "),
                                Span::raw(padded_name),
                                Span::raw(" "),
                                Span::styled(status.to_string(), status_style),
                            ])
                        } else {
                            let path = plan.spec.config_target.path.display().to_string();
                            let used = 3 // checkbox + space
                                + padded_name.len()
                                + 1 // space
                                + status.len()
                                + 1; // space before path
                            let avail = (area.width as usize).saturating_sub(used);
                            let truncated = if path.len() > avail && avail > 2 {
                                format!("{}…", &path[..avail - 1])
                            } else if avail <= 2 {
                                String::new()
                            } else {
                                path
                            };
                            Line::from(vec![
                                Span::raw(checkbox),
                                Span::raw(" "),
                                Span::raw(padded_name),
                                Span::raw(" "),
                                Span::styled(status.to_string(), status_style),
                                Span::raw(" "),
                                Span::styled(truncated, Style::new().fg(Color::DarkGray)),
                            ])
                        }
                    })
                    .collect();

                let selected = list_state.selected().unwrap_or(0);
                let count_selected = checked.iter().filter(|&&c| c).count();
                let total = checked.len();
                let items_len = items.len();

                let header_text = format!(" {} of {} selected", count_selected, total);

                let instructions = "↑/↓/k/j: move  Space: toggle  a: all  n: none  Enter: confirm  Esc: reset defaults";
                let footer_lines = if area.width < 40 { 2u16 } else { 1u16 };
                let extra_rows = 1u16; // header line

                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(1 + extra_rows), // header
                        Constraint::Min(0),                 // list
                        Constraint::Length(footer_lines),   // instructions
                    ])
                    .split(area);

                // Header
                let header_block = Block::default()
                    .borders(Borders::TOP | Borders::LEFT | Borders::RIGHT)
                    .title(" Select integrations to configure ");
                frame.render_widget(header_block, chunks[0]);
                let header_para = Paragraph::new(Line::from(Span::styled(
                    header_text,
                    Style::new().fg(Color::Cyan),
                )));
                frame.render_widget(header_para, chunks[0]);

                // List
                let list = List::new(items)
                    .block(Block::default().borders(Borders::LEFT | Borders::RIGHT))
                    .highlight_style(
                        Style::new()
                            .fg(Color::Black)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("> ");
                let mut list_state = ListState::default().with_selected(Some(selected));
                frame.render_stateful_widget(list, chunks[1], &mut list_state);

                // Scrollbar
                let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(Some("▲"))
                    .end_symbol(Some("▼"));
                let mut scrollbar_state =
                    ratatui::widgets::ScrollbarState::new(items_len)
                        .position(selected);
                let scrollbar_area = ratatui::layout::Rect {
                    x: chunks[1].right().saturating_sub(1),
                    y: chunks[1].y,
                    width: 1,
                    height: chunks[1].height,
                };
                frame.render_stateful_widget(
                    scrollbar,
                    scrollbar_area,
                    &mut scrollbar_state,
                );

                // Footer instructions
                let footer_block = Block::default()
                    .borders(Borders::LEFT | Borders::RIGHT | Borders::BOTTOM);
                frame.render_widget(footer_block, chunks[2]);
                let footer_para = Paragraph::new(Line::from(Span::styled(
                    instructions,
                    Style::new().add_modifier(Modifier::DIM),
                )));
                frame.render_widget(footer_para, chunks[2]);
            })?;

            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let selected = list_state.selected().unwrap_or(0);
                let n = integrations.len();
                match key.code {
                    KeyCode::Char('c')
                        if key
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::CONTROL) =>
                    {
                        return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        list_state.select(Some(selected.saturating_sub(1)));
                    }
                    KeyCode::Down | KeyCode::Char('j') if selected + 1 < n => {
                        list_state.select(Some(selected + 1));
                    }
                    KeyCode::Char(' ') => {
                        checked[selected] = !checked[selected];
                    }
                    KeyCode::Char('a' | 'A') => {
                        checked.iter_mut().for_each(|c| *c = true);
                    }
                    KeyCode::Char('n' | 'N') => {
                        checked.iter_mut().for_each(|c| *c = false);
                    }
                    KeyCode::Enter => break,
                    KeyCode::Esc => {
                        for (i, p) in integrations.iter().enumerate() {
                            checked[i] = p.detection_status == DetectionStatus::InstalledOrUsed;
                        }
                        break;
                    }
                    _ => {}
                }
            }
        }

        Ok(integrations
            .iter()
            .zip(checked.iter())
            .filter(|(_, &c)| c)
            .map(|(p, _)| p.integration)
            .collect())
    })();

    terminal::disable_raw_mode()?;
    crossterm::execute!(io::stdout(), crossterm::terminal::LeaveAlternateScreen)?;
    let _ = crossterm::execute!(io::stdout(), crossterm::cursor::Show);
    writeln!(out)?;

    result
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
