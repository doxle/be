use serde::{Deserialize, Serialize};


#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Block {
    pub block_id: String,
    pub block_name: String,
    pub block_type: String,
    pub block_company: Option<String>,
    pub block_state: String,
    pub block_locked: bool,
    pub image_count: u32,
    pub approved_image_count: u32,
    pub annotation_count: u32,
    pub block_created_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateBlockPayload {
    pub block_name: String,
    pub block_type:String,
    pub block_company: Option<String>,
    // pub block_label:Option<BlockLabel>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateBlockPayload {
    pub block_name: Option<String>,
    pub block_state: Option<String>,
    pub block_locked: Option<bool>,
    // pub block_assigned_to: Option<String>,
}
