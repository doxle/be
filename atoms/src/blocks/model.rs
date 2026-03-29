use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum BlockType {
    Annotation,
    File,
    Building,
}

impl BlockType {
    pub fn as_str(&self) -> &str {
        match self {
            BlockType::Annotation => "annotation",
            BlockType::File => "file",
            BlockType::Building => "building",
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Block {
    pub block_id: String,
    pub project_id: String,
    pub block_name: String,
    pub block_type: BlockType,
    pub block_company: Option<String>,
    pub block_state: String,
    pub block_locked: bool,
    pub image_count: u32,
    pub approved_image_count: u32,
    pub annotation_count: u32,
    pub block_created_at: String,
    pub block_updated_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateBlockPayload {
    pub block_name: String,
    pub block_type: BlockType,
    pub block_company: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateBlockPayload {
    pub block_name: Option<String>,
    pub block_state: Option<String>,
    pub block_locked: Option<bool>,
    // pub block_assigned_to: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_type_serde() {
        assert_eq!(serde_json::to_string(&BlockType::Annotation).unwrap(), r#""annotation""#);
        assert_eq!(serde_json::to_string(&BlockType::File).unwrap(), r#""file""#);
        assert_eq!(serde_json::to_string(&BlockType::Building).unwrap(), r#""building""#);

        let bt: BlockType = serde_json::from_str(r#""annotation""#).unwrap();
        assert_eq!(bt, BlockType::Annotation);
    }

    #[test]
    fn block_type_as_str() {
        assert_eq!(BlockType::Annotation.as_str(), "annotation");
        assert_eq!(BlockType::File.as_str(), "file");
        assert_eq!(BlockType::Building.as_str(), "building");
    }

    #[test]
    fn block_serde_roundtrip() {
        let block = Block {
            block_id: "blk1".into(),
            project_id: "proj1".into(),
            block_name: "Test Block".into(),
            block_type: BlockType::Annotation,
            block_company: Some("Doxle".into()),
            block_state: "active".into(),
            block_locked: false,
            image_count: 10,
            approved_image_count: 5,
            annotation_count: 42,
            block_created_at: "2025-01-01T00:00:00Z".into(),
            block_updated_at: "2025-01-02T00:00:00Z".into(),
        };
        let json = serde_json::to_string(&block).unwrap();
        let block2: Block = serde_json::from_str(&json).unwrap();
        assert_eq!(block2.block_id, "blk1");
        assert_eq!(block2.block_type, BlockType::Annotation);
        assert_eq!(block2.block_company, Some("Doxle".into()));
    }

    #[test]
    fn create_block_payload_parses() {
        let json = r#"{"block_name":"New Block","block_type":"file"}"#;
        let p: CreateBlockPayload = serde_json::from_str(json).unwrap();
        assert_eq!(p.block_name, "New Block");
        assert_eq!(p.block_type, BlockType::File);
        assert!(p.block_company.is_none());
    }

    #[test]
    fn create_block_rejects_invalid_type() {
        let json = r#"{"block_name":"Bad","block_type":"invalid"}"#;
        assert!(serde_json::from_str::<CreateBlockPayload>(json).is_err());
    }

    #[test]
    fn update_block_payload_all_optional() {
        let p: UpdateBlockPayload = serde_json::from_str("{}").unwrap();
        assert!(p.block_name.is_none());
        assert!(p.block_state.is_none());
        assert!(p.block_locked.is_none());
    }
}
