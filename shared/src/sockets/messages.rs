use serde::{Deserialize, Serialize};

/// Incoming WebSocket message from client
#[derive(Debug, Deserialize)]
pub struct WebSocketMessage {
    pub action: String,
    #[serde(flatten)]
    pub data: serde_json::Value,
}

/// WebSocket action types
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebSocketAction {
    // Project actions
    CreateProject,
    UpdateProject,
    DeleteProject,
    
    // Block actions
    CreateBlock,
    UpdateBlock,
    DeleteBlock,
    
    // Image actions
    CreateImage,
    UpdateImage,
    DeleteImage,
    
    // Annotation actions
    CreateAnnotation,
    UpdateAnnotation,
    DeleteAnnotation,
    BatchCreateAnnotations,
    
    // Class actions
    CreateClass,
    UpdateClass,
    DeleteClass,
}

/// Broadcast message sent to all clients
#[derive(Debug, Serialize)]
pub struct BroadcastMessage {
    pub r#type: String,
    #[serde(flatten)]
    pub data: serde_json::Value,
}

impl BroadcastMessage {
    pub fn _new(message_type: &str, data: serde_json::Value) -> Self {
        Self {
            r#type: message_type.to_string(),
            data,
        }
    }
}
