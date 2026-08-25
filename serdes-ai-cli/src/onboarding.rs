//! Onboarding wizard for first-time users

use anyhow::Result;
use std::io::{self, Write};

use crate::bus;
use crate::config;
use crate::terminal;

const ONBOARDING_MARKER: &str = "onboarding_complete";

/// Run onboarding if not already completed
pub async fn maybe_run_onboarding() -> Result<()> {
    let marker = config::get_state_dir().join(ONBOARDING_MARKER);

    if marker.exists() {
        return Ok(());
    }

    run_onboarding_wizard().await?;

    // Mark as complete
    std::fs::create_dir_all(config::get_state_dir())?;
    std::fs::write(&marker, b"1")?;

    Ok(())
}

async fn run_onboarding_wizard() -> Result<()> {
    terminal::clear_screen();

    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║                                                              ║");
    println!("║  Welcome to SerdesAI CLI                                     ║");
    println!("║                                                              ║");
    println!("║  Your AI-powered coding assistant                            ║");
    println!("║                                                              ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();

    // Step 1: API Key setup
    println!("Step 1: API Key Configuration");
    println!("─────────────────────────────────");
    println!("SerdesAI needs an API key to communicate with AI providers.");
    println!();

    let providers = vec![
        (
            "OpenAI",
            "OPENAI_API_KEY",
            "https://platform.openai.com/api-keys",
        ),
        (
            "Anthropic (Claude)",
            "ANTHROPIC_API_KEY",
            "https://console.anthropic.com/settings/keys",
        ),
        ("Groq", "GROQ_API_KEY", "https://console.groq.com/keys"),
    ];

    for (name, env_var, url) in &providers {
        if std::env::var(env_var).is_ok() {
            println!("  {} API key found", name);
        } else {
            println!("  {} API key not set", name);
            println!("     Get one at: {}", url);
            println!("     Then run: export {}=<your-key>", env_var);
        }
    }

    println!();
    wait_for_enter("Press Enter to continue...").await?;

    // Step 2: Basic commands
    terminal::clear_screen();
    println!("Step 2: Basic Commands");
    println!("─────────────────────────────────");
    println!();
    println!("  /help          - Show all available commands");
    println!("  /model         - Show or change the AI model");
    println!("  /agent         - Show or change the agent");
    println!("  /clear         - Clear conversation history");
    println!("  /exit          - Exit the application");
    println!();
    println!("  @filename      - Attach a file to your prompt");
    println!("  @/path/to/dir  - Attach a directory");
    println!();

    wait_for_enter("Press Enter to continue...").await?;

    // Step 3: Tips
    terminal::clear_screen();
    println!("Step 3: Pro Tips");
    println!("─────────────────────────────────");
    println!();
    println!("  Use Tab for command completion");
    println!("  Press Alt+M or F2 for multiline mode");
    println!("  Use @ to attach files: \"@main.rs explain this code\"");
    println!("  The agent can use tools: web_search, web_fetch, read_file, etc.");
    println!("  Press Ctrl+C to cancel, Ctrl+D to exit");
    println!();

    wait_for_enter("Press Enter to start using SerdesAI!").await?;

    terminal::clear_screen();

    bus::emit_info("Onboarding complete! Type /help anytime for help.".to_string());

    Ok(())
}

async fn wait_for_enter(prompt: &str) -> io::Result<()> {
    print!("{}", prompt);
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    Ok(())
}
