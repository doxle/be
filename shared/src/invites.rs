use lambda_http::{Body, Error, Response, http::StatusCode};
use aws_sdk_dynamodb::Client as DynamoClient;
use aws_sdk_cognitoidentityprovider::Client as CognitoClient;
use aws_sdk_sesv2::Client as SesClient;
use serde::{Deserialize, Serialize};
use chrono::Utc;
use uuid::Uuid;
use std::env;

#[derive(Debug, Deserialize)]
pub struct CreateInviteRequest {
    pub email: String,
    pub role: String,
    #[serde(default = "default_expires_days")]
    pub expires_days: i64,
}

fn default_expires_days() -> i64 {
    7 // Default 7 days expiry
}

#[derive(Debug, Serialize)]
pub struct InviteResponse {
    pub invite_code: String,
    pub email: String,
    pub role: String,
    pub expires_at: String,
    pub created_at: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
    message: String,
}

/// Create a new invite
pub async fn create_invite(
    dynamo_client: &DynamoClient,
    ses_client: &SesClient,
    table_name: &str,
    admin_user_id: &str,
    body: &Body,
) -> Result<Response<Body>, Error> {
    let body_str = match body {
        Body::Text(text) => text,
        Body::Binary(bytes) => std::str::from_utf8(bytes).unwrap_or(""),
        Body::Empty => "",
    };

    let request: CreateInviteRequest = match serde_json::from_str(body_str) {
        Ok(req) => req,
        Err(e) => {
            let error = ErrorResponse {
                error: "InvalidRequest".to_string(),
                message: format!("Invalid request body: {}", e),
            };
            return Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header("Content-Type", "application/json")
                .header("Access-Control-Allow-Origin", "*")
                .body(serde_json::to_string(&error)?.into())
                .map_err(Box::new)?);
        }
    };

    let invite_code = Uuid::new_v4().to_string();
    let now = Utc::now();
    let expires_at = now + chrono::Duration::days(request.expires_days);

    // Store invite in DynamoDB
    let result = dynamo_client
        .put_item()
        .table_name(table_name)
        .item("PK", aws_sdk_dynamodb::types::AttributeValue::S(format!("INVITE#{}", invite_code)))
        .item("SK", aws_sdk_dynamodb::types::AttributeValue::S("METADATA".to_string()))
        .item("invite_code", aws_sdk_dynamodb::types::AttributeValue::S(invite_code.clone()))
        .item("email", aws_sdk_dynamodb::types::AttributeValue::S(request.email.clone()))
        .item("status", aws_sdk_dynamodb::types::AttributeValue::S("pending".to_string()))
        .item("created_by", aws_sdk_dynamodb::types::AttributeValue::S(admin_user_id.to_string()))
        .item("role", aws_sdk_dynamodb::types::AttributeValue::S(request.role.clone()))
        .item("created_at", aws_sdk_dynamodb::types::AttributeValue::S(now.to_rfc3339()))
        .item("expires_at", aws_sdk_dynamodb::types::AttributeValue::S(expires_at.to_rfc3339()))
        .send()
        .await;

    match result {
        Ok(_) => {
            // Send invite email
            let frontend_url = env::var("FRONTEND_URL").unwrap_or_else(|_| "http://localhost:8080".to_string());
            
            if let Err(e) = crate::email::send_invite_email(
                ses_client,
                &request.email,
                &invite_code,
                &frontend_url,
            )
            .await
            {
                tracing::error!("Failed to send invite email: {}", e);
                // Don't fail the invite creation if email fails
                // The invite code is still valid and can be shared manually
            } else {
                tracing::info!("Invite email sent successfully to {}", request.email);
            }
            
            let response = InviteResponse {
                invite_code,
                email: request.email,
                role: request.role,
                expires_at: expires_at.to_rfc3339(),
                created_at: now.to_rfc3339(),
                status: "pending".to_string(),
            };

            Ok(Response::builder()
                .status(StatusCode::CREATED)
                .header("Content-Type", "application/json")
                .header("Access-Control-Allow-Origin", "*")
                .body(serde_json::to_string(&response)?.into())
                .map_err(Box::new)?)
        }
        Err(e) => {
            tracing::error!("Failed to create invite: {:?}", e);
            let error = ErrorResponse {
                error: "InviteCreationFailed".to_string(),
                message: "Failed to create invite".to_string(),
            };
            Ok(Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header("Content-Type", "application/json")
                .header("Access-Control-Allow-Origin", "*")
                .body(serde_json::to_string(&error)?.into())
                .map_err(Box::new)?)
        }
    }
}

/// Validate an invite code
pub async fn validate_invite(
    client: &DynamoClient,
    table_name: &str,
    invite_code: &str,
    email: &str,
) -> Result<bool, String> {
    let result = client
        .get_item()
        .table_name(table_name)
        .key("PK", aws_sdk_dynamodb::types::AttributeValue::S(format!("INVITE#{}", invite_code)))
        .key("SK", aws_sdk_dynamodb::types::AttributeValue::S("METADATA".to_string()))
        .send()
        .await
        .map_err(|e| format!("Failed to fetch invite: {:?}", e))?;

    let item = result.item().ok_or("Invite code not found")?;

    // Check status
    let status = item
        .get("status")
        .and_then(|v| v.as_s().ok())
        .ok_or("Invalid invite status")?;

    if status != "pending" {
        return Err("Invite code has already been used".to_string());
    }

    // Check email match
    let invite_email = item
        .get("email")
        .and_then(|v| v.as_s().ok())
        .ok_or("Invalid invite email")?;

    if invite_email != email {
        return Err("Email does not match invite".to_string());
    }

    // Check expiry
    let expires_at = item
        .get("expires_at")
        .and_then(|v| v.as_s().ok())
        .ok_or("Invalid invite expiry")?;

    let expiry_time = chrono::DateTime::parse_from_rfc3339(expires_at)
        .map_err(|_| "Invalid expiry format")?;

    if expiry_time < Utc::now() {
        return Err("Invite code has expired".to_string());
    }

    Ok(true)
}

/// Mark invite as used
pub async fn mark_invite_used(
    client: &DynamoClient,
    table_name: &str,
    invite_code: &str,
) -> Result<(), String> {
    client
        .update_item()
        .table_name(table_name)
        .key("PK", aws_sdk_dynamodb::types::AttributeValue::S(format!("INVITE#{}", invite_code)))
        .key("SK", aws_sdk_dynamodb::types::AttributeValue::S("METADATA".to_string()))
        .update_expression("SET #status = :used, used_at = :now")
        .expression_attribute_names("#status", "status")
        .expression_attribute_values(":used", aws_sdk_dynamodb::types::AttributeValue::S("active".to_string()))
        .expression_attribute_values(":now", aws_sdk_dynamodb::types::AttributeValue::S(Utc::now().to_rfc3339()))
        .send()
        .await
        .map_err(|e| format!("Failed to mark invite as used: {:?}", e))?;

    Ok(())
}

/// List all invites (admin-only)
pub async fn list_invites(
    client: &DynamoClient,
    table_name: &str,
) -> Result<Response<Body>, Error> {
    // Scan for all INVITE# records
    let mut invites = Vec::new();
    let mut last_key = None;

    loop {
        let mut scan = client
            .scan()
            .table_name(table_name)
            .filter_expression("begins_with(PK, :pk_prefix)")
            .expression_attribute_values(":pk_prefix", aws_sdk_dynamodb::types::AttributeValue::S("INVITE#".to_string()));

        if let Some(key) = last_key {
            scan = scan.set_exclusive_start_key(Some(key));
        }

        let result = scan.send().await.map_err(|e| format!("DynamoDB scan error: {:?}", e))?;

        for item in result.items() {
            let invite_code = item.get("invite_code").and_then(|v| v.as_s().ok()).unwrap_or(&String::new()).clone();
            let email = item.get("email").and_then(|v| v.as_s().ok()).unwrap_or(&String::new()).clone();
            let role = item.get("role").and_then(|v| v.as_s().ok()).unwrap_or(&String::new()).clone();
            let status = item.get("status").and_then(|v| v.as_s().ok()).unwrap_or(&String::new()).clone();
            let expires_at = item.get("expires_at").and_then(|v| v.as_s().ok()).unwrap_or(&String::new()).clone();
            let created_at = item.get("created_at").and_then(|v| v.as_s().ok()).unwrap_or(&String::new()).clone();

            invites.push(InviteResponse {
                invite_code,
                email,
                role,
                status,
                expires_at,
                created_at,
            });
        }

        match result.last_evaluated_key() {
            Some(key) => last_key = Some(key.clone()),
            None => break,
        }
    }

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(serde_json::to_string(&invites)?.into())
        .map_err(Box::new)?)
}

/// Delete an invite (admin-only)
/// Also removes the corresponding Cognito user if they already signed up.
pub async fn delete_invite(
    client: &DynamoClient,
    cognito_client: &CognitoClient,
    table_name: &str,
    invite_code: &str,
) -> Result<Response<Body>, Error> {
    // Fetch the invite first to get the email
    let invite_result = client
        .get_item()
        .table_name(table_name)
        .key("PK", aws_sdk_dynamodb::types::AttributeValue::S(format!("INVITE#{}", invite_code)))
        .key("SK", aws_sdk_dynamodb::types::AttributeValue::S("METADATA".to_string()))
        .send()
        .await
        .map_err(|e| format!("Failed to fetch invite: {:?}", e))?;

    // Try to delete the Cognito user if we have the email
    if let Some(item) = invite_result.item() {
        if let Some(email) = item.get("email").and_then(|v| v.as_s().ok()) {
            if let Ok(user_pool_id) = env::var("COGNITO_USER_POOL_ID") {
                match cognito_client
                    .admin_delete_user()
                    .user_pool_id(&user_pool_id)
                    .username(email)
                    .send()
                    .await
                {
                    Ok(_) => tracing::info!("Deleted Cognito user: {}", email),
                    Err(e) => tracing::warn!("Could not delete Cognito user (may not exist): {:?}", e),
                }
            }
        }
    }

    // Delete the invite from DynamoDB
    client
        .delete_item()
        .table_name(table_name)
        .key("PK", aws_sdk_dynamodb::types::AttributeValue::S(format!("INVITE#{}", invite_code)))
        .key("SK", aws_sdk_dynamodb::types::AttributeValue::S("METADATA".to_string()))
        .send()
        .await
        .map_err(|e| format!("Failed to delete invite: {:?}", e))?;

    Ok(Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header("Access-Control-Allow-Origin", "*")
        .body(Body::Empty)
        .map_err(Box::new)?)
}

/// Get invite details (for frontend to pre-fill email)
pub async fn get_invite(
    client: &DynamoClient,
    table_name: &str,
    invite_code: &str,
) -> Result<Response<Body>, Error> {
    let result = client
        .get_item()
        .table_name(table_name)
        .key("PK", aws_sdk_dynamodb::types::AttributeValue::S(format!("INVITE#{}", invite_code)))
        .key("SK", aws_sdk_dynamodb::types::AttributeValue::S("METADATA".to_string()))
        .send()
        .await;

    match result {
        Ok(output) => {
            if let Some(item) = output.item() {
                let email = item.get("email").and_then(|v| v.as_s().ok()).unwrap_or(&String::new()).clone();
                let role = item.get("role").and_then(|v| v.as_s().ok()).unwrap_or(&String::new()).clone();
                let status = item.get("status").and_then(|v| v.as_s().ok()).unwrap_or(&String::new()).clone();
                let expires_at = item.get("expires_at").and_then(|v| v.as_s().ok()).unwrap_or(&String::new()).clone();
                let created_at = item.get("created_at").and_then(|v| v.as_s().ok()).unwrap_or(&String::new()).clone();

                let response = InviteResponse {
                    invite_code: invite_code.to_string(),
                    email: email.to_string(),
                    role: role.to_string(),
                    expires_at: expires_at.to_string(),
                    created_at: created_at.to_string(),
                    status: status.to_string(),
                };

                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", "application/json")
                    .header("Access-Control-Allow-Origin", "*")
                    .body(serde_json::to_string(&response)?.into())
                    .map_err(Box::new)?)
            } else {
                let error = ErrorResponse {
                    error: "NotFound".to_string(),
                    message: "Invite code not found".to_string(),
                };
                Ok(Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .header("Content-Type", "application/json")
                    .header("Access-Control-Allow-Origin", "*")
                    .body(serde_json::to_string(&error)?.into())
                    .map_err(Box::new)?)
            }
        }
        Err(e) => {
            tracing::error!("Failed to get invite: {:?}", e);
            let error = ErrorResponse {
                error: "InviteFetchFailed".to_string(),
                message: "Failed to fetch invite".to_string(),
            };
            Ok(Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header("Content-Type", "application/json")
                .header("Access-Control-Allow-Origin", "*")
                .body(serde_json::to_string(&error)?.into())
                .map_err(Box::new)?)
        }
    }
}
