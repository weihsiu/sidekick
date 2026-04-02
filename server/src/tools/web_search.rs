use async_trait::async_trait;
use reqwest::Client;
use scraper::{Html, Selector};
use serde_json::Value;
use synaptic::core::{SynapticError, Tool};

const DDG_URL: &str = "https://html.duckduckgo.com/html/";

pub struct WebSearch {
    http: Client,
}

impl WebSearch {
    pub fn new() -> Self {
        Self {
            http: Client::builder()
                .user_agent("Mozilla/5.0 (compatible; Sidekick/1.0)")
                .build()
                .expect("failed to build HTTP client"),
        }
    }
}

#[async_trait]
impl Tool for WebSearch {
    fn name(&self) -> &'static str {
        "web_search"
    }

    fn description(&self) -> &'static str {
        "Search the web using DuckDuckGo. Returns titles, URLs, and snippets for the top results. \
         Use this ONLY for information that changes over time or requires real-time data: \
         current news, live prices, recent events, or verifying something time-sensitive. \
         Do NOT use for general knowledge, definitions, or anything you can answer from training data."
    }

    fn parameters(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query."
                },
                "num": {
                    "type": "integer",
                    "description": "Number of results to return (1-10, default 5).",
                    "default": 5
                }
            },
            "required": ["query"]
        }))
    }

    async fn call(&self, args: Value) -> Result<Value, SynapticError> {
        let query = args["query"]
            .as_str()
            .ok_or_else(|| SynapticError::Tool("query is required".into()))?;

        let num = args["num"].as_u64().unwrap_or(5).clamp(1, 10) as usize;

        let html = self
            .http
            .get(DDG_URL)
            .query(&[("q", query)])
            .send()
            .await
            .map_err(|e| SynapticError::Tool(format!("search request failed: {e}")))?
            .text()
            .await
            .map_err(|e| SynapticError::Tool(format!("failed to read response: {e}")))?;

        let document = Html::parse_document(&html);
        let result_sel = Selector::parse(".result").unwrap();
        let title_sel = Selector::parse(".result__a").unwrap();
        let snippet_sel = Selector::parse(".result__snippet").unwrap();
        let url_sel = Selector::parse(".result__url").unwrap();

        let results: Vec<Value> = document
            .select(&result_sel)
            .filter_map(|result| {
                let title = result.select(&title_sel).next()?.text().collect::<String>();
                let title = title.trim().to_string();
                if title.is_empty() {
                    return None;
                }
                let snippet = result
                    .select(&snippet_sel)
                    .next()
                    .map(|el| el.text().collect::<String>().trim().to_string())
                    .unwrap_or_default();
                let url = result
                    .select(&url_sel)
                    .next()
                    .map(|el| el.text().collect::<String>().trim().to_string())
                    .unwrap_or_default();
                Some(serde_json::json!({ "title": title, "url": url, "snippet": snippet }))
            })
            .take(num)
            .collect();

        if results.is_empty() {
            return Ok(serde_json::json!("No results found."));
        }

        Ok(serde_json::json!(results))
    }
}
