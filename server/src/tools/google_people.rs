use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Method;
use serde_json::Value;
use synaptic::core::{SynapticError, Tool};

use super::google_api::GoogleApiClient;

const BASE_URL: &str = "https://people.googleapis.com/v1";
const LIST_FIELDS: &str = "names,nicknames,emailAddresses,phoneNumbers,organizations,addresses,birthdays,photos";
const DETAIL_FIELDS: &str = "names,nicknames,emailAddresses,phoneNumbers,organizations,addresses,birthdays,biographies,photos,urls,relations,events,occupations,userDefined,imClients,sipAddresses,calendarUrls,externalIds,locations,memberships,miscKeywords,fileAses,genders,interests";

// ---------------------------------------------------------------------------
// List Contacts
// ---------------------------------------------------------------------------

pub struct ListContacts {
    api: Arc<GoogleApiClient>,
}

impl ListContacts {
    pub fn new(api: Arc<GoogleApiClient>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl Tool for ListContacts {
    fn name(&self) -> &'static str {
        "list_contacts"
    }

    fn description(&self) -> &'static str {
        "List the user's Google contacts. Returns resourceName, names, emails, phones, organizations, addresses, birthdays."
    }

    fn parameters(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "page_size": {
                    "type": "integer",
                    "description": "Number of contacts to return (default 20, max 1000).",
                    "default": 20
                },
                "sort_order": {
                    "type": "string",
                    "description": "Sort order: 'LAST_MODIFIED_ASCENDING', 'LAST_MODIFIED_DESCENDING', 'FIRST_NAME_ASCENDING', or 'LAST_NAME_ASCENDING'.",
                    "default": "LAST_NAME_ASCENDING"
                }
            }
        }))
    }

    async fn call(&self, args: Value) -> Result<Value, SynapticError> {
        let page_size = args["page_size"].as_u64().unwrap_or(20).min(1000);
        let sort_order = args["sort_order"].as_str().unwrap_or("LAST_NAME_ASCENDING");

        let url = format!(
            "{}/people/me/connections?pageSize={}&sortOrder={}&personFields={}",
            BASE_URL, page_size, urlencoding::encode(sort_order), LIST_FIELDS
        );

        self.api.call(Method::GET, &url, None).await
    }
}

// ---------------------------------------------------------------------------
// Search Contacts
// ---------------------------------------------------------------------------

pub struct SearchContacts {
    api: Arc<GoogleApiClient>,
}

impl SearchContacts {
    pub fn new(api: Arc<GoogleApiClient>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl Tool for SearchContacts {
    fn name(&self) -> &'static str {
        "search_contacts"
    }

    fn description(&self) -> &'static str {
        "Search contacts by name, email, or phone. Use the returned resourceName (e.g. 'people/c12345') with get_contact for full details."
    }

    fn parameters(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query (name, email, or phone number). Required."
                },
                "page_size": {
                    "type": "integer",
                    "description": "Number of results to return (default 10, max 30).",
                    "default": 10
                }
            },
            "required": ["query"]
        }))
    }

    async fn call(&self, args: Value) -> Result<Value, SynapticError> {
        let query = args["query"]
            .as_str()
            .ok_or_else(|| SynapticError::Tool("query is required".into()))?;
        let page_size = args["page_size"].as_u64().unwrap_or(10).min(30);

        let url = format!(
            "{}/people:searchContacts?query={}&pageSize={}&readMask={}",
            BASE_URL,
            urlencoding::encode(query),
            page_size,
            LIST_FIELDS
        );

        self.api.call(Method::GET, &url, None).await
    }
}

// ---------------------------------------------------------------------------
// Get Contact
// ---------------------------------------------------------------------------

pub struct GetContact {
    api: Arc<GoogleApiClient>,
}

impl GetContact {
    pub fn new(api: Arc<GoogleApiClient>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl Tool for GetContact {
    fn name(&self) -> &'static str {
        "get_contact"
    }

    fn description(&self) -> &'static str {
        "Get all details of a contact. Requires resourceName from search_contacts or list_contacts (e.g. 'people/c12345')."
    }

    fn parameters(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "resource_name": {
                    "type": "string",
                    "description": "The contact's resource name (e.g. 'people/c12345'). Required."
                }
            },
            "required": ["resource_name"]
        }))
    }

    async fn call(&self, args: Value) -> Result<Value, SynapticError> {
        let resource_name = args["resource_name"]
            .as_str()
            .ok_or_else(|| SynapticError::Tool("resource_name is required".into()))?;

        // Strip leading slash if present, and ensure it starts with "people/"
        let resource_name = resource_name.trim_start_matches('/');
        tracing::debug!(resource_name, "get_contact called");

        let url = format!(
            "{}/{}?personFields={}",
            BASE_URL,
            resource_name,
            DETAIL_FIELDS
        );

        self.api.call(Method::GET, &url, None).await
    }
}

/// Create all Google People (Contacts) tools with a shared API client.
pub fn create_tools(api: Arc<GoogleApiClient>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ListContacts::new(api.clone())),
        Arc::new(SearchContacts::new(api.clone())),
        Arc::new(GetContact::new(api)),
    ]
}
