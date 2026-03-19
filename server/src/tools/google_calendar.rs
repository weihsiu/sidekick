use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Method;
use serde_json::Value;
use synaptic::core::{SynapticError, Tool};

use super::google_api::GoogleApiClient;

const BASE_URL: &str = "https://www.googleapis.com/calendar/v3";

// ---------------------------------------------------------------------------
// List Events
// ---------------------------------------------------------------------------

pub struct ListCalendarEvents {
    api: Arc<GoogleApiClient>,
}

impl ListCalendarEvents {
    pub fn new(api: Arc<GoogleApiClient>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl Tool for ListCalendarEvents {
    fn name(&self) -> &'static str {
        "list_calendar_events"
    }

    fn description(&self) -> &'static str {
        "List events from the user's Google Calendar within a time range. \
         Returns event summaries, times, locations, and attendees."
    }

    fn parameters(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "calendar_id": {
                    "type": "string",
                    "description": "Calendar ID. Use 'primary' for the user's main calendar.",
                    "default": "primary"
                },
                "time_min": {
                    "type": "string",
                    "description": "Start of time range (RFC 3339, e.g. '2026-03-18T00:00:00Z'). Required."
                },
                "time_max": {
                    "type": "string",
                    "description": "End of time range (RFC 3339). Required."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of events to return (default 10, max 250).",
                    "default": 10
                }
            },
            "required": ["time_min", "time_max"]
        }))
    }

    async fn call(&self, args: Value) -> Result<Value, SynapticError> {
        let cal_id = args["calendar_id"].as_str().unwrap_or("primary");
        let time_min = args["time_min"]
            .as_str()
            .ok_or_else(|| SynapticError::Tool("time_min is required".into()))?;
        let time_max = args["time_max"]
            .as_str()
            .ok_or_else(|| SynapticError::Tool("time_max is required".into()))?;
        let max_results = args["max_results"].as_u64().unwrap_or(10).min(250);

        let url = format!(
            "{}/calendars/{}/events?timeMin={}&timeMax={}&maxResults={}&singleEvents=true&orderBy=startTime",
            BASE_URL,
            urlencoding::encode(cal_id),
            urlencoding::encode(time_min),
            urlencoding::encode(time_max),
            max_results
        );

        self.api.call(Method::GET, &url, None).await
    }
}

// ---------------------------------------------------------------------------
// Create Event
// ---------------------------------------------------------------------------

pub struct CreateCalendarEvent {
    api: Arc<GoogleApiClient>,
}

impl CreateCalendarEvent {
    pub fn new(api: Arc<GoogleApiClient>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl Tool for CreateCalendarEvent {
    fn name(&self) -> &'static str {
        "create_calendar_event"
    }

    fn description(&self) -> &'static str {
        "Create a new event on the user's Google Calendar."
    }

    fn parameters(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "calendar_id": {
                    "type": "string",
                    "description": "Calendar ID. Use 'primary' for the user's main calendar.",
                    "default": "primary"
                },
                "summary": {
                    "type": "string",
                    "description": "Event title/summary. Required."
                },
                "description": {
                    "type": "string",
                    "description": "Event description or notes."
                },
                "start_time": {
                    "type": "string",
                    "description": "Start time in RFC 3339 format (e.g. '2026-03-20T10:00:00-07:00'). Required."
                },
                "end_time": {
                    "type": "string",
                    "description": "End time in RFC 3339 format. Required."
                },
                "location": {
                    "type": "string",
                    "description": "Event location."
                },
                "attendees": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "List of attendee email addresses."
                }
            },
            "required": ["summary", "start_time", "end_time"]
        }))
    }

    async fn call(&self, args: Value) -> Result<Value, SynapticError> {
        let cal_id = args["calendar_id"].as_str().unwrap_or("primary");
        let summary = args["summary"]
            .as_str()
            .ok_or_else(|| SynapticError::Tool("summary is required".into()))?;
        let start_time = args["start_time"]
            .as_str()
            .ok_or_else(|| SynapticError::Tool("start_time is required".into()))?;
        let end_time = args["end_time"]
            .as_str()
            .ok_or_else(|| SynapticError::Tool("end_time is required".into()))?;

        let mut event = serde_json::json!({
            "summary": summary,
            "start": { "dateTime": start_time },
            "end": { "dateTime": end_time },
        });

        if let Some(desc) = args["description"].as_str() {
            event["description"] = Value::String(desc.to_string());
        }
        if let Some(loc) = args["location"].as_str() {
            event["location"] = Value::String(loc.to_string());
        }
        if let Some(attendees) = args["attendees"].as_array() {
            let attendee_list: Vec<Value> = attendees
                .iter()
                .filter_map(|a| a.as_str())
                .map(|email| serde_json::json!({"email": email}))
                .collect();
            event["attendees"] = Value::Array(attendee_list);
        }

        let url = format!(
            "{}/calendars/{}/events",
            BASE_URL,
            urlencoding::encode(cal_id)
        );

        self.api.call(Method::POST, &url, Some(&event)).await
    }
}

// ---------------------------------------------------------------------------
// Get Event
// ---------------------------------------------------------------------------

pub struct GetCalendarEvent {
    api: Arc<GoogleApiClient>,
}

impl GetCalendarEvent {
    pub fn new(api: Arc<GoogleApiClient>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl Tool for GetCalendarEvent {
    fn name(&self) -> &'static str {
        "get_calendar_event"
    }

    fn description(&self) -> &'static str {
        "Get details of a specific event from the user's Google Calendar."
    }

    fn parameters(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "calendar_id": {
                    "type": "string",
                    "description": "Calendar ID. Use 'primary' for the user's main calendar.",
                    "default": "primary"
                },
                "event_id": {
                    "type": "string",
                    "description": "The event ID to retrieve. Required."
                }
            },
            "required": ["event_id"]
        }))
    }

    async fn call(&self, args: Value) -> Result<Value, SynapticError> {
        let cal_id = args["calendar_id"].as_str().unwrap_or("primary");
        let event_id = args["event_id"]
            .as_str()
            .ok_or_else(|| SynapticError::Tool("event_id is required".into()))?;

        let url = format!(
            "{}/calendars/{}/events/{}",
            BASE_URL,
            urlencoding::encode(cal_id),
            urlencoding::encode(event_id)
        );

        self.api.call(Method::GET, &url, None).await
    }
}

// ---------------------------------------------------------------------------
// Update Event
// ---------------------------------------------------------------------------

pub struct UpdateCalendarEvent {
    api: Arc<GoogleApiClient>,
}

impl UpdateCalendarEvent {
    pub fn new(api: Arc<GoogleApiClient>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl Tool for UpdateCalendarEvent {
    fn name(&self) -> &'static str {
        "update_calendar_event"
    }

    fn description(&self) -> &'static str {
        "Update an existing event on the user's Google Calendar. \
         Only the provided fields will be changed."
    }

    fn parameters(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "calendar_id": {
                    "type": "string",
                    "description": "Calendar ID. Use 'primary' for the user's main calendar.",
                    "default": "primary"
                },
                "event_id": {
                    "type": "string",
                    "description": "The event ID to update. Required."
                },
                "summary": {
                    "type": "string",
                    "description": "New event title."
                },
                "description": {
                    "type": "string",
                    "description": "New event description."
                },
                "start_time": {
                    "type": "string",
                    "description": "New start time in RFC 3339 format."
                },
                "end_time": {
                    "type": "string",
                    "description": "New end time in RFC 3339 format."
                },
                "location": {
                    "type": "string",
                    "description": "New event location."
                }
            },
            "required": ["event_id"]
        }))
    }

    async fn call(&self, args: Value) -> Result<Value, SynapticError> {
        let cal_id = args["calendar_id"].as_str().unwrap_or("primary");
        let event_id = args["event_id"]
            .as_str()
            .ok_or_else(|| SynapticError::Tool("event_id is required".into()))?;

        let mut patch = serde_json::Map::new();
        if let Some(s) = args["summary"].as_str() {
            patch.insert("summary".into(), Value::String(s.to_string()));
        }
        if let Some(s) = args["description"].as_str() {
            patch.insert("description".into(), Value::String(s.to_string()));
        }
        if let Some(s) = args["location"].as_str() {
            patch.insert("location".into(), Value::String(s.to_string()));
        }
        if let Some(s) = args["start_time"].as_str() {
            patch.insert("start".into(), serde_json::json!({"dateTime": s}));
        }
        if let Some(s) = args["end_time"].as_str() {
            patch.insert("end".into(), serde_json::json!({"dateTime": s}));
        }

        let body = Value::Object(patch);
        let url = format!(
            "{}/calendars/{}/events/{}",
            BASE_URL,
            urlencoding::encode(cal_id),
            urlencoding::encode(event_id)
        );

        self.api.call(Method::PATCH, &url, Some(&body)).await
    }
}

// ---------------------------------------------------------------------------
// Delete Event
// ---------------------------------------------------------------------------

pub struct DeleteCalendarEvent {
    api: Arc<GoogleApiClient>,
}

impl DeleteCalendarEvent {
    pub fn new(api: Arc<GoogleApiClient>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl Tool for DeleteCalendarEvent {
    fn name(&self) -> &'static str {
        "delete_calendar_event"
    }

    fn description(&self) -> &'static str {
        "Delete an event from the user's Google Calendar."
    }

    fn parameters(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "calendar_id": {
                    "type": "string",
                    "description": "Calendar ID. Use 'primary' for the user's main calendar.",
                    "default": "primary"
                },
                "event_id": {
                    "type": "string",
                    "description": "The event ID to delete. Required."
                }
            },
            "required": ["event_id"]
        }))
    }

    async fn call(&self, args: Value) -> Result<Value, SynapticError> {
        let cal_id = args["calendar_id"].as_str().unwrap_or("primary");
        let event_id = args["event_id"]
            .as_str()
            .ok_or_else(|| SynapticError::Tool("event_id is required".into()))?;

        let url = format!(
            "{}/calendars/{}/events/{}",
            BASE_URL,
            urlencoding::encode(cal_id),
            urlencoding::encode(event_id)
        );

        self.api.call(Method::DELETE, &url, None).await
    }
}

// ---------------------------------------------------------------------------
// Find Free Time (FreeBusy)
// ---------------------------------------------------------------------------

pub struct FindFreeTime {
    api: Arc<GoogleApiClient>,
}

impl FindFreeTime {
    pub fn new(api: Arc<GoogleApiClient>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl Tool for FindFreeTime {
    fn name(&self) -> &'static str {
        "find_free_time"
    }

    fn description(&self) -> &'static str {
        "Query the user's Google Calendar for free/busy information within a time range."
    }

    fn parameters(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "time_min": {
                    "type": "string",
                    "description": "Start of time range (RFC 3339). Required."
                },
                "time_max": {
                    "type": "string",
                    "description": "End of time range (RFC 3339). Required."
                },
                "calendars": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Calendar IDs to check. Defaults to ['primary']."
                }
            },
            "required": ["time_min", "time_max"]
        }))
    }

    async fn call(&self, args: Value) -> Result<Value, SynapticError> {
        let time_min = args["time_min"]
            .as_str()
            .ok_or_else(|| SynapticError::Tool("time_min is required".into()))?;
        let time_max = args["time_max"]
            .as_str()
            .ok_or_else(|| SynapticError::Tool("time_max is required".into()))?;

        let calendars: Vec<Value> = args["calendars"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| c.as_str())
                    .map(|id| serde_json::json!({"id": id}))
                    .collect()
            })
            .unwrap_or_else(|| vec![serde_json::json!({"id": "primary"})]);

        let body = serde_json::json!({
            "timeMin": time_min,
            "timeMax": time_max,
            "items": calendars
        });

        let url = format!("{}/freeBusy", BASE_URL);
        self.api.call(Method::POST, &url, Some(&body)).await
    }
}

/// Create all Google Calendar tools with a shared API client.
pub fn create_tools(api: Arc<GoogleApiClient>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ListCalendarEvents::new(api.clone())),
        Arc::new(CreateCalendarEvent::new(api.clone())),
        Arc::new(GetCalendarEvent::new(api.clone())),
        Arc::new(UpdateCalendarEvent::new(api.clone())),
        Arc::new(DeleteCalendarEvent::new(api.clone())),
        Arc::new(FindFreeTime::new(api)),
    ]
}
