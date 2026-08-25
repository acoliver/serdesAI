//! Special commands: /paste, /motd, /tools, /tutorial, /wiggum, /wiggum_stop, /generate-pr-description

use crate::bus;
use crate::clipboard;
use crate::commands::registry::{CommandCategory, CommandResult};
use crate::register_command;
use crate::tui::{mark_tutorial_complete, run_tutorial_wizard, TutorialResult};
use crate::wiggum;

const MOTD_CONTENT: &str = concat!(
    "╔══════════════════════════════════════════════════════════════╗\n",
    "║   Welcome to SerdesAI CLI                                  ║\n",
    "╠══════════════════════════════════════════════════════════════╣\n",
    "║  Version: ",
    env!("CARGO_PKG_VERSION"),
    "                                           ║\n",
    "║  Quick tips:                                                 ║\n",
    "║   • /help         Show all commands                         ║\n",
    "║   • @file         Attach file to your prompt                ║\n",
    "║   • /tools        Show tool capabilities                    ║\n",
    "║   • /paste        Attach clipboard image                    ║\n",
    "╠══════════════════════════════════════════════════════════════╣\n",
    "║  Need ideas? Ask: \"review my current branch changes\"         ║\n",
    "╚══════════════════════════════════════════════════════════════╝\n"
);

const TOOLS_CONTENT: &str = r#"# Available Tools & Capabilities

## File Operations
- **list_files** - List files and directories (recursive-aware)
- **read_file** - Read files with optional line ranges
- **edit_file** - Safe create/replace/delete snippet operations
- **delete_file** - Delete files with operation reporting

## Search & Discovery
- **grep** - Fast text search powered by ripgrep

## Shell & Execution
- **run_shell_command** - Run shell commands with timeout control

## Reasoning & Planning
- **share_your_reasoning** - Explicitly show plan and next steps

## Agent Orchestration
- **list_agents** - Discover available sub-agents
- **invoke_agent** - Delegate work to a selected sub-agent

## User Interaction
- **ask_user_question** - Structured interactive question flow

## Skills
- **list_or_search_skills** - Discover skills by query
- **activate_skill** - Load and apply a skill playbook
"#;

const PR_DESCRIPTION_PROMPT: &str = r#"Generate a comprehensive PR description for my current branch changes. Follow these steps:

## Step 1: Discover the changes
Use git CLI to:
- Find the base branch (usually main/master/develop)
- Get the list of changed files: `git diff --name-only BASE_BRANCH..HEAD`
- Get commit history: `git log BASE_BRANCH..HEAD --oneline`
- Get diffs: `git diff BASE_BRANCH..HEAD`

## Step 2: Analyze the code
Read and analyze all modified files to understand:
- What functionality was added/changed/removed
- The technical approach and implementation details
- Any architectural or design pattern changes
- Dependencies added/removed/updated

## Step 3: Generate structured PR description
Create sections:
- **Title**: Concise, descriptive title (50 chars max)
- **Summary**: Brief overview of what this PR accomplishes
- **Changes Made**: Detailed bullet points of specific changes
- **Technical Details**: Implementation approach, design decisions, patterns used
- **Files Modified**: List of key files with brief description of changes
- **Testing**: What was tested and how (if applicable)
- **Breaking Changes**: Any breaking changes (if applicable)
- **Additional Notes**: Any other relevant information

## Step 4: Create markdown file
Generate PR_DESCRIPTION.md with proper GitHub markdown formatting.

## Step 5: Update PR (optional)
If `gh` CLI is installed and authenticated, find the PR for current branch and update it directly, then delete PR_DESCRIPTION.md.

## Working Directory
{directory_context}

Proceed with the analysis and create the PR description."#;

pub fn init() {
    // /paste - Paste from clipboard
    register_command!(
        name = "paste",
        description = "Paste image from clipboard",
        usage = "/paste",
        aliases = ["clipboard", "cb"],
        category = CommandCategory::Core,
        handler = handle_paste
    )
    .ok();

    // /motd - Message of the day
    register_command!(
        name = "motd",
        description = "Show message of the day",
        usage = "/motd",
        aliases = [],
        category = CommandCategory::Core,
        handler = handle_motd
    )
    .ok();

    // /tools - Show tools (enhanced)
    register_command!(
        name = "tools",
        description = "Show available tools and capabilities",
        usage = "/tools",
        aliases = [],
        category = CommandCategory::Tools,
        handler = handle_tools
    )
    .ok();

    // /wiggum - Loop mode
    register_command!(
        name = "wiggum",
        description = "Start wiggum loop mode (auto re-prompt)",
        usage = "/wiggum <prompt>",
        aliases = [],
        category = CommandCategory::Core,
        handler = handle_wiggum
    )
    .ok();

    // /wiggum_stop - Stop loop mode
    register_command!(
        name = "wiggum_stop",
        description = "Stop wiggum loop mode",
        usage = "/wiggum_stop",
        aliases = ["stopwiggum", "ws"],
        category = CommandCategory::Core,
        handler = handle_wiggum_stop
    )
    .ok();

    // /tutorial - Launch onboarding tutorial wizard
    register_command!(
        name = "tutorial",
        description = "Run onboarding tutorial wizard",
        usage = "/tutorial",
        aliases = [],
        category = CommandCategory::Core,
        handler = handle_tutorial
    )
    .ok();

    // /generate-pr-description - Generate PR description
    register_command!(
        name = "generate-pr-description",
        description = "Generate comprehensive PR description",
        usage = "/generate-pr-description [@dir]",
        aliases = ["pr"],
        category = CommandCategory::Core,
        handler = handle_generate_pr_description
    )
    .ok();
}

fn handle_paste(_cmd: &str) -> CommandResult {
    if !clipboard::has_image_in_clipboard() {
        bus::emit_warning("No image found in clipboard".to_string());
        bus::emit_info("Copy an image and try again".to_string());
        return CommandResult::Handled;
    }

    match clipboard::capture_clipboard_image_to_pending() {
        Some(placeholder) => {
            let count = clipboard::get_pending_count();
            bus::emit_success(format!("Captured: {}", placeholder));
            bus::emit_info(format!("Total pending images: {}", count));
        }
        None => {
            bus::emit_warning("Failed to capture clipboard image".to_string());
        }
    }

    CommandResult::Handled
}

fn handle_motd(_cmd: &str) -> CommandResult {
    bus::emit_info(MOTD_CONTENT.to_string());
    CommandResult::Handled
}

fn handle_tools(_cmd: &str) -> CommandResult {
    bus::emit_info(TOOLS_CONTENT.to_string());
    CommandResult::Handled
}

fn handle_wiggum(cmd: &str) -> CommandResult {
    let prompt = command_arg(cmd);
    if prompt.is_empty() {
        bus::emit_warning("Usage: /wiggum <prompt>".to_string());
        return CommandResult::Handled;
    }

    wiggum::start_wiggum(prompt);

    bus::emit_success("Wiggum loop mode activated".to_string());
    bus::emit_info("Use /wiggum_stop to end loop mode".to_string());

    CommandResult::Prompt(prompt.to_string())
}

fn handle_wiggum_stop(_cmd: &str) -> CommandResult {
    wiggum::stop_wiggum();

    bus::emit_success("Wiggum loop mode stopped".to_string());
    CommandResult::Handled
}

fn handle_tutorial(_cmd: &str) -> CommandResult {
    match run_tutorial_wizard() {
        Ok(TutorialResult::Completed) => {
            mark_tutorial_complete();
            bus::emit_success("Tutorial complete! Welcome to Serdes AI!".to_string());
        }
        Ok(TutorialResult::Skipped) => {
            bus::emit_info("Tutorial skipped. Run /tutorial anytime!".to_string());
        }
        Err(e) => {
            bus::emit_error(format!("Tutorial failed: {}", e));
        }
    }
    CommandResult::Handled
}

fn handle_generate_pr_description(cmd: &str) -> CommandResult {
    // Parse optional @dir argument
    let tokens: Vec<&str> = cmd.split_whitespace().collect();
    let mut directory_context = String::new();

    for token in &tokens {
        if let Some(dir) = token.strip_prefix('@') {
            directory_context = format!("\nWorking directory: {}\n", dir);

            // Change to that directory if valid
            if std::path::Path::new(dir).is_dir() {
                if let Err(e) = std::env::set_current_dir(dir) {
                    bus::emit_warning(format!("Could not change to directory '{}': {}", dir, e));
                }
            }
        }
    }

    // Construct the prompt with directory context
    let prompt = PR_DESCRIPTION_PROMPT.replace("{directory_context}", &directory_context);

    // Return the prompt as a CommandResult::Prompt
    // This will be processed by the agent
    bus::emit_info("Generating PR description...".to_string());
    bus::emit_info(
        "The agent will analyze your git changes and create a PR description.".to_string(),
    );

    CommandResult::Prompt(prompt)
}

fn command_arg(cmd: &str) -> &str {
    let trimmed = cmd.trim();
    let Some((_, rest)) = trimmed.split_once(char::is_whitespace) else {
        return "";
    };

    rest.trim()
}
