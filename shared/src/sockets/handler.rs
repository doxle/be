use super::connections::{remove_connection, save_connection};
use super::messages::WebSocketMessage;
use crate::AppState;
use crate::projects;
use annotations_block::{blocks, labels};
use lambda_http::{http::StatusCode, Body, Error, Request, RequestExt, Response};
use std::{env, sync::Arc};

/// Handle WebSocket events ($connect, $disconnect, $default)
pub async fn handle_websocket_event(
    event: Request,
    state: Arc<AppState>,
) -> Result<Response<Body>, Error> {
    let table_name = env::var("TABLE_NAME").unwrap_or_else(|_| "doxle-annotations".to_string());

    // For WebSocket events, connection ID and route key come from headers/context
    // API Gateway puts these in specific headers for WebSocket
    let connection_id = event
        .headers()
        .get("connectionid")
        .or_else(|| event.headers().get("connectionId"))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    let route_key = event
        .headers()
        .get("routekey")
        .or_else(|| event.headers().get("routeKey"))
        .and_then(|v| v.to_str().ok())
        .unwrap_or(event.uri().path());

    tracing::info!(
        "WebSocket event: {} for connection: {}",
        route_key,
        connection_id
    );

    match route_key {
        "$connect" => handle_connect(event, state, &table_name, &connection_id).await,
        "$disconnect" => handle_disconnect(state, &table_name, &connection_id).await,
        "$default" => handle_message(event, state, &table_name, &connection_id).await,
        _ => {
            tracing::warn!("Unknown WebSocket route: {}", route_key);
            Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::Empty)
                .map_err(Box::new)?)
        }
    }
}

/// Handle $connect event
async fn handle_connect(
    event: Request,
    state: Arc<AppState>,
    table_name: &str,
    connection_id: &str,
) -> Result<Response<Body>, Error> {
    // Extract user ID from query parameters or JWT
    let user_id = event
        .query_string_parameters_ref()
        .and_then(|params| params.first("user_id"))
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

    tracing::info!("WebSocket connect: {} (user: {})", connection_id, user_id);

    // Save connection to DynamoDB
    save_connection(&state.dynamo_client, table_name, connection_id, &user_id).await?;

    Ok(Response::builder()
        .status(StatusCode::OK)
        .body(Body::Empty)
        .map_err(Box::new)?)
}

/// Handle $disconnect event
async fn handle_disconnect(
    state: Arc<AppState>,
    table_name: &str,
    connection_id: &str,
) -> Result<Response<Body>, Error> {
    tracing::info!("WebSocket disconnect: {}", connection_id);

    // Remove connection from DynamoDB
    remove_connection(&state.dynamo_client, table_name, connection_id).await?;

    Ok(Response::builder()
        .status(StatusCode::OK)
        .body(Body::Empty)
        .map_err(Box::new)?)
}

/// Handle $default event (incoming messages)
async fn handle_message(
    event: Request,
    state: Arc<AppState>,
    table_name: &str,
    _connection_id: &str,
) -> Result<Response<Body>, Error> {
    let body = event.body();

    // Parse incoming message
    let message: WebSocketMessage = match serde_json::from_slice(body) {
        Ok(msg) => msg,
        Err(e) => {
            tracing::error!("Failed to parse WebSocket message: {}", e);
            return Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from(format!(
                    r#"{{"error": "Invalid message format: {}"}}"#,
                    e
                )))
                .map_err(Box::new)?);
        }
    };

    tracing::info!("WebSocket message action: {}", message.action);

    // Get user_id from JWT or message data
    let user_id = event
        .request_context()
        .authorizer()
        .and_then(|auth| auth.jwt.as_ref())
        .and_then(|jwt| jwt.claims.get("sub"))
        .map(|s| s.to_string())
        .or_else(|| {
            message
                .data
                .get("user_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "test-user-123".to_string());

    // Route message to appropriate handler
    match message.action.as_str() {
        // Project actions
        "create_project" => {
            let body_bytes = serde_json::to_vec(&message.data)?;
            projects::create_project(&state.dynamo_client, table_name, &user_id, &body_bytes).await
        }
        "update_project" => {
            let project_id = message
                .data
                .get("project_id")
                .and_then(|v| v.as_str())
                .ok_or("Missing project_id")?;
            let body_bytes = serde_json::to_vec(&message.data)?;
            projects::update_project(&state.dynamo_client, table_name, project_id, &body_bytes)
                .await
        }
        "delete_project" => {
            let project_id = message
                .data
                .get("project_id")
                .and_then(|v| v.as_str())
                .ok_or("Missing project_id")?;
            projects::delete_project(
                &state.dynamo_client,
                &state.s3_client,
                table_name,
                project_id,
                &user_id,
            )
            .await
        }

        // Block actions
        "create_block" => {
            // let _project_id = message
            //     .data
            //     .get("project_id")
            //     .and_then(|v| v.as_str())
            //     .ok_or("Missing project_id")?;
            let body_bytes = serde_json::to_vec(&message.data)?;
            blocks::create_block(&state.dynamo_client, table_name, &body_bytes).await
        }
        "update_block" => {
            // let project_id = message
            //     .data
            //     .get("project_id")
            //     .and_then(|v| v.as_str())
            //     .ok_or("Missing project_id")?;
            let block_id = message
                .data
                .get("block_id")
                .and_then(|v| v.as_str())
                .ok_or("Missing block_id")?;
            let body_bytes = serde_json::to_vec(&message.data)?;
            blocks::update_block(
                &state.dynamo_client,
                table_name,
                block_id,
                &body_bytes,
            )
            .await
        }
        "delete_block" => {
            // let project_id = message
            //     .data
            //     .get("project_id")
            //     .and_then(|v| v.as_str())
            //     .ok_or("Missing project_id")?;
            let block_id = message
                .data
                .get("block_id")
                .and_then(|v| v.as_str())
                .ok_or("Missing block_id")?;
            blocks::delete_block(
                &state.dynamo_client,
                &state.s3_client,
                table_name,
                block_id
            )
            .await
        }

        // Class actions
        "create_class" => {
            let project_id = message
                .data
                .get("project_id")
                .and_then(|v| v.as_str())
                .ok_or("Missing project_id")?;
            let body_bytes = serde_json::to_vec(&message.data)?;
            labels::create_label(&state.dynamo_client, table_name, project_id, &body_bytes).await
        }
        "update_class" => {
            let project_id = message
                .data
                .get("project_id")
                .and_then(|v| v.as_str())
                .ok_or("Missing project_id")?;
            let class_id = message
                .data
                .get("class_id")
                .and_then(|v| v.as_str())
                .ok_or("Missing class_id")?;
            let body_bytes = serde_json::to_vec(&message.data)?;
            labels::update_label(
                &state.dynamo_client,
                table_name,
                project_id,
                class_id,
                &body_bytes,
            )
            .await
        }
        "delete_class" => {
            let project_id = message
                .data
                .get("project_id")
                .and_then(|v| v.as_str())
                .ok_or("Missing project_id")?;
            let class_id = message
                .data
                .get("class_id")
                .and_then(|v| v.as_str())
                .ok_or("Missing class_id")?;
            labels::delete_label(&state.dynamo_client, table_name, project_id, class_id).await
        }

        // Annotation actions
        "create_annotation" => {
            let block_id = message
                .data
                .get("block_id")
                .and_then(|v| v.as_str())
                .ok_or("Missing block_id")?;
            let image_id = message
                .data
                .get("image_id")
                .and_then(|v| v.as_str())
                .ok_or("Missing image_id")?;
            let body_bytes = serde_json::to_vec(&message.data)?;
            doxle_atoms::drawing::create_annotation(
                &state.dynamo_client,
                table_name,
                block_id,
                image_id,
                &user_id,
                &body_bytes,
            )
            .await
        }
        "update_annotation" => {
            let image_id = message
                .data
                .get("image_id")
                .and_then(|v| v.as_str())
                .ok_or("Missing image_id")?;
            let annotation_id = message
                .data
                .get("annotation_id")
                .and_then(|v| v.as_str())
                .ok_or("Missing annotation_id")?;
            let body_bytes = serde_json::to_vec(&message.data)?;
            doxle_atoms::drawing::update_annotation(
                &state.dynamo_client,
                table_name,
                image_id,
                annotation_id,
                &body_bytes,
            )
            .await
        }
        "delete_annotation" => {
            let block_id = message
                .data
                .get("block_id")
                .and_then(|v| v.as_str())
                .ok_or("Missing block_id")?;
            let image_id = message
                .data
                .get("image_id")
                .and_then(|v| v.as_str())
                .ok_or("Missing image_id")?;
            let annotation_id = message
                .data
                .get("annotation_id")
                .and_then(|v| v.as_str())
                .ok_or("Missing annotation_id")?;
            doxle_atoms::drawing::delete_annotation(
                &state.dynamo_client,
                table_name,
                block_id,
                image_id,
                annotation_id,
            )
            .await
        }

        _ => {
            tracing::warn!("Unknown action: {}", message.action);
            Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from(format!(
                    r#"{{"error": "Unknown action: {}"}}"#,
                    message.action
                )))
                .map_err(Box::new)?)
        }
    }
}
