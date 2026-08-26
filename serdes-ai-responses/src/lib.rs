//! OpenAI Responses API server support for serdesAI.
//!
//! This crate exposes a serdesAI [`Model`] as an OpenAI
//! [Responses API](https://platform.openai.com/docs/api-reference/responses)
//! compatible service, following the Open Responses interoperability profile
//! (<https://openresponses.org>):
//!
//! - `POST /v1/responses` returning JSON or SSE (`stream: true`)
//! - `GET /v1/responses/{id}` retrieving stored responses
//! - stateful chaining via `previous_response_id` backed by a
//!   [`store::ResponseStore`]
//! - a websocket transport on the same path where each connection holds its
//!   own session state, including continuations for `store: false` turns
//! - a `/responses` route alias so clients like the codex CLI (which posts to
//!   `{base_url}/responses`) can use the server directly
//!
//! The server brokers client-side function tools only: hosted tools such as
//! `web_search_preview` are rejected with a 400.
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use serdes_ai_models::mock::FunctionModel;
//! use serdes_ai_responses::ResponsesEngine;
//! use std::net::SocketAddr;
//! use std::sync::Arc;
//!
//! let engine = ResponsesEngine::new(Arc::new(FunctionModel::constant_text("hi")));
//! let server = serdes_ai_responses::server::ResponsesServer::new(engine);
//! let addr: SocketAddr = "127.0.0.1:8080".parse()?;
//! server.serve(addr).await?;
//! # Ok(())
//! # }
//! ```
//!
//! Streaming over SSE terminates with a `data: [DONE]` sentinel; the
//! websocket transport terminates each turn with `response.completed`,
//! `response.incomplete`, or `response.failed`.

#![warn(missing_docs)]
#![deny(unsafe_code)]

pub mod convert;
pub mod engine;
pub mod error;
pub mod store;
pub mod types;

#[cfg(feature = "server")]
pub mod server;
#[cfg(feature = "server")]
pub mod websocket;

pub use engine::{PreparedTurn, ResponsesEngine, TurnOutput};
pub use error::ResponsesError;
pub use store::{InMemoryResponseStore, ResponseStore, SessionResponseCache, StoredResponse};
pub use types::{
    CreateResponseRequest, OutputItem, ResponseInput, ResponseObject, ResponseStatus, StreamEvent,
};
