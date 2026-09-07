//! Web search tool with DuckDuckGo integration.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serdes_ai_tools::{RunContext, Tool, ToolDefinition, ToolError, ToolResult, ToolReturn};

const DUCKDUCKGO_URL: &str = "https://html.duckduckgo.com/html/";

#[derive(Debug, Clone)]
pub struct DuckDuckGoSearchTool;

#[derive(Debug, Deserialize, Serialize)]
pub struct SearchArgs {
    query: String,
    #[serde(default)]
    max_results: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

impl DuckDuckGoSearchTool {
    pub fn new() -> Self {
        Self
    }

    async fn search(&self, query: &str, max_results: usize) -> Result<Vec<SearchResult>> {
        let client = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36")
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        let params = [("q", query), ("kl", "us-en")];

        let response = client.post(DUCKDUCKGO_URL).form(&params).send().await?;

        let html = response.text().await?;
        self.parse_results(&html, max_results)
    }

    fn parse_results(&self, html: &str, max_results: usize) -> Result<Vec<SearchResult>> {
        let document = scraper::Html::parse_document(html);
        let result_selector = scraper::Selector::parse(".result").unwrap();
        let title_selector = scraper::Selector::parse(".result__title a").unwrap();
        let snippet_selector = scraper::Selector::parse(".result__snippet").unwrap();

        let mut results = Vec::new();

        for element in document.select(&result_selector).take(max_results) {
            let title = element
                .select(&title_selector)
                .next()
                .map(|e| e.text().collect::<String>().trim().to_string())
                .unwrap_or_default();

            let url = element
                .select(&title_selector)
                .next()
                .and_then(|e| e.value().attr("href"))
                .map(|s| s.to_string())
                .unwrap_or_default();

            let snippet = element
                .select(&snippet_selector)
                .next()
                .map(|e| e.text().collect::<String>().trim().to_string())
                .unwrap_or_default();

            if !title.is_empty() && !url.is_empty() {
                results.push(SearchResult {
                    title,
                    url,
                    snippet,
                });
            }
        }

        Ok(results)
    }
}

#[async_trait::async_trait]
impl Tool for DuckDuckGoSearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "web_search",
            "Search the web for information using DuckDuckGo. Returns titles, URLs, and snippets of search results.",
        )
            .with_parameters(serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query"
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum number of results (1-10, default 5)",
                        "minimum": 1,
                        "maximum": 10
                    }
                },
                "required": ["query"]
            }))
    }

    async fn call(&self, _ctx: &RunContext, args: Value) -> ToolResult {
        let args: SearchArgs = serde_json::from_value(args).map_err(|e| {
            ToolError::invalid_arguments("web_search", format!("Invalid arguments: {}", e))
        })?;

        if args.query.trim().is_empty() {
            return Err(ToolError::invalid_arguments(
                "web_search",
                "Query cannot be empty",
            ));
        }

        let max_results = args.max_results.unwrap_or(5).clamp(1, 10);

        match self.search(&args.query, max_results).await {
            Ok(results) => {
                let formatted = format_results(&results);
                Ok(ToolReturn::json(serde_json::json!({
                    "query": args.query,
                    "results_count": results.len(),
                    "results": results,
                    "formatted": formatted
                })))
            }
            Err(e) => Err(ToolError::execution_failed(format!("Search failed: {}", e))),
        }
    }
}

fn format_results(results: &[SearchResult]) -> String {
    let mut output = String::new();
    output.push_str(&format!("Found {} results:\n\n", results.len()));

    for (i, result) in results.iter().enumerate() {
        output.push_str(&format!("{}. {}\n", i + 1, result.title));
        output.push_str(&format!("   URL: {}\n", result.url));
        if !result.snippet.is_empty() {
            output.push_str(&format!("   {}\n", result.snippet));
        }
        output.push('\n');
    }

    output
}

/// Tavily search for higher quality results (if API key available)
#[allow(dead_code)]
pub struct TavilySearchTool {
    api_key: String,
}

impl TavilySearchTool {
    #[allow(dead_code)]
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }
}

#[async_trait::async_trait]
impl Tool for TavilySearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "web_search_tavily",
            "High-quality AI search using Tavily. Requires TAVILY_API_KEY.",
        )
        .with_parameters(serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" },
                "max_results": { "type": "integer", "minimum": 1, "maximum": 10 }
            },
            "required": ["query"]
        }))
    }

    async fn call(&self, _ctx: &RunContext, args: Value) -> ToolResult {
        let args: SearchArgs = serde_json::from_value(args).map_err(|e| {
            ToolError::invalid_arguments("web_search_tavily", format!("Invalid arguments: {}", e))
        })?;

        let client = reqwest::Client::new();
        let response = client
            .post("https://api.tavily.com/search")
            .json(&serde_json::json!({
                "api_key": self.api_key,
                "query": args.query,
                "max_results": args.max_results.unwrap_or(5),
                "search_depth": "basic"
            }))
            .send()
            .await
            .map_err(|e| ToolError::execution_failed(format!("Request failed: {}", e)))?;

        let result: Value = response
            .json()
            .await
            .map_err(|e| ToolError::execution_failed(format!("Parse failed: {}", e)))?;

        Ok(ToolReturn::json(result))
    }
}
