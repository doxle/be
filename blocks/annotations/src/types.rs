use serde::{Deserialize, Serialize};


// ========== IMAGE ==========
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Image {
    pub image_id: String,
    pub task_id:String,
    pub block_id: String,
    pub image_url: String,
    pub image_locked: bool,
    pub annotation_count:u32,
    pub image_created_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateImagePayload {
    pub task_id:String,
    pub image_url: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateImagePayload {
    pub image_locked: Option<bool>,
   
}


// ========== LABELS ==========
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Label {
    pub label_id: String,
    pub block_id: String,
    pub label_name: String,
    pub label_color: String,
    pub label_properties: Option<serde_json::Value>,
    pub label_count: u32,
}

#[derive(Debug, Deserialize)]
pub struct CreateLabelPayload {
    pub label_name: String,
    pub label_color: String,
    pub label_properties: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateLabelPayload {
    pub label_name: Option<String>,
    pub label_color: Option<String>,
    pub label_properties: Option<serde_json::Value>,
}

// ========== BLOCK RESPONSE (annotation) ==========
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AnnotationBlock {
    #[serde(flatten)]
    pub block: doxle_atoms::blocks::model::Block,
    #[serde(default)]
    pub labels: Vec<Label>,
}


// ========== ANNOTATION ==========
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Point {
    pub point_x: f64,
    pub point_y: f64,
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
    pub annotation_created_by: String, // USER#123
    pub annotation_created_at: String,
    pub annotation_updated_at: Option<String>,
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

// ========== TASKS ==========
// Re-export from shared atoms
pub use doxle_atoms::tasks::model::{Task, CreateTaskPayload, UpdateTaskPayload};

// ========== IMAGES ==========
// Re-export from shared atoms
pub use doxle_atoms::media::model::Image as MediaImage;
