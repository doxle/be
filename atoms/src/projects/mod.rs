pub mod model;
pub mod service;

// HTTP handlers for project CRUD
use aws_sdk_dynamodb::Client as DynamoClient;
use lambda_http::{Body, Error, Response, http::StatusCode};

pub async fn http_list_projects(
    client: &DynamoClient,
    table_name: &str,
) -> Result<Response<Body>, Error> {
    match service::list_projects(client, table_name).await {
        Ok(projects) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::to_string(&projects)?.into())
            .map_err(Box::new)?),
        Err(e) => Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::json!({"error": e}).to_string().into())
            .map_err(Box::new)?),
    }
}

pub async fn http_create_project(
    client: &DynamoClient,
    table_name: &str,
    owner_id: &str,
    body: &[u8],
) -> Result<Response<Body>, Error> {
    let payload: model::CreateProjectPayload = serde_json::from_slice(body)?;
    match service::create_project(client, table_name, owner_id, payload).await {
        Ok(project) => Ok(Response::builder()
            .status(StatusCode::CREATED)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::to_string(&project)?.into())
            .map_err(Box::new)?),
        Err(e) => Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::json!({"error": e}).to_string().into())
            .map_err(Box::new)?),
    }
}

pub async fn http_get_project(
    client: &DynamoClient,
    table_name: &str,
    project_id: &str,
) -> Result<Response<Body>, Error> {
    match service::get_project(client, table_name, project_id).await {
        Ok(project) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::to_string(&project)?.into())
            .map_err(Box::new)?),
        Err(e) if e == "Project not found" => Ok(Response::builder()
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

pub async fn http_update_project(
    client: &DynamoClient,
    table_name: &str,
    project_id: &str,
    body: &[u8],
) -> Result<Response<Body>, Error> {
    let payload: model::UpdateProjectPayload = serde_json::from_slice(body)?;
    match service::update_project(client, table_name, project_id, payload).await {
        Ok(project) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::to_string(&project)?.into())
            .map_err(Box::new)?),
        Err(e) => Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::json!({"error": e}).to_string().into())
            .map_err(Box::new)?),
    }
}

pub async fn http_delete_project(
    client: &DynamoClient,
    table_name: &str,
    project_id: &str,
) -> Result<Response<Body>, Error> {
    match service::delete_project(client, table_name, project_id).await {
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
