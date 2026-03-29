use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum UserRole {
    Admin,
    Annotator,
    Builder,
}

impl UserRole {
    /// Parse from a DynamoDB string, defaulting to Annotator if unknown.
    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "admin" => Self::Admin,
            "builder" => Self::Builder,
            _ => Self::Annotator,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Annotator => "annotator",
            Self::Builder => "builder",
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct User {
    pub user_id: String,
    pub user_name: String,
    pub user_email: String,
    pub user_company: Option<String>,
    pub user_role: UserRole,
    pub user_created_at: String,
    pub user_last_login: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateUserPayload {
    pub user_name: String,
    pub user_email: String,
    pub user_company: Option<String>,
    pub user_role: UserRole,
}

#[derive(Debug, Deserialize)]
pub struct UpdateUserPayload {
    pub user_name: Option<String>,
    pub user_company: Option<String>,
    pub user_role: Option<UserRole>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_role_serde_roundtrip() {
        for role in [UserRole::Admin, UserRole::Annotator, UserRole::Builder] {
            let json = serde_json::to_string(&role).unwrap();
            let parsed: UserRole = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, role);
        }
    }

    #[test]
    fn user_role_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&UserRole::Admin).unwrap(), r#""admin""#);
        assert_eq!(serde_json::to_string(&UserRole::Annotator).unwrap(), r#""annotator""#);
        assert_eq!(serde_json::to_string(&UserRole::Builder).unwrap(), r#""builder""#);
    }

    #[test]
    fn from_str_loose_known() {
        assert_eq!(UserRole::from_str_loose("admin"), UserRole::Admin);
        assert_eq!(UserRole::from_str_loose("ADMIN"), UserRole::Admin);
        assert_eq!(UserRole::from_str_loose("builder"), UserRole::Builder);
        assert_eq!(UserRole::from_str_loose("Builder"), UserRole::Builder);
    }

    #[test]
    fn from_str_loose_defaults_to_annotator() {
        assert_eq!(UserRole::from_str_loose("unknown"), UserRole::Annotator);
        assert_eq!(UserRole::from_str_loose(""), UserRole::Annotator);
    }

    #[test]
    fn as_str_roundtrips() {
        for role in [UserRole::Admin, UserRole::Annotator, UserRole::Builder] {
            assert_eq!(UserRole::from_str_loose(role.as_str()), role);
        }
    }

    #[test]
    fn user_serde_roundtrip() {
        let user = User {
            user_id: "u1".into(),
            user_name: "Alice".into(),
            user_email: "alice@test.com".into(),
            user_company: Some("Doxle".into()),
            user_role: UserRole::Admin,
            user_created_at: "2025-01-01T00:00:00Z".into(),
            user_last_login: None,
        };
        let json = serde_json::to_string(&user).unwrap();
        let user2: User = serde_json::from_str(&json).unwrap();
        assert_eq!(user2.user_email, "alice@test.com");
        assert_eq!(user2.user_role, UserRole::Admin);
        assert!(user2.user_last_login.is_none());
    }

    #[test]
    fn create_user_payload_parses() {
        let json = r#"{"user_name":"Bob","user_email":"bob@test.com","user_role":"annotator"}"#;
        let p: CreateUserPayload = serde_json::from_str(json).unwrap();
        assert_eq!(p.user_name, "Bob");
        assert_eq!(p.user_role, UserRole::Annotator);
        assert!(p.user_company.is_none());
    }
}
