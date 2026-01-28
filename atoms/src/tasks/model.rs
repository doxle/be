use serde::{Deserialize, Serialize};

/// Task domain model - represents a unit of work in a block
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Task {
    pub task_id: String,
    pub block_id: String,
    pub task_name: String,
    pub task_state: String, // "todo" | "in_progress" | "done"

    // FE expects plain strings (empty string = no assignee)
    pub assignee: String,

    /// Reviewer in FE / API; stored as "checked_by" in DynamoDB
    #[serde(rename = "reviewer")]
    pub checked_by: String,

    /// Whether this task is locked from editing
    pub locked: bool,

    pub image_count:u32,

    pub created_at: String,
    
    /// Images associated with this task, filled in by be/blocks/* when joining with media
    #[serde(default)]
    pub images: Vec<crate::media::model::Image>,
}

#[derive(Debug, Deserialize)]
pub struct CreateTaskPayload {
    pub task_name: String,
    pub assignee: Option<String>,
    pub checked_by: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTaskPayload {
    pub task_name: Option<String>,
    pub task_state: Option<String>,
    pub assignee: Option<String>,
    pub checked_by: Option<String>,
}
