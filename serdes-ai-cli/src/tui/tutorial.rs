//! Tutorial/Onboarding Wizard - Multi-step onboarding for new users

use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    Frame, Terminal,
    backend::Backend,
    layout::{Alignment, Constraint, Direction, Layout},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Tabs, Wrap},
};

use crate::config;
use crate::tui::theme::Theme;

/// Tutorial step
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TutorialStep {
    Welcome,
    ApiKeys,
    AgentSelection,
    ModelSelection,
    FirstPrompt,
    Complete,
}

/// Tutorial wizard state
pub struct TutorialWizard {
    step: TutorialStep,
    theme: Theme,
}

impl TutorialWizard {
    pub fn new() -> Self {
        Self {
            step: TutorialStep::Welcome,
            theme: Theme::default(),
        }
    }

    pub fn run<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> io::Result<TutorialResult> {
        loop {
            terminal.draw(|f| self.draw(f))?;

            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(TutorialResult::Skipped),
                    KeyCode::Char('n') | KeyCode::Right | KeyCode::Enter => self.next_step(),
                    KeyCode::Char('p') | KeyCode::Left => self.prev_step(),
                    KeyCode::Char('s') => return Ok(TutorialResult::Skipped),
                    _ => {}
                }

                if self.step == TutorialStep::Complete {
                    return Ok(TutorialResult::Completed);
                }
            }
        }
    }

    fn next_step(&mut self) {
        self.step = match self.step {
            TutorialStep::Welcome => TutorialStep::ApiKeys,
            TutorialStep::ApiKeys => TutorialStep::AgentSelection,
            TutorialStep::AgentSelection => TutorialStep::ModelSelection,
            TutorialStep::ModelSelection => TutorialStep::FirstPrompt,
            TutorialStep::FirstPrompt => TutorialStep::Complete,
            TutorialStep::Complete => TutorialStep::Complete,
        };
    }

    fn prev_step(&mut self) {
        self.step = match self.step {
            TutorialStep::Welcome => TutorialStep::Welcome,
            TutorialStep::ApiKeys => TutorialStep::Welcome,
            TutorialStep::AgentSelection => TutorialStep::ApiKeys,
            TutorialStep::ModelSelection => TutorialStep::AgentSelection,
            TutorialStep::FirstPrompt => TutorialStep::ModelSelection,
            TutorialStep::Complete => TutorialStep::FirstPrompt,
        };
    }

    fn draw(&self, frame: &mut Frame) {
        let area = frame.area();
        frame.render_widget(Clear, area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(2)
            .constraints([
                Constraint::Length(3), // Title
                Constraint::Length(3), // Progress tabs
                Constraint::Min(10),   // Content
                Constraint::Length(3), // Navigation
            ])
            .split(area);

        // Title
        let title = Paragraph::new("Serdes AI Tutorial")
            .style(self.theme.title_style())
            .alignment(Alignment::Center);
        frame.render_widget(title, chunks[0]);

        // Progress tabs
        let tabs = Tabs::new(vec![
            "1. Welcome".to_string(),
            "2. API Keys".to_string(),
            "3. Agent".to_string(),
            "4. Model".to_string(),
            "5. First Prompt".to_string(),
        ])
        .block(Block::default().borders(Borders::ALL))
        .select(match self.step {
            TutorialStep::Welcome => 0,
            TutorialStep::ApiKeys => 1,
            TutorialStep::AgentSelection => 2,
            TutorialStep::ModelSelection => 3,
            TutorialStep::FirstPrompt | TutorialStep::Complete => 4,
        })
        .style(self.theme.normal_style())
        .highlight_style(self.theme.selected_style());
        frame.render_widget(tabs, chunks[1]);

        // Content
        let content = self.render_content();
        frame.render_widget(content, chunks[2]);

        // Navigation
        let help = match self.step {
            TutorialStep::Welcome => "n/Enter: Next  s: Skip  q/Esc: Cancel",
            TutorialStep::Complete => "Press any key to finish",
            _ => "←/p: Previous  n/Enter: Next  s: Skip  q/Esc: Cancel",
        };
        let nav = Paragraph::new(help)
            .style(self.theme.muted_style())
            .alignment(Alignment::Center);
        frame.render_widget(nav, chunks[3]);
    }

    fn render_content(&self) -> Paragraph<'static> {
        let (title, body) = match self.step {
            TutorialStep::Welcome => self.render_welcome(),
            TutorialStep::ApiKeys => self.render_api_keys(),
            TutorialStep::AgentSelection => self.render_agent_selection(),
            TutorialStep::ModelSelection => self.render_model_selection(),
            TutorialStep::FirstPrompt => self.render_first_prompt(),
            TutorialStep::Complete => self.render_complete(),
        };

        let mut text = Text::default();
        text.push_line(Line::from(vec![Span::styled(
            title.clone(),
            self.theme.title_style(),
        )]));
        text.push_line(Line::from(""));
        text.push_line(Line::from(body));

        Paragraph::new(text)
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: true })
    }

    fn render_welcome(&self) -> (String, String) {
        let title = "Welcome to Serdes AI!".to_string();
        let body = r#"Welcome! This tutorial will walk you through:

1. Setting up your API keys
2. Choosing your first agent
3. Selecting a model
4. Running your first prompt

Serdes AI provides:
- Interactive TUIs for selection
- Session management with autosave
- Rich console rendering
- Multiple model support (OpenAI, Anthropic, etc.)

Press 'n' or Enter to continue, 's' to skip, or 'q' to exit."#
            .to_string();
        (title, body)
    }

    fn render_api_keys(&self) -> (String, String) {
        let title = "Step 1: API Keys".to_string();
        let body = r#"You'll need API keys to use language models.

Supported providers:
• OpenAI (GPT-4, GPT-4o) - https://platform.openai.com
• Anthropic (Claude) - https://console.anthropic.com
• Google (Gemini) - https://ai.google.dev

Set keys via environment variables:
  export OPENAI_API_KEY="sk-..."
  export ANTHROPIC_API_KEY="sk-..."Or run /tutorial again and select OAuth for browser-based auth.

Press 'n' to continue."#
            .to_string();
        (title, body)
    }

    fn render_agent_selection(&self) -> (String, String) {
        let title = "Step 2: Choose an Agent".to_string();
        let body = r#"Agents are specialized assistants for different tasks:

• newcode: General purpose coding assistant (default)
• code-reviewer: Focused on code review
• python-programmer: Python-specific expert
• javascript-reviewer: JavaScript specialist

Use /agent to switch agents anytime.

The default 'newcode' suits most tasks.

Press 'n' to continue."#
            .to_string();
        (title, body)
    }

    fn render_model_selection(&self) -> (String, String) {
        let title = "Step 3: Choose a Model".to_string();
        let body = r#"Models power your AI interactions:

Recommended models:
• gpt-4o: Best overall capability
• claude-3-opus: Excellent for long contexts
• gpt-4o-mini: Fast and cost-effective

Use /model to change models, or /model_settings to configure per-model settings.

Press 'n' to continue."#
            .to_string();
        (title, body)
    }

    fn render_first_prompt(&self) -> (String, String) {
        let title = "Step 4: Your First Prompt".to_string();
        let body = r#"You're all set! Try these commands:

• /help - Show all available commands
• /session - Check your session
• /compact - Compact conversation history
• Type any question or task for the AI

Example prompts:
  "Create a Python script to download images"
  "Review this code for bugs"
  "Explain how async works in Rust"Press 'n' to finish the tutorial!"#
            .to_string();
        (title, body)
    }

    fn render_complete(&self) -> (String, String) {
        let title = "Tutorial Complete!".to_string();
        let body = r#"You're ready to use Serdes AI!

Quick reference:
• /help - All commands
• /model - Change model
• /agent - Change agent
• /session - Session info
• Ctrl+C - Cancel current operation

Happy coding! "#
            .to_string();
        (title, body)
    }
}

impl Default for TutorialWizard {
    fn default() -> Self {
        Self::new()
    }
}

/// Tutorial result
#[derive(Debug, Clone)]
pub enum TutorialResult {
    Completed,
    Skipped,
}

/// Helper function to run tutorial wizard
pub fn run_tutorial_wizard() -> io::Result<TutorialResult> {
    crate::tui::run_tui(|terminal| {
        let mut wizard = TutorialWizard::new();
        wizard.run(terminal)
    })
}

/// Check if tutorial should run (first time user)
pub fn should_run_tutorial() -> bool {
    // Check config if onboarding is complete
    !config::is_onboarding_complete()
}

/// Mark tutorial as complete
pub fn mark_tutorial_complete() {
    config::set_onboarding_complete(true);
}
