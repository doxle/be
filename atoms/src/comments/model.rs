use serde::{Deserialize, Serialize};

/// A thread anchored to a parent resource (image, file, block, etc.)
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CommentThread {
    pub thread_id: String,
    pub parent_id: String,
    /// JSON metadata — e.g. {"world_x":245,"world_y":180} for canvas, {"page":3} for files
    pub metadata: Option<String>,
    pub resolved: bool,
    pub created_by: String,
    pub created_at: String,
    #[serde(default)]
    pub comments: Vec<Comment>,
}

/// A single comment within a thread
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Comment {
    pub comment_id: String,
    pub thread_id: String,
    pub user_id: String,
    pub user_name: String,
    pub text: String,
    pub created_at: String,
}

// ============================================
// Request Payloads
// ============================================

#[derive(Debug, Deserialize)]
pub struct CreateThreadPayload {
    /// FE-generated thread ID
    pub thread_id: String,
    /// JSON metadata (world coords, page number, etc.)
    pub metadata: Option<String>,
    /// First comment text — thread always starts with a comment
    pub text: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateCommentPayload {
    pub text: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateThreadPayload {
    pub resolved: Option<bool>,
    pub metadata: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comment_thread_serde_roundtrip() {
        let thread = CommentThread {
            thread_id: "th1".into(),
            parent_id: "img1".into(),
            metadata: Some(r#"{"world_x":245,"world_y":180}"#.into()),
            resolved: false,
            created_by: "user1".into(),
            created_at: "2025-01-01T00:00:00Z".into(),
            comments: vec![
                Comment {
                    comment_id: "c1".into(),
                    thread_id: "th1".into(),
                    user_id: "user1".into(),
                    user_name: "Alice".into(),
                    text: "Fix this annotation".into(),
                    created_at: "2025-01-01T00:00:00Z".into(),
                },
            ],
        };
        let json = serde_json::to_string(&thread).unwrap();
        let thread2: CommentThread = serde_json::from_str(&json).unwrap();
        assert_eq!(thread2.thread_id, "th1");
        assert_eq!(thread2.comments.len(), 1);
        assert_eq!(thread2.comments[0].text, "Fix this annotation");
        assert!(!thread2.resolved);
    }

    #[test]
    fn comment_thread_default_empty_comments() {
        // comments has #[serde(default)] so missing field should give empty vec
        let json = r#"{
            "thread_id": "th2",
            "parent_id": "img2",
            "metadata": null,
            "resolved": true,
            "created_by": "user2",
            "created_at": "2025-01-01T00:00:00Z"
        }"#;
        let thread: CommentThread = serde_json::from_str(json).unwrap();
        assert!(thread.comments.is_empty());
        assert!(thread.resolved);
        assert!(thread.metadata.is_none());
    }

    #[test]
    fn create_thread_payload_parses() {
        let json = r#"{"thread_id":"th3","text":"Hello"}"#;
        let p: CreateThreadPayload = serde_json::from_str(json).unwrap();
        assert_eq!(p.thread_id, "th3");
        assert_eq!(p.text, "Hello");
        assert!(p.metadata.is_none());
    }

    #[test]
    fn create_comment_payload_parses() {
        let json = r#"{"text":"Great work!"}"#;
        let p: CreateCommentPayload = serde_json::from_str(json).unwrap();
        assert_eq!(p.text, "Great work!");
    }

    #[test]
    fn update_thread_payload_all_optional() {
        let p: UpdateThreadPayload = serde_json::from_str("{}").unwrap();
        assert!(p.resolved.is_none());
        assert!(p.metadata.is_none());
    }
}
