//! Universal Constructor - Dynamic tool creation

use std::collections::HashMap;
use std::sync::Mutex;

use once_cell::sync::Lazy;

/// UC Tool definition
#[derive(Debug, Clone)]
pub struct UCTool {
    pub name: String,
    pub description: String,
    pub code: String,
    pub language: String,
}

/// UC State
pub struct UCState {
    enabled: Mutex<bool>,
    sandbox_enabled: Mutex<bool>,
    tools: Mutex<HashMap<String, UCTool>>,
}

impl UCState {
    pub fn new() -> Self {
        Self {
            enabled: Mutex::new(false),
            sandbox_enabled: Mutex::new(true),
            tools: Mutex::new(HashMap::new()),
        }
    }

    pub fn is_enabled(&self) -> bool {
        *self.enabled.lock().unwrap()
    }

    pub fn set_enabled(&self, enabled: bool) {
        *self.enabled.lock().unwrap() = enabled;
    }

    pub fn is_sandbox_enabled(&self) -> bool {
        *self.sandbox_enabled.lock().unwrap()
    }

    pub fn set_sandbox_enabled(&self, enabled: bool) {
        *self.sandbox_enabled.lock().unwrap() = enabled;
    }

    pub fn create_tool(&self, name: &str, description: &str, code: &str) -> anyhow::Result<()> {
        let tool = UCTool {
            name: name.to_string(),
            description: description.to_string(),
            code: code.to_string(),
            language: "python".to_string(),
        };
        self.tools.lock().unwrap().insert(name.to_string(), tool);
        Ok(())
    }

    pub fn get_tool(&self, name: &str) -> Option<UCTool> {
        self.tools.lock().unwrap().get(name).cloned()
    }

    pub fn list_tools(&self) -> Vec<UCTool> {
        self.tools.lock().unwrap().values().cloned().collect()
    }

    pub fn remove_tool(&self, name: &str) -> bool {
        self.tools.lock().unwrap().remove(name).is_some()
    }
}

impl Default for UCState {
    fn default() -> Self {
        Self::new()
    }
}

// Global UC state
static UC: Lazy<UCState> = Lazy::new(UCState::new);

// Public API
pub fn is_enabled() -> bool {
    UC.is_enabled()
}

pub fn set_enabled(enabled: bool) {
    UC.set_enabled(enabled);
}

pub fn is_sandbox_enabled() -> bool {
    UC.is_sandbox_enabled()
}

pub fn set_sandbox_enabled(enabled: bool) {
    UC.set_sandbox_enabled(enabled);
}

pub fn create_tool(name: &str, description: &str, code: &str) -> anyhow::Result<()> {
    UC.create_tool(name, description, code)
}

pub fn get_tool(name: &str) -> Option<UCTool> {
    UC.get_tool(name)
}

pub fn list_tools() -> Vec<UCTool> {
    UC.list_tools()
}

pub fn remove_tool(name: &str) -> bool {
    UC.remove_tool(name)
}

/// UC Tool template for agent
pub const UC_TOOL_TEMPLATE: &str = r#"You can create custom tools using the Universal Constructor.

To create a tool, describe what you want it to do and provide Python code.

Example:
Tool Name: calculate_fibonacci
Description: Calculate Fibonacci numbers
Code:
```python
def calculate_fibonacci(n: int) -> int:
    if n <= 1:
        return n
    a, b = 0, 1
    for _ in range(2, n + 1):
        a, b = b, a + b
    return b
```

The tool will be available immediately and can be invoked by name."#;
