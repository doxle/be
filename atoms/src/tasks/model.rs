use serde::{Deserialize, Serialize};
use crate::users::model::UserRole;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    Todo,
    Working,
    Submitted,
    Rejected,
    Approved,
}

impl TaskState {
    /// Parse from a DynamoDB string, defaulting to Todo if unknown.
    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "working" => Self::Working,
            "submitted" => Self::Submitted,
            "rejected" => Self::Rejected,
            "approved" => Self::Approved,
            _ => Self::Todo,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::Working => "working",
            Self::Submitted => "submitted",
            Self::Rejected => "rejected",
            Self::Approved => "approved",
        }
    }

    /// Check if a state transition is allowed for the given role.
    pub fn can_transition(from: &TaskState, to: &TaskState, role: &UserRole) -> bool {
        if role == &UserRole::Admin {
            return true;
        }
        matches!(
            (from, to),
            (TaskState::Todo, TaskState::Working)
                | (TaskState::Working, TaskState::Submitted)
                | (TaskState::Rejected, TaskState::Working)
        )
    }
}

/// Task domain model - represents a unit of work in a block
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Task {
    pub task_id: String,
    pub block_id: String,
    pub task_name: String,
    pub task_state: TaskState,
    // FE expects plain strings (empty string = no assignee)
    pub assignee: String,
    /// Reviewer in FE / API; stored as "checked_by" in DynamoDB
    #[serde(rename = "reviewer")]
    pub checked_by: String,
    /// Whether this task is locked from editing
    pub locked: bool,
    pub image_count: u32,
    pub created_at: String,
    pub images: Vec<crate::media::model::Image>,
    pub annotation_count: u32,
    #[serde(default)]
    pub labels_count: std::collections::HashMap<String, u32>,
    #[serde(default)]
    pub bbox_count: std::collections::HashMap<String, u32>,
    #[serde(default)]
    pub polygon_count: std::collections::HashMap<String, u32>,
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
    pub task_state: Option<TaskState>,
    pub assignee: Option<String>,
    pub checked_by: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // === TaskState serde ===

    #[test]
    fn task_state_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&TaskState::Todo).unwrap(), r#""todo""#);
        assert_eq!(serde_json::to_string(&TaskState::Working).unwrap(), r#""working""#);
        assert_eq!(serde_json::to_string(&TaskState::Submitted).unwrap(), r#""submitted""#);
        assert_eq!(serde_json::to_string(&TaskState::Rejected).unwrap(), r#""rejected""#);
        assert_eq!(serde_json::to_string(&TaskState::Approved).unwrap(), r#""approved""#);
    }

    #[test]
    fn task_state_deserializes_lowercase() {
        let s: TaskState = serde_json::from_str(r#""todo""#).unwrap();
        assert_eq!(s, TaskState::Todo);
        let s: TaskState = serde_json::from_str(r#""approved""#).unwrap();
        assert_eq!(s, TaskState::Approved);
    }

    #[test]
    fn task_state_rejects_unknown_json() {
        // serde rename_all = "lowercase" means "Todo" (capitalized) is invalid JSON
        assert!(serde_json::from_str::<TaskState>(r#""Todo""#).is_err());
    }

    // === TaskState::from_str_loose ===

    #[test]
    fn from_str_loose_known_values() {
        assert_eq!(TaskState::from_str_loose("working"), TaskState::Working);
        assert_eq!(TaskState::from_str_loose("SUBMITTED"), TaskState::Submitted);
        assert_eq!(TaskState::from_str_loose("Rejected"), TaskState::Rejected);
        assert_eq!(TaskState::from_str_loose("approved"), TaskState::Approved);
    }

    #[test]
    fn from_str_loose_defaults_to_todo() {
        assert_eq!(TaskState::from_str_loose("garbage"), TaskState::Todo);
        assert_eq!(TaskState::from_str_loose(""), TaskState::Todo);
    }

    // === TaskState::as_str ===

    #[test]
    fn as_str_roundtrips_with_from_str_loose() {
        for state in [TaskState::Todo, TaskState::Working, TaskState::Submitted, TaskState::Rejected, TaskState::Approved] {
            assert_eq!(TaskState::from_str_loose(state.as_str()), state);
        }
    }

    // === TaskState::can_transition ===

    #[test]
    fn admin_can_do_any_transition() {
        assert!(TaskState::can_transition(&TaskState::Todo, &TaskState::Approved, &UserRole::Admin));
        assert!(TaskState::can_transition(&TaskState::Approved, &TaskState::Todo, &UserRole::Admin));
    }

    #[test]
    fn annotator_allowed_transitions() {
        let role = UserRole::Annotator;
        assert!(TaskState::can_transition(&TaskState::Todo, &TaskState::Working, &role));
        assert!(TaskState::can_transition(&TaskState::Working, &TaskState::Submitted, &role));
        assert!(TaskState::can_transition(&TaskState::Rejected, &TaskState::Working, &role));
    }

    #[test]
    fn annotator_blocked_transitions() {
        let role = UserRole::Annotator;
        assert!(!TaskState::can_transition(&TaskState::Submitted, &TaskState::Approved, &role));
        assert!(!TaskState::can_transition(&TaskState::Todo, &TaskState::Approved, &role));
        assert!(!TaskState::can_transition(&TaskState::Working, &TaskState::Rejected, &role));
    }

    // === Task serde roundtrip ===

    #[test]
    fn task_serde_roundtrip() {
        let task = Task {
            task_id: "t1".into(),
            block_id: "b1".into(),
            task_name: "Label cats".into(),
            task_state: TaskState::Working,
            assignee: "user@test.com".into(),
            checked_by: "reviewer@test.com".into(),
            locked: false,
            image_count: 5,
            created_at: "2025-01-01T00:00:00Z".into(),
            images: vec![],
            annotation_count: 10,
            labels_count: std::collections::HashMap::new(),
            bbox_count: std::collections::HashMap::new(),
            polygon_count: std::collections::HashMap::new(),
        };
        let json = serde_json::to_string(&task).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // checked_by should serialize as "reviewer" due to #[serde(rename)]
        assert_eq!(parsed["reviewer"], "reviewer@test.com");
        assert!(parsed.get("checked_by").is_none());
        assert_eq!(parsed["task_state"], "working");

        // Deserialize back
        let task2: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(task2.checked_by, "reviewer@test.com");
        assert_eq!(task2.task_state, TaskState::Working);
    }

    // === Payload deserialization ===

    #[test]
    fn create_task_payload_parses() {
        let json = r#"{"task_name": "My task", "assignee": "alice"}"#;
        let p: CreateTaskPayload = serde_json::from_str(json).unwrap();
        assert_eq!(p.task_name, "My task");
        assert_eq!(p.assignee, Some("alice".into()));
        assert_eq!(p.checked_by, None);
    }

    #[test]
    fn update_task_payload_all_optional() {
        let p: UpdateTaskPayload = serde_json::from_str("{}").unwrap();
        assert!(p.task_name.is_none());
        assert!(p.task_state.is_none());
        assert!(p.assignee.is_none());
        assert!(p.checked_by.is_none());
    }
}
