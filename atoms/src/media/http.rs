use aws_sdk_dynamodb::Client as DynamoClient;
use lambda_http::{Body, Error as LambdaError, Response, http::StatusCode};
use super::model::UpdateImagePayload;
use super::service::{delete_image, get_image, update_image};

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
    block_id: &str,
    image_id: &str,
) -> Result<Response<Body>, LambdaError> {
    match delete_image(client, table_name, block_id, image_id).await {
        Ok(_) => Ok(Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header("Access-Control-Allow-Origin", "*")
            .body(Body::Empty)
            .map_err(Box::new)?),
        Err(e) => Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::json!({"error": e}).to_string().into())
            .map_err(Box::new)?),
    }
}
