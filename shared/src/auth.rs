use aws_sdk_cognitoidentityprovider::{
    types::AuthFlowType::RefreshTokenAuth, Client as CognitoClient,
};
use base64::{engine::general_purpose, Engine as _};
use hmac::{Hmac, Mac};
use lambda_http::{http::StatusCode, Body, Error, Response};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::str::from_utf8;

// Cookie configuration
const COOKIE_DOMAIN: &str = ".doxle.ai";
const ACCESS_TOKEN_COOKIE: &str = "access_token";
const REFRESH_TOKEN_COOKIE: &str = "refresh_token";

// Allowed origins for CORS
const ALLOWED_ORIGINS: &[&str] = &["https://doxle.ai", "http://localhost:8080"];

/// Get CORS origin - returns the origin if allowed, otherwise None
pub fn get_cors_origin(origin: Option<&str>) -> &'static str {
    match origin {
        Some(o) if ALLOWED_ORIGINS.contains(&o) => {
            if o == "https://doxle.ai" { "https://doxle.ai" }
            else { "http://localhost:8080" }
        }
        _ => "https://doxle.ai" // default
    }
}

/// Create a Set-Cookie header value for httpOnly secure cookie
pub fn create_cookie(name: &str, value: &str, max_age_secs: i64, http_only: bool) -> String {
    let mut cookie = format!(
        "{}={}; Domain={}; Path=/; Max-Age={}; SameSite=None; Secure",
        name, value, COOKIE_DOMAIN, max_age_secs
    );
    if http_only {
        cookie.push_str("; HttpOnly");
    }
    cookie
}

/// Create cookie that clears/expires immediately
pub fn clear_cookie(name: &str) -> String {
    format!(
        "{}=; Domain={}; Path=/; Max-Age=0; SameSite=None; Secure; HttpOnly",
        name, COOKIE_DOMAIN
    )
}

/// Extract a cookie value from Cookie header
pub fn get_cookie_value(cookie_header: &str, name: &str) -> Option<String> {
    cookie_header
        .split(';')
        .map(|s| s.trim())
        .find(|s| s.starts_with(&format!("{}=", name)))
        .and_then(|s| s.split('=').nth(1))
        .map(|s| s.to_string())
}

#[derive(Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct SignupRequest {
    pub email: String,
    pub password: String,
    pub invite_code: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub id_token: String,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i32,
}

#[derive(Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
    pub message: String,
}

type HmacSha256 = Hmac<Sha256>;

/// Compute the SECRET_HASH for Cognito authentication
fn compute_secret_hash(username: &str, client_id: &str, client_secret: &str) -> String {
    let message = format!("{}{}", username, client_id);
    let mut mac = HmacSha256::new_from_slice(client_secret.as_bytes())
        .expect("HMAC can take key of any size");
    mac.update(message.as_bytes());
    let result = mac.finalize();
    general_purpose::STANDARD.encode(result.into_bytes())
}

/// Handle user login with Cognito
pub async fn login(
    cognito_client: &CognitoClient,
    client_id: &str,
    client_secret: &str,
    body: &Body,
) -> Result<Response<Body>, Error> {
    // Parse request body
    let body_str = match body {
        Body::Text(text) => text,
        Body::Binary(bytes) => std::str::from_utf8(bytes).unwrap_or(""),
        Body::Empty => "",
    };

    tracing::info!("Login request received");

    let login_request: LoginRequest = match serde_json::from_str(body_str) {
        Ok(req) => req,
        Err(e) => {
            tracing::error!("Failed to parse request body: {}", e);
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

    tracing::info!("Authenticating user: {}", login_request.email);

    // Compute SECRET_HASH
    let secret_hash = compute_secret_hash(&login_request.email, client_id, client_secret);

    // Authenticate with Cognito
    let auth_result = cognito_client
        .initiate_auth()
        .auth_flow(aws_sdk_cognitoidentityprovider::types::AuthFlowType::UserPasswordAuth)
        .client_id(client_id)
        .auth_parameters("USERNAME", &login_request.email)
        .auth_parameters("PASSWORD", &login_request.password)
        .auth_parameters("SECRET_HASH", &secret_hash)
        .send()
        .await;

    match auth_result {
        Ok(response) => {
            if let Some(auth_result) = response.authentication_result() {
                tracing::info!(
                    "Authentication successful for user: {}",
                    login_request.email
                );

                let login_response = LoginResponse {
                    id_token: auth_result.id_token().unwrap_or_default().to_string(),
                    access_token: auth_result.access_token().unwrap_or_default().to_string(),
                    refresh_token: auth_result.refresh_token().unwrap_or_default().to_string(),
                    expires_in: auth_result.expires_in(),
                };

                // Create httpOnly cookies
                let access_cookie = create_cookie(
                    ACCESS_TOKEN_COOKIE,
                    &login_response.access_token,
                    login_response.expires_in as i64,
                    false, // access_token readable by JS (for now)
                );
                let refresh_cookie = create_cookie(
                    REFRESH_TOKEN_COOKIE,
                    &login_response.refresh_token,
                    60 * 60 * 24 * 30, // 30 days
                    true, // httpOnly - JS cannot read
                );

                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", "application/json")
                    .header("Access-Control-Allow-Origin", "https://doxle.ai")
                    .header("Access-Control-Allow-Credentials", "true")
                    .header("Set-Cookie", access_cookie)
                    .header("Set-Cookie", refresh_cookie)
                    .body(serde_json::to_string(&login_response)?.into())
                    .map_err(Box::new)?)
            } else {
                tracing::error!("No authentication result returned");
                let error = ErrorResponse {
                    error: "AuthenticationFailed".to_string(),
                    message: "No authentication result returned".to_string(),
                };
                Ok(Response::builder()
                    .status(StatusCode::UNAUTHORIZED)
                    .header("Content-Type", "application/json")
                    .header("Access-Control-Allow-Origin", "*")
                    .body(serde_json::to_string(&error)?.into())
                    .map_err(Box::new)?)
            }
        }
        Err(e) => {
            let error_message = format!("{:?}", e);
            tracing::error!("Cognito authentication error: {}", error_message);

            // Extract user-friendly error message
            let user_message = if error_message.contains("NotAuthorizedException") {
                "Incorrect email or password".to_string()
            } else if error_message.contains("UserNotConfirmedException") {
                "Please verify your email before logging in".to_string()
            } else if error_message.contains("UserNotFoundException") {
                "No account found with this email".to_string()
            } else if error_message.contains("PasswordResetRequiredException") {
                "Password reset required".to_string()
            } else if error_message.contains("TooManyRequestsException") {
                "Too many login attempts. Please try again later".to_string()
            } else {
                "Login failed. Please check your credentials".to_string()
            };

            let error = ErrorResponse {
                error: "AuthenticationFailed".to_string(),
                message: user_message,
            };
            Ok(Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .header("Content-Type", "application/json")
                .header("Access-Control-Allow-Origin", "*")
                .body(serde_json::to_string(&error)?.into())
                .map_err(Box::new)?)
        }
    }
}

/// Handle user signup with Cognito
pub async fn signup(
    cognito_client: &CognitoClient,
    dynamo_client: &aws_sdk_dynamodb::Client,
    table_name: &str,
    client_id: &str,
    client_secret: &str,
    body: &Body,
) -> Result<Response<Body>, Error> {
    let body_str = match body {
        Body::Text(text) => text,
        Body::Binary(bytes) => std::str::from_utf8(bytes).unwrap_or(""),
        Body::Empty => "",
    };

    tracing::info!("Signup request received");

    let signup_request: SignupRequest = match serde_json::from_str(body_str) {
        Ok(req) => req,
        Err(e) => {
            tracing::error!("Failed to parse request body: {}", e);
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

    tracing::info!("Signing up user: {}", signup_request.email);

    // Validate invite code
    if let Err(e) = crate::invites::validate_invite(
        dynamo_client,
        table_name,
        &signup_request.invite_code,
        &signup_request.email,
    )
    .await
    {
        let error = ErrorResponse {
            error: "InvalidInvite".to_string(),
            message: e,
        };
        return Ok(Response::builder()
            .status(StatusCode::FORBIDDEN)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::to_string(&error)?.into())
            .map_err(Box::new)?);
    }

    let secret_hash = compute_secret_hash(&signup_request.email, client_id, client_secret);

    let signup_result = cognito_client
        .sign_up()
        .client_id(client_id)
        .username(&signup_request.email)
        .password(&signup_request.password)
        .secret_hash(&secret_hash)
        .user_attributes(
            aws_sdk_cognitoidentityprovider::types::AttributeType::builder()
                .name("email")
                .value(&signup_request.email)
                .build()?,
        )
        .send()
        .await;

    match signup_result {
        Ok(_response) => {
            tracing::info!("Signup successful for user: {}", signup_request.email);

            // Auto-confirm user since they used a valid invite (email already verified)
            if let Ok(user_pool_id) = std::env::var("COGNITO_USER_POOL_ID") {
                if let Err(e) = cognito_client
                    .admin_confirm_sign_up()
                    .user_pool_id(&user_pool_id)
                    .username(&signup_request.email)
                    .send()
                    .await
                {
                    tracing::error!("Failed to auto-confirm user: {:?}", e);
                    // Don't fail signup, user can still verify via email
                } else {
                    tracing::info!("User auto-confirmed: {}", signup_request.email);
                }
            } else {
                tracing::warn!("COGNITO_USER_POOL_ID not set; skipping auto-confirm");
            }

            // Mark invite as used
            if let Err(e) = crate::invites::mark_invite_used(
                dynamo_client,
                table_name,
                &signup_request.invite_code,
            )
            .await
            {
                tracing::error!("Failed to mark invite as used: {}", e);
                // Don't fail the signup if we can't mark invite as used
            }

            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json")
                .header("Access-Control-Allow-Origin", "*")
                .body(
                    serde_json::json!({"message": "Signup successful"})
                        .to_string()
                        .into(),
                )
                .map_err(Box::new)?)
        }
        Err(e) => {
            let error_message = format!("{:?}", e);
            tracing::error!("Cognito signup error: {}", error_message);

            // Extract user-friendly error message (only send this to frontend)
            let user_message = if error_message.contains("InvalidPasswordException") {
                "Password must contain at least 8 characters with uppercase, lowercase, number, and special character".to_string()
            } else if error_message.contains("UsernameExistsException") {
                "An account with this email already exists".to_string()
            } else if error_message.contains("InvalidParameterException") {
                "Invalid email or password format".to_string()
            } else {
                "Signup failed. Please check your credentials and try again.".to_string()
            };

            let error = ErrorResponse {
                error: "SignupFailed".to_string(),
                message: user_message,
            };
            Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header("Content-Type", "application/json")
                .header("Access-Control-Allow-Origin", "*")
                .body(serde_json::to_string(&error)?.into())
                .map_err(Box::new)?)
        }
    }
}

/// Handle token refresh with Cognitio
pub async fn refresh_token(
    cognitio_client: &CognitoClient,
    client_id: &str,
    _client_secret: &str,
    body: &Body,
) -> Result<Response<Body>, Error> {
    let body_str = match body {
        Body::Text(text) => text,
        Body::Binary(bytes) => from_utf8(bytes).unwrap_or(""),
        Body::Empty => "",
    };

    tracing::info!("Token refresh request received");

    let refresh_request: RefreshRequest = match serde_json::from_str(body_str) {
        Ok(req) => req,
        Err(e) => {
            tracing::error!("Failed to parse request body: {}", e);
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

    tracing::info!("Refreshing token using REFRESH_TOKEN_AUTH flow");
    let auth_result = cognitio_client
        .initiate_auth()
        .auth_flow(RefreshTokenAuth)
        .client_id(client_id)
        .auth_parameters("REFRESH_TOKEN", &refresh_request.refresh_token)
        .send()
        .await;

    match auth_result {
        Ok(response) => {
            if let Some(auth_result) = response.authentication_result() {
                tracing::info!("Token refreshed successfully");
                let login_response = LoginResponse {
                    id_token: auth_result.id_token().unwrap_or_default().to_string(),
                    access_token: auth_result.access_token().unwrap_or_default().to_string(),
                    refresh_token: auth_result
                        .refresh_token()
                        .unwrap_or(&refresh_request.refresh_token)
                        .to_string(),
                    expires_in: auth_result.expires_in(),
                };
                 // Create httpOnly cookies
                let access_cookie = create_cookie(
                    ACCESS_TOKEN_COOKIE,
                    &login_response.access_token,
                    login_response.expires_in as i64,
                    false,
                );
                let refresh_cookie = create_cookie(
                    REFRESH_TOKEN_COOKIE,
                    &login_response.refresh_token,
                    60 * 60 * 24 * 30, // 30 days for refresh token
                    true,
                );
                let response = Response::builder()
                    .status(StatusCode::OK)
                    .header("Access-Control-Allow-Origin", "https://doxle.ai")
                    .header("Access-Control-Allow-Credentials", "true")
                    .header("Set-Cookie", access_cookie)
                    .header("Set-Cookie", refresh_cookie)
                    .body(serde_json::to_string(&login_response)?.into())
                    .map_err(Box::new)?;
                Ok(response)
            } else {
                tracing::error!("No authentication result returned from refresh");
                let error = ErrorResponse {
                    error: "RefreshFailed".to_string(),
                    message: "No authentication result returned".to_string(),
                };
                Ok(Response::builder()
                    .status(StatusCode::UNAUTHORIZED)
                    .header("Content-Type", "application/json")
                    .header("Access-Control-Allow-Origin", "*")
                    .body(serde_json::to_string(&error)?.into())
                    .map_err(Box::new)?)
            }
        }
        Err(e) => {
            let error_message = format!("{:?}", e);
            tracing::error!("Cognito refresh error: {}", error_message);

            let user_message = if error_message.contains("NotAuthorizedException") {
                "Refresh token expired or invalid. Please login again".to_string()
            } else {
                "Token refresh failed. Please login again".to_string()
            };

            let error = ErrorResponse {
                error: "RefreshFailed".to_string(),
                message: user_message,
            };
            Ok(Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .header("Content-Type", "application/json")
                .header("Access-Control-Allow-Origin", "*")
                .body(serde_json::to_string(&error)?.into())
                .map_err(Box::new)?)
        }
    }
}
