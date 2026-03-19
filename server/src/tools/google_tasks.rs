use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Method;
use serde_json::Value;
use synaptic::core::{SynapticError, Tool};

use super::google_api::GoogleApiClient;

const BASE_URL: &str = "https://tasks.googleapis.com/tasks/v1";

// ---------------------------------------------------------------------------
// List Task Lists
// ---------------------------------------------------------------------------

pub struct ListTaskLists {
    api: Arc<GoogleApiClient>,
}

impl ListTaskLists {
    pub fn new(api: Arc<GoogleApiClient>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl Tool for ListTaskLists {
    fn name(&self) -> &'static str {
        "list_task_lists"
    }

    fn description(&self) -> &'static str {
        "List all task lists in the user's Google Tasks."
    }

    fn parameters(&self) -> Option<Value> {
        None
    }

    async fn call(&self, _args: Value) -> Result<Value, SynapticError> {
        let url = format!("{}/users/@me/lists", BASE_URL);
        self.api.call(Method::GET, &url, None).await
    }
}

// ---------------------------------------------------------------------------
// List Tasks
// ---------------------------------------------------------------------------

pub struct ListTasks {
    api: Arc<GoogleApiClient>,
}

impl ListTasks {
    pub fn new(api: Arc<GoogleApiClient>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl Tool for ListTasks {
    fn name(&self) -> &'static str {
        "list_tasks"
    }

    fn description(&self) -> &'static str {
        "List tasks in a Google Tasks task list. Can filter by completion status."
    }

    fn parameters(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "task_list_id": {
                    "type": "string",
                    "description": "Task list ID. Use '@default' for the default list.",
                    "default": "@default"
                },
                "show_completed": {
                    "type": "boolean",
                    "description": "Whether to include completed tasks. Default true.",
                    "default": true
                },
                "show_hidden": {
                    "type": "boolean",
                    "description": "Whether to include hidden/deleted tasks. Default false.",
                    "default": false
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of tasks to return (default 20, max 100).",
                    "default": 20
                },
                "due_min": {
                    "type": "string",
                    "description": "Lower bound for task due date (RFC 3339)."
                },
                "due_max": {
                    "type": "string",
                    "description": "Upper bound for task due date (RFC 3339)."
                }
            }
        }))
    }

    async fn call(&self, args: Value) -> Result<Value, SynapticError> {
        let list_id = args["task_list_id"].as_str().unwrap_or("@default");
        let max_results = args["max_results"].as_u64().unwrap_or(20).min(100);
        let show_completed = args["show_completed"].as_bool().unwrap_or(true);
        let show_hidden = args["show_hidden"].as_bool().unwrap_or(false);

        let mut url = format!(
            "{}/lists/{}/tasks?maxResults={}&showCompleted={}&showHidden={}",
            BASE_URL,
            urlencoding::encode(list_id),
            max_results,
            show_completed,
            show_hidden
        );

        if let Some(due_min) = args["due_min"].as_str() {
            url.push_str(&format!("&dueMin={}", urlencoding::encode(due_min)));
        }
        if let Some(due_max) = args["due_max"].as_str() {
            url.push_str(&format!("&dueMax={}", urlencoding::encode(due_max)));
        }

        self.api.call(Method::GET, &url, None).await
    }
}

// ---------------------------------------------------------------------------
// Create Task
// ---------------------------------------------------------------------------

pub struct CreateTask {
    api: Arc<GoogleApiClient>,
}

impl CreateTask {
    pub fn new(api: Arc<GoogleApiClient>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl Tool for CreateTask {
    fn name(&self) -> &'static str {
        "create_task"
    }

    fn description(&self) -> &'static str {
        "Create a new task in a Google Tasks task list."
    }

    fn parameters(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "task_list_id": {
                    "type": "string",
                    "description": "Task list ID. Use '@default' for the default list.",
                    "default": "@default"
                },
                "title": {
                    "type": "string",
                    "description": "Task title. Required."
                },
                "notes": {
                    "type": "string",
                    "description": "Task notes/description."
                },
                "due": {
                    "type": "string",
                    "description": "Due date in RFC 3339 format (e.g. '2026-03-20T00:00:00Z'). Note: Google Tasks only supports date-level granularity — the time portion is ignored. For time-specific items, use create_calendar_event instead."
                }
            },
            "required": ["title"]
        }))
    }

    async fn call(&self, args: Value) -> Result<Value, SynapticError> {
        let list_id = args["task_list_id"].as_str().unwrap_or("@default");
        let title = args["title"]
            .as_str()
            .ok_or_else(|| SynapticError::Tool("title is required".into()))?;

        let mut task = serde_json::json!({ "title": title });
        if let Some(notes) = args["notes"].as_str() {
            task["notes"] = Value::String(notes.to_string());
        }
        if let Some(due) = args["due"].as_str() {
            task["due"] = Value::String(due.to_string());
        }

        let url = format!(
            "{}/lists/{}/tasks",
            BASE_URL,
            urlencoding::encode(list_id)
        );

        self.api.call(Method::POST, &url, Some(&task)).await
    }
}

// ---------------------------------------------------------------------------
// Update Task
// ---------------------------------------------------------------------------

pub struct UpdateTask {
    api: Arc<GoogleApiClient>,
}

impl UpdateTask {
    pub fn new(api: Arc<GoogleApiClient>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl Tool for UpdateTask {
    fn name(&self) -> &'static str {
        "update_task"
    }

    fn description(&self) -> &'static str {
        "Update an existing task in Google Tasks. Can change title, notes, due date, or status."
    }

    fn parameters(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "task_list_id": {
                    "type": "string",
                    "description": "Task list ID. Use '@default' for the default list.",
                    "default": "@default"
                },
                "task_id": {
                    "type": "string",
                    "description": "The task ID to update. Required."
                },
                "title": {
                    "type": "string",
                    "description": "New task title."
                },
                "notes": {
                    "type": "string",
                    "description": "New task notes."
                },
                "due": {
                    "type": "string",
                    "description": "New due date in RFC 3339 format. Note: only the date portion is used — time is ignored by Google Tasks."
                },
                "status": {
                    "type": "string",
                    "description": "Task status: 'needsAction' or 'completed'."
                }
            },
            "required": ["task_id"]
        }))
    }

    async fn call(&self, args: Value) -> Result<Value, SynapticError> {
        let list_id = args["task_list_id"].as_str().unwrap_or("@default");
        let task_id = args["task_id"]
            .as_str()
            .ok_or_else(|| SynapticError::Tool("task_id is required".into()))?;

        let mut patch = serde_json::Map::new();
        if let Some(s) = args["title"].as_str() {
            patch.insert("title".into(), Value::String(s.to_string()));
        }
        if let Some(s) = args["notes"].as_str() {
            patch.insert("notes".into(), Value::String(s.to_string()));
        }
        if let Some(s) = args["due"].as_str() {
            patch.insert("due".into(), Value::String(s.to_string()));
        }
        if let Some(s) = args["status"].as_str() {
            patch.insert("status".into(), Value::String(s.to_string()));
        }

        let url = format!(
            "{}/lists/{}/tasks/{}",
            BASE_URL,
            urlencoding::encode(list_id),
            urlencoding::encode(task_id)
        );

        self.api
            .call(Method::PATCH, &url, Some(&Value::Object(patch)))
            .await
    }
}

// ---------------------------------------------------------------------------
// Delete Task
// ---------------------------------------------------------------------------

pub struct DeleteTask {
    api: Arc<GoogleApiClient>,
}

impl DeleteTask {
    pub fn new(api: Arc<GoogleApiClient>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl Tool for DeleteTask {
    fn name(&self) -> &'static str {
        "delete_task"
    }

    fn description(&self) -> &'static str {
        "Delete a task from a Google Tasks task list."
    }

    fn parameters(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "task_list_id": {
                    "type": "string",
                    "description": "Task list ID. Use '@default' for the default list.",
                    "default": "@default"
                },
                "task_id": {
                    "type": "string",
                    "description": "The task ID to delete. Required."
                }
            },
            "required": ["task_id"]
        }))
    }

    async fn call(&self, args: Value) -> Result<Value, SynapticError> {
        let list_id = args["task_list_id"].as_str().unwrap_or("@default");
        let task_id = args["task_id"]
            .as_str()
            .ok_or_else(|| SynapticError::Tool("task_id is required".into()))?;

        let url = format!(
            "{}/lists/{}/tasks/{}",
            BASE_URL,
            urlencoding::encode(list_id),
            urlencoding::encode(task_id)
        );

        self.api.call(Method::DELETE, &url, None).await
    }
}

/// Create all Google Tasks tools with a shared API client.
pub fn create_tools(api: Arc<GoogleApiClient>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ListTaskLists::new(api.clone())),
        Arc::new(ListTasks::new(api.clone())),
        Arc::new(CreateTask::new(api.clone())),
        Arc::new(UpdateTask::new(api.clone())),
        Arc::new(DeleteTask::new(api)),
    ]
}
