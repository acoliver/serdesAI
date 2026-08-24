//! Tool registry for the CLI.
//!
//! Provides access to built-in tools and custom tool registration.

use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use serdes_ai_agent::{AgentBuilder, RunContext};
use serdes_ai_tools::{Tool, ToolError, ToolReturn};
use tracing::{debug, info};

mod web_fetch;
mod web_search;

use web_fetch::WebFetchTool;
use web_search::DuckDuckGoSearchTool;

use crate::bus::MessageBus;

/// Tool registry for the CLI.
pub struct CliToolRegistry {
    pub(crate) _bus: Arc<MessageBus>,
}

impl CliToolRegistry {
    pub fn new(bus: Arc<MessageBus>) -> Self {
        Self { _bus: bus }
    }

    /// Register all built-in tools.
    pub fn register_builtin_tools(&mut self) -> Result<()> {
        info!("Registering built-in tools");
        info!("Registered tool: web_search");
        info!("Registered tool: web_fetch");
        info!("Registered tool: file_search");
        info!("Registered tool: code_execution");
        info!("MCP tools: (none configured)");
        Ok(())
    }

    /// Apply tools to an agent builder.
    pub fn apply_to_builder(&self, builder: AgentBuilder<(), String>) -> AgentBuilder<(), String> {
        let mut builder = builder;

        // web_search
        builder = builder.tool_fn_async(
            "web_search",
            "Search the web for information",
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
        builder = builder.tool_fn_async(
            "web_fetch",
            "Fetch content from a URL",
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
        builder = builder.tool_fn_async(
            "file_search",
            "Search files using semantic similarity",
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
        builder = builder.tool_fn_async(
            "code_execution",
            "Execute code in a sandbox",
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

        // read_file
        builder = builder.tool_fn_async(
            "read_file",
            "Read contents of a file",
            |_ctx: &RunContext<()>, args: ReadFileArgs| async move {
                match tokio::fs::read_to_string(&args.path).await {
                    Ok(content) => Ok(ToolReturn::text(content)),
                    Err(err) => Ok(ToolReturn::error(format!(
                        "Error reading file '{}': {}",
                        args.path, err
                    ))),
                }
            },
        );

        // list_files (minimal safe implementation)
        builder =
            builder.tool_fn_async(
                "list_files",
                "List files in a directory",
                |_ctx: &RunContext<()>, args: ListFilesArgs| async move {
                    let path = args.path.unwrap_or_else(|| ".".to_string());
                    let mut entries = tokio::fs::read_dir(&path).await.map_err(|e| {
                        ToolError::execution_failed(format!("read_dir failed: {e}"))
                    })?;

                    let mut items = Vec::new();
                    while let Some(entry) = entries.next_entry().await.map_err(|e| {
                        ToolError::execution_failed(format!("next_entry failed: {e}"))
                    })? {
                        let name = entry.file_name().to_string_lossy().to_string();
                        let ty = entry.file_type().await.map_err(|e| {
                            ToolError::execution_failed(format!("file_type failed: {e}"))
                        })?;
                        let kind = if ty.is_dir() { "dir" } else { "file" };
                        items.push(serde_json::json!({ "name": name, "kind": kind }));
                    }

                    let output = serde_json::json!({
                        "path": path,
                        "recursive": args.recursive.unwrap_or(false),
                        "items": items,
                    });

                    Ok(ToolReturn::json(output))
                },
            );

        // grep (text contains search for now)
        builder = builder.tool_fn_async(
            "grep",
            "Search for patterns in files",
            |_ctx: &RunContext<()>, args: GrepArgs| async move {
                let base = std::path::PathBuf::from(args.path.unwrap_or_else(|| ".".to_string()));
                let mut stack = vec![base];
                let mut matches = Vec::new();

                while let Some(dir) = stack.pop() {
                    let mut rd = match tokio::fs::read_dir(&dir).await {
                        Ok(v) => v,
                        Err(_) => continue,
                    };

                    while let Ok(Some(entry)) = rd.next_entry().await {
                        let path = entry.path();
                        let file_type = match entry.file_type().await {
                            Ok(v) => v,
                            Err(_) => continue,
                        };

                        if file_type.is_dir() {
                            stack.push(path);
                            continue;
                        }

                        if !file_type.is_file() {
                            continue;
                        }

                        let content = match tokio::fs::read_to_string(&path).await {
                            Ok(v) => v,
                            Err(_) => continue,
                        };

                        for (idx, line) in content.lines().enumerate() {
                            if line.contains(&args.pattern) {
                                matches.push(serde_json::json!({
                                    "file_path": path.display().to_string(),
                                    "line_number": idx + 1,
                                    "line_content": line,
                                }));
                            }

                            if matches.len() >= 200 {
                                break;
                            }
                        }

                        if matches.len() >= 200 {
                            break;
                        }
                    }

                    if matches.len() >= 200 {
                        break;
                    }
                }

                Ok(ToolReturn::json(serde_json::json!({ "matches": matches })))
            },
        );

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

#[derive(Debug, Deserialize)]
struct ReadFileArgs {
    path: String,
}

#[derive(Debug, Deserialize)]
struct ListFilesArgs {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    recursive: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct GrepArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
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
