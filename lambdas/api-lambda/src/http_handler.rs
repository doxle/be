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

use lambda_http::http::header::{HeaderValue, SET_COOKIE, VARY};

fn with_set_cookies(mut resp: Response<Body>, cookies: &[String]) -> Response<Body> {
    let headers = resp.headers_mut();
    for cookie in cookies {
        if let Ok(v) = HeaderValue::from_str(cookie) {
            headers.append(SET_COOKIE, v);
        }
    }
    resp
}

fn with_cors_headers(mut resp: Response<Body>, request_origin: Option<&str>) -> Response<Body> {
    let cors_origin = auth::get_cors_origin(request_origin);

    let headers = resp.headers_mut();
    headers.insert(
        "Access-Control-Allow-Origin",
        HeaderValue::from_str(&cors_origin)
            .unwrap_or_else(|_| HeaderValue::from_static("https://doxle.ai")),
    );
    headers.insert("Access-Control-Allow-Credentials", HeaderValue::from_static("true"));
    headers.insert(
        "Access-Control-Allow-Methods",
        HeaderValue::from_static("GET,POST,PUT,PATCH,DELETE,OPTIONS"),
    );
    headers.insert(
        "Access-Control-Allow-Headers",
        HeaderValue::from_static("Content-Type,Authorization,X-User-Id,Cookie"),
    );
    headers.append(VARY, HeaderValue::from_static("Origin"));

    resp
}

fn finalize_response(
    resp: Result<Response<Body>, Error>,
    request_origin: Option<&str>,
    cookies: &[String],
) -> Result<Response<Body>, Error> {
    resp.map(|r| with_cors_headers(with_set_cookies(r, cookies), request_origin))
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
    let request_origin = event.headers().get("Origin").and_then(|v| v.to_str().ok());
    tracing::info!(
        "🚀 API Lambda v2.1.0 invoked - Method: {} Path: {}",
        method,
        path
    );

    // Handle CORS preflight
    if method == "OPTIONS" {
        let resp = Response::builder()
            .status(StatusCode::OK)
            .body(Body::Empty)
            .map_err(Box::new)?;
        return Ok(with_cors_headers(resp, request_origin));
    }

    // Route to auth endpoints (no JWT validation)
    if path.starts_with("/login") {
        let client_id = env::var("COGNITO_CLIENT_ID").expect("COGNITO_CLIENT_ID must be set");
        let client_secret =
            env::var("COGNITO_CLIENT_SECRET").expect("COGNITO_CLIENT_SECRET must be set");

        return match method {
            &Method::POST => finalize_response(
                auth::login(&state.cognito_client, &client_id, &client_secret, body).await,
                request_origin,
                &[],
            ),
            _ => {
                let resp = Response::builder()
                    .status(StatusCode::METHOD_NOT_ALLOWED)
                    .header("Content-Type", "application/json")
                    .body(
                        serde_json::json!({"error": "Method not allowed"})
                            .to_string()
                            .into(),
                    )
                    .map_err(Box::new)?;
                finalize_response(Ok(resp), request_origin, &[])
            }
        };
    }

    if path.starts_with("/signup") {
        let client_id = env::var("COGNITO_CLIENT_ID").expect("COGNITO_CLIENT_ID must be set");
        let client_secret =
            env::var("COGNITO_CLIENT_SECRET").expect("COGNITO_CLIENT_SECRET must be set");
        let table_name = env::var("TABLE_NAME").unwrap_or_else(|_| "doxle".to_string());

        return match method {
            &Method::POST => finalize_response(
                auth::signup(
                    &state.cognito_client,
                    &state.dynamo_client,
                    &table_name,
                    &client_id,
                    &client_secret,
                    body,
                )
                .await,
                request_origin,
                &[],
            ),
            _ => {
                let resp = Response::builder()
                    .status(StatusCode::METHOD_NOT_ALLOWED)
                    .header("Content-Type", "application/json")
                    .body(
                        serde_json::json!({"error": "Method not allowed"})
                            .to_string()
                            .into(),
                    )
                    .map_err(Box::new)?;
                finalize_response(Ok(resp), request_origin, &[])
            }
        };
    }

    if path.starts_with("/refresh") {
        let client_id = env::var("COGNITO_CLIENT_ID").expect("COGNITO_CLIENT_ID must be set");
        let client_secret =
            env::var("COGNITO_CLIENT_SECRET").expect("COGNITO_CLIENT_SECRET must be set");

        let cookie_header = event.headers().get("Cookie").and_then(|v| v.to_str().ok());

        return match method {
            &Method::POST => finalize_response(
                auth::refresh_token(
                    &state.cognito_client,
                    &client_id,
                    &client_secret,
                    body,
                    cookie_header,
                )
                .await,
                request_origin,
                &[],
            ),
            _ => {
                let resp = Response::builder()
                    .status(StatusCode::METHOD_NOT_ALLOWED)
                    .header("Content-Type", "application/json")
                    .body(
                        serde_json::json!({"error": "Method not allowed"})
                            .to_string()
                            .into(),
                    )
                    .map_err(Box::new)?;
                finalize_response(Ok(resp), request_origin, &[])
            }
        };
    }

    if path.starts_with("/logout") {
        return match method {
            &Method::POST => {
                let resp = Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", "application/json")
                    .header("Set-Cookie", auth::clear_cookie(auth::ACCESS_TOKEN_COOKIE))
                    .header("Set-Cookie", auth::clear_cookie_for_domain(auth::ACCESS_TOKEN_COOKIE, auth::LEGACY_COOKIE_DOMAIN))
                    .header("Set-Cookie", auth::clear_cookie(auth::REFRESH_TOKEN_COOKIE))
                    .header("Set-Cookie", auth::clear_cookie_for_domain(auth::REFRESH_TOKEN_COOKIE, auth::LEGACY_COOKIE_DOMAIN))
                    .header("Set-Cookie", auth::clear_cookie(auth::USERNAME_COOKIE))
                    .header("Set-Cookie", auth::clear_cookie_for_domain(auth::USERNAME_COOKIE, auth::LEGACY_COOKIE_DOMAIN))
                    .body(serde_json::json!({"message": "ok"}).to_string().into())
                    .map_err(Box::new)?;
                finalize_response(Ok(resp), request_origin, &[])
            }
            _ => {
                let resp = Response::builder()
                    .status(StatusCode::METHOD_NOT_ALLOWED)
                    .header("Content-Type", "application/json")
                    .body(
                        serde_json::json!({"error": "Method not allowed"})
                            .to_string()
                            .into(),
                    )
                    .map_err(Box::new)?;
                finalize_response(Ok(resp), request_origin, &[])
            }
        };
    }

    // CloudFront signed cookies endpoint
    if path == "/auth/cloudfront-cookies" {
        if method != &Method::POST {
            let resp = Response::builder()
                .status(StatusCode::METHOD_NOT_ALLOWED)
                .header("Content-Type", "application/json")
                .body(
                    serde_json::json!({"error": "Method not allowed"})
                        .to_string()
                        .into(),
                )
                .map_err(Box::new)?;
            return finalize_response(Ok(resp), request_origin, &[]);
        }

        let client_id = env::var("COGNITO_CLIENT_ID").expect("COGNITO_CLIENT_ID must be set");
        let client_secret =
            env::var("COGNITO_CLIENT_SECRET").expect("COGNITO_CLIENT_SECRET must be set");
        let cookie_header = event.headers().get("Cookie").and_then(|v| v.to_str().ok());

        let auth_ctx = match auth::authenticate_cookie_request(
            &state.cognito_client,
            &client_id,
            &client_secret,
            cookie_header,
        )
        .await
        {
            Ok(ctx) => ctx,
            Err(resp) => return Ok(with_cors_headers(resp, request_origin)),
        };

        return finalize_response(
            cloudfront::issue_signed_cookies_response(&auth_ctx.user_id, 43200, request_origin),
            request_origin,
            &auth_ctx.set_cookies,
        );
    }

    // Image proxy route (public - serves images from S3)
    if path.starts_with("/proxy-image/") {
        // URL format: /proxy-image/projects/{pid}/blocks/{bid}/{image}.ext
        let image_path = path.strip_prefix("/proxy-image/").unwrap_or("");
        let bucket_name = env::var("S3_BUCKET_NAME").unwrap_or_else(|_| "doxle-app".to_string());
        return finalize_response(
            image_proxy::proxy_image(&state.s3_client, &bucket_name, image_path).await,
            request_origin,
            &[],
        );
    }

    // Contact form route (public - no auth required)
    if path == "/contact" {
        return match method {
            &Method::POST => finalize_response(
                contact::handle_contact(&state.ses_client, body).await,
                request_origin,
                &[],
            ),
            _ => {
                let resp = Response::builder()
                    .status(StatusCode::METHOD_NOT_ALLOWED)
                    .header("Content-Type", "application/json")
                    .body(
                        serde_json::json!({"error": "Method not allowed"})
                            .to_string()
                            .into(),
                    )
                    .map_err(Box::new)?;
                finalize_response(Ok(resp), request_origin, &[])
            }
        };
    }

    // Invites routes (public GET, authenticated POST)
    if path.starts_with("/invites") {
        let table_name = env::var("TABLE_NAME").unwrap_or_else(|_| "doxle".to_string());
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

        return match (method, parts.as_slice()) {
            // GET /invites/{code} - public endpoint to view invite details
            (&Method::GET, ["invites", invite_code]) => finalize_response(
                invites::get_invite(&state.dynamo_client, &table_name, invite_code).await,
                request_origin,
                &[],
            ),
            // POST /invites - create invite (requires auth)
            (&Method::POST, ["invites"]) => {
                let client_id = env::var("COGNITO_CLIENT_ID").expect("COGNITO_CLIENT_ID must be set");
                let client_secret = env::var("COGNITO_CLIENT_SECRET")
                    .expect("COGNITO_CLIENT_SECRET must be set");
                let cookie_header = event.headers().get("Cookie").and_then(|v| v.to_str().ok());

                let auth_ctx = match auth::authenticate_cookie_request(
                    &state.cognito_client,
                    &client_id,
                    &client_secret,
                    cookie_header,
                )
                .await
                {
                    Ok(ctx) => ctx,
                    Err(resp) => return Ok(with_cors_headers(resp, request_origin)),
                };

                finalize_response(
                    invites::create_invite(
                        &state.dynamo_client,
                        &state.ses_client,
                        &table_name,
                        &auth_ctx.user_id,
                        body,
                    )
                    .await,
                    request_origin,
                    &auth_ctx.set_cookies,
                )
            }
            _ => finalize_response(not_found(), request_origin, &[]),
        };
    }

    // Route to user endpoints (cookie auth)
    if path.starts_with("/users") {
        let table_name = env::var("TABLE_NAME").unwrap_or_else(|_| "doxle".to_string());
        let client_id = env::var("COGNITO_CLIENT_ID").expect("COGNITO_CLIENT_ID must be set");
        let client_secret =
            env::var("COGNITO_CLIENT_SECRET").expect("COGNITO_CLIENT_SECRET must be set");
        let cookie_header = event.headers().get("Cookie").and_then(|v| v.to_str().ok());

        let auth_ctx = match auth::authenticate_cookie_request(
            &state.cognito_client,
            &client_id,
            &client_secret,
            cookie_header,
        )
        .await
        {
            Ok(ctx) => ctx,
            Err(resp) => return Ok(with_cors_headers(resp, request_origin)),
        };

        let resp = match (method, path) {
            (&Method::POST, "/users") => {
                users::create_user(&state.dynamo_client, &table_name, &auth_ctx.user_id, body).await
            }
            (&Method::GET, "/users/me") => {
                users::get_user(&state.dynamo_client, &table_name, &auth_ctx.user_id).await
            }
            (&Method::PATCH, "/users/me") => {
                users::update_user(&state.dynamo_client, &table_name, &auth_ctx.user_id, body).await
            }
            _ => {
                let resp = Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .header("Content-Type", "application/json")
                    .body(serde_json::json!({"error": "Not found"}).to_string().into())
                    .map_err(Box::new)?;
                Ok(resp)
            }
        };

        return finalize_response(resp, request_origin, &auth_ctx.set_cookies);
    }

    // All other routes require auth (cookie auth + auto-refresh)
    let table_name = env::var("TABLE_NAME").unwrap_or_else(|_| "doxle".to_string());
    let client_id = env::var("COGNITO_CLIENT_ID").expect("COGNITO_CLIENT_ID must be set");
    let client_secret = env::var("COGNITO_CLIENT_SECRET").expect("COGNITO_CLIENT_SECRET must be set");
    let cookie_header = event.headers().get("Cookie").and_then(|v| v.to_str().ok());

    let auth_ctx = match auth::authenticate_cookie_request(
        &state.cognito_client,
        &client_id,
        &client_secret,
        cookie_header,
    )
    .await
    {
        Ok(ctx) => ctx,
        Err(resp) => return Ok(with_cors_headers(resp, request_origin)),
    };

    let user_id = auth_ctx.user_id.clone();

    // Blocks routes (project-free)
    if path.starts_with("/blocks") {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

        let resp = match (method, parts.as_slice()) {
            // --- BLOCKS ---
            // GET /blocks - list all blocks
            (&Method::GET, ["blocks"]) => match blocks::list_blocks(&state.dynamo_client, &table_name).await {
                Ok(resp) => Ok(resp),
                Err(e) => {
                    tracing::error!("Failed to list blocks: {}", e);
                    Ok(Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .header("Content-Type", "application/json")
                        .body(
                            serde_json::json!({
                                "error": e.to_string()
                            })
                            .to_string()
                            .into(),
                        )
                        .map_err(Box::new)?)
                }
            },
            // POST /blocks - create block
            (&Method::POST, ["blocks"]) => blocks::create_block(&state.dynamo_client, &table_name, body).await,
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
                    body,
                )
                .await
            }
            // GET /blocks/{bid}/tasks/{tid}/images - list images for task
            (&Method::GET, ["blocks", block_id, "tasks", task_id, "images"]) => {
                annotations_block::images::list_images_for_task_handler(
                    &state.dynamo_client,
                    &table_name,
                    &block_id,
                    &task_id,
                )
                .await
            }

            _ => not_found(),
        };

        return finalize_response(resp, request_origin, &auth_ctx.set_cookies);
    }

    // Upload routes (S3) images
    if path.starts_with("/annotate/upload") {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        tracing::info!("📎 Upload route matched - Parts: {:?}", parts);

        let resp = match (method, parts.as_slice()) {
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

        return finalize_response(resp, request_origin, &auth_ctx.set_cookies);
    }

    // Images routes
    if path.starts_with("/images") {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

        let resp = match (method, parts.as_slice()) {
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
                let block_id = event
                    .query_string_parameters_ref()
                    .and_then(|params| params.first("block_id"))
                    .ok_or("Missing block id query parameter")?;

                atoms::drawing::update_annotation(
                    &state.dynamo_client,
                    &table_name,
                    &block_id,
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

        return finalize_response(resp, request_origin, &auth_ctx.set_cookies);
    }

    // No matching route
    tracing::warn!("⚠️ No route matched - Method: {} Path: {}", method, path);
    finalize_response(not_found(), request_origin, &auth_ctx.set_cookies)
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
