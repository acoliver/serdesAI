pub mod bus;
pub mod collapse;
pub mod commands;
pub mod config;
pub mod messages;
pub mod orchestration;
pub mod renderer;
pub mod screen;
pub mod stream_render;
pub mod streaming;
pub mod tui;

pub mod completion;
pub mod picker;
pub mod render;

// Placeholder modules (intentionally minimal for now)
pub mod attachments;
pub mod clipboard;
pub mod input;
pub mod onboarding;

pub mod args;
pub mod mcp_runtime;
pub mod model_factory;
pub mod models;
pub mod oauth;
pub mod runner;
pub mod session;
pub mod shell;
pub mod terminal;
pub mod tools;
pub mod turn_ui;
pub mod uc;
pub mod version;
pub mod wiggum;

// Re-exports for convenient access
pub use args::Cli;
pub use bus::AnyMessage;
pub use bus::{MessageBus, emit_error, emit_info, emit_success, emit_warning, get_message_bus};
pub use config::{
    ensure_config_exists, get_agent_name, get_model_name, load_api_keys_to_environment,
    set_agent_name, set_model_name,
};
pub use messages::{BaseMessage, MessageCategory, MessageLevel, TextMessage};
pub use runner::run;

/// Commonly used CLI types and helpers.
pub mod prelude {
    pub use crate::args::Cli;
    pub use crate::bus::{
        AnyMessage, MessageBus, emit_error, emit_info, emit_success, emit_warning, get_message_bus,
    };
    pub use crate::config::{
        ensure_config_exists, get_agent_name, get_model_name, load_api_keys_to_environment,
        set_agent_name, set_model_name,
    };
    pub use crate::messages::{BaseMessage, MessageCategory, MessageLevel, TextMessage};
    pub use crate::runner::run;
}
