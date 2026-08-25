//! Terminal UI tests for custom and OpenAI-compatible endpoints.
//!
//! Reaching a self-hosted or proxied server means two things have to work: the
//! request must go to the chosen address rather than the provider's default,
//! and the model name must be passed through untouched, since such a server
//! names its models however it likes.
//!
//! These run against a stub HTTP server rather than the scripted model, because
//! the point is the request actually leaving the process.

#[path = "ui/harness.rs"]
mod harness;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};

use harness::TerminalApp;

/// What a stub server saw.
#[derive(Debug)]
struct Request {
    path: String,
    body: String,
}

/// An OpenAI-compatible server that answers once and reports what it received.
struct StubServer {
    port: u16,
    seen: Receiver<Request>,
}

impl StubServer {
    fn start(reply: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("could not bind");
        let port = listener.local_addr().unwrap().port();
        let (tx, seen) = mpsc::channel();

        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                if let Some(request) = handle(stream, reply) {
                    let _ = tx.send(request);
                }
            }
        });

        Self { port, seen }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    /// The first request the server received.
    fn first_request(&self) -> Request {
        self.seen
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the server was never contacted")
    }
}

fn handle(mut stream: TcpStream, reply: &str) -> Option<Request> {
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];

    // Read until the body is complete, using content-length from the headers.
    loop {
        let n = stream.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);

        let text = String::from_utf8_lossy(&raw);
        if let Some(split) = text.find("\r\n\r\n") {
            let length: usize = text[..split]
                .lines()
                .find_map(|l| {
                    let (name, value) = l.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().ok())?
                })
                .unwrap_or(0);

            if text.len() >= split + 4 + length {
                break;
            }
        }
    }

    let text = String::from_utf8_lossy(&raw).to_string();
    let path = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or_default()
        .to_string();
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or("")
        .to_string();

    let payload = serde_json::json!({
        "id": "stub",
        "object": "chat.completion",
        "created": 0,
        "model": "stub",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": reply},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18}
    })
    .to_string();

    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
        payload.len(),
        payload
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();

    Some(Request { path, body })
}

#[test]
fn base_url_flag_sends_the_request_to_that_endpoint() {
    let server = StubServer::start("REPLY-VIA-FLAG");

    let app = TerminalApp::builder()
        .args([
            "--base-url",
            &server.url(),
            "-m",
            "openai:local-model",
            "-p",
            "hello",
        ])
        .env("OPENAI_API_KEY", "not-needed")
        .spawn()
        .expect("failed to spawn");

    app.wait_for("REPLY-VIA-FLAG")
        .expect("the answer from the custom endpoint never arrived");

    let request = server.first_request();
    assert!(
        request.path.contains("/v1/chat/completions"),
        "unexpected path: {}",
        request.path
    );
}

#[test]
fn the_model_name_reaches_the_server_unchanged() {
    // A self-hosted server names its models however it likes, so the name has to
    // pass through rather than being mapped to something known.
    let server = StubServer::start("ok");

    let app = TerminalApp::builder()
        .args([
            "--base-url",
            &server.url(),
            "-m",
            "openai:some-unusual-name-v3",
            "-p",
            "hello",
        ])
        .env("OPENAI_API_KEY", "not-needed")
        .spawn()
        .expect("failed to spawn");

    app.wait_for("ok").expect("no answer");

    let request = server.first_request();
    assert!(
        request.body.contains("some-unusual-name-v3"),
        "the model name was altered: {}",
        request.body
    );
}

#[test]
fn the_openai_base_url_variable_is_honoured() {
    let server = StubServer::start("REPLY-VIA-ENV");

    let app = TerminalApp::builder()
        .args(["-m", "openai:local-model", "-p", "hello"])
        .env("OPENAI_API_KEY", "not-needed")
        .env("OPENAI_BASE_URL", server.url())
        .spawn()
        .expect("failed to spawn");

    app.wait_for("REPLY-VIA-ENV")
        .expect("OPENAI_BASE_URL did not redirect the request");
}

#[test]
fn the_generic_variable_is_honoured() {
    let server = StubServer::start("REPLY-VIA-GENERIC");

    let app = TerminalApp::builder()
        .args(["-m", "openai:local-model", "-p", "hello"])
        .env("OPENAI_API_KEY", "not-needed")
        .env("SERDES_AI_BASE_URL", server.url())
        .spawn()
        .expect("failed to spawn");

    app.wait_for("REPLY-VIA-GENERIC")
        .expect("SERDES_AI_BASE_URL did not redirect the request");
}

#[test]
fn the_flag_beats_the_environment() {
    // The flag is the more specific instruction, so it has to win.
    let wanted = StubServer::start("REPLY-FROM-FLAG-SERVER");
    let ignored = StubServer::start("REPLY-FROM-ENV-SERVER");

    let app = TerminalApp::builder()
        .args([
            "--base-url",
            &wanted.url(),
            "-m",
            "openai:local-model",
            "-p",
            "hello",
        ])
        .env("OPENAI_API_KEY", "not-needed")
        .env("OPENAI_BASE_URL", ignored.url())
        .spawn()
        .expect("failed to spawn");

    app.wait_for("REPLY-FROM-FLAG-SERVER")
        .expect("the environment overrode the flag");
}

#[test]
fn the_endpoint_in_use_is_shown() {
    // Sending requests somewhere other than the default without saying so would
    // make a misconfigured endpoint very hard to notice.
    let server = StubServer::start("ok");

    let app = TerminalApp::builder()
        .args([
            "--base-url",
            &server.url(),
            "-m",
            "openai:local-model",
            "-p",
            "hello",
        ])
        .env("OPENAI_API_KEY", "not-needed")
        .spawn()
        .expect("failed to spawn");

    app.wait_for(&server.url())
        .expect("the endpoint in use was never displayed");
}

#[test]
fn an_unknown_provider_says_how_to_reach_a_custom_server() {
    // A bare "unknown provider" leaves the user stuck. The provider selects the
    // wire protocol, so the fix is to name the protocol and redirect it.
    let app = TerminalApp::builder()
        .args(["-m", "mycompany:internal-7b", "-p", "hello"])
        .spawn()
        .expect("failed to spawn");

    app.wait_for("unknown provider")
        .expect("an unknown provider was not reported");
    app.wait_for("--base-url")
        .expect("the error did not say how to reach a custom server");
    app.wait_for("openai:internal-7b")
        .expect("the error did not suggest the corrected command");
}

#[test]
fn an_empty_base_url_is_rejected() {
    let app = TerminalApp::builder()
        .args(["--base-url", "", "-p", "hello"])
        .spawn()
        .expect("failed to spawn");

    app.wait_for("cannot be empty")
        .expect("an empty endpoint was accepted");
}

#[test]
fn usage_from_a_real_response_is_reported() {
    // The stub reports token counts, so this covers the summary path that the
    // scripted model cannot: real numbers rather than "not reported".
    let server = StubServer::start("counted");

    let app = TerminalApp::builder()
        .args([
            "--base-url",
            &server.url(),
            "-m",
            "openai:local-model",
            "-p",
            "hello",
        ])
        .env("OPENAI_API_KEY", "not-needed")
        .spawn()
        .expect("failed to spawn");

    app.wait_for("18 tokens")
        .expect("token usage from the provider was not reported");
    app.wait_for("(11 in, 7 out)")
        .expect("the split between input and output was not reported");
}
