use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, OnceLock, RwLock};

/// Result of attempting to handle a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandResult {
    /// Command was handled successfully; continue main loop.
    Handled,
    /// Command was not recognized/handled.
    NotHandled,
    /// Return this prompt as user input for normal processing.
    Prompt(String),
    /// Exit the application.
    Exit,
}

/// Trait for command handlers.
pub trait CommandHandler: Send + Sync {
    fn handle(&self, command: &str) -> CommandResult;
}

/// Convenience impl so closures/functions can be used as handlers directly.
impl<F> CommandHandler for F
where
    F: Fn(&str) -> CommandResult + Send + Sync,
{
    fn handle(&self, command: &str) -> CommandResult {
        self(command)
    }
}

/// Logical grouping of commands for help output and organization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CommandCategory {
    /// Essential commands like /help, /exit
    Core,
    /// Session management like /session, /autosave_load
    Session,
    /// Configuration like /model, /agent, /colors
    Config,
    /// MCP server commands like /mcp list, /mcp install
    Mcp,
    /// Universal Constructor commands
    Uc,
    /// Tool-related commands
    Tools,
    /// User-defined commands
    Custom,
}

impl fmt::Display for CommandCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Core => "Core",
            Self::Session => "Session",
            Self::Config => "Config",
            Self::Mcp => "MCP",
            Self::Uc => "UC",
            Self::Tools => "Tools",
            Self::Custom => "Custom",
        };
        write!(f, "{label}")
    }
}

/// Metadata for a registered command.
#[derive(Clone)]
pub struct CommandInfo {
    /// Primary command name (without leading `/`)
    pub name: String,
    /// Short description
    pub description: String,
    /// Full usage string (e.g., "/cd <dir>")
    pub usage: String,
    /// Alternative names
    pub aliases: Vec<String>,
    /// Grouping category
    pub category: CommandCategory,
    /// Optional detailed help
    pub detailed_help: Option<String>,
    /// Handler function/object
    pub handler: Arc<dyn CommandHandler>,
}

impl fmt::Debug for CommandInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandInfo")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("usage", &self.usage)
            .field("aliases", &self.aliases)
            .field("category", &self.category)
            .field("detailed_help", &self.detailed_help)
            .field("handler", &"<dyn CommandHandler>")
            .finish()
    }
}

impl CommandInfo {
    /// Build a command info with defaults normalized:
    /// - name/aliases are stored without leading `/`
    /// - usage defaults to `/{name}` when empty
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        usage: impl Into<String>,
        aliases: Vec<String>,
        category: CommandCategory,
        detailed_help: Option<String>,
        handler: Arc<dyn CommandHandler>,
    ) -> Self {
        let mut name = normalize_command_name(&name.into());
        if name.is_empty() {
            name = "unknown".to_string();
        }

        let mut usage = usage.into().trim().to_string();
        if usage.is_empty() {
            usage = format!("/{name}");
        }

        let aliases = aliases
            .into_iter()
            .map(|a| normalize_command_name(&a))
            .filter(|a| !a.is_empty())
            .collect();

        Self {
            name,
            description: description.into(),
            usage,
            aliases,
            category,
            detailed_help,
            handler,
        }
    }
}

/// Errors for command registry operations.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum CommandError {
    #[error("command name cannot be empty")]
    EmptyName,
    #[error("handler registration conflict for '{name}'")]
    NameConflict { name: String },
}

/// Internal global registry:
/// maps normalized command/alias name -> shared `CommandInfo`.
static COMMAND_REGISTRY: OnceLock<RwLock<HashMap<String, Arc<CommandInfo>>>> = OnceLock::new();

fn registry() -> &'static RwLock<HashMap<String, Arc<CommandInfo>>> {
    COMMAND_REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

fn normalize_command_name(name: &str) -> String {
    name.trim().trim_start_matches('/').to_ascii_lowercase()
}

/// Register a command and all its aliases.
///
/// Case-insensitive: keys are normalized to lowercase.
/// Aliases point to the same `CommandInfo` allocation.
pub fn register_command(info: CommandInfo) -> Result<(), CommandError> {
    let primary = normalize_command_name(&info.name);
    if primary.is_empty() {
        return Err(CommandError::EmptyName);
    }

    let mut keys = Vec::with_capacity(info.aliases.len() + 1);
    keys.push(primary);
    keys.extend(info.aliases.iter().map(|a| normalize_command_name(a)));

    let mut dedup = HashSet::new();
    keys.retain(|k| !k.is_empty() && dedup.insert(k.clone()));

    let mut map = registry().write().expect("command registry poisoned");

    if let Some(conflict) = keys.iter().find(|k| map.contains_key((*k).as_str())) {
        return Err(CommandError::NameConflict {
            name: conflict.clone(),
        });
    }

    let shared = Arc::new(info);
    for key in keys {
        map.insert(key, Arc::clone(&shared));
    }

    Ok(())
}

/// Get command by name or alias (case-insensitive).
pub fn get_command(name: &str) -> Option<CommandInfo> {
    let key = normalize_command_name(name);
    if key.is_empty() {
        return None;
    }

    let map = registry().read().ok()?;
    map.get(&key).map(|cmd| (*cmd.as_ref()).clone())
}

/// Get all registered command entries.
///
/// Note: aliases are included as separate map entries in the registry, but this
/// returns deduplicated command objects by default semantics of this API.
pub fn get_all_commands() -> Vec<CommandInfo> {
    let map = match registry().read() {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };

    let mut seen = HashSet::new();
    map.values()
        .filter_map(|arc| {
            let id = Arc::as_ptr(arc) as usize;
            if seen.insert(id) {
                Some((**arc).clone())
            } else {
                None
            }
        })
        .collect()
}

/// Get unique commands (no duplicates from aliases).
pub fn get_unique_commands() -> Vec<CommandInfo> {
    get_all_commands()
}

/// Clear registry (useful for testing).
pub fn clear_registry() {
    if let Ok(mut map) = registry().write() {
        map.clear();
    }
}

/// Execute a command string by resolving its handler.
///
/// Behavior:
/// - Non-command input (`!starts_with('/')`) => `NotHandled`
/// - Unknown command => `NotHandled`
/// - Known command => dispatch to handler with full command text
pub fn execute_command(command: &str) -> CommandResult {
    if !is_command(command) {
        return CommandResult::NotHandled;
    }

    let input = command.trim();
    let name = input
        .trim_start_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or_default();

    let Some(info) = get_command(name) else {
        return CommandResult::NotHandled;
    };

    info.handler.handle(input)
}

/// Check if a string looks like a command.
pub fn is_command(input: &str) -> bool {
    input.trim_start().starts_with('/')
}

/// Generate grouped help text for all unique commands.
pub fn get_commands_help() -> String {
    let mut grouped: BTreeMap<CommandCategory, Vec<CommandInfo>> = BTreeMap::new();

    for cmd in get_unique_commands() {
        grouped.entry(cmd.category).or_default().push(cmd);
    }

    for cmds in grouped.values_mut() {
        cmds.sort_by(|a, b| a.name.cmp(&b.name));
    }

    let mut lines = Vec::new();
    lines.push("Available commands:".to_string());

    for (category, commands) in grouped {
        lines.push(String::new());
        lines.push(format!("{category}:"));

        for cmd in commands {
            let alias_suffix = if cmd.aliases.is_empty() {
                String::new()
            } else {
                format!(" (aliases: {})", cmd.aliases.join(", "))
            };

            lines.push(format!(
                "  {:<24} - {}{}",
                cmd.usage, cmd.description, alias_suffix
            ));
        }
    }

    lines.join("\n")
}

#[macro_export]
macro_rules! register_command {
    (
        name = $name:expr_2021,
        description = $desc:expr_2021,
        usage = $usage:expr_2021,
        aliases = [$($alias:expr_2021),* $(,)?],
        category = $cat:expr_2021,
        handler = $handler:expr_2021
        $(,)?
    ) => {{
        let info = $crate::commands::registry::CommandInfo::new(
            $name,
            $desc,
            $usage,
            vec![$($alias.to_string()),*],
            $cat,
            None,
            std::sync::Arc::new($handler),
        );
        $crate::commands::registry::register_command(info)
    }};
}
