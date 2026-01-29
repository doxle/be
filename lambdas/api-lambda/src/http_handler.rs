use aws_sdk_dynamodb::{types::AttributeValue, Client as DynamoClient};
use aws_sdk_s3::Client as S3Client;
use doxle_atoms as atoms;
use doxle_shared::{
    auth, cloudfront, contact, image_proxy, invites,
    s3_multipart, users, AppState,
};
use annotations_block::{self, blocks, labels};
use lambda_http::{
    http::{Method, StatusCode},
    Body, Error, Request, RequestExt, Response,
};
use serde::Deserialize;
use std::env;


/// Validate JWT from cookie and return user_id
fn validate_jwt_from_cookie(event: &Request) -> Option<String> {
    // Get cookie header
    let cookie_header = event.headers().get("Cookie")?.to_str().ok()?;
    
    // Extract access_token from cookies
    let token = doxle_shared::auth::get_cookie_value(cookie_header, "access_token")?;
    
    // Decode JWT payload (base64) to get user_id
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    
    use base64::{engine::general_purpose, Engine as _};
    let decoded = general_purpose::URL_SAFE_NO_PAD.decode(parts[1]).ok()?;
    let json_str = String::from_utf8(decoded).ok()?;
    let json: serde_json::Value = serde_json::from_str(&json_str).ok()?;
    
    // Check expiration
    let exp = json.get("exp")?.as_i64()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    
    if now > exp {
        tracing::warn!("JWT expired");
        return None;
    }
    
    // Return user_id (sub claim)
    json.get("sub")?.as_str().map(|s| s.to_string())
}


use std::sync::Arc;

#[derive(Deserialize)]
struct AbortUploadRequest {
    block_id: String,
    image_id: String,
    upload_id: String,
    extension: String,
}

/// Main Lambda handler - routes requests to auth or user endpoints
pub(crate) async fn function_handler(
    event: Request,
    state: Arc<AppState>,
) -> Result<Response<Body>, Error> {
    let method = event.method();
    let path = event.uri().path();
    let body = event.body();
    tracing::info!(
        "🚀 API Lambda v2.1.0 invoked - Method: {} Path: {}",
        method,
        path
    );

    // Handle CORS preflight
    if method == "OPTIONS" {
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Access-Control-Allow-Origin", "https://doxle.ai")
            .header("Access-Control-Allow-Credentials", "true")
            .header(
                "Access-Control-Allow-Methods",
                "GET,POST,PUT,PATCH,DELETE,OPTIONS",
            )
            .header(
                "Access-Control-Allow-Headers",
                "Content-Type,Authorization,X-User-Id,Cookie",
            )
            .body(Body::Empty)
            .map_err(Box::new)?);
    }

    // Route to auth endpoints (no JWT validation)
    if path.starts_with("/login") {
        let client_id = env::var("COGNITO_CLIENT_ID").expect("COGNITO_CLIENT_ID must be set");
        let client_secret =
            env::var("COGNITO_CLIENT_SECRET").expect("COGNITO_CLIENT_SECRET must be set");

        return match method {
            &Method::POST => {
                auth::login(&state.cognito_client, &client_id, &client_secret, body).await
            }
            _ => {
                let resp = Response::builder()
                    .status(StatusCode::METHOD_NOT_ALLOWED)
                    .header("Content-Type", "application/json")
                    .header("Access-Control-Allow-Origin", "*")
                    .body(
                        serde_json::json!({"error": "Method not allowed"})
                            .to_string()
                            .into(),
                    )
                    .map_err(Box::new)?;
                Ok(resp)
            }
        };
    }

    if path.starts_with("/signup") {
        let client_id = env::var("COGNITO_CLIENT_ID").expect("COGNITO_CLIENT_ID must be set");
        let client_secret =
            env::var("COGNITO_CLIENT_SECRET").expect("COGNITO_CLIENT_SECRET must be set");
        let table_name = env::var("TABLE_NAME").unwrap_or_else(|_| "doxle".to_string());

        return match method {
            &Method::POST => {
                auth::signup(
                    &state.cognito_client,
                    &state.dynamo_client,
                    &table_name,
                    &client_id,
                    &client_secret,
                    body,
                )
                .await
            }
            _ => {
                let resp = Response::builder()
                    .status(StatusCode::METHOD_NOT_ALLOWED)
                    .header("Content-Type", "application/json")
                    .header("Access-Control-Allow-Origin", "*")
                    .body(
                        serde_json::json!({"error": "Method not allowed"})
                            .to_string()
                            .into(),
                    )
                    .map_err(Box::new)?;
                Ok(resp)
            }
        };
    }

    if path.starts_with("/refresh") {
        let client_id = env::var("COGNITO_CLIENT_ID").expect("COGNITO_CLIENT_ID must be set");
        let client_secret =
            env::var("COGNITO_CLIENT_SECRET").expect("COGNITO_CLIENT_SECRET must be set");

        return match method {
            &Method::POST => {
                auth::refresh_token(&state.cognito_client, &client_id, &client_secret, body).await
            }
            _ => {
                let resp = Response::builder()
                    .status(StatusCode::METHOD_NOT_ALLOWED)
                    .header("Content-Type", "application/json")
                    .header("Access-Control-Allow-Origin", "*")
                    .body(
                        serde_json::json!({"error": "Method not allowed"})
                            .to_string()
                            .into(),
                    )
                    .map_err(Box::new)?;
                Ok(resp)
            }
        };
    }

    // CloudFront signed cookies endpoint (requires JWT auth)
    if path == "/auth/cloudfront-cookies" {
        if method != &Method::POST {
            return Ok(Response::builder()
                .status(StatusCode::METHOD_NOT_ALLOWED)
                .header("Content-Type", "application/json")
                .header("Access-Control-Allow-Origin", "*")
                .body(
                    serde_json::json!({"error": "Method not allowed"})
                        .to_string()
                        .into(),
                )
                .map_err(Box::new)?);
        }

        // Validate Authorization header is present
        let auth_header = event.headers().get("Authorization");
        if auth_header.is_none() {
            return Ok(Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .header("Content-Type", "application/json")
                .header("Access-Control-Allow-Origin", "*")
                .body(
                    serde_json::json!({"error": "Missing Authorization header"})
                        .to_string()
                        .into(),
                )
                .map_err(Box::new)?);
        }

        // Extract user ID from JWT (API Gateway should have validated the token)
        let user_id = event
            .headers()
            .get("X-User-Id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .or_else(|| {
                event
                    .request_context()
                    .authorizer()
                    .and_then(|auth| auth.jwt.as_ref())
                    .and_then(|jwt| jwt.claims.get("sub"))
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| "authenticated-user".to_string()); // Fallback - cookies still work

        // Issue CloudFront signed cookies (valid for 12 hours)
        let origin_header = event.headers().get("Origin").and_then(|v| v.to_str().ok());
        return cloudfront::issue_signed_cookies_response(&user_id, 43200, origin_header);
    }

    // Image proxy route (public - serves images from S3)
    if path.starts_with("/proxy-image/") {
        // URL format: /proxy-image/projects/{pid}/blocks/{bid}/{image}.ext
        let image_path = path.strip_prefix("/proxy-image/").unwrap_or("");
        let bucket_name = env::var("S3_BUCKET_NAME").unwrap_or_else(|_| "doxle-app".to_string());
        return image_proxy::proxy_image(&state.s3_client, &bucket_name, image_path).await;
    }

    // Contact form route (public - no auth required)
    if path == "/contact" {
        return match method {
            &Method::POST => {
                contact::handle_contact(&state.ses_client, body).await
            }
            _ => {
                let resp = Response::builder()
                    .status(StatusCode::METHOD_NOT_ALLOWED)
                    .header("Content-Type", "application/json")
                    .header("Access-Control-Allow-Origin", "*")
                    .body(
                        serde_json::json!({"error": "Method not allowed"})
                            .to_string()
                            .into(),
                    )
                    .map_err(Box::new)?;
                Ok(resp)
            }
        };
    }

    // Invites routes (public GET, authenticated POST)
    if path.starts_with("/invites") {
        let table_name = env::var("TABLE_NAME").unwrap_or_else(|_| "doxle".to_string());
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

        return match (method, parts.as_slice()) {
            // GET /invites/{code} - public endpoint to view invite details
            (&Method::GET, ["invites", invite_code]) => {
                invites::get_invite(&state.dynamo_client, &table_name, invite_code).await
            }
            // POST /invites - create invite (requires auth)
            (&Method::POST, ["invites"]) => {
                // Get user ID from JWT for admin check
                let user_id = event
                    .headers()
                    .get("X-User-Id")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string())
                    .or_else(|| {
                        event
                            .request_context()
                            .authorizer()
                            .and_then(|auth| auth.jwt.as_ref())
                            .and_then(|jwt| jwt.claims.get("sub"))
                            .map(|s| s.to_string())
                    })
                    .unwrap_or_else(|| "anonymous".to_string());

                invites::create_invite(
                    &state.dynamo_client,
                    &state.ses_client,
                    &table_name,
                    &user_id,
                    body,
                )
                .await
            }
            _ => not_found(),
        };
    }

    // Route to user endpoints (JWT validated by API Gateway)
    if path.starts_with("/users") {
        let table_name = env::var("TABLE_NAME").unwrap_or_else(|_| "doxle".to_string());

        // Get user ID from JWT claims (HTTP API passes JWT claims in request context)
        // For HTTP APIs with JWT authorizer, claims are in requestContext.authorizer.jwt.claims
        // In local development, allow override with X-User-Id header
        let user_id = event
            .headers()
            .get("X-User-Id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .or_else(|| {
                event
                    .request_context()
                    .authorizer()
                    .and_then(|auth| {
                        tracing::info!("Authorizer context: {:?}", auth);
                        auth.jwt.as_ref()
                    })
                    .and_then(|jwt| jwt.claims.get("sub"))
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| {
                tracing::warn!("Could not extract user ID from JWT or header, using fallback");
                "test-user-123".to_string()
            });

        tracing::info!("User ID from JWT: {}", user_id);

        return match (method, path) {
            (&Method::POST, "/users") => {
                users::create_user(&state.dynamo_client, &table_name, &user_id, body).await
            }
            (&Method::GET, "/users/me") => {
                users::get_user(&state.dynamo_client, &table_name, &user_id).await
            }
            (&Method::PATCH, "/users/me") => {
                users::update_user(&state.dynamo_client, &table_name, &user_id, body).await
            }
            _ => {
                let resp = Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .header("Content-Type", "application/json")
                    .header("Access-Control-Allow-Origin", "*")
                    .body(serde_json::json!({"error": "Not found"}).to_string().into())
                    .map_err(Box::new)?;
                Ok(resp)
            }
        };
    }

    // All other routes require auth
    let table_name = env::var("TABLE_NAME").unwrap_or_else(|_| "doxle".to_string());

    // Allow X-User-Id header override for local development
    let user_id = event
        .headers()
        .get("X-User-Id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| validate_jwt_from_cookie(&event))
        .unwrap_or_else(|| {
            tracing::warn!("No valid auth found, using fallback user");
            "test-user-123".to_string()
        });

    

    // Blocks routes (project-free)
    if path.starts_with("/blocks") {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

        return match (method, parts.as_slice()) {
            // --- BLOCKS ---
            // GET /blocks - list all blocks
            (&Method::GET, ["blocks"]) => {
                match blocks::list_blocks(&state.dynamo_client, &table_name).await {
                    Ok(resp) => Ok(resp),
                    Err(e)=> {
                        tracing::error!("Failed to list blocks: {}", e);
                        Ok(Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .header("Access-Control-Allow-Origin", "*")
                            .body(
                                serde_json::json!({
                                    "error": e.to_string()
                                }).to_string().into()
                            )
                            .map_err(Box::new)?)
                    }
                }
            }
            // POST /blocks - create block
            (&Method::POST, ["blocks"]) => {
                blocks::create_block(&state.dynamo_client, &table_name, body).await
            }
            // GET /blocks/{id} - get specific block
            (&Method::GET, ["blocks", block_id]) => {
                blocks::get_block(&state.dynamo_client, &table_name, block_id).await
            }
            // PATCH /blocks/{id} - update block
            (&Method::PATCH, ["blocks", block_id]) => {
                blocks::update_block(&state.dynamo_client, &table_name, block_id, body).await
            }
            // DELETE /blocks/{id} - delete block
            (&Method::DELETE, ["blocks", block_id]) => {
                blocks::delete_block(
                    &state.dynamo_client,
                    &state.s3_client,
                    &table_name,
                    &block_id,
                )
                .await
            }

            // --- LABELS ---
            // GET /blocks/{bid}/labels - list block labels
            (&Method::GET, ["blocks", block_id, "labels"]) => {
                labels::list_block_labels(&state.dynamo_client, &table_name, block_id).await
            }
            // POST /blocks/{bid}/labels - create label
            (&Method::POST, ["blocks", block_id, "labels"]) => {
                labels::create_label(&state.dynamo_client, &table_name, block_id, body).await
            }
            // GET /blocks/{bid}/labels/{lid} - get label
            (&Method::GET, ["blocks", block_id, "labels", label_id]) => {
                labels::get_label(&state.dynamo_client, &table_name, block_id, label_id).await
            }
            // PATCH /blocks/{bid}/labels/{lid} - update label
            (&Method::PATCH, ["blocks", block_id, "labels", label_id]) => {
                labels::update_label(
                    &state.dynamo_client,
                    &table_name,
                    &block_id,
                    &label_id,
                    body,
                )
                .await
            }
            // DELETE /blocks/{bid}/labels/{lid} - delete label
            (&Method::DELETE, ["blocks", block_id, "labels", label_id]) => {
                labels::delete_label(&state.dynamo_client, &table_name, block_id, label_id).await
            }

            // --- TASKS ---
            // GET /blocks/{bid}/tasks - list tasks (WITH IMAGES - JOIN LOGIC)
            (&Method::GET, ["blocks", block_id, "tasks"]) => {
                annotations_block::tasks::list_block_tasks(&state.dynamo_client, &table_name, block_id).await
            }
            // POST /blocks/{bid}/tasks - create task
            (&Method::POST, ["blocks", block_id, "tasks"]) => {
                annotations_block::tasks::create_task(&state.dynamo_client, &table_name, block_id, body).await
            }
            // GET /blocks/{bid}/tasks/{tid} - get task
            (&Method::GET, ["blocks", block_id, "tasks", task_id]) => {
                annotations_block::tasks::get_task(&state.dynamo_client, &table_name, block_id, task_id).await
            }
            // PATCH /blocks/{bid}/tasks/{tid} - update task
            (&Method::PATCH, ["blocks", block_id, "tasks", task_id]) => {
                annotations_block::tasks::update_task(&state.dynamo_client, &table_name, block_id, task_id, body).await
            }
            // DELETE /blocks/{bid}/tasks/{tid} - delete task
            (&Method::DELETE, ["blocks", block_id, "tasks", task_id]) => {
                annotations_block::tasks::delete_task(&state.dynamo_client, &table_name, block_id, task_id).await
            }
            // --- TASK IMAGES ---
            // POST /blocks/{bid}/tasks/{tid}/images - create image for task
            (&Method::POST, ["blocks", block_id, "tasks", task_id, "images"]) => {
                annotations_block::images::create_image_for_task_handler(
                    &state.dynamo_client,
                    &table_name,
                    &block_id,
                    &task_id,
                    body
                    ).await
            }
            // GET /blocks/{bid}/tasks/{tid}/images - list images for task
            (&Method::GET, ["blocks", block_id, "tasks", task_id, "images"]) =>{
                annotations_block::images::list_images_for_task_handler(
                    &state.dynamo_client,
                    &table_name,
                    &block_id,
                    &task_id
                    ).await
            }


            _ => not_found(),
        };
    }

    // Upload routes (S3) images
    if path.starts_with("/annotate/upload") {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        tracing::info!("📎 Upload route matched - Parts: {:?}", parts);

        return match (method, parts.as_slice()) {
            // POST /annotate/upload/initiate - initiate upload (single or multipart)
            (&Method::POST, ["annotate", "upload", "initiate"]) => {
                let request: s3_multipart::InitiateUploadRequest = serde_json::from_slice(body)?;
                s3_multipart::initiate_upload(&state.s3_client, request).await
            }
            // POST /annotate/upload/complete - complete multipart upload
            (&Method::POST, ["annotate", "upload", "complete"]) => {
                let request: s3_multipart::CompleteMultipartRequest = serde_json::from_slice(body)?;
                s3_multipart::complete_multipart_upload(&state.s3_client, request).await
            }
            // DELETE /annotate/upload/abort - abort multipart upload
            (&Method::DELETE, ["annotate", "upload", "abort"]) => {
                let request: AbortUploadRequest = serde_json::from_slice(body)?;
                s3_multipart::abort_multipart_upload(
                    &state.s3_client,
                    request.block_id,
                    request.image_id,
                    request.upload_id,
                    request.extension,
                )
                .await
            }
            _ => not_found(),
        };
    }

    // Images routes
    if path.starts_with("/images") {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

        return match (method, parts.as_slice()) {
            // GET /images/{id} - get image
            (&Method::GET, ["images", image_id]) => {
                let block_id = event
                    .query_string_parameters_ref()
                    .and_then(|params| params.first("block_id"))
                    .ok_or("Missing block id query parameter")?;
                atoms::media::get_image_handler(&state.dynamo_client, &table_name, block_id, image_id).await
            }
            // PATCH /images/{id} - update image
            (&Method::PATCH, ["images", image_id]) => {
                let block_id = event
                    .query_string_parameters_ref()
                    .and_then(|params| params.first("block_id"))
                    .ok_or("Missing block id query parameter")?;
                atoms::media::update_image_handler(&state.dynamo_client, &table_name, block_id, image_id, body)
                    .await
            }
            // DELETE /images/{id} - delete image
            (&Method::DELETE, ["images", image_id]) => {
                let block_id = event
                    .query_string_parameters_ref()
                    .and_then(|params| params.first("block_id"))
                    .ok_or("Missing block id query parameter")?;
                atoms::media::delete_image_handler(&state.dynamo_client, &table_name, block_id, image_id).await
            }
            // GET /images/{id}/annotations - list image annotations
            (&Method::GET, ["images", image_id, "annotations"]) => {
                atoms::drawing::list_image_annotations(&state.dynamo_client, &table_name, image_id)
                    .await
            }
            // POST /images/{id}/annotations - create annotation
            (&Method::POST, ["images", image_id, "annotations"]) => {
                let block_id = event
                    .query_string_parameters_ref()
                    .and_then(|params| params.first("block_id"))
                    .ok_or("Missing block id query parameter")?;


                atoms::drawing::create_annotation(
                    &state.dynamo_client,
                    &table_name,
                    &block_id,
                    &image_id,
                    &user_id,
                    body,
                )
                .await
            }
            // GET /images/{iid}/annotations/{aid} - get annotation
            (&Method::GET, ["images", image_id, "annotations", annotation_id]) => {
                atoms::drawing::get_annotation(
                    &state.dynamo_client,
                    &table_name,
                    &image_id,
                    &annotation_id,
                )
                .await
            }
            // PATCH /images/{iid}/annotations/{aid} - update annotation
            (&Method::PATCH, ["images", image_id, "annotations", annotation_id]) => {
                atoms::drawing::update_annotation(
                    &state.dynamo_client,
                    &table_name,
                    &image_id,
                    &annotation_id,
                    body,
                )
                .await
            }
            // DELETE /images/{iid}/annotations/{aid} - delete annotation
            (&Method::DELETE, ["images", image_id, "annotations", annotation_id]) => {

                let block_id = event
                .query_string_parameters_ref()
                .and_then(|params| params.first("block_id"))
                .ok_or("Missing block id query parameter")?;

                atoms::drawing::delete_annotation(
                    &state.dynamo_client,
                    &table_name,
                    &block_id,
                    &image_id,
                    &annotation_id,
                )
                .await
            }
            _ => not_found(),
        };
    }

    // No matching route
    tracing::warn!("⚠️ No route matched - Method: {} Path: {}", method, path);
    not_found()
}

// Helper: parse bucket and key from an S3 URL like https://bucket.s3.amazonaws.com/key or https://s3.<region>.amazonaws.com/bucket/key
fn _parse_bucket_and_key(url: &str) -> Option<(String, String)> {
    let no_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let (host, path) = no_scheme.split_once('/')?;

    // Handle both formats:
    // 1. bucket.s3.amazonaws.com/key
    // 2. s3.region.amazonaws.com/bucket/key
    let (bucket, key) = if host.starts_with("s3.") {
        // Format: s3.region.amazonaws.com/bucket/key
        let parts: Vec<&str> = path.splitn(2, '/').collect();
        if parts.len() == 2 {
            (parts[0].to_string(), parts[1].to_string())
        } else {
            return None;
        }
    } else {
        // Format: bucket.s3.amazonaws.com/key
        (host.split(".s3").next()?.to_string(), path.to_string())
    };

    Some((bucket, key))
}

async fn _list_block_images_signed(
    dynamo: &DynamoClient,
    _s3: &S3Client,
    table_name: &str,
    block_id: &str,
) -> Result<Response<Body>, Error> {
    let pk = format!("BLOCK#{}", block_id);

    let result = dynamo
        .query()
        .table_name(table_name)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(pk))
        .expression_attribute_values(":sk_prefix", AttributeValue::S("IMAGE#".to_string()))
        .send()
        .await?;

    let mut images_json = Vec::new();

    for item in result.items() {
        if let Some(sk) = item.get("SK").and_then(|v| v.as_s().ok()) {
            if let Some(image_id) = sk.strip_prefix("IMAGE#") {
                let url_str = item
                    .get("url")
                    .and_then(|v| v.as_s().ok())
                    .map(|s| s.to_string())
                    .unwrap_or_default();

                // Generate Lambda proxy URL
                let final_url = if let Some((_bucket, key)) = _parse_bucket_and_key(&url_str) {
                    // Return URL that goes through Lambda proxy
                    format!("https://api.doxle.ai/proxy-image/{}", key)
                } else {
                    url_str.clone()
                };

                let locked = item
                    .get("locked")
                    .and_then(|v| v.as_bool().ok())
                    .copied()
                    .unwrap_or(false);
                let order = item
                    .get("order")
                    .and_then(|v| v.as_n().ok())
                    .and_then(|n| n.parse::<i32>().ok());
                let uploaded_at = item
                    .get("uploaded_at")
                    .and_then(|v| v.as_s().ok())
                    .map(|s| s.to_string())
                    .unwrap_or_default();

                images_json.push(serde_json::json!({
                    "image_id": image_id,
                    "block_id": block_id,
                    "url": final_url,
                    "locked": locked,
                    "order": order,
                    "uploaded_at": uploaded_at,
                }));
            }
        }
    }

    // Sort by order like shared implementation
    images_json.sort_by(|a, b| {
        let ao = a.get("order").and_then(|v| v.as_i64());
        let bo = b.get("order").and_then(|v| v.as_i64());
        match (ao, bo) {
            (Some(a), Some(b)) => a.cmp(&b),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    });

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(serde_json::to_string(&images_json)?.into())
        .map_err(Box::new)?)
}

fn not_found() -> Result<Response<Body>, Error> {
    Ok(Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(serde_json::json!({"error": "Not found"}).to_string().into())
        .map_err(Box::new)?)
}
