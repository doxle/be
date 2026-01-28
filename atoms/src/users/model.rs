use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct User {
    pub user_id: String,
    pub user_name: String,
    pub user_email: String,
    pub user_company: Option<String>,
    pub user_role: String, // admin | annotator | builder
    pub user_created_at: String,
    pub user_last_login: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateUserPayload {
    pub user_name: String,
    pub user_email: String,
    pub user_company: Option<String>,
    pub user_role: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateUserPayload {
    pub user_name: Option<String>,
    pub user_company: Option<String>,
    pub user_role: Option<String>,
}
