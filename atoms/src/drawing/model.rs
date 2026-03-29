use serde::{Deserialize, Serialize};

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
    pub annotation_id:String,
    pub label_id: String,
    pub label_name: String,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polygon_geometry_serde() {
        let geo = Geometry::Polygon {
            points: vec![
                Point { x: 0.0, y: 0.0 },
                Point { x: 100.5, y: 0.0 },
                Point { x: 100.5, y: 200.3 },
            ],
        };
        let json = serde_json::to_string(&geo).unwrap();
        assert!(json.contains(r#""type":"polygon""#));

        let parsed: Geometry = serde_json::from_str(&json).unwrap();
        match parsed {
            Geometry::Polygon { points } => assert_eq!(points.len(), 3),
            _ => panic!("expected Polygon"),
        }
    }

    #[test]
    fn bbox_geometry_serde() {
        let geo = Geometry::BBox {
            start: Point { x: 10.0, y: 20.0 },
            end: Point { x: 110.0, y: 220.0 },
        };
        let json = serde_json::to_string(&geo).unwrap();
        assert!(json.contains(r#""type":"bbox""#));

        let parsed: Geometry = serde_json::from_str(&json).unwrap();
        match parsed {
            Geometry::BBox { start, end } => {
                assert_eq!(start.x, 10.0);
                assert_eq!(end.y, 220.0);
            }
            _ => panic!("expected BBox"),
        }
    }

    #[test]
    fn annotation_serde_roundtrip() {
        let ann = Annotation {
            annotation_id: "a1".into(),
            image_id: "img1".into(),
            label_id: "lbl1".into(),
            geometry: Geometry::BBox {
                start: Point { x: 0.0, y: 0.0 },
                end: Point { x: 50.0, y: 50.0 },
            },
            created_by: "user1".into(),
            created_at: "2025-01-01T00:00:00Z".into(),
            updated_at: None,
        };
        let json = serde_json::to_string(&ann).unwrap();
        let ann2: Annotation = serde_json::from_str(&json).unwrap();
        assert_eq!(ann2.annotation_id, "a1");
        assert!(ann2.updated_at.is_none());
    }

    #[test]
    fn create_annotation_payload_parses() {
        let json = r#"{
            "annotation_id": "a2",
            "label_id": "lbl1",
            "label_name": "cat",
            "geometry": {"type": "polygon", "points": [{"x": 1.0, "y": 2.0}]}
        }"#;
        let p: CreateAnnotationPayload = serde_json::from_str(json).unwrap();
        assert_eq!(p.label_id, "lbl1");
    }

    #[test]
    fn batch_payload_parses() {
        let json = r#"{"annotations": [
            {"annotation_id": "a1", "label_id": "l1", "label_name": "dog", "geometry": {"type": "bbox", "start": {"x":0,"y":0}, "end": {"x":1,"y":1}}},
            {"annotation_id": "a2", "label_id": "l2", "label_name": "cat", "geometry": {"type": "polygon", "points": [{"x":0,"y":0}]}}
        ]}"#;
        let p: CreateBatchAnnotationsPayload = serde_json::from_str(json).unwrap();
        assert_eq!(p.annotations.len(), 2);
    }

    #[test]
    fn geometry_rejects_unknown_type() {
        let json = r#"{"type": "circle", "radius": 5}"#;
        assert!(serde_json::from_str::<Geometry>(json).is_err());
    }
}
