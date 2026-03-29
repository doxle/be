
use std::collections::HashMap;
use serde::{Deserialize, Serialize};

fn default_media_type() -> String {
    "image".to_string()
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MarkupRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Image domain model - represents a file/media asset
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Image {
    pub image_id: String,
    pub block_id: String,
    pub task_id: Option<String>,
    pub image_name: String,
    pub url: String,
    pub locked: bool,
    pub order: Option<i32>,
    pub annotation_count:u32, // no of annotations which is the # of label_counts
    pub labels_count:HashMap<String, u32>, // # list of all label counts
    pub bbox_count:HashMap<String, u32>,
    pub polygon_count:HashMap<String, u32>,
    pub uploaded_at: String,
    #[serde(default = "default_media_type")]
    pub media_type: String,
    #[serde(default)]
    pub markup_rects: Vec<MarkupRect>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct CreateImagePayload {
    pub image_id: String,
    pub image_name: String,
    pub url: String,
    pub task_id: Option<String>,
    pub order: Option<i32>,
    pub media_type: Option<String>,
    pub markup_rects: Option<Vec<MarkupRect>>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateImagePayload {
    pub locked: Option<bool>,
    pub order: Option<i32>,
    pub media_type: Option<String>,
    pub markup_rects: Option<Vec<MarkupRect>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_serde_roundtrip() {
        let img = Image {
            image_id: "img1".into(),
            block_id: "b1".into(),
            task_id: Some("t1".into()),
            image_name: "photo.jpg".into(),
            url: "https://example.com/photo.jpg".into(),
            locked: false,
            order: Some(1),
            annotation_count: 3,
            labels_count: HashMap::from([("cat".into(), 2), ("dog".into(), 1)]),
            bbox_count: HashMap::from([("cat".into(), 1)]),
            polygon_count: HashMap::from([("dog".into(), 1)]),
            uploaded_at: "2025-01-01T00:00:00Z".into(),
            media_type: "image".into(),
            markup_rects: vec![],
            width: Some(1920),
            height: Some(1080),
        };
        let json = serde_json::to_string(&img).unwrap();
        let img2: Image = serde_json::from_str(&json).unwrap();
        assert_eq!(img2.image_id, "img1");
        assert_eq!(img2.labels_count.get("cat"), Some(&2));
        assert_eq!(img2.annotation_count, 3);
        assert_eq!(img2.task_id, Some("t1".into()));
        assert_eq!(img2.width, Some(1920));
        assert_eq!(img2.height, Some(1080));
    }

    #[test]
    fn image_with_empty_counts() {
        let img = Image {
            image_id: "img2".into(),
            block_id: "b1".into(),
            task_id: None,
            image_name: "empty.png".into(),
            url: "https://example.com/empty.png".into(),
            locked: true,
            order: None,
            annotation_count: 0,
            labels_count: HashMap::new(),
            bbox_count: HashMap::new(),
            polygon_count: HashMap::new(),
            uploaded_at: "2025-01-01T00:00:00Z".into(),
            media_type: "image".into(),
            markup_rects: vec![],
            width: None,
            height: None,
        };
        let json = serde_json::to_string(&img).unwrap();
        let img2: Image = serde_json::from_str(&json).unwrap();
        assert!(img2.task_id.is_none());
        assert!(img2.order.is_none());
        assert!(img2.labels_count.is_empty());
        assert!(img2.locked);
        assert!(img2.width.is_none());
        assert_eq!(img2.media_type, "image");
    }

    #[test]
    fn create_image_payload_parses() {
        let json = r#"{"image_id":"i1","image_name":"test.jpg","url":"https://s3/test.jpg"}"#;
        let p: CreateImagePayload = serde_json::from_str(json).unwrap();
        assert_eq!(p.image_name, "test.jpg");
        assert!(p.task_id.is_none());
        assert!(p.order.is_none());
    }

    #[test]
    fn update_image_payload_all_optional() {
        let p: UpdateImagePayload = serde_json::from_str("{}").unwrap();
        assert!(p.locked.is_none());
        assert!(p.order.is_none());
    }
}
