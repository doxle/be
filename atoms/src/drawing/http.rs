use aws_sdk_dynamodb::Client as DynamoClient;
use lambda_http::{Body, Error, Response, http::StatusCode};
use super::model::{CreateAnnotationPayload, UpdateAnnotationPayload};
use super::service;

pub async fn create_annotation(
    client: &DynamoClient,
    table_name: &str,
    block_id:&str,
    image_id: &str,
    user_id: &str,
    body: &[u8],
) -> Result<Response<Body>, Error> {
    let payload: CreateAnnotationPayload = serde_json::from_slice(body)?;
    
    match service::create_annotation(client, table_name, block_id, image_id, user_id, payload).await {
        Ok(annotation) => Ok(Response::builder()
            .status(StatusCode::CREATED)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::to_string(&annotation)?.into())
            .map_err(Box::new)?),
        Err(e) => Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::json!({ "error": e }).to_string().into())
            .map_err(Box::new)?)
    }
}

pub async fn list_image_annotations(
    client: &DynamoClient,
    table_name: &str,
    image_id: &str,
) -> Result<Response<Body>, Error> {
    match service::list_annotations(client, table_name, image_id).await {
        Ok(annotations) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::to_string(&annotations)?.into())
            .map_err(Box::new)?),
        Err(e) => Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::json!({ "error": e }).to_string().into())
            .map_err(Box::new)?)
    }
}

pub async fn delete_annotation(
    client: &DynamoClient,
    table_name: &str,
    block_id:&str,
    image_id: &str,
    annotation_id: &str,
) -> Result<Response<Body>, Error> {
    match service::delete_annotation(client, table_name, block_id, image_id, annotation_id).await {
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
            .map_err(Box::new)?)
    }
}

pub async fn update_annotation(
    client: &DynamoClient,
    table_name: &str,
    image_id: &str,
    annotation_id: &str,
    body: &[u8],
) -> Result<Response<Body>, Error> {
    let payload: UpdateAnnotationPayload = serde_json::from_slice(body)?;

    match service::update_annotation(client, table_name, image_id, annotation_id, payload).await {
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
            .map_err(Box::new)?)
    }
}

pub async fn get_annotation(
    _client: &DynamoClient,
    _table_name: &str,
    _image_id: &str,
    _annotation_id: &str,
) -> Result<Response<Body>, Error> {
    Ok(Response::builder()
        .status(StatusCode::NOT_IMPLEMENTED)
        .body(Body::from("Get not implemented"))
        .map_err(Box::new)?)
}

