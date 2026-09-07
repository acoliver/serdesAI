//! Inline picker widgets (replacing full-screen ratatui pickers).

use std::io::{self, Write};

use crossterm::{
    ExecutableCommand, QueueableCommand, cursor,
    event::{self, Event, KeyCode, KeyEventKind},
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{self, Clear, ClearType},
};

const MAX_VISIBLE_ITEMS: usize = 10;

/// Agent info for picker
#[derive(Clone)]
pub struct AgentInfo {
    pub name: String,
    pub description: String,
}

/// Inline model picker.
///
/// Appears inline in terminal output, redraws only the picker region,
/// and returns selected model or `None` if cancelled.
pub fn pick_model_inline(models: &[String], current: Option<&str>) -> io::Result<Option<String>> {
    if models.is_empty() {
        return Ok(None);
    }

    let mut stdout = io::stdout();
    let mut selected = initial_selected(models, current);
    let mut filter = String::new();

    terminal::enable_raw_mode()?;

    // Header printed once; body is redrawn beneath this line.
    stdout.execute(SetForegroundColor(Color::Cyan))?;
    stdout.execute(Print(
        "Select a model (↑/↓, Enter=choose, Esc=cancel, type=filter):\r\n",
    ))?;
    stdout.execute(ResetColor)?;

    let (_, start_row) = cursor::position()?;

    loop {
        let filtered = filtered_models(models, &filter);
        selected = selected.min(filtered.len().saturating_sub(1));

        render_picker_body(&mut stdout, start_row, &filter, &filtered, selected)?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            match key.code {
                KeyCode::Enter => {
                    terminal::disable_raw_mode()?;
                    if let Some((_, model)) = filtered.get(selected) {
                        stdout.queue(Print("\r\n"))?;
                        stdout.flush()?;
                        return Ok(Some((*model).clone()));
                    }
                    stdout.queue(Print("\r\n"))?;
                    stdout.flush()?;
                    return Ok(None);
                }
                KeyCode::Esc => {
                    terminal::disable_raw_mode()?;
                    stdout.queue(Print("\r\n"))?;
                    stdout.flush()?;
                    return Ok(None);
                }
                KeyCode::Up => {
                    selected = selected.saturating_sub(1);
                }
                KeyCode::Down => {
                    let max_idx = filtered.len().saturating_sub(1);
                    selected = (selected + 1).min(max_idx);
                }
                KeyCode::Char(c) => {
                    filter.push(c);
                    selected = 0;
                }
                KeyCode::Backspace => {
                    filter.pop();
                    selected = 0;
                }
                _ => {}
            }
        }
    }
}

fn render_picker_body(
    stdout: &mut io::Stdout,
    start_row: u16,
    filter: &str,
    filtered: &[(usize, &String)],
    selected: usize,
) -> io::Result<()> {
    stdout.queue(cursor::MoveTo(0, start_row))?;
    stdout.queue(Clear(ClearType::FromCursorDown))?;

    if !filter.is_empty() {
        stdout.queue(SetForegroundColor(Color::DarkGrey))?;
        stdout.queue(Print(format!("Filter: {filter}\r\n")))?;
        stdout.queue(ResetColor)?;
    }

    let visible_count = filtered.len().min(MAX_VISIBLE_ITEMS);

    if visible_count == 0 {
        stdout.queue(SetForegroundColor(Color::Yellow))?;
        stdout.queue(Print("No matching models. Keep typing or backspace.\r\n"))?;
        stdout.queue(ResetColor)?;
        stdout.flush()?;
        return Ok(());
    }

    for (row, (_, model)) in filtered.iter().take(visible_count).enumerate() {
        if row == selected {
            stdout.queue(SetForegroundColor(Color::Green))?;
            stdout.queue(Print(format!("> {model}\r\n")))?;
            stdout.queue(ResetColor)?;
        } else {
            stdout.queue(Print(format!("  {model}\r\n")))?;
        }
    }

    if filtered.len() > MAX_VISIBLE_ITEMS {
        stdout.queue(SetForegroundColor(Color::DarkGrey))?;
        stdout.queue(Print(format!(
            "  ... and {} more\r\n",
            filtered.len() - MAX_VISIBLE_ITEMS
        )))?;
        stdout.queue(ResetColor)?;
    }

    stdout.flush()
}

fn filtered_models<'a>(models: &'a [String], filter: &str) -> Vec<(usize, &'a String)> {
    let needle = filter.to_ascii_lowercase();

    models
        .iter()
        .enumerate()
        .filter(|(_, model)| model.to_ascii_lowercase().contains(&needle))
        .collect()
}

fn initial_selected(models: &[String], current: Option<&str>) -> usize {
    current
        .and_then(|name| models.iter().position(|m| m == name))
        .unwrap_or(0)
}

/// Inline agent picker.
///
/// Appears inline in terminal output, redraws only the picker region,
/// and returns selected agent name or `None` if cancelled.
pub fn pick_agent_inline(
    agents: &[AgentInfo],
    current: Option<&str>,
) -> io::Result<Option<String>> {
    if agents.is_empty() {
        return Ok(None);
    }

    let mut stdout = io::stdout();
    let mut selected = current
        .and_then(|curr| agents.iter().position(|a| a.name == curr))
        .unwrap_or(0);
    let mut filter = String::new();

    terminal::enable_raw_mode()?;

    stdout.execute(SetForegroundColor(Color::Cyan))?;
    stdout.execute(Print(
        "Select an agent (↑/↓, Enter=choose, Esc=cancel, type=filter):\r\n",
    ))?;
    stdout.execute(ResetColor)?;

    let (_, start_row) = cursor::position()?;

    loop {
        let filtered = filtered_agents(agents, &filter);
        selected = selected.min(filtered.len().saturating_sub(1));

        render_agent_picker_body(&mut stdout, start_row, &filter, &filtered, selected)?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            match key.code {
                KeyCode::Enter => {
                    terminal::disable_raw_mode()?;
                    if let Some((_, agent)) = filtered.get(selected) {
                        stdout.queue(Print("\r\n"))?;
                        stdout.flush()?;
                        return Ok(Some(agent.name.clone()));
                    }
                    stdout.queue(Print("\r\n"))?;
                    stdout.flush()?;
                    return Ok(None);
                }
                KeyCode::Esc => {
                    terminal::disable_raw_mode()?;
                    stdout.queue(Print("\r\n"))?;
                    stdout.flush()?;
                    return Ok(None);
                }
                KeyCode::Up => {
                    selected = selected.saturating_sub(1);
                }
                KeyCode::Down => {
                    let max_idx = filtered.len().saturating_sub(1);
                    selected = (selected + 1).min(max_idx);
                }
                KeyCode::Char(c) => {
                    filter.push(c);
                    selected = 0;
                }
                KeyCode::Backspace => {
                    filter.pop();
                    selected = 0;
                }
                _ => {}
            }
        }
    }
}

fn render_agent_picker_body(
    stdout: &mut io::Stdout,
    start_row: u16,
    filter: &str,
    filtered: &[(usize, &AgentInfo)],
    selected: usize,
) -> io::Result<()> {
    stdout.queue(cursor::MoveTo(0, start_row))?;
    stdout.queue(Clear(ClearType::FromCursorDown))?;

    if !filter.is_empty() {
        stdout.queue(SetForegroundColor(Color::DarkGrey))?;
        stdout.queue(Print(format!("Filter: {filter}\r\n")))?;
        stdout.queue(ResetColor)?;
    }

    let visible_count = filtered.len().min(MAX_VISIBLE_ITEMS);

    if visible_count == 0 {
        stdout.queue(SetForegroundColor(Color::Yellow))?;
        stdout.queue(Print("No matching agents. Keep typing or backspace.\r\n"))?;
        stdout.queue(ResetColor)?;
        stdout.flush()?;
        return Ok(());
    }

    for (row, (_, agent)) in filtered.iter().take(visible_count).enumerate() {
        if row == selected {
            stdout.queue(SetForegroundColor(Color::Green))?;
            stdout.queue(Print(format!(
                "> {:<20} {}\r\n",
                agent.name, agent.description
            )))?;
            stdout.queue(ResetColor)?;
        } else {
            stdout.queue(Print(format!(
                "  {:<20} {}\r\n",
                agent.name, agent.description
            )))?;
        }
    }

    if filtered.len() > MAX_VISIBLE_ITEMS {
        stdout.queue(SetForegroundColor(Color::DarkGrey))?;
        stdout.queue(Print(format!(
            "  ... and {} more\r\n",
            filtered.len() - MAX_VISIBLE_ITEMS
        )))?;
        stdout.queue(ResetColor)?;
    }

    stdout.flush()
}

fn filtered_agents<'a>(agents: &'a [AgentInfo], filter: &str) -> Vec<(usize, &'a AgentInfo)> {
    let needle = filter.to_ascii_lowercase();

    agents
        .iter()
        .enumerate()
        .filter(|(_, agent)| {
            agent.name.to_ascii_lowercase().contains(&needle)
                || agent.description.to_ascii_lowercase().contains(&needle)
        })
        .collect()
}
