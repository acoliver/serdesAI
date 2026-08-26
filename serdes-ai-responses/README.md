# serdes-ai-responses

Serve any serdesAI model through the **OpenAI Responses API**, following the
[Open Responses](https://openresponses.org) interoperability profile: plain
JSON and SSE over HTTP, a WebSocket transport with connection-local state,
and stateful conversation chaining via `previous_response_id`.

This turns a serdesAI `Model` into an endpoint that standard Responses API
clients can talk to, including the OpenAI **codex** CLI pointed at a custom
`model_provider`.

## Features

- `POST /v1/responses` — JSON responses, or SSE streaming when
  `"stream": true` (`data: {...}` frames terminated by `data: [DONE]`).
- `POST /responses` — alias so the codex CLI can use the server as its
  `base_url` (it posts to `{base_url}/responses`).
- `GET /v1/responses/{id}` — fetch a stored response.
- `GET /v1/responses` — WebSocket upgrade; clients send
  `{"type":"response.create","response":{...}}` frames and receive the same
  events as SSE, one turn at a time.
- **Stateful mode** — `store: true` (the default) persists each response in
  a [`ResponseStore`]; later turns can chain with `previous_response_id`.
  `store: false` turns on a WebSocket connection keep their state in a
  connection-local cache, so nothing is persisted globally while chaining
  still works on that socket (the codex CLI default).
- **codex compatibility** — `store:false` + `instructions` + function tools
  wire shapes, unprefixed mid-stream event names, and the error codes codex
  treats as retryable (`previous_response_not_found`,
  `websocket_connection_limit_reached`).

Not supported: hosted tools (`web_search_preview` etc. — only client-side
function tools), `background: true`, and item references (`item_reference`).

## Quick start

```rust,ignore
use serdes_ai_models::mock::FunctionModel;
use serdes_ai_responses::server::ResponsesServer;
use serdes_ai_responses::ResponsesEngine;
use std::net::SocketAddr;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = FunctionModel::constant_text("hello"); // any Arc<dyn Model>
    let server = ResponsesServer::new(ResponsesEngine::new(Arc::new(model)));
    let addr: SocketAddr = "127.0.0.1:8080".parse()?;
    server.serve(addr).await?;
    Ok(())
}
```

## Pointing the codex CLI at the server

```toml
# ~/.codex/config.toml
[model_providers.serdes]
name = "serdesAI"
base_url = "http://127.0.0.1:8080"
wire_api = "responses"
```

```sh
codex --profile serdes -m gpt-5.1-codex-mini "fix the failing test"
```

## Error codes

| HTTP | code | meaning |
| --- | --- | --- |
| 400 | `invalid_request_error` | malformed body or unsupported option |
| 404 | `not_found_error` | unknown response id on GET |
| 404 | `previous_response_not_found` | chain target missing (client should replay full input) |
| 429 | `websocket_connection_limit_reached` | WS connection exceeded its lifetime; reconnect |
| 502 | `model_error` | backing model failed |

WebSocket errors use the envelope `{"type":"error","status_code":N,"error":{"code":..,"message":..}}`.
