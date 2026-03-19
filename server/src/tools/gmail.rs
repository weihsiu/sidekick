use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Method;
use serde_json::Value;
use synaptic::core::{SynapticError, Tool};

use super::google_api::GoogleApiClient;

const BASE_URL: &str = "https://gmail.googleapis.com/gmail/v1/users/me";

// ---------------------------------------------------------------------------
// List Messages
// ---------------------------------------------------------------------------

pub struct ListGmailMessages {
    api: Arc<GoogleApiClient>,
}

impl ListGmailMessages {
    pub fn new(api: Arc<GoogleApiClient>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl Tool for ListGmailMessages {
    fn name(&self) -> &'static str {
        "list_gmail_messages"
    }

    fn description(&self) -> &'static str {
        "Search or list messages in the user's Gmail. Returns message IDs and thread IDs. \
         Use a Gmail search query to filter (e.g. 'from:alice subject:meeting is:unread')."
    }

    fn parameters(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Gmail search query (e.g. 'is:unread', 'from:bob', 'subject:invoice after:2026/03/01'). Defaults to all messages."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of messages to return (default 10, max 100).",
                    "default": 10
                },
                "label_ids": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Only return messages with these label IDs (e.g. ['INBOX', 'UNREAD'])."
                }
            }
        }))
    }

    async fn call(&self, args: Value) -> Result<Value, SynapticError> {
        let max_results = args["max_results"].as_u64().unwrap_or(10).min(100);
        let mut url = format!("{}/messages?maxResults={}", BASE_URL, max_results);

        if let Some(q) = args["query"].as_str() {
            url.push_str(&format!("&q={}", urlencoding::encode(q)));
        }
        if let Some(labels) = args["label_ids"].as_array() {
            for label in labels.iter().filter_map(|l| l.as_str()) {
                url.push_str(&format!("&labelIds={}", urlencoding::encode(label)));
            }
        }

        self.api.call(Method::GET, &url, None).await
    }
}

// ---------------------------------------------------------------------------
// Get Message
// ---------------------------------------------------------------------------

pub struct GetGmailMessage {
    api: Arc<GoogleApiClient>,
}

impl GetGmailMessage {
    pub fn new(api: Arc<GoogleApiClient>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl Tool for GetGmailMessage {
    fn name(&self) -> &'static str {
        "get_gmail_message"
    }

    fn description(&self) -> &'static str {
        "Get the full content of a specific Gmail message by its ID. \
         Returns headers (from, to, subject, date) and body."
    }

    fn parameters(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "message_id": {
                    "type": "string",
                    "description": "The message ID to retrieve. Required."
                },
                "format": {
                    "type": "string",
                    "description": "Response format: 'full', 'metadata', or 'minimal'. Default 'full'.",
                    "default": "full"
                }
            },
            "required": ["message_id"]
        }))
    }

    async fn call(&self, args: Value) -> Result<Value, SynapticError> {
        let message_id = args["message_id"]
            .as_str()
            .ok_or_else(|| SynapticError::Tool("message_id is required".into()))?;
        let format = args["format"].as_str().unwrap_or("full");

        let url = format!(
            "{}/messages/{}?format={}",
            BASE_URL,
            urlencoding::encode(message_id),
            urlencoding::encode(format)
        );

        self.api.call(Method::GET, &url, None).await
    }
}

// ---------------------------------------------------------------------------
// Send Message
// ---------------------------------------------------------------------------

pub struct SendGmailMessage {
    api: Arc<GoogleApiClient>,
}

impl SendGmailMessage {
    pub fn new(api: Arc<GoogleApiClient>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl Tool for SendGmailMessage {
    fn name(&self) -> &'static str {
        "send_gmail_message"
    }

    fn description(&self) -> &'static str {
        "Send an email from the user's Gmail account."
    }

    fn parameters(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "to": {
                    "type": "string",
                    "description": "Recipient email address. Required."
                },
                "subject": {
                    "type": "string",
                    "description": "Email subject. Required."
                },
                "body": {
                    "type": "string",
                    "description": "Email body (plain text). Required."
                },
                "cc": {
                    "type": "string",
                    "description": "CC recipient email address."
                },
                "bcc": {
                    "type": "string",
                    "description": "BCC recipient email address."
                }
            },
            "required": ["to", "subject", "body"]
        }))
    }

    async fn call(&self, args: Value) -> Result<Value, SynapticError> {
        let to = args["to"]
            .as_str()
            .ok_or_else(|| SynapticError::Tool("to is required".into()))?;
        let subject = args["subject"]
            .as_str()
            .ok_or_else(|| SynapticError::Tool("subject is required".into()))?;
        let body = args["body"]
            .as_str()
            .ok_or_else(|| SynapticError::Tool("body is required".into()))?;

        let mut headers = format!("To: {}\r\nSubject: {}\r\n", to, subject);
        if let Some(cc) = args["cc"].as_str() {
            headers.push_str(&format!("Cc: {}\r\n", cc));
        }
        if let Some(bcc) = args["bcc"].as_str() {
            headers.push_str(&format!("Bcc: {}\r\n", bcc));
        }
        headers.push_str("Content-Type: text/plain; charset=utf-8\r\n");

        let raw = format!("{}\r\n{}", headers, body);
        use base64::Engine;
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw.as_bytes());

        let payload = serde_json::json!({ "raw": encoded });
        let url = format!("{}/messages/send", BASE_URL);

        self.api.call(Method::POST, &url, Some(&payload)).await
    }
}

// ---------------------------------------------------------------------------
// Modify Message Labels
// ---------------------------------------------------------------------------

pub struct ModifyGmailMessage {
    api: Arc<GoogleApiClient>,
}

impl ModifyGmailMessage {
    pub fn new(api: Arc<GoogleApiClient>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl Tool for ModifyGmailMessage {
    fn name(&self) -> &'static str {
        "modify_gmail_message"
    }

    fn description(&self) -> &'static str {
        "Modify labels on a Gmail message. Use this to mark as read/unread, \
         archive, star, or move to trash. Common labels: UNREAD, STARRED, TRASH, INBOX."
    }

    fn parameters(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "message_id": {
                    "type": "string",
                    "description": "The message ID to modify. Required."
                },
                "add_labels": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Label IDs to add (e.g. ['STARRED', 'UNREAD'])."
                },
                "remove_labels": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Label IDs to remove (e.g. ['UNREAD'] to mark as read)."
                }
            },
            "required": ["message_id"]
        }))
    }

    async fn call(&self, args: Value) -> Result<Value, SynapticError> {
        let message_id = args["message_id"]
            .as_str()
            .ok_or_else(|| SynapticError::Tool("message_id is required".into()))?;

        let mut body = serde_json::Map::new();
        if let Some(add) = args["add_labels"].as_array() {
            body.insert("addLabelIds".into(), Value::Array(add.clone()));
        }
        if let Some(remove) = args["remove_labels"].as_array() {
            body.insert("removeLabelIds".into(), Value::Array(remove.clone()));
        }

        let url = format!(
            "{}/messages/{}/modify",
            BASE_URL,
            urlencoding::encode(message_id)
        );

        self.api
            .call(Method::POST, &url, Some(&Value::Object(body)))
            .await
    }
}

// ---------------------------------------------------------------------------
// List Labels
// ---------------------------------------------------------------------------

pub struct ListGmailLabels {
    api: Arc<GoogleApiClient>,
}

impl ListGmailLabels {
    pub fn new(api: Arc<GoogleApiClient>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl Tool for ListGmailLabels {
    fn name(&self) -> &'static str {
        "list_gmail_labels"
    }

    fn description(&self) -> &'static str {
        "List all labels in the user's Gmail account."
    }

    fn parameters(&self) -> Option<Value> {
        None
    }

    async fn call(&self, _args: Value) -> Result<Value, SynapticError> {
        let url = format!("{}/labels", BASE_URL);
        self.api.call(Method::GET, &url, None).await
    }
}

/// Create all Gmail tools with a shared API client.
pub fn create_tools(api: Arc<GoogleApiClient>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ListGmailMessages::new(api.clone())),
        Arc::new(GetGmailMessage::new(api.clone())),
        Arc::new(SendGmailMessage::new(api.clone())),
        Arc::new(ModifyGmailMessage::new(api.clone())),
        Arc::new(ListGmailLabels::new(api)),
    ]
}
