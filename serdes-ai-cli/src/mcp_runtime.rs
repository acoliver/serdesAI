//! MCP (Model Context Protocol) runtime for managing external tool servers.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::Stdio;
use tokio::process::{Child, Command};
use tracing::{info, warn};

use crate::config;

/// MCP server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub disabled: bool,
}

/// MCP server process handle
pub struct McpServer {
    pub config: McpServerConfig,
    pub process: Option<Child>,
    pub status: ServerStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerStatus {
    Stopped,
    Starting,
    Running,
    Error(String),
}

/// MCP runtime for managing servers
pub struct McpRuntime {
    servers: HashMap<String, McpServer>,
}

impl McpRuntime {
    pub fn new() -> Self {
        Self {
            servers: HashMap::new(),
        }
    }

    /// Load MCP configurations from config
    pub fn load_config(&mut self) -> Result<()> {
        let config_path = config::get_config_dir().join("mcp_servers.json");

        if !config_path.exists() {
            info!("No MCP server config found at {:?}", config_path);
            return Ok(());
        }

        let content = std::fs::read_to_string(&config_path)?;
        let configs: Vec<McpServerConfig> = serde_json::from_str(&content)?;

        for cfg in configs {
            if !cfg.disabled {
                self.servers.insert(
                    cfg.name.clone(),
                    McpServer {
                        config: cfg,
                        process: None,
                        status: ServerStatus::Stopped,
                    },
                );
            }
        }

        info!("Loaded {} MCP server configurations", self.servers.len());
        Ok(())
    }

    /// Start an MCP server
    pub async fn start_server(&mut self, name: &str) -> Result<()> {
        let server = self
            .servers
            .get_mut(name)
            .ok_or_else(|| anyhow!("Server '{}' not found", name))?;

        if server.status == ServerStatus::Running {
            info!("Server '{}' is already running", name);
            return Ok(());
        }

        info!("Starting MCP server '{}'", name);
        server.status = ServerStatus::Starting;

        let mut cmd = Command::new(&server.config.command);
        cmd.args(&server.config.args)
            .envs(&server.config.env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        match cmd.spawn() {
            Ok(child) => {
                server.process = Some(child);
                server.status = ServerStatus::Running;
                info!("MCP server '{}' started successfully", name);
                Ok(())
            }
            Err(e) => {
                let err_msg = format!("Failed to start: {}", e);
                server.status = ServerStatus::Error(err_msg.clone());
                Err(anyhow!(err_msg))
            }
        }
    }

    /// Stop an MCP server
    pub async fn stop_server(&mut self, name: &str) -> Result<()> {
        let server = self
            .servers
            .get_mut(name)
            .ok_or_else(|| anyhow!("Server '{}' not found", name))?;

        if let Some(mut child) = server.process.take() {
            info!("Stopping MCP server '{}'", name);
            let _ = child.kill().await;
        }

        server.status = ServerStatus::Stopped;
        info!("MCP server '{}' stopped", name);
        Ok(())
    }

    /// Get server status
    pub fn get_status(&self, name: &str) -> Option<ServerStatus> {
        self.servers.get(name).map(|s| s.status.clone())
    }

    /// List all servers and their status
    pub fn list_servers(&self) -> Vec<(String, ServerStatus)> {
        self.servers
            .iter()
            .map(|(name, server)| (name.clone(), server.status.clone()))
            .collect()
    }

    /// Start all enabled servers
    pub async fn start_all(&mut self) -> Vec<(String, Result<()>)> {
        let names: Vec<String> = self.servers.keys().cloned().collect();
        let mut results = Vec::new();

        for name in names {
            let result = self.start_server(&name).await;
            results.push((name, result));
        }

        results
    }

    /// Stop all servers
    pub async fn stop_all(&mut self) {
        let names: Vec<String> = self.servers.keys().cloned().collect();
        for name in names {
            if let Err(e) = self.stop_server(&name).await {
                warn!("Error stopping server '{}': {}", name, e);
            }
        }
    }
}

impl Default for McpRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for McpRuntime {
    fn drop(&mut self) {
        for (name, server) in &mut self.servers {
            if let Some(child) = server.process.as_mut() {
                info!("Cleaning up MCP server '{}'", name);
                let _ = child.start_kill();
            }
            server.status = ServerStatus::Stopped;
        }
    }
}

/// Install a new MCP server
pub async fn install_server(name: &str, command: &str, args: &[String]) -> Result<()> {
    let config_path = config::get_config_dir().join("mcp_servers.json");

    // Load existing configs
    let mut configs: Vec<McpServerConfig> = if config_path.exists() {
        let content = std::fs::read_to_string(&config_path)?;
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        Vec::new()
    };

    // Check if already exists
    if configs.iter().any(|c| c.name == name) {
        return Err(anyhow!("Server '{}' already exists", name));
    }

    // Add new config
    configs.push(McpServerConfig {
        name: name.to_string(),
        command: command.to_string(),
        args: args.to_vec(),
        env: HashMap::new(),
        disabled: false,
    });

    // Save
    let json = serde_json::to_string_pretty(&configs)?;
    std::fs::create_dir_all(config::get_config_dir())?;
    std::fs::write(&config_path, json)?;

    info!("MCP server '{}' installed successfully", name);
    Ok(())
}

/// Remove an MCP server
pub async fn remove_server(name: &str) -> Result<()> {
    let config_path = config::get_config_dir().join("mcp_servers.json");

    if !config_path.exists() {
        return Err(anyhow!("No MCP servers configured"));
    }

    let content = std::fs::read_to_string(&config_path)?;
    let mut configs: Vec<McpServerConfig> = serde_json::from_str(&content)?;

    let initial_len = configs.len();
    configs.retain(|c| c.name != name);

    if configs.len() == initial_len {
        return Err(anyhow!("Server '{}' not found", name));
    }

    let json = serde_json::to_string_pretty(&configs)?;
    std::fs::write(&config_path, json)?;

    info!("MCP server '{}' removed", name);
    Ok(())
}
