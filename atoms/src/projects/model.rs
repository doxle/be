use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ProjectStatus {
    Active,
    Archived,
    Completed,
}

impl ProjectStatus {
    pub fn as_str(&self) -> &str {
        match self {
            ProjectStatus::Active => "active",
            ProjectStatus::Archived => "archived",
            ProjectStatus::Completed => "completed",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "archived" => Self::Archived,
            "completed" => Self::Completed,
            _ => Self::Active,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Project {
    pub project_id: String,
    pub project_name: String,
    pub block_count: u32,
    pub project_company: Option<String>,
    pub project_status: ProjectStatus,
    pub project_address: Option<String>,
    pub project_email: Option<String>,
    pub project_owner: String,
    pub project_members: Vec<String>,
    pub project_start_date: Option<String>,
    pub project_end_date: Option<String>,
    pub project_description: Option<String>,
    pub project_budget: Option<String>,
    pub project_client_name: Option<String>,
    pub project_created_at: String,
    pub project_updated_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateProjectPayload {
    pub project_name: String,
    pub project_company: Option<String>,
    pub project_address: Option<String>,
    pub project_email: Option<String>,
    pub project_members: Option<Vec<String>>,
    pub project_start_date: Option<String>,
    pub project_end_date: Option<String>,
    pub project_description: Option<String>,
    pub project_budget: Option<String>,
    pub project_client_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateProjectPayload {
    pub project_name: Option<String>,
    pub project_company: Option<String>,
    pub project_status: Option<ProjectStatus>,
    pub project_address: Option<String>,
    pub project_email: Option<String>,
    pub project_members: Option<Vec<String>>,
    pub project_start_date: Option<String>,
    pub project_end_date: Option<String>,
    pub project_description: Option<String>,
    pub project_budget: Option<String>,
    pub project_client_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_status_serde() {
        assert_eq!(serde_json::to_string(&ProjectStatus::Active).unwrap(), r#""active""#);
        assert_eq!(serde_json::to_string(&ProjectStatus::Archived).unwrap(), r#""archived""#);
        assert_eq!(serde_json::to_string(&ProjectStatus::Completed).unwrap(), r#""completed""#);

        let s: ProjectStatus = serde_json::from_str(r#""active""#).unwrap();
        assert_eq!(s, ProjectStatus::Active);
    }

    #[test]
    fn project_status_from_str_loose() {
        assert_eq!(ProjectStatus::from_str_loose("active"), ProjectStatus::Active);
        assert_eq!(ProjectStatus::from_str_loose("ARCHIVED"), ProjectStatus::Archived);
        assert_eq!(ProjectStatus::from_str_loose("completed"), ProjectStatus::Completed);
        assert_eq!(ProjectStatus::from_str_loose("garbage"), ProjectStatus::Active);
        assert_eq!(ProjectStatus::from_str_loose(""), ProjectStatus::Active);
    }

    #[test]
    fn project_serde_roundtrip() {
        let project = Project {
            project_id: "p1".into(),
            project_name: "Test Project".into(),
            block_count: 3,
            project_company: Some("Doxle".into()),
            project_status: ProjectStatus::Active,
            project_address: Some("123 Main St".into()),
            project_email: Some("test@doxle.ai".into()),
            project_owner: "user-123".into(),
            project_members: vec!["user-123".into(), "user-456".into()],
            project_start_date: Some("2025-01-01".into()),
            project_end_date: Some("2025-12-31".into()),
            project_description: Some("A test project".into()),
            project_budget: Some("100000".into()),
            project_client_name: Some("Client Corp".into()),
            project_created_at: "2025-01-01T00:00:00Z".into(),
            project_updated_at: "2025-01-02T00:00:00Z".into(),
        };
        let json = serde_json::to_string(&project).unwrap();
        let p2: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(p2.project_id, "p1");
        assert_eq!(p2.project_status, ProjectStatus::Active);
        assert_eq!(p2.project_members.len(), 2);
        assert_eq!(p2.project_budget, Some("100000".into()));
    }

    #[test]
    fn create_project_payload_parses() {
        let json = r#"{"project_name":"New Project"}"#;
        let p: CreateProjectPayload = serde_json::from_str(json).unwrap();
        assert_eq!(p.project_name, "New Project");
        assert!(p.project_company.is_none());
        assert!(p.project_members.is_none());
    }

    #[test]
    fn update_project_payload_all_optional() {
        let p: UpdateProjectPayload = serde_json::from_str("{}").unwrap();
        assert!(p.project_name.is_none());
        assert!(p.project_status.is_none());
        assert!(p.project_members.is_none());
    }
}
