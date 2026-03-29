use aws_sdk_dynamodb::Client as DynamoClient;
use lambda_http::{Body, Error as LambdaError, Response, http::StatusCode};
use super::model::{CreateImagePayload, UpdateImagePayload};
use super::service::{create_image, delete_image, get_image, load_images_for_block, update_image};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct CreateBlockMediaRequest {
    image_id: String,
    image_name: String,
    url: String,
    media_type: Option<String>,
}

/// HTTP Handler: GET /projects/{project_id}/blocks/{block_id}/media
pub async fn list_block_media_handler(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
) -> Result<Response<Body>, LambdaError> {
    match load_images_for_block(client, table_name, block_id).await {
        Ok(images) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::to_string(&images)?.into())
            .map_err(Box::new)?),
        Err(e) => Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::json!({ "error": e }).to_string().into())
            .map_err(Box::new)?),
    }
}

/// HTTP Handler: POST /projects/{project_id}/blocks/{block_id}/media
pub async fn create_block_media_handler(
    client: &DynamoClient,
    table_name: &str,
    project_id: &str,
    block_id: &str,
    body: &[u8],
) -> Result<Response<Body>, LambdaError> {
    let req: CreateBlockMediaRequest = serde_json::from_slice(body)?;
    let payload = CreateImagePayload {
        image_id: req.image_id,
        image_name: req.image_name,
        url: req.url,
        task_id: None,
        order: None,
        media_type: req.media_type,
        markup_rects: None,
        width: None,
        height: None,
    };

    match create_image(client, table_name, project_id, block_id, payload).await {
        Ok(image) => Ok(Response::builder()
            .status(StatusCode::CREATED)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::to_string(&image)?.into())
            .map_err(Box::new)?),
        Err(e) => Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::json!({ "error": e }).to_string().into())
            .map_err(Box::new)?),
    }
}

/// HTTP Handler: GET /images/{id}
pub async fn get_image_handler(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    image_id: &str,
) -> Result<Response<Body>, LambdaError> {
    match get_image(client, table_name, block_id, image_id).await {
        Ok(image) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::to_string(&image)?.into())
            .map_err(Box::new)?),
        Err(e) if e == "Image not found" => Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::json!({"error": e}).to_string().into())
            .map_err(Box::new)?),
        Err(e) => Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::json!({"error": e}).to_string().into())
            .map_err(Box::new)?),
    }
}

/// HTTP Handler: PATCH /images/{id}
pub async fn update_image_handler(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    image_id: &str,
    body: &[u8],
) -> Result<Response<Body>, LambdaError> {
    let payload: UpdateImagePayload = serde_json::from_slice(body)?;
    
    match update_image(client, table_name, block_id, image_id, payload).await {
        Ok(image) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::to_string(&image)?.into())
            .map_err(Box::new)?),
        Err(e) => Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::json!({"error": e}).to_string().into())
            .map_err(Box::new)?),
    }
}

/// HTTP Handler: DELETE /images/{id}
pub async fn delete_image_handler(
    client: &DynamoClient,
    table_name: &str,
    project_id: &str,
    block_id: &str,
    image_id: &str,
) -> Result<Response<Body>, LambdaError> {
    match delete_image(client, table_name, project_id, block_id, image_id).await {
        Ok(result) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::to_string(&result)?.into())
            .map_err(Box::new)?),
        Err(e) => Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::json!({"error": e}).to_string().into())
            .map_err(Box::new)?),
    }
}
