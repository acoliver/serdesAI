use clap::{ArgAction, CommandFactory, Parser};

/// SerdesAI CLI arguments.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "serdes-ai",
    about = "SerdesAI - A code generation agent",
    version = env!("CARGO_PKG_VERSION"),
    author = env!("CARGO_PKG_AUTHORS"),
    disable_version_flag = true,
    after_help = "Examples:\n  serdes-ai -i\n      Run in interactive mode\n\n  serdes-ai -p \"refactor this function\"\n      Execute a single prompt and exit\n\n  serdes-ai -a code-puppy -m gpt-5 -p \"write tests\"\n      Select agent/model and run one prompt\n\n  serdes-ai fix failing tests\n      Deprecated positional command form (use -p instead)",
)]
pub struct Cli {
    /// Show version and exit
    #[arg(short = 'v', long = "version", action = ArgAction::Version)]
    pub version: (),

    /// Run in interactive mode
    #[arg(short = 'i', long = "interactive")]
    pub interactive: bool,

    /// Execute a single prompt and exit (no interactive mode)
    #[arg(short = 'p', long = "prompt", value_name = "TEXT")]
    pub prompt: Option<String>,

    /// Specify which agent to use (e.g., --agent code-puppy)
    #[arg(short = 'a', long = "agent", value_name = "NAME")]
    pub agent: Option<String>,

    /// Specify which model to use (e.g., --model gpt-5)
    #[arg(short = 'm', long = "model", value_name = "NAME")]
    pub model: Option<String>,

    /// Run a single command (deprecated, use -p instead)
    #[arg(value_name = "COMMAND", num_args = 0..)]
    pub command: Vec<String>,
}

impl Cli {
    /// Parse CLI arguments from environment
    pub fn parse_args() -> Self {
        let mut cli = Self::parse();
        cli.normalize();
        cli.warn_if_deprecated_command_used();
        cli
    }

    /// Check if running in prompt-only mode
    pub fn is_prompt_only(&self) -> bool {
        self.prompt.is_some()
    }

    /// Check if running in interactive mode
    pub fn is_interactive(&self) -> bool {
        self.interactive || (!self.is_prompt_only() && self.command.is_empty())
    }

    /// Get the initial command/prompt if any
    pub fn get_initial_prompt(&self) -> Option<String> {
        if let Some(ref prompt) = self.prompt {
            return Some(prompt.clone());
        }
        if !self.command.is_empty() {
            return Some(self.command.join(" "));
        }
        None
    }

    /// Get model name if specified
    pub fn get_model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Get agent name if specified
    pub fn get_agent(&self) -> Option<&str> {
        self.agent.as_deref()
    }

    /// Build clap command metadata (useful for tests/help customization).
    pub fn command() -> clap::Command {
        <Self as CommandFactory>::command()
    }

    fn normalize(&mut self) {
        self.prompt = trim_option(self.prompt.take());
        self.agent = trim_option(self.agent.take());
        self.model = trim_option(self.model.take());
    }

    fn warn_if_deprecated_command_used(&self) {
        if !self.command.is_empty() && self.prompt.is_none() {
            eprintln!(
                "Warning: positional COMMAND arguments are deprecated; use --prompt/-p instead."
            );
        }
    }
}

fn trim_option(value: Option<String>) -> Option<String> {
    value.and_then(|s| {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::Parser;

    #[test]
    fn interactive_defaults_when_no_prompt_or_command() {
        let cli = Cli::parse_from(["serdes-ai"]);
        assert!(cli.is_interactive());
        assert!(!cli.is_prompt_only());
    }

    #[test]
    fn prompt_mode_detected() {
        let mut cli = Cli::parse_from(["serdes-ai", "-p", "  hello  "]);
        cli.normalize();
        assert!(cli.is_prompt_only());
        assert_eq!(cli.get_initial_prompt().as_deref(), Some("hello"));
        assert!(!cli.is_interactive());
    }

    #[test]
    fn legacy_command_builds_initial_prompt() {
        let cli = Cli::parse_from(["serdes-ai", "fix", "the", "tests"]);
        assert_eq!(cli.get_initial_prompt().as_deref(), Some("fix the tests"));
        assert!(!cli.is_interactive());
    }

    #[test]
    fn trims_agent_and_model() {
        let mut cli = Cli::parse_from(["serdes-ai", "-a", "  code-puppy  ", "-m", "  gpt-5  "]);
        cli.normalize();
        assert_eq!(cli.get_agent(), Some("code-puppy"));
        assert_eq!(cli.get_model(), Some("gpt-5"));
    }

    #[test]
    fn empty_trimmed_values_become_none() {
        let mut cli = Cli::parse_from(["serdes-ai", "-a", "   ", "-m", "\t"]);
        cli.normalize();
        assert_eq!(cli.get_agent(), None);
        assert_eq!(cli.get_model(), None);
    }
}
