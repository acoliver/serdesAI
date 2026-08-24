//! Web fetch tool for retrieving URL content.

use anyhow::Result;
use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serdes_ai_tools::{RunContext, Tool, ToolDefinition, ToolError, ToolResult, ToolReturn};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct WebFetchTool;

#[derive(Debug, Deserialize, Serialize)]
pub struct FetchArgs {
    url: String,
    #[serde(default)]
    max_length: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct FetchResult {
    url: String,
    title: Option<String>,
    content: String,
    content_type: String,
    length: usize,
}

impl WebFetchTool {
    pub fn new() -> Self {
        Self
    }

    async fn fetch(&self, url: &str, max_length: usize) -> Result<FetchResult> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;

        let mut headers = HeaderMap::new();
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static("Mozilla/5.0 (compatible; SerdesAI/1.0)"),
        );

        let response = client.get(url).headers(headers).send().await?;

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("text/plain")
            .to_string();

        let final_url = response.url().to_string();

        // Handle different content types
        let content = if content_type.contains("text/html") {
            let html = response.text().await?;
            self.extract_text_from_html(&html, max_length)
        } else if content_type.contains("application/json") {
            let json: Value = response.json().await?;
            serde_json::to_string_pretty(&json)?
        } else {
            response.text().await?.chars().take(max_length).collect()
        };

        // Extract title from first line or HTML
        let title = content
            .lines()
            .next()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        Ok(FetchResult {
            url: final_url,
            title,
            content: content.chars().take(max_length).collect(),
            content_type,
            length: content.len().min(max_length),
        })
    }

    fn extract_text_from_html(&self, html: &str, max_length: usize) -> String {
        let document = scraper::Html::parse_document(html);

        // Try to get article content first
        let article_selector = scraper::Selector::parse("article, main, [role='main']").unwrap();
        if let Some(article) = document.select(&article_selector).next() {
            let text = article.text().collect::<String>();
            if text.len() > 100 {
                return self.clean_text(&text, max_length);
            }
        }

        // Fallback to body content
        let body_selector = scraper::Selector::parse("body").unwrap();
        if let Some(body) = document.select(&body_selector).next() {
            let text = body.text().collect::<String>();
            return self.clean_text(&text, max_length);
        }

        // Last resort: just clean the HTML
        self.clean_text(&html.replace(['<', '>'], " "), max_length)
    }

    fn clean_text(&self, text: &str, max_length: usize) -> String {
        text.lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
            .chars()
            .take(max_length)
            .collect()
    }
}

#[async_trait::async_trait]
impl Tool for WebFetchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "web_fetch",
            "Fetch and extract readable content from a web page. Returns the text content, title, and content type.",
        )
            .with_parameters(serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to fetch"
                    },
                    "max_length": {
                        "type": "integer",
                        "description": "Maximum characters to return (default 8000)",
                        "minimum": 100,
                        "maximum": 50000
                    }
                },
                "required": ["url"]
            }))
    }

    async fn call(&self, _ctx: &RunContext, args: Value) -> ToolResult {
        let args: FetchArgs = serde_json::from_value(args).map_err(|e| {
            ToolError::invalid_arguments("web_fetch", format!("Invalid arguments: {}", e))
        })?;

        // Validate URL
        if !args.url.starts_with("http://") && !args.url.starts_with("https://") {
            return Err(ToolError::invalid_arguments(
                "web_fetch",
                "URL must start with http:// or https://",
            ));
        }

        let max_length = args.max_length.unwrap_or(8000).clamp(100, 50000);

        match self.fetch(&args.url, max_length).await {
            Ok(result) => {
                let summary = format!(
                    "Fetched: {}\nTitle: {}\nType: {}\nLength: {} chars\n\n{}",
                    result.url,
                    result.title.as_deref().unwrap_or("N/A"),
                    result.content_type,
                    result.length,
                    result.content
                );

                Ok(ToolReturn::json(serde_json::json!({
                    "url": result.url,
                    "title": result.title,
                    "content_type": result.content_type,
                    "length": result.length,
                    "content": result.content,
                    "summary": summary
                })))
            }
            Err(e) => Err(ToolError::execution_failed(format!("Fetch failed: {}", e))),
        }
    }
}
