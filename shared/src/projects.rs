use aws_sdk_dynamodb::Client as DynamoClient;
use aws_sdk_s3::Client as S3Client;
use lambda_http::{http::StatusCode, Body, Error, Response};

/// Projects have been removed from the domain model.
/// These functions are kept only to keep older routes compiling if they are still called.
/// All of them now return 410 Gone.

pub async fn create_project(
    _client: &DynamoClient,
    _table_name: &str,
    _user_id: &str,
    _body: &[u8],
) -> Result<Response<Body>, Error> {
    Ok(Response::builder()
        .status(StatusCode::GONE)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body("{\"error\": \"Projects have been removed\"}".into())
        .map_err(Box::new)?)
}

pub async fn get_project(
    _client: &DynamoClient,
    _table_name: &str,
    _project_id: &str,
) -> Result<Response<Body>, Error> {
    Ok(Response::builder()
        .status(StatusCode::GONE)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body("{\"error\": \"Projects have been removed\"}".into())
        .map_err(Box::new)?)
}

pub async fn list_user_projects(
    _client: &DynamoClient,
    _table_name: &str,
    _user_id: &str,
) -> Result<Response<Body>, Error> {
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body("[]".into())
        .map_err(Box::new)?)
}

pub async fn update_project(
    _client: &DynamoClient,
    _table_name: &str,
    _project_id: &str,
    _body: &[u8],
) -> Result<Response<Body>, Error> {
    Ok(Response::builder()
        .status(StatusCode::GONE)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .header("Access-Control-Allow-Methods", "GET, POST, PUT, PATCH, DELETE, OPTIONS")
        .header("Access-Control-Allow-Headers", "*")
        .body("{\"error\": \"Projects have been removed\"}".into())
        .map_err(Box::new)?)
}

pub async fn delete_project(
    _client: &DynamoClient,
    _s3_client: &S3Client,
    _table_name: &str,
    _project_id: &str,
    _user_id: &str,
) -> Result<Response<Body>, Error> {
    Ok(Response::builder()
        .status(StatusCode::GONE)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body("{\"error\": \"Projects have been removed\"}".into())
        .map_err(Box::new)?)
}
