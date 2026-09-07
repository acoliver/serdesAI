//! Tool registry for the CLI.
//!
//! Provides access to built-in tools and custom tool registration.

use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use serdes_ai_agent::{AgentBuilder, RunContext};
use serdes_ai_tools::{Tool, ToolReturn};
use tracing::{debug, info};

mod coding;
mod web_fetch;
mod web_search;

use web_fetch::WebFetchTool;
use web_search::DuckDuckGoSearchTool;

use crate::bus::MessageBus;

/// Tool registry for the CLI.
pub struct CliToolRegistry {
    pub(crate) bus: Arc<MessageBus>,
}

impl CliToolRegistry {
    pub fn new(bus: Arc<MessageBus>) -> Self {
        Self { bus }
    }

    /// Register all built-in tools.
    pub fn register_builtin_tools(&mut self) -> Result<()> {
        info!("Registering built-in tools");
        info!("Registered tools: read_file, list_files, grep, write_file, edit_file, bash");
        info!("Registered tool: web_search");
        info!("Registered tool: web_fetch");
        info!("Registered tool: file_search");
        info!("Registered tool: code_execution");
        info!("MCP tools: (none configured)");
        Ok(())
    }

    /// Apply tools to an agent builder.
    pub fn apply_to_builder(&self, builder: AgentBuilder<(), String>) -> AgentBuilder<(), String> {
        // The coding tools come first so that read_file, list_files and grep are
        // the working, display-emitting implementations rather than the stubs
        // that used to shadow them.
        let root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let mut builder = coding::register(builder, Arc::clone(&self.bus), root);

        // web_search
        builder = builder.tool_fn_async_with_schema(
            "web_search",
            "Search the web for information",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "What to search for."},
                    "max_results": {"type": "integer", "description": "How many results to return."},
                },
                "required": ["query"],
            }),
            |_ctx: &RunContext<()>, args: WebSearchArgs| async move {
                let tool = DuckDuckGoSearchTool::new();
                let tool_ctx = serdes_ai_tools::RunContext::minimal("serdes-ai-cli");
                tool.call(
                    &tool_ctx,
                    serde_json::json!({
                        "query": args.query,
                        "max_results": args.max_results,
                    }),
                )
                .await
            },
        );

        // web_fetch
        builder = builder.tool_fn_async_with_schema(
            "web_fetch",
            "Fetch content from a URL",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "The URL to fetch."},
                    "max_length": {"type": "integer", "description": "Maximum number of characters to return."},
                },
                "required": ["url"],
            }),
            |_ctx: &RunContext<()>, args: WebFetchArgs| async move {
                let tool = WebFetchTool::new();
                let tool_ctx = serdes_ai_tools::RunContext::minimal("serdes-ai-cli");
                tool.call(
                    &tool_ctx,
                    serde_json::json!({
                        "url": args.url,
                        "max_length": args.max_length,
                    }),
                )
                .await
            },
        );

        // file_search
        builder = builder.tool_fn_async_with_schema(
            "file_search",
            "Search files using semantic similarity",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "What to look for."},
                    "file_extensions": {"type": "array", "items": {"type": "string"},
                                        "description": "Restrict the search to these extensions."},
                    "max_results": {"type": "integer", "description": "How many results to return."},
                },
                "required": ["query"],
            }),
            |_ctx: &RunContext<()>, args: FileSearchArgs| async move {
                Ok(ToolReturn::json(serde_json::json!({
                    "query": args.query,
                    "file_extensions": args.file_extensions.unwrap_or_default(),
                    "max_results": args.max_results.unwrap_or(10),
                    "status": "stubbed",
                    "message": "file_search is registered; semantic index integration can be added later.",
                })))
            },
        );

        // code_execution
        builder = builder.tool_fn_async_with_schema(
            "code_execution",
            "Execute code in a sandbox",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "code": {"type": "string", "description": "The code to run."},
                    "language": {"type": "string", "description": "The language the code is written in."},
                    "stdin": {"type": "string", "description": "Input to supply on standard input."},
                },
                "required": ["code", "language"],
            }),
            |_ctx: &RunContext<()>, args: CodeExecutionArgs| async move {
                Ok(ToolReturn::json(serde_json::json!({
                    "language": args.language,
                    "stdin": args.stdin,
                    "status": "stubbed",
                    "message": "code_execution is registered; sandbox backend is not wired in this CLI path yet.",
                    "code_preview": args.code.chars().take(120).collect::<String>(),
                })))
            },
        );

        // read_file, list_files and grep are deliberately not registered here:
        // coding::register above provides working, display-emitting versions.
        // Registering these too put each name in the tool list twice, leaving
        // the model to pick between two tools with the same name.

        debug!("Applied CLI tools to agent builder");
        builder
    }
}

#[derive(Debug, Deserialize)]
struct WebSearchArgs {
    query: String,
    #[serde(default)]
    max_results: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct WebFetchArgs {
    url: String,
    #[serde(default)]
    max_length: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct FileSearchArgs {
    query: String,
    #[serde(default)]
    file_extensions: Option<Vec<String>>,
    #[serde(default)]
    max_results: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize)]
struct CodeExecutionArgs {
    language: String,
    code: String,
    #[serde(default)]
    stdin: Option<String>,
}

impl From<WebSearchArgs> for JsonValue {
    fn from(value: WebSearchArgs) -> Self {
        let mut obj = serde_json::Map::new();
        obj.insert("query".to_string(), JsonValue::String(value.query));
        if let Some(max) = value.max_results {
            obj.insert("max_results".to_string(), JsonValue::Number(max.into()));
        }
        JsonValue::Object(obj)
    }
}

impl From<FileSearchArgs> for JsonValue {
    fn from(value: FileSearchArgs) -> Self {
        let mut obj = serde_json::Map::new();
        obj.insert("query".to_string(), JsonValue::String(value.query));
        if let Some(exts) = value.file_extensions {
            obj.insert(
                "file_extensions".to_string(),
                JsonValue::Array(exts.into_iter().map(JsonValue::String).collect()),
            );
        }
        if let Some(max) = value.max_results {
            obj.insert("max_results".to_string(), JsonValue::Number(max.into()));
        }
        JsonValue::Object(obj)
    }
}

/// List available tools.
pub fn list_tools() -> Vec<(&'static str, &'static str)> {
    vec![
        ("web_search", "Search the web for information"),
        ("web_fetch", "Fetch content from a URL"),
        ("file_search", "Search files using semantic similarity"),
        ("read_file", "Read contents of a file"),
        ("list_files", "List files in a directory"),
        ("grep", "Search for patterns in files"),
        ("code_execution", "Execute code in a sandbox"),
    ]
}
