use aws_sdk_cognitoidentityprovider::{
    types::AuthFlowType::RefreshTokenAuth, Client as CognitoClient,
};
use aws_sdk_dynamodb::Client as DynamoClient;
use aws_sdk_dynamodb::types::AttributeValue;
use base64::{engine::general_purpose, Engine as _};
use doxle_atoms::users::model::UserRole;
use hmac::{Hmac, Mac};
use lambda_http::{http::StatusCode, Body, Error, Response};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::str::from_utf8;

// Cookie configuration
// NOTE: Auth cookies should be isolated to the API host so they are NOT sent with CloudFront image requests.
// Set IS_LOCAL=true as a Lambda/runtime env var for local dev. Defaults to deployed (production).
pub fn is_local() -> bool {
    std::env::var("IS_LOCAL")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false)
}

pub fn cookie_domain() -> &'static str {
    if is_local() { "localhost" } else { "api.doxle.ai" }
}
pub const LEGACY_COOKIE_DOMAIN: &str = ".doxle.ai";
pub const ACCESS_TOKEN_COOKIE: &str = "access_token";
pub const REFRESH_TOKEN_COOKIE: &str = "refresh_token";
pub const USERNAME_COOKIE: &str = "cognito_username";

// Allowed origins for CORS
const ALLOWED_ORIGINS: &[&str] = &[
    "https://doxle.ai",
    "https://annotate.doxle.ai",
    "http://localhost:8080",
    "http://localhost:3000",  // LOCAL backend
];

/// Get CORS origin - returns the origin if allowed, otherwise falls back to the default.
pub fn get_cors_origin(origin: Option<&str>) -> String {
    match origin {
        Some(o) if ALLOWED_ORIGINS.contains(&o) => o.to_string(),
        _ => "https://doxle.ai".to_string(),
    }
}

/// Create a Set-Cookie header value for httpOnly secure cookie
pub fn create_cookie(name: &str, value: &str, max_age_secs: i64, http_only: bool) -> String {
    let mut cookie = if is_local() {
        // LOCAL: SameSite=None allows cross-origin cookies (localhost:8080 -> localhost:9000)
        // Secure flag works on localhost in modern browsers
        format!(
            "{}={}; Path=/; Max-Age={}; SameSite=None; Secure",
            name, value, max_age_secs
        )
    } else {
        // DEPLOYED: Secure + SameSite=None for cross-origin HTTPS
        format!(
            "{}={}; Domain={}; Path=/; Max-Age={}; SameSite=None; Secure",
            name, value, cookie_domain(), max_age_secs
        )
    };
    if http_only {
        cookie.push_str("; HttpOnly");
    }
    cookie
}

pub fn clear_cookie_for_domain(name: &str, domain: &str) -> String {
    if is_local() {
        // LOCAL: SameSite=None (matches create_cookie behavior)
        format!(
            "{}=; Path=/; Max-Age=0; SameSite=None; Secure; HttpOnly",
            name
        )
    } else {
        // DEPLOYED: Secure + SameSite=None
        format!(
            "{}=; Domain={}; Path=/; Max-Age=0; SameSite=None; Secure; HttpOnly",
            name, domain
        )
    }
}

/// Create cookie that clears/expires immediately
pub fn clear_cookie(name: &str) -> String {
    clear_cookie_for_domain(name, cookie_domain())
}

/// Extract a cookie value from Cookie header
pub fn get_cookie_value(cookie_header: &str, name: &str) -> Option<String> {
    cookie_header
        .split(';')
        .map(|s| s.trim())
        .filter(|s| s.starts_with(&format!("{}=", name)))
        .last()
        .and_then(|s| s.split('=').nth(1))
        .map(|s| s.to_string())
}

pub struct CookieAuthResult {
    pub user_id: String,
    pub user_role: UserRole,
    pub set_cookies: Vec<String>,
}

/// Fetch user role from DynamoDB. Returns Annotator if not found.
async fn fetch_user_role(
    dynamo_client: &DynamoClient,
    table_name: &str,
    user_id: &str,
) -> UserRole {
    // Super-admin bypass — always Admin even if DynamoDB record is missing/deleted
    if let Ok(super_admin) = std::env::var("SUPER_ADMIN_SUB") {
        if user_id == super_admin {
            return UserRole::Admin;
        }
    }

    let sk = format!("USER#{}", user_id);
    let result = dynamo_client
        .get_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S("USER".to_string()))
        .key("SK", AttributeValue::S(sk))
        .projection_expression("user_role")
        .send()
        .await;

    match result {
        Ok(output) => output
            .item()
            .and_then(|item| item.get("user_role"))
            .and_then(|v| v.as_s().ok())
            .map(|s| UserRole::from_str_loose(s))
            .unwrap_or(UserRole::Annotator),
        Err(e) => {
            tracing::warn!("Failed to fetch user role: {}", e);
            UserRole::Annotator
        }
    }
}

fn extract_sub_and_exp_from_jwt(token: &str) -> Option<(String, i64)> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }

    let decoded = general_purpose::URL_SAFE_NO_PAD.decode(parts[1]).ok()?;
    let json_str = String::from_utf8(decoded).ok()?;
    let json: serde_json::Value = serde_json::from_str(&json_str).ok()?;

    let sub = json.get("sub")?.as_str()?.to_string();
    let exp = json.get("exp")?.as_i64()?;

    Some((sub, exp))
}

fn unix_now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn unauthorized_clear_cookies(message: &str) -> Result<Response<Body>, Error> {
    let error = ErrorResponse {
        error: "Unauthorized".to_string(),
        message: message.to_string(),
    };

    Ok(Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("Content-Type", "application/json")
        .header("Set-Cookie", clear_cookie(ACCESS_TOKEN_COOKIE))
        .header("Set-Cookie", clear_cookie_for_domain(ACCESS_TOKEN_COOKIE, LEGACY_COOKIE_DOMAIN))
        .header("Set-Cookie", clear_cookie(REFRESH_TOKEN_COOKIE))
        .header("Set-Cookie", clear_cookie_for_domain(REFRESH_TOKEN_COOKIE, LEGACY_COOKIE_DOMAIN))
        .header("Set-Cookie", clear_cookie(USERNAME_COOKIE))
        .header("Set-Cookie", clear_cookie_for_domain(USERNAME_COOKIE, LEGACY_COOKIE_DOMAIN))
        .body(serde_json::to_string(&error)?.into())
        .map_err(Box::new)?)
}

/// Cookie-based auth with auto-refresh.
///
/// - Reads access_token/refresh_token/username from cookies
/// - If access token is missing/expired, refreshes with Cognito
/// - Returns user_id + any Set-Cookie values to attach to the final response
pub async fn authenticate_cookie_request(
    cognito_client: &CognitoClient,
    dynamo_client: &DynamoClient,
    table_name: &str,
    client_id: &str,
    client_secret: &str,
    cookie_header: Option<&str>,
) -> Result<CookieAuthResult, Response<Body>> {
    let cookie_header = cookie_header.ok_or_else(|| {
        unauthorized_clear_cookies("Missing Cookie header")
            .unwrap_or_else(|_| Response::builder().status(StatusCode::UNAUTHORIZED).body(Body::Empty).unwrap())
    })?;

    let access_token = get_cookie_value(cookie_header, ACCESS_TOKEN_COOKIE);
    let refresh_token = get_cookie_value(cookie_header, REFRESH_TOKEN_COOKIE);
    let username = get_cookie_value(cookie_header, USERNAME_COOKIE);

    // Fast path: valid access token
    if let Some(access_token) = &access_token {
        if let Some((user_id, exp)) = extract_sub_and_exp_from_jwt(access_token) {
            if unix_now_secs() <= exp {
                let user_role = fetch_user_role(dynamo_client, table_name, &user_id).await;
                return Ok(CookieAuthResult {
                    user_id,
                    user_role,
                    set_cookies: vec![],
                });
            }
        }
    }

    // Refresh path
    let refresh_token = match refresh_token {
        Some(t) if !t.is_empty() => t,
        _ => {
            return Err(
                unauthorized_clear_cookies("Missing refresh token")
                    .unwrap_or_else(|_| Response::builder().status(StatusCode::UNAUTHORIZED).body(Body::Empty).unwrap()),
            )
        }
    };

    let username = username.or_else(|| {
        access_token
            .as_ref()
            .and_then(|t| extract_username_from_token(t))
    });

    let username = match username {
        Some(u) if !u.is_empty() => u,
        _ => {
            return Err(
                unauthorized_clear_cookies("Missing username for refresh")
                    .unwrap_or_else(|_| Response::builder().status(StatusCode::UNAUTHORIZED).body(Body::Empty).unwrap()),
            )
        }
    };

    let secret_hash = compute_secret_hash(&username, client_id, client_secret);

    let auth_result = cognito_client
        .initiate_auth()
        .auth_flow(RefreshTokenAuth)
        .client_id(client_id)
        .auth_parameters("REFRESH_TOKEN", &refresh_token)
        .auth_parameters("SECRET_HASH", &secret_hash)
        .send()
        .await;

    match auth_result {
        Ok(response) => {
            let auth_result = match response.authentication_result() {
                Some(r) => r,
                None => {
                    return Err(
                        unauthorized_clear_cookies("Refresh failed")
                            .unwrap_or_else(|_| {
                                Response::builder().status(StatusCode::UNAUTHORIZED).body(Body::Empty).unwrap()
                            }),
                    )
                }
            };

            let access_token = auth_result.access_token().unwrap_or_default().to_string();
            let refresh_token = auth_result
                .refresh_token()
                .unwrap_or(&refresh_token)
                .to_string();
            let expires_in = auth_result.expires_in();

            let (user_id, _) = extract_sub_and_exp_from_jwt(&access_token).ok_or_else(|| {
                unauthorized_clear_cookies("Invalid refreshed access token")
                    .unwrap_or_else(|_| Response::builder().status(StatusCode::UNAUTHORIZED).body(Body::Empty).unwrap())
            })?;

            let access_cookie = create_cookie(
                ACCESS_TOKEN_COOKIE,
                &access_token,
                expires_in as i64,
                true,
            );
            let refresh_cookie = create_cookie(
                REFRESH_TOKEN_COOKIE,
                &refresh_token,
                60 * 60 * 24 * 30,
                true,
            );
            let username_cookie = create_cookie(
                USERNAME_COOKIE,
                &username,
                60 * 60 * 24 * 30,
                true,
            );

            let user_role = fetch_user_role(dynamo_client, table_name, &user_id).await;
            Ok(CookieAuthResult {
                user_id,
                user_role,
                set_cookies: vec![access_cookie, refresh_cookie, username_cookie],
            })
        }
        Err(_e) => Err(
            unauthorized_clear_cookies("Refresh token expired or invalid")
                .unwrap_or_else(|_| Response::builder().status(StatusCode::UNAUTHORIZED).body(Body::Empty).unwrap()),
        ),
    }
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
pub struct SessionResponse {
    pub message: String,
    pub expires_in: i32,
}

struct TokenSet {
    _id_token: String,
    access_token: String,
    refresh_token: String,
    expires_in: i32,
}

#[derive(Deserialize, Default)]
pub struct RefreshRequest {
    #[serde(default)]
    pub refresh_token: Option<String>,
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

/// Extract username/sub from a JWT token (access token or refresh token)
fn extract_username_from_token(token: &str) -> Option<String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() < 2 {
        return None;
    }
    
    // Try standard base64 first, then URL-safe
    let decoded = general_purpose::STANDARD.decode(parts[1])
        .or_else(|_| general_purpose::URL_SAFE_NO_PAD.decode(parts[1]))
        .ok()?;
    
    let json_str = String::from_utf8(decoded).ok()?;
    let json: serde_json::Value = serde_json::from_str(&json_str).ok()?;
    
    // Try 'username' first (Cognito uses this), then 'sub', then 'email'
    json.get("username")
        .or_else(|| json.get("cognito:username"))
        .or_else(|| json.get("sub"))
        .or_else(|| json.get("email"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
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

                let token_set = TokenSet {
                    _id_token: auth_result.id_token().unwrap_or_default().to_string(),
                    access_token: auth_result.access_token().unwrap_or_default().to_string(),
                    refresh_token: auth_result.refresh_token().unwrap_or_default().to_string(),
                    expires_in: auth_result.expires_in(),
                };

                // Create httpOnly cookies (no frontend token management)
                let access_cookie = create_cookie(
                    ACCESS_TOKEN_COOKIE,
                    &token_set.access_token,
                    token_set.expires_in as i64,
                    true,
                );
                let refresh_cookie = create_cookie(
                    REFRESH_TOKEN_COOKIE,
                    &token_set.refresh_token,
                    60 * 60 * 24 * 30, // 30 days
                    true,
                );
                let username_cookie = create_cookie(
                    USERNAME_COOKIE,
                    &login_request.email,
                    60 * 60 * 24 * 30, // 30 days
                    true,
                );

                let session_response = SessionResponse {
                    message: "ok".to_string(),
                    expires_in: token_set.expires_in,
                };

                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", "application/json")
                    .header("Access-Control-Allow-Origin", "https://doxle.ai")
                    .header("Access-Control-Allow-Credentials", "true")
                    .header("Set-Cookie", access_cookie)
                    .header("Set-Cookie", refresh_cookie)
                    .header("Set-Cookie", username_cookie)
                    .body(serde_json::to_string(&session_response)?.into())
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

    // Read role from invite record
    let invite_role = {
        let pk = format!("INVITE#{}", signup_request.invite_code);
        let result = dynamo_client
            .get_item()
            .table_name(table_name)
            .key("PK", AttributeValue::S(pk))
            .key("SK", AttributeValue::S("METADATA".to_string()))
            .projection_expression("#r")
            .expression_attribute_names("#r", "role")
            .send()
            .await;
        result
            .ok()
            .and_then(|o| o.item().cloned())
            .and_then(|item| item.get("role").and_then(|v| v.as_s().ok()).map(|s| s.to_string()))
            .unwrap_or_else(|| "annotator".to_string())
    };

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
                    serde_json::json!({"message": "Signup successful", "role": invite_role})
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

#[cfg(test)]
mod tests {
    use super::*;

    // === get_cors_origin ===

    #[test]
    fn cors_allows_known_origins() {
        assert_eq!(get_cors_origin(Some("https://doxle.ai")), "https://doxle.ai");
        assert_eq!(get_cors_origin(Some("https://annotate.doxle.ai")), "https://annotate.doxle.ai");
        assert_eq!(get_cors_origin(Some("http://localhost:8080")), "http://localhost:8080");
        assert_eq!(get_cors_origin(Some("http://localhost:3000")), "http://localhost:3000");
    }

    #[test]
    fn cors_rejects_unknown_origin() {
        assert_eq!(get_cors_origin(Some("https://evil.com")), "https://doxle.ai");
        assert_eq!(get_cors_origin(None), "https://doxle.ai");
    }

    // === get_cookie_value ===

    #[test]
    fn get_cookie_single() {
        assert_eq!(
            get_cookie_value("access_token=abc123", "access_token"),
            Some("abc123".into())
        );
    }

    #[test]
    fn get_cookie_multiple() {
        let header = "access_token=abc; refresh_token=xyz; cognito_username=alice";
        assert_eq!(get_cookie_value(header, "access_token"), Some("abc".into()));
        assert_eq!(get_cookie_value(header, "refresh_token"), Some("xyz".into()));
        assert_eq!(get_cookie_value(header, "cognito_username"), Some("alice".into()));
    }

    #[test]
    fn get_cookie_missing() {
        assert_eq!(get_cookie_value("access_token=abc", "refresh_token"), None);
    }

    #[test]
    fn get_cookie_duplicate_takes_last() {
        // Browser behavior: last value wins for duplicates
        let header = "access_token=old; access_token=new";
        assert_eq!(get_cookie_value(header, "access_token"), Some("new".into()));
    }

    // === create_cookie (deployed mode) ===

    #[test]
    fn create_cookie_deployed_http_only() {
        // Ensure IS_LOCAL is not set for this test
        std::env::remove_var("IS_LOCAL");
        let cookie = create_cookie("access_token", "mytoken", 3600, true);
        assert!(cookie.contains("access_token=mytoken"));
        assert!(cookie.contains("Domain="));
        assert!(cookie.contains("Max-Age=3600"));
        assert!(cookie.contains("Secure"));
        assert!(cookie.contains("SameSite=None"));
        assert!(cookie.contains("HttpOnly"));
    }

    #[test]
    fn create_cookie_not_http_only() {
        std::env::remove_var("IS_LOCAL");
        let cookie = create_cookie("username", "alice", 100, false);
        assert!(!cookie.contains("HttpOnly"));
    }

    // === extract_sub_and_exp_from_jwt ===

    #[test]
    fn jwt_extraction_valid() {
        // Build a fake JWT: header.payload.signature
        // payload: {"sub": "user-123", "exp": 9999999999}
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"sub":"user-123","exp":9999999999}"#);
        let token = format!("header.{}.signature", payload);
        let (sub, exp) = extract_sub_and_exp_from_jwt(&token).unwrap();
        assert_eq!(sub, "user-123");
        assert_eq!(exp, 9999999999);
    }

    #[test]
    fn jwt_extraction_invalid_parts() {
        assert!(extract_sub_and_exp_from_jwt("not-a-jwt").is_none());
        assert!(extract_sub_and_exp_from_jwt("").is_none());
    }

    #[test]
    fn jwt_extraction_missing_fields() {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"iss":"cognito"}"#);
        let token = format!("h.{}.s", payload);
        assert!(extract_sub_and_exp_from_jwt(&token).is_none());
    }

    // === extract_username_from_token ===

    #[test]
    fn extract_username_from_username_field() {
        let payload = base64::engine::general_purpose::STANDARD
            .encode(r#"{"username":"alice@test.com","sub":"sub-123"}"#);
        let token = format!("h.{}.s", payload);
        // Should prefer "username" over "sub"
        assert_eq!(extract_username_from_token(&token), Some("alice@test.com".into()));
    }

    #[test]
    fn extract_username_falls_back_to_sub() {
        let payload = base64::engine::general_purpose::STANDARD
            .encode(r#"{"sub":"sub-456"}"#);
        let token = format!("h.{}.s", payload);
        assert_eq!(extract_username_from_token(&token), Some("sub-456".into()));
    }

    #[test]
    fn extract_username_invalid_token() {
        assert!(extract_username_from_token("bad").is_none());
    }

    // === compute_secret_hash ===

    #[test]
    fn secret_hash_is_deterministic() {
        let h1 = compute_secret_hash("user", "client-id", "secret");
        let h2 = compute_secret_hash("user", "client-id", "secret");
        assert_eq!(h1, h2);
        assert!(!h1.is_empty());
    }

    #[test]
    fn secret_hash_changes_with_input() {
        let h1 = compute_secret_hash("user1", "client-id", "secret");
        let h2 = compute_secret_hash("user2", "client-id", "secret");
        assert_ne!(h1, h2);
    }

    // === LoginRequest / SignupRequest serde ===

    #[test]
    fn login_request_parses() {
        let json = r#"{"email":"a@b.com","password":"pass123"}"#;
        let req: LoginRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.email, "a@b.com");
    }

    #[test]
    fn signup_request_parses() {
        let json = r#"{"email":"a@b.com","password":"pass","invite_code":"abc-123"}"#;
        let req: SignupRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.invite_code, "abc-123");
    }

    #[test]
    fn error_response_serializes() {
        let err = ErrorResponse {
            error: "NotFound".into(),
            message: "Resource not found".into(),
        };
        let json = serde_json::to_string(&err).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["error"], "NotFound");
    }
}

/// Handle token refresh with Cognito
pub async fn refresh_token(
    cognito_client: &CognitoClient,
    client_id: &str,
    client_secret: &str,
    body: &Body,
    cookie_header: Option<&str>,
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

    // Prefer httpOnly cookie; allow body refresh_token for backward compatibility.
    let refresh_token = cookie_header
        .and_then(|h| get_cookie_value(h, REFRESH_TOKEN_COOKIE))
        .or_else(|| refresh_request.refresh_token.clone());

    let refresh_token = match refresh_token {
        Some(t) if !t.is_empty() => t,
        _ => {
            let error = ErrorResponse {
                error: "RefreshFailed".to_string(),
                message: "No refresh token found".to_string(),
            };
            return Ok(Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .header("Content-Type", "application/json")
                .header("Access-Control-Allow-Origin", "*")
                .header("Set-Cookie", clear_cookie(ACCESS_TOKEN_COOKIE))
                .header("Set-Cookie", clear_cookie_for_domain(ACCESS_TOKEN_COOKIE, LEGACY_COOKIE_DOMAIN))
                .header("Set-Cookie", clear_cookie(REFRESH_TOKEN_COOKIE))
                .header("Set-Cookie", clear_cookie_for_domain(REFRESH_TOKEN_COOKIE, LEGACY_COOKIE_DOMAIN))
                .header("Set-Cookie", clear_cookie(USERNAME_COOKIE))
                .header("Set-Cookie", clear_cookie_for_domain(USERNAME_COOKIE, LEGACY_COOKIE_DOMAIN))
                .body(serde_json::to_string(&error)?.into())
                .map_err(Box::new)?);
        }
    };

    // Username is required for SECRET_HASH when the app client has a secret.
    // Store it in an httpOnly cookie on login so we can refresh even if access_token cookie has expired.
    let username = cookie_header
        .and_then(|h| get_cookie_value(h, USERNAME_COOKIE))
        .or_else(|| {
            cookie_header
                .and_then(|h| get_cookie_value(h, ACCESS_TOKEN_COOKIE))
                .and_then(|t| extract_username_from_token(&t))
        });

    let username = match username {
        Some(u) if !u.is_empty() => u,
        _ => {
            let error = ErrorResponse {
                error: "RefreshFailed".to_string(),
                message: "Missing username for refresh".to_string(),
            };
            return Ok(Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .header("Content-Type", "application/json")
                .header("Access-Control-Allow-Origin", "*")
                .header("Set-Cookie", clear_cookie(ACCESS_TOKEN_COOKIE))
                .header("Set-Cookie", clear_cookie_for_domain(ACCESS_TOKEN_COOKIE, LEGACY_COOKIE_DOMAIN))
                .header("Set-Cookie", clear_cookie(REFRESH_TOKEN_COOKIE))
                .header("Set-Cookie", clear_cookie_for_domain(REFRESH_TOKEN_COOKIE, LEGACY_COOKIE_DOMAIN))
                .header("Set-Cookie", clear_cookie(USERNAME_COOKIE))
                .header("Set-Cookie", clear_cookie_for_domain(USERNAME_COOKIE, LEGACY_COOKIE_DOMAIN))
                .body(serde_json::to_string(&error)?.into())
                .map_err(Box::new)?);
        }
    };

    tracing::info!(
        "Refreshing token using REFRESH_TOKEN_AUTH flow for user: {}",
        username
    );

    // Compute SECRET_HASH - required for apps with client secret
    let secret_hash = compute_secret_hash(&username, client_id, client_secret);

    let auth_result = cognito_client
        .initiate_auth()
        .auth_flow(RefreshTokenAuth)
        .client_id(client_id)
        .auth_parameters("REFRESH_TOKEN", &refresh_token)
        .auth_parameters("SECRET_HASH", &secret_hash)
        .send()
        .await;

    match auth_result {
        Ok(response) => {
            if let Some(auth_result) = response.authentication_result() {
                tracing::info!("Token refreshed successfully");

                let token_set = TokenSet {
                    _id_token: auth_result.id_token().unwrap_or_default().to_string(),
                    access_token: auth_result.access_token().unwrap_or_default().to_string(),
                    refresh_token: auth_result
                        .refresh_token()
                        .unwrap_or(&refresh_token)
                        .to_string(),
                    expires_in: auth_result.expires_in(),
                };

                // Create httpOnly cookies
                let access_cookie = create_cookie(
                    ACCESS_TOKEN_COOKIE,
                    &token_set.access_token,
                    token_set.expires_in as i64,
                    true,
                );
                let refresh_cookie = create_cookie(
                    REFRESH_TOKEN_COOKIE,
                    &token_set.refresh_token,
                    60 * 60 * 24 * 30, // 30 days for refresh token
                    true,
                );
                let username_cookie = create_cookie(
                    USERNAME_COOKIE,
                    &username,
                    60 * 60 * 24 * 30, // 30 days
                    true,
                );

                let session_response = SessionResponse {
                    message: "ok".to_string(),
                    expires_in: token_set.expires_in,
                };

                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", "application/json")
                    .header("Access-Control-Allow-Origin", "https://doxle.ai")
                    .header("Access-Control-Allow-Credentials", "true")
                    .header("Set-Cookie", access_cookie)
                    .header("Set-Cookie", refresh_cookie)
                    .header("Set-Cookie", username_cookie)
                    .body(serde_json::to_string(&session_response)?.into())
                    .map_err(Box::new)?)
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
                    .header("Set-Cookie", clear_cookie(ACCESS_TOKEN_COOKIE))
                    .header("Set-Cookie", clear_cookie_for_domain(ACCESS_TOKEN_COOKIE, LEGACY_COOKIE_DOMAIN))
                    .header("Set-Cookie", clear_cookie(REFRESH_TOKEN_COOKIE))
                    .header("Set-Cookie", clear_cookie_for_domain(REFRESH_TOKEN_COOKIE, LEGACY_COOKIE_DOMAIN))
                    .header("Set-Cookie", clear_cookie(USERNAME_COOKIE))
                    .header("Set-Cookie", clear_cookie_for_domain(USERNAME_COOKIE, LEGACY_COOKIE_DOMAIN))
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
                .header("Set-Cookie", clear_cookie(ACCESS_TOKEN_COOKIE))
                .header("Set-Cookie", clear_cookie_for_domain(ACCESS_TOKEN_COOKIE, LEGACY_COOKIE_DOMAIN))
                .header("Set-Cookie", clear_cookie(REFRESH_TOKEN_COOKIE))
                .header("Set-Cookie", clear_cookie_for_domain(REFRESH_TOKEN_COOKIE, LEGACY_COOKIE_DOMAIN))
                .header("Set-Cookie", clear_cookie(USERNAME_COOKIE))
                .header("Set-Cookie", clear_cookie_for_domain(USERNAME_COOKIE, LEGACY_COOKIE_DOMAIN))
                .body(serde_json::to_string(&error)?.into())
                .map_err(Box::new)?)
        }
    }
}

