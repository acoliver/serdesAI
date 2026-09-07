//! Client for the OpenAI Responses API as spoken by the OpenAI codex CLI and
//! [Open Responses](https://openresponses.org)-compatible servers.
//!
//! [`OpenResponsesModel`] is a serdesAI [`Model`](serdes_ai_models::Model)
//! implementation that drives a Responses API endpoint:
//!
//! - **WebSocket transport** (`wss://…/v1/responses`): sends
//!   `{"type":"response.create","model":…,"input":[…]}` frames and maps the event
//!   stream onto `ModelResponseStreamEvent`s.
//! - **Session-stateful mode**: the model keeps `previous_response_id` in the
//!   socket session and sends `store: false` plus only the *new* input items
//!   each turn, instead of replaying the full history every run. When a
//!   continuation fails (`previous_response_not_found`) the cached id is
//!   dropped and the full input replayed; when the server enforces its
//!   connection lifetime (`websocket_connection_limit_reached`) the socket is
//!   reconnected and the turn replayed, mirroring codex CLI behavior.
//! - **HTTP stateful mode** (`POST /v1/responses`, `store: true` +
//!   `previous_response_id`) for endpoints without websockets, including the
//!   codex endpoint shape (`{base_url}/responses` with a bearer token, e.g.
//!   `chatgpt.com/backend-api/codex`).
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use serdes_ai_responses::client::OpenResponsesModel;
//!
//! let model = OpenResponsesModel::new("gpt-5.1-codex-mini", "wss://host/v1/responses")
//!     .bearer("sk-…");
//! // use it like any other serdesAI model: agent.run(...) / agent.run_stream(...)
//! # let _ = &model;
//! # Ok(())
//! # }
//! ```
//!
//! The `test-server` feature (off by default) compiles a wire-accurate local
//! server used as a test rig for this crate's integration tests. It is not a
//! product surface.

#![warn(missing_docs)]
#![deny(unsafe_code)]

pub mod client;
pub mod convert;
pub mod error;
pub mod types;

#[cfg(feature = "test-server")]
pub mod engine;
#[cfg(feature = "test-server")]
pub mod server;
#[cfg(feature = "test-server")]
pub mod store;
#[cfg(feature = "test-server")]
pub mod websocket;

pub use client::{OpenResponsesModel, Transport};
pub use error::ResponsesError;
pub use types::{CreateResponseRequest, OutputItem, ResponseInput, ResponseObject, StreamEvent};
