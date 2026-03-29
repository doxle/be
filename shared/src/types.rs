use serde::{Deserialize, Serialize};


// ========== USER ==========
pub use doxle_atoms::users::model::{User, UserRole, CreateUserPayload, UpdateUserPayload};

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

#[cfg(test)]
mod tests {
    use super::*;

    // === ImageMetadata ===

    #[test]
    fn image_metadata_serde_roundtrip() {
        let meta = ImageMetadata {
            original_width: 4955,
            original_height: 3503,
            file_size: 1_870_256,
            format: "png".into(),
            levels: vec![
                ImageLevel {
                    width: 4955,
                    height: 3503,
                    path: "4955w.png".into(),
                    size: 1_870_256,
                    purpose: "full".into(),
                },
                ImageLevel {
                    width: 2477,
                    height: 1751,
                    path: "2477w.jpg".into(),
                    size: 420_000,
                    purpose: "preview".into(),
                },
            ],
        };
        let json = serde_json::to_string(&meta).unwrap();
        let meta2: ImageMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(meta2.original_width, 4955);
        assert_eq!(meta2.levels.len(), 2);
        assert_eq!(meta2.levels[1].purpose, "preview");
    }

    // === Geometry (types.rs copy) ===

    #[test]
    fn geometry_polygon_serde() {
        let geo = Geometry::Polygon {
            points: vec![Point { x: 1.0, y: 2.0 }, Point { x: 3.0, y: 4.0 }],
        };
        let json = serde_json::to_string(&geo).unwrap();
        assert!(json.contains(r#""type":"polygon""#));
        let geo2: Geometry = serde_json::from_str(&json).unwrap();
        match geo2 {
            Geometry::Polygon { points } => assert_eq!(points.len(), 2),
            _ => panic!("expected polygon"),
        }
    }

    #[test]
    fn geometry_bbox_serde() {
        let geo = Geometry::BBox {
            start: Point { x: 0.0, y: 0.0 },
            end: Point { x: 100.0, y: 200.0 },
        };
        let json = serde_json::to_string(&geo).unwrap();
        let geo2: Geometry = serde_json::from_str(&json).unwrap();
        match geo2 {
            Geometry::BBox { start, end } => {
                assert_eq!(start.x, 0.0);
                assert_eq!(end.y, 200.0);
            }
            _ => panic!("expected bbox"),
        }
    }

    // === Annotation ===

    #[test]
    fn annotation_serde_roundtrip() {
        let ann = Annotation {
            annotation_id: "a1".into(),
            image_id: "img1".into(),
            label_id: "lbl1".into(),
            geometry: Geometry::BBox {
                start: Point { x: 10.0, y: 20.0 },
                end: Point { x: 30.0, y: 40.0 },
            },
            created_by: "user1".into(),
            created_at: "2025-01-01T00:00:00Z".into(),
            updated_at: Some("2025-01-02T00:00:00Z".into()),
        };
        let json = serde_json::to_string(&ann).unwrap();
        let ann2: Annotation = serde_json::from_str(&json).unwrap();
        assert_eq!(ann2.annotation_id, "a1");
        assert_eq!(ann2.updated_at, Some("2025-01-02T00:00:00Z".into()));
    }
}

// ========== TASKS ==========
pub use doxle_atoms::tasks::model::{Task, TaskState, CreateTaskPayload, UpdateTaskPayload};
