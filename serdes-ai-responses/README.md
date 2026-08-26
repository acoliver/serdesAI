# serdes-ai-responses

A serdesAI `Model` client for the **OpenAI Responses API**, following the
[Open Responses](https://openresponses.org) interoperability profile: drive
OpenAI, the codex endpoint, or any Open Responses-compatible server from any
serdesAI agent, unchanged.

## Usage

```rust,ignore
use serdes_ai_responses::client::OpenResponsesModel;

// Any Open Responses-compatible websocket endpoint.
let model = OpenResponsesModel::new("gpt-5.1-codex-mini", "wss://host/v1/responses")
    .bearer("sk-…");

// Or the codex endpoint over HTTP.
let model = OpenResponsesModel::new(
    "gpt-5.1-codex-mini",
    "https://chatgpt.com/backend-api/codex/responses",
)
.bearer("oauth-token")
.header("chatgpt-account-id", "…");

// Use it like any other serdesAI model.
let agent = Agent::new(model).build()?;
```

## Transports

- **WebSocket** (`wss://`/`ws://`, the default for that scheme): sends
  `{"type":"response.create","response":{…}}` frames and maps the event
  stream (`output_item.added`, `output_text.delta`,
  `reasoning_summary_text.delta`, `function_call_arguments.delta`, …) onto
  `ModelResponseStreamEvent`s, ending with exactly one terminal
  `StreamComplete` carrying the finish reason and token usage.
- **HTTP** (`https://`/`http://`): `POST` per turn with `store: true` and
  `previous_response_id` chaining; `"stream": true` requests are served by
  an SSE parser (`data: {...}` frames terminated by `data: [DONE]`).

## Session-stateful mode

The model keeps conversation state in the session, so each turn only sends
the *new* input items:

- On websockets the socket session holds `previous_response_id`; turns are
  sent with `store: false` and delta-only input. Assistant output the server
  already produced is never re-sent.
- When a continuation fails (`previous_response_not_found`) the cached id is
  dropped and the full input replayed once, mirroring codex CLI recovery.
- When the server enforces its connection lifetime
  (`websocket_connection_limit_reached`) or the socket dies, the client
  reconnects and replays the turn.
- Recovery applies only before any event has reached the caller, so partial
  output is never duplicated.
- Turns are sequential: the protocol has no way to match interleaved
  responses, so concurrent `request`/`request_stream` calls on one model
  instance are serialized by the session lock.

HTTP chaining follows the same shape with `store: true`.

## Error codes surfaced

| code | meaning |
| --- | --- |
| `invalid_request_error` | malformed request or unsupported option |
| `previous_response_not_found` | chain target missing (recovered by replay) |
| `websocket_connection_limit_reached` | WS lifetime exceeded (recovered by reconnect) |
| `model_error` | backing model failed |
| `internal_error` | server-side failure |

Errors map onto `ModelError::Provider` with the wire code, except transport
failures which surface as `ModelError::Connection`.

## Test rig (`test-server` feature, off by default)

The crate ships a wire-accurate Open Responses server used as a test rig for
its own integration tests. It is not a product surface:

```toml
[dev-dependencies]
serdes-ai-responses = { path = ".", features = ["test-server"] }
```

It exercises both transports, chaining, TTL enforcement between turns, and
the error codes above, so client behavior is verified against the same wire
shapes codex expects.
