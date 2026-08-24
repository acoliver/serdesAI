//! OAuth Authentication Flows for ChatGPT and Claude

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::Deserialize;

/// OAuth provider configuration
#[derive(Debug, Clone)]
pub struct OAuthProvider {
    pub name: String,
    pub auth_url: String,
    pub token_url: String,
    pub client_id: String,
    pub scope: String,
    pub redirect_port: u16,
}

/// OAuth token response
#[derive(Debug, Clone, Deserialize)]
pub struct OAuthToken {
    pub access_token: String,
    pub token_type: String,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub refresh_token: Option<String>,
}

/// OAuth state for pending flows
pub struct OAuthState {
    pub provider: String,
    pub code_verifier: String,
    pub state: String,
    pub start_time: Instant,
}

/// ChatGPT OAuth configuration
pub fn chatgpt_oauth_config() -> OAuthProvider {
    OAuthProvider {
        name: "ChatGPT".to_string(),
        auth_url: "https://auth.openai.com/authorize".to_string(),
        token_url: "https://auth.openai.com/token".to_string(),
        client_id: "chatgpt-desktop".to_string(),
        scope: "openid profile email offline_access".to_string(),
        redirect_port: 8080,
    }
}

/// Claude OAuth configuration
pub fn claude_oauth_config() -> OAuthProvider {
    OAuthProvider {
        name: "Claude".to_string(),
        auth_url: "https://claude.ai/login".to_string(),
        token_url: "https://claude.ai/api/auth/token".to_string(),
        client_id: "claude-desktop".to_string(),
        scope: "read write".to_string(),
        redirect_port: 8081,
    }
}

/// Start OAuth flow for a provider
pub async fn start_oauth_flow(provider: &OAuthProvider) -> Result<OAuthToken> {
    // Generate PKCE parameters
    let code_verifier = generate_code_verifier();
    let code_challenge = generate_code_challenge(&code_verifier);
    let state = generate_state();

    // Build authorization URL
    let auth_url = format!(
        "{}?client_id={}&redirect_uri=http://localhost:{}/callback&response_type=code&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        provider.auth_url,
        provider.client_id,
        provider.redirect_port,
        urlencoding::encode(&provider.scope),
        state,
        code_challenge
    );

    // Open browser
    println!("Opening browser for {} authentication...", provider.name);
    open::that(&auth_url)?;

    // Start local server to receive callback
    let (code, received_state) =
        receive_oauth_callback(provider.redirect_port, Duration::from_secs(300)).await?;

    // Verify state
    if received_state != state {
        return Err(anyhow!("OAuth state mismatch - possible CSRF attack"));
    }

    // Exchange code for token
    let token = exchange_code_for_token(provider, &code, &code_verifier).await?;

    Ok(token)
}

/// Receive OAuth callback via local HTTP server
async fn receive_oauth_callback(port: u16, timeout: Duration) -> Result<(String, String)> {
    let listener = TcpListener::bind(format!("127.0.0.1:{}", port))?;
    let start_time = Instant::now();

    listener.set_nonblocking(true)?;

    loop {
        if start_time.elapsed() > timeout {
            return Err(anyhow!("OAuth timeout - authentication took too long"));
        }

        match listener.accept() {
            Ok((stream, _)) => {
                if let Some((code, state)) = handle_oauth_request(stream)? {
                    return Ok((code, state));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Handle individual OAuth HTTP request
fn handle_oauth_request(mut stream: TcpStream) -> Result<Option<(String, String)>> {
    let buf_reader = BufReader::new(&stream);
    let request_line = buf_reader.lines().next().transpose()?;

    if let Some(line) = request_line {
        // Parse GET /callback?code=...&state=... HTTP/1.1
        if line.starts_with("GET /callback") {
            if let Some(query_start) = line.find('?') {
                if let Some(space_pos) = line.find(" HTTP/") {
                    let query = &line[query_start + 1..space_pos];
                    let params: HashMap<String, String> = query
                        .split('&')
                        .filter_map(|p| {
                            let mut parts = p.splitn(2, '=');
                            Some((
                                parts.next()?.to_string(),
                                parts.next().unwrap_or("").to_string(),
                            ))
                        })
                        .collect();

                    let code = params.get("code").cloned().unwrap_or_default();
                    let state = params.get("state").cloned().unwrap_or_default();

                    // Send success response
                    let response = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n<!DOCTYPE html><html><body><h1>Authentication successful!</h1><p>You can close this tab and return to the CLI.</p></body></html>";
                    stream.write_all(response.as_bytes())?;

                    return Ok(Some((code, state)));
                }
            }
        }
    }

    // Send error response
    let response = "HTTP/1.1 400 Bad Request\r\n\r\nInvalid request";
    stream.write_all(response.as_bytes())?;

    Ok(None)
}

/// Exchange authorization code for access token
async fn exchange_code_for_token(
    provider: &OAuthProvider,
    code: &str,
    code_verifier: &str,
) -> Result<OAuthToken> {
    let client = reqwest::Client::new();
    let redirect_uri = format!("http://localhost:{}/callback", provider.redirect_port);

    let params = [
        ("grant_type", "authorization_code"),
        ("client_id", provider.client_id.as_str()),
        ("code", code),
        ("redirect_uri", redirect_uri.as_str()),
        ("code_verifier", code_verifier),
    ];

    let response = client
        .post(&provider.token_url)
        .form(&params)
        .send()
        .await?;

    if !response.status().is_success() {
        let error_text = response.text().await?;
        return Err(anyhow!("Token exchange failed: {}", error_text));
    }

    let token: OAuthToken = response.json().await?;
    Ok(token)
}

/// Generate PKCE code verifier
fn generate_code_verifier() -> String {
    use rand::Rng;

    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let mut rng = rand::thread_rng();
    (0..128)
        .map(|_| CHARSET[rng.gen_range(0..CHARSET.len())] as char)
        .collect()
}

/// Generate PKCE code challenge from verifier
fn generate_code_challenge(verifier: &str) -> String {
    use sha2::{Digest, Sha256};

    let hash = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hash)
}

/// Generate random state parameter
fn generate_state() -> String {
    use rand::Rng;

    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..32)
        .map(|_| CHARSET[rng.gen_range(0..CHARSET.len())] as char)
        .collect()
}

/// Save OAuth token to config
pub fn save_oauth_token(provider: &str, token: &OAuthToken) -> Result<()> {
    // Store in config under oauth_tokens.{provider}
    crate::config::set_oauth_token(provider, &token.access_token)?;
    Ok(())
}

/// Load OAuth token from config
pub fn load_oauth_token(provider: &str) -> Option<String> {
    crate::config::get_oauth_token(provider)
}

/// Check if OAuth token exists
pub fn has_oauth_token(provider: &str) -> bool {
    load_oauth_token(provider).is_some()
}

/// Run ChatGPT OAuth flow
pub async fn run_chatgpt_oauth() -> Result<()> {
    let config = chatgpt_oauth_config();
    let token = start_oauth_flow(&config).await?;
    save_oauth_token("chatgpt", &token)?;
    println!("✅ ChatGPT authentication successful!");
    Ok(())
}

/// Run Claude OAuth flow
pub async fn run_claude_oauth() -> Result<()> {
    let config = claude_oauth_config();
    let token = start_oauth_flow(&config).await?;
    save_oauth_token("claude", &token)?;
    println!("✅ Claude authentication successful!");
    Ok(())
}

/// Initiate OAuth flow for a provider by name
pub async fn initiate_oauth(provider_name: &str) -> Result<()> {
    match provider_name.to_lowercase().as_str() {
        "chatgpt" | "openai" => run_chatgpt_oauth().await,
        "claude" | "anthropic" => run_claude_oauth().await,
        _ => Err(anyhow!("Unknown OAuth provider: {}", provider_name)),
    }
}
