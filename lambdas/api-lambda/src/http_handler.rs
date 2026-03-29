use aws_sdk_dynamodb::{types::AttributeValue, Client as DynamoClient};
use aws_sdk_s3::Client as S3Client;
use doxle_atoms as atoms;
use doxle_atoms::users::model::UserRole;

use doxle_shared::{
    auth, cloudfront, contact, image_proxy, invites,
    s3_multipart, users, AppState,
};
use annotations_block::{self, backup_jobs, blocks, import_jobs, labels, reconcile_jobs};
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
    upload_namespace: Option<String>,
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

        let cf_table_name = env::var("TABLE_NAME").unwrap_or_else(|_| "doxle".to_string());
        let auth_ctx = match auth::authenticate_cookie_request(
            &state.cognito_client,
            &state.dynamo_client,
            &cf_table_name,
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

        // GET /invites/{code} is public (no auth)
        if method == &Method::GET && parts.len() == 2 {
            return finalize_response(
                invites::get_invite(&state.dynamo_client, &table_name, parts[1]).await,
                request_origin,
                &[],
            );
        }

        // All other invite routes require auth
        let client_id = env::var("COGNITO_CLIENT_ID").expect("COGNITO_CLIENT_ID must be set");
        let client_secret = env::var("COGNITO_CLIENT_SECRET")
            .expect("COGNITO_CLIENT_SECRET must be set");
        let cookie_header = event.headers().get("Cookie").and_then(|v| v.to_str().ok());

        let auth_ctx = match auth::authenticate_cookie_request(
            &state.cognito_client,
            &state.dynamo_client,
            &table_name,
            &client_id,
            &client_secret,
            cookie_header,
        )
        .await
        {
            Ok(ctx) => ctx,
            Err(resp) => return Ok(with_cors_headers(resp, request_origin)),
        };

        // All invite management is admin-only
        if auth_ctx.user_role != UserRole::Admin {
            return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies);
        }

        return match (method, parts.as_slice()) {
            // GET /invites - list all invites (admin-only)
            (&Method::GET, ["invites"]) => finalize_response(
                invites::list_invites(&state.dynamo_client, &table_name).await,
                request_origin,
                &auth_ctx.set_cookies,
            ),
            // POST /invites - create invite (admin-only)
            (&Method::POST, ["invites"]) => finalize_response(
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
            ),
            // DELETE /invites/{code} - delete invite (admin-only)
            (&Method::DELETE, ["invites", invite_code]) => finalize_response(
                invites::delete_invite(&state.dynamo_client, &state.cognito_client, &table_name, invite_code).await,
                request_origin,
                &auth_ctx.set_cookies,
            ),
            _ => finalize_response(not_found(), request_origin, &auth_ctx.set_cookies),
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
            &state.dynamo_client,
            &table_name,
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
            (&Method::GET, "/users") => {
                users::list_users(&state.dynamo_client, &table_name).await
            }
            (&Method::POST, "/users") => {
                users::create_user(&state.dynamo_client, &table_name, &auth_ctx.user_id, body).await
            }
            (&Method::GET, "/users/me") => {
                let resp = users::get_user(&state.dynamo_client, &table_name, &auth_ctx.user_id).await?;
                if resp.status() == StatusCode::NOT_FOUND {
                    // Auto-create profile from auth context
                    let email = cookie_header
                        .and_then(|c| auth::get_cookie_value(c, auth::USERNAME_COOKIE))
                        .unwrap_or_default();
                    let name = atoms::users::service::to_title_case(email.split('@').next().unwrap_or("User"));
                    let create_body = serde_json::json!({
                        "user_name": name,
                        "user_email": email,
                        "user_role": "annotator"
                    });
                    let body_bytes = serde_json::to_vec(&create_body)?;
                    users::create_user(&state.dynamo_client, &table_name, &auth_ctx.user_id, &body_bytes).await
                } else {
                    Ok(resp)
                }
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
        &state.dynamo_client,
        &table_name,
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
    let user_role = &auth_ctx.user_role;

    // Projects routes
    if path.starts_with("/projects") {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

        let resp = match (method, parts.as_slice()) {
            // --- PROJECTS ---
            // GET /projects - list all projects
            (&Method::GET, ["projects"]) => {
                atoms::projects::http_list_projects(&state.dynamo_client, &table_name).await
            }
            // POST /projects - create project (admin only)
            (&Method::POST, ["projects"]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                atoms::projects::http_create_project(&state.dynamo_client, &table_name, &user_id, body).await
            }
            // GET /projects/{pid} - get specific project
            (&Method::GET, ["projects", project_id]) => {
                atoms::projects::http_get_project(&state.dynamo_client, &table_name, project_id).await
            }
            // PATCH /projects/{pid} - update project (admin only)
            (&Method::PATCH, ["projects", project_id]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                atoms::projects::http_update_project(&state.dynamo_client, &table_name, project_id, body).await
            }
            // DELETE /projects/{pid} - delete project (admin only)
            (&Method::DELETE, ["projects", project_id]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                atoms::projects::http_delete_project(&state.dynamo_client, &table_name, project_id).await
            }

            // --- BLOCKS (under projects) ---
            // GET /projects/{pid}/blocks - list blocks for project
            (&Method::GET, ["projects", project_id, "blocks"]) => {
                blocks::list_blocks(&state.dynamo_client, &table_name, project_id).await
            }
            // POST /projects/{pid}/blocks - create block (admin only)
            (&Method::POST, ["projects", project_id, "blocks"]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                blocks::create_block(&state.dynamo_client, &table_name, project_id, body).await
            }
            // GET /projects/{pid}/blocks/{bid} - get specific block
            (&Method::GET, ["projects", project_id, "blocks", block_id]) => {
                blocks::get_block(&state.dynamo_client, &table_name, project_id, block_id).await
            }
            // PATCH /projects/{pid}/blocks/{bid} - update block (admin only)
            (&Method::PATCH, ["projects", project_id, "blocks", block_id]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                blocks::update_block(&state.dynamo_client, &table_name, project_id, block_id, body).await
            }
            // DELETE /projects/{pid}/blocks/{bid} - delete block (admin only)
            (&Method::DELETE, ["projects", project_id, "blocks", block_id]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                blocks::delete_block(
                    &state.dynamo_client,
                    &state.s3_client,
                    &table_name,
                    project_id,
                    block_id,
                )
                .await
            }

            // --- LABELS ---
            // GET /projects/{pid}/blocks/{bid}/labels
            (&Method::GET, ["projects", project_id, "blocks", block_id, "labels"]) => {
                labels::list_block_labels(&state.dynamo_client, &table_name, project_id, block_id).await
            }
            // POST /projects/{pid}/blocks/{bid}/labels
            (&Method::POST, ["projects", _project_id, "blocks", block_id, "labels"]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                labels::create_label(&state.dynamo_client, &table_name, block_id, body).await
            }
            // GET /projects/{pid}/blocks/{bid}/labels/{lid}
            (&Method::GET, ["projects", _project_id, "blocks", block_id, "labels", label_id]) => {
                labels::get_label(&state.dynamo_client, &table_name, block_id, label_id).await
            }
            // PATCH /projects/{pid}/blocks/{bid}/labels/{lid}
            (&Method::PATCH, ["projects", _project_id, "blocks", block_id, "labels", label_id]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                labels::update_label(&state.dynamo_client, &table_name, block_id, label_id, body).await
            }
            // DELETE /projects/{pid}/blocks/{bid}/labels/{lid}
            (&Method::DELETE, ["projects", _project_id, "blocks", block_id, "labels", label_id]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                labels::delete_label(&state.dynamo_client, &table_name, block_id, label_id).await
            }

            // --- BLOCK MEDIA ---
            (&Method::GET, ["projects", _project_id, "blocks", block_id, "media"]) => {
                atoms::media::list_block_media_handler(&state.dynamo_client, &table_name, block_id).await
            }
            (&Method::POST, ["projects", project_id, "blocks", block_id, "media"]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                atoms::media::create_block_media_handler(
                    &state.dynamo_client,
                    &table_name,
                    project_id,
                    block_id,
                    body,
                )
                .await
            }
            (&Method::PATCH, ["projects", _project_id, "blocks", block_id, "media", image_id, "markup"]) => {
                atoms::media::update_image_handler(&state.dynamo_client, &table_name, block_id, image_id, body).await
            }

            // --- IMPORT ---
            (&Method::POST, ["projects", _project_id, "blocks", block_id, "import", "initiate"]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                annotations_block::bulk_import::initiate_import(&state.s3_client, block_id, body).await
            }
            (&Method::POST, ["projects", _project_id, "blocks", _block_id, "import", "complete"]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                annotations_block::bulk_import::complete_import_upload(&state.s3_client, body).await
            }
            (&Method::POST, ["projects", _project_id, "blocks", _block_id, "import", "abort"]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                annotations_block::bulk_import::abort_import_upload(&state.s3_client, body).await
            }
            (&Method::POST, ["projects", project_id, "blocks", block_id, "import", "process"]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                annotations_block::bulk_import::process_import(&state.s3_client, &state.dynamo_client, &table_name, project_id, block_id, body).await
            }
            (&Method::POST, ["projects", _project_id, "blocks", block_id, "import", "parse"]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                annotations_block::bulk_import::parse_import(&state.s3_client, block_id, body).await
            }
            (&Method::POST, ["projects", project_id, "blocks", block_id, "import", "process-batch"]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                annotations_block::bulk_import::process_batch(&state.s3_client, &state.dynamo_client, &table_name, project_id, block_id, body).await
            }
            (&Method::POST, ["projects", _project_id, "blocks", _block_id, "import", "cleanup"]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                annotations_block::bulk_import::cleanup_import(&state.s3_client, body).await
            }
            // --- BACKUPS ---
            (&Method::POST, ["projects", project_id, "blocks", block_id, "backups"]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                backup_jobs::start_backup_job(
                    &state.dynamo_client,
                    &table_name,
                    project_id,
                    block_id,
                    &user_id,
                    body,
                )
                .await
            }
            (&Method::GET, ["projects", project_id, "blocks", block_id, "backups", backup_id]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                backup_jobs::get_backup_job_status(
                    &state.dynamo_client,
                    &state.s3_client,
                    &table_name,
                    project_id,
                    block_id,
                    backup_id,
                )
                .await
            }
            (&Method::POST, ["projects", project_id, "blocks", block_id, "imports"]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                import_jobs::start_import_job(
                    &state.dynamo_client,
                    &table_name,
                    project_id,
                    block_id,
                    &user_id,
                    body,
                )
                .await
            }
            (&Method::GET, ["projects", project_id, "blocks", block_id, "imports", import_job_id]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                import_jobs::get_import_job_status(
                    &state.dynamo_client,
                    &table_name,
                    project_id,
                    block_id,
                    import_job_id,
                )
                .await
            }
            (&Method::POST, ["projects", project_id, "blocks", block_id, "reconcile-counts"]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                reconcile_jobs::start_reconcile_job(
                    &state.dynamo_client,
                    &table_name,
                    project_id,
                    block_id,
                    &user_id,
                    body,
                )
                .await
            }
            (&Method::GET, ["projects", project_id, "blocks", block_id, "reconcile-counts", reconcile_job_id]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                reconcile_jobs::get_reconcile_job_status(
                    &state.dynamo_client,
                    &table_name,
                    project_id,
                    block_id,
                    reconcile_job_id,
                )
                .await
            }

            // --- EXPORT ---
            (&Method::GET, ["projects", project_id, "blocks", block_id, "export"]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                annotations_block::export::export_block(&state.dynamo_client, &state.s3_client, &table_name, project_id, block_id).await
            }

            // --- TASKS ---
            (&Method::GET, ["projects", project_id, "blocks", block_id, "tasks"]) => {
                annotations_block::tasks::list_block_tasks(
                    &state.dynamo_client, &table_name, block_id, &user_id, user_role,
                ).await
            }
            (&Method::POST, ["projects", _project_id, "blocks", block_id, "tasks"]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                annotations_block::tasks::create_task(&state.dynamo_client, &table_name, block_id, body).await
            }
            (&Method::GET, ["projects", _project_id, "blocks", block_id, "tasks", task_id]) => {
                annotations_block::tasks::get_task(&state.dynamo_client, &table_name, block_id, task_id).await
            }
            (&Method::PATCH, ["projects", project_id, "blocks", block_id, "tasks", task_id]) => {
                annotations_block::tasks::update_task_with_role(
                    &state.dynamo_client, &table_name, project_id, block_id, task_id, body, &user_id, user_role,
                ).await
            }
            (&Method::DELETE, ["projects", project_id, "blocks", block_id, "tasks", task_id]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                annotations_block::tasks::delete_task(
                    &state.dynamo_client,
                    &state.s3_client,
                    &table_name,
                    project_id,
                    block_id,
                    task_id,
                )
                .await
            }

            // --- TASK IMAGES ---
            (&Method::POST, ["projects", project_id, "blocks", block_id, "tasks", task_id, "images"]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                annotations_block::images::create_image_for_task_handler(
                    &state.dynamo_client, &table_name, project_id, block_id, task_id, body,
                ).await
            }
            (&Method::GET, ["projects", _project_id, "blocks", block_id, "tasks", task_id, "images"]) => {
                annotations_block::images::list_images_for_task_handler(
                    &state.dynamo_client, &table_name, block_id, task_id,
                ).await
            }

            _ => not_found(),
        };

        return finalize_response(resp, request_origin, &auth_ctx.set_cookies);
    }

    // Comments routes (generic - parent_id can be image_id, file_id, block_id, etc.)
    if path.starts_with("/comments") {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

        let resp = match (method, parts.as_slice()) {
            // GET /comments/{parent_id}/threads - list threads for parent
            (&Method::GET, ["comments", parent_id, "threads"]) => {
                atoms::comments::http::list_threads(&state.dynamo_client, &table_name, parent_id).await
            }
            // POST /comments/{parent_id}/threads - create thread
            (&Method::POST, ["comments", parent_id, "threads"]) => {
                atoms::comments::http::create_thread(
                    &state.dynamo_client, &table_name, parent_id, &user_id, body,
                ).await
            }
            // PATCH /comments/{parent_id}/threads/{tid} - update thread (resolve/unresolve)
            (&Method::PATCH, ["comments", parent_id, "threads", thread_id]) => {
                atoms::comments::http::update_thread(
                    &state.dynamo_client, &table_name, parent_id, thread_id, body,
                ).await
            }
            // DELETE /comments/{parent_id}/threads/{tid} - delete thread + comments
            (&Method::DELETE, ["comments", parent_id, "threads", thread_id]) => {
                atoms::comments::http::delete_thread(
                    &state.dynamo_client, &table_name, parent_id, thread_id,
                ).await
            }
            // POST /comments/{parent_id}/threads/{tid}/comments - add comment to thread
            (&Method::POST, ["comments", _parent_id, "threads", thread_id, "comments"]) => {
                atoms::comments::http::add_comment(
                    &state.dynamo_client, &table_name, thread_id, &user_id, body,
                ).await
            }
            _ => not_found(),
        };

        return finalize_response(resp, request_origin, &auth_ctx.set_cookies);
    }

    // Upload routes (S3) images (admin only)
    if path.starts_with("/media/upload") || path.starts_with("/annotate/upload") {
        if user_role != &UserRole::Admin {
            return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies);
        }
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        tracing::info!("📎 Upload route matched - Parts: {:?}", parts);
        let resp = match (method, parts.as_slice()) {
            // POST /media/upload/initiate OR /annotate/upload/initiate
            (&Method::POST, [prefix, "upload", "initiate"]) if *prefix == "media" || *prefix == "annotate" => {
                let request: s3_multipart::InitiateUploadRequest = serde_json::from_slice(body)?;
                s3_multipart::initiate_upload(&state.s3_client, request).await
            }
            // POST /media/upload/complete OR /annotate/upload/complete
            (&Method::POST, [prefix, "upload", "complete"]) if *prefix == "media" || *prefix == "annotate" => {
                let request: s3_multipart::CompleteMultipartRequest = serde_json::from_slice(body)?;
                s3_multipart::complete_multipart_upload(&state.s3_client, request).await
            }
            // DELETE /media/upload/abort OR /annotate/upload/abort
            (&Method::DELETE, [prefix, "upload", "abort"]) if *prefix == "media" || *prefix == "annotate" => {
                let request: AbortUploadRequest = serde_json::from_slice(body)?;
                s3_multipart::abort_multipart_upload(
                    &state.s3_client,
                    request.block_id,
                    request.image_id,
                    request.upload_id,
                    request.extension,
                    request.upload_namespace.unwrap_or_else(|| "annotation".to_string()),
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
            // PATCH /images/{id} - update image (admin only)
            (&Method::PATCH, ["images", image_id]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                let block_id = event
                    .query_string_parameters_ref()
                    .and_then(|params| params.first("block_id"))
                    .ok_or("Missing block id query parameter")?;
                atoms::media::update_image_handler(&state.dynamo_client, &table_name, block_id, image_id, body)
                    .await
            }
            // DELETE /images/{id} - delete image (admin only)
            (&Method::DELETE, ["images", image_id]) => {
                if user_role != &UserRole::Admin { return finalize_response(forbidden(), request_origin, &auth_ctx.set_cookies); }
                let block_id = event
                    .query_string_parameters_ref()
                    .and_then(|params| params.first("block_id"))
                    .ok_or("Missing block_id query parameter")?;
                let project_id = event
                    .query_string_parameters_ref()
                    .and_then(|params| params.first("project_id"))
                    .ok_or("Missing project_id query parameter")?;
                atoms::media::delete_image_handler(&state.dynamo_client, &table_name, project_id, block_id, image_id).await
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
                let project_id = event
                    .query_string_parameters_ref()
                    .and_then(|params| params.first("project_id"));

                atoms::drawing::create_annotation(
                    &state.dynamo_client,
                    &table_name,
                    project_id,
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
                let project_id = event
                    .query_string_parameters_ref()
                    .and_then(|params| params.first("project_id"));

                atoms::drawing::delete_annotation(
                    &state.dynamo_client,
                    &table_name,
                    project_id,
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

fn forbidden() -> Result<Response<Body>, Error> {
    Ok(Response::builder()
        .status(StatusCode::FORBIDDEN)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(serde_json::json!({"error": "Forbidden", "message": "You do not have permission to perform this action"}).to_string().into())
        .map_err(Box::new)?)
}
