use aws_sdk_dynamodb::Client as DynamoClient;
use aws_sdk_dynamodb::types::AttributeValue;
use lambda_http::{Body, Error, Response, http::StatusCode};
use super::model::{CreateThreadPayload, CreateCommentPayload, UpdateThreadPayload};
use super::service;

/// Fetch user_name from DynamoDB for a given user_id
async fn fetch_user_name(client: &DynamoClient, table_name: &str, user_id: &str) -> String {
    let sk = format!("USER#{}", user_id);
    let result = client
        .get_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S("USER".to_string()))
        .key("SK", AttributeValue::S(sk))
        .projection_expression("user_name, user_email")
        .send()
        .await;

    match result {
        Ok(output) => {
            if let Some(item) = output.item() {
                let name = item.get("user_name")
                    .and_then(|v| v.as_s().ok())
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.to_string());
                if let Some(n) = name {
                    return n;
                }
                item.get("user_email")
                    .and_then(|v| v.as_s().ok())
                    .and_then(|e| e.split('@').next())
                    .unwrap_or("User")
                    .to_string()
            } else {
                "User".to_string()
            }
        }
        Err(_) => "User".to_string(),
    }
}

/// GET /comments/{parent_id}/threads
pub async fn list_threads(
    client: &DynamoClient,
    table_name: &str,
    parent_id: &str,
) -> Result<Response<Body>, Error> {
    match service::list_threads(client, table_name, parent_id).await {
        Ok(threads) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::to_string(&threads)?.into())
            .map_err(Box::new)?),
        Err(e) => Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::json!({ "error": e }).to_string().into())
            .map_err(Box::new)?),
    }
}

/// POST /comments/{parent_id}/threads
pub async fn create_thread(
    client: &DynamoClient,
    table_name: &str,
    parent_id: &str,
    user_id: &str,
    body: &[u8],
) -> Result<Response<Body>, Error> {
    let payload: CreateThreadPayload = serde_json::from_slice(body)?;
    let user_name = fetch_user_name(client, table_name, user_id).await;

    match service::create_thread(client, table_name, parent_id, user_id, &user_name, payload).await {
        Ok(thread) => Ok(Response::builder()
            .status(StatusCode::CREATED)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::to_string(&thread)?.into())
            .map_err(Box::new)?),
        Err(e) => Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::json!({ "error": e }).to_string().into())
            .map_err(Box::new)?),
    }
}

/// PATCH /comments/{parent_id}/threads/{thread_id}
pub async fn update_thread(
    client: &DynamoClient,
    table_name: &str,
    parent_id: &str,
    thread_id: &str,
    body: &[u8],
) -> Result<Response<Body>, Error> {
    let payload: UpdateThreadPayload = serde_json::from_slice(body)?;

    match service::update_thread(client, table_name, parent_id, thread_id, payload).await {
        Ok(_) => Ok(Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header("Access-Control-Allow-Origin", "*")
            .body(Body::Empty)
            .map_err(Box::new)?),
        Err(e) => Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::json!({ "error": e }).to_string().into())
            .map_err(Box::new)?),
    }
}

/// DELETE /comments/{parent_id}/threads/{thread_id}
pub async fn delete_thread(
    client: &DynamoClient,
    table_name: &str,
    parent_id: &str,
    thread_id: &str,
) -> Result<Response<Body>, Error> {
    match service::delete_thread(client, table_name, parent_id, thread_id).await {
        Ok(_) => Ok(Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header("Access-Control-Allow-Origin", "*")
            .body(Body::Empty)
            .map_err(Box::new)?),
        Err(e) => Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::json!({ "error": e }).to_string().into())
            .map_err(Box::new)?),
    }
}

/// POST /comments/{parent_id}/threads/{thread_id}/comments
pub async fn add_comment(
    client: &DynamoClient,
    table_name: &str,
    thread_id: &str,
    user_id: &str,
    body: &[u8],
) -> Result<Response<Body>, Error> {
    let payload: CreateCommentPayload = serde_json::from_slice(body)?;
    let user_name = fetch_user_name(client, table_name, user_id).await;

    match service::add_comment(client, table_name, thread_id, user_id, &user_name, payload).await {
        Ok(comment) => Ok(Response::builder()
            .status(StatusCode::CREATED)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::to_string(&comment)?.into())
            .map_err(Box::new)?),
        Err(e) => Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::json!({ "error": e }).to_string().into())
            .map_err(Box::new)?),
    }
}
