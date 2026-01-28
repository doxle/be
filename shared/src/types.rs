use serde::{Deserialize, Serialize};


// ========== USER ==========
pub use doxle_atoms::users::model::{User, CreateUserPayload, UpdateUserPayload};

// ========== BLOCK ==========
pub use doxle_atoms::blocks::model::{Block, CreateBlockPayload, UpdateBlockPayload};


// // ========== UNIT ==========
// #[derive(Debug, Serialize, Deserialize, Clone)]
// pub struct Unit {
//     pub unit_id:String,
//     pub block_id:String,
//     pub project_id:String,
//     pub unit_name:String,
//     pub unit_state:String, // "todo" | "in_progress" | "done" | "qa"
//     pub unit_locked:String,
//     pub unit_assigned_to:String,
//     pub unit_created_at:String,
//     pub unit_image_count:Option<u32>,
//     pub annotated_image_count:Option<u32>,
// }

// ========== IMAGE ==========
pub use doxle_atoms::media::model::{Image, CreateImagePayload, UpdateImagePayload};

// ========== IMAGE METADATA (Pyramid) ==========
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ImageMetadata {
    pub original_width: u32,
    pub original_height: u32,
    pub file_size: usize,
    pub format: String,
    pub levels: Vec<ImageLevel>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ImageLevel {
    pub width: u32,
    pub height: u32,
    pub path: String,
    pub size: usize,
    pub purpose: String,
}



// ========== ANNOTATION ==========
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type")]
pub enum Geometry {
    #[serde(rename = "polygon")]
    Polygon { points: Vec<Point> },
    #[serde(rename = "bbox")]
    BBox { start: Point, end: Point },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Annotation {
    pub annotation_id: String,
    pub image_id: String,
    pub label_id: String,
    pub geometry: Geometry,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateAnnotationPayload {
    pub label_id: String,
    pub geometry: Geometry,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAnnotationPayload {
    pub label_id: Option<String>,
    pub geometry: Option<Geometry>,
}

#[derive(Debug, Deserialize)]
pub struct CreateBatchAnnotationsPayload {
    pub annotations: Vec<CreateAnnotationPayload>,
}

// ========== COMMENT ==========
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Comment {
    pub comment_id: String,
    pub image_id: String,
    pub user_id: String,
    pub text: String,
    pub resolved: bool,
    pub created_at: String,
}

// ========== TASKS ==========
pub use doxle_atoms::tasks::model::{Task, CreateTaskPayload, UpdateTaskPayload};
