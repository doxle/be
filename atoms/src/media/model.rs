
use serde::{Deserialize, Serialize};

/// Image domain model - represents a file/media asset
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Image {
    pub image_id: String,
    pub block_id: String,
    pub task_id: Option<String>,
    pub url: String,
    pub locked: bool,
    pub order: Option<i32>,
    pub annotation_count:u32,
    pub uploaded_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateImagePayload {
    pub url: String,
    pub task_id: Option<String>,
    pub order: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateImagePayload {
    pub locked: Option<bool>,
    pub order: Option<i32>,
}
