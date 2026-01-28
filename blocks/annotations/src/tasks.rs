use lambda_http::{Body, Error, Response, http::StatusCode};
use aws_sdk_dynamodb::Client as DynamoClient;
use doxle_atoms::{tasks, media};
use std::collections::HashMap;

/// Create a new task
pub async fn create_task(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    body: &[u8],
) -> Result<Response<Body>, Error> {
    let payload: tasks::model::CreateTaskPayload = serde_json::from_slice(body)?;
    
    let task = tasks::service::create_task(client, table_name, block_id, payload)
        .await
        .map_err(|e| Box::new(std::io::Error::new(std::io::ErrorKind::Other, e)) as Box<dyn std::error::Error + Send + Sync>)?;
    
    Ok(Response::builder()
        .status(StatusCode::CREATED)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(serde_json::to_string(&task)?.into())
        .map_err(Box::new)?)
}

/// List all tasks for a block (returns tasks WITH images)
pub async fn list_block_tasks(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
) -> Result<Response<Body>, Error> {
    // 1) Load tasks via domain service (images empty)
    let mut task_rows = tasks::service::load_tasks_for_block(client, table_name, block_id)
        .await
        .map_err(|e| Box::new(std::io::Error::new(std::io::ErrorKind::Other, e)) as Box<dyn std::error::Error + Send + Sync>)?;

    // 2) Load ALL images for block
    let image_rows = media::service::load_images_for_block(client, table_name, block_id)
        .await
        .map_err(|e| Box::new(std::io::Error::new(std::io::ErrorKind::Other, e)) as Box<dyn std::error::Error + Send + Sync>)?;

    // 3) Group images by task_id
    let mut images_by_task: HashMap<String, Vec<media::model::Image>> = HashMap::new();
    for img in image_rows {
        if let Some(tid) = &img.task_id {
            images_by_task.entry(tid.clone()).or_default().push(img);
        }
    }

    // 4) Populate tasks with images
    for t in &mut task_rows {
        t.images = images_by_task.remove(&t.task_id).unwrap_or_default();
    }

    // Sort by created_at desc (newest first)
    task_rows.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(serde_json::to_string(&task_rows)?.into())
        .map_err(Box::new)?)
}

/// Update a task
pub async fn update_task(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    task_id: &str,
    body: &[u8],
) -> Result<Response<Body>, Error> {
    let payload: tasks::model::UpdateTaskPayload = serde_json::from_slice(body)?;
    
    let task = tasks::service::update_task(client, table_name, block_id, task_id, payload)
        .await
        .map_err(|e| Box::new(std::io::Error::new(std::io::ErrorKind::Other, e)) as Box<dyn std::error::Error + Send + Sync>)?;
    
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(serde_json::to_string(&task)?.into())
        .map_err(Box::new)?)
}

/// Delete a task
pub async fn delete_task(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    task_id: &str,
) -> Result<Response<Body>, Error> {
    tasks::service::delete_task(client, table_name, block_id, task_id)
        .await
        .map_err(|e| Box::new(std::io::Error::new(std::io::ErrorKind::Other, e)) as Box<dyn std::error::Error + Send + Sync>)?;
    
    Ok(Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header("Access-Control-Allow-Origin", "*")
        .body(Body::Empty)
        .map_err(Box::new)?)
}

/// Get a single task
pub async fn get_task(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    task_id: &str,
) -> Result<Response<Body>, Error> {
    let task = tasks::service::get_task(client, table_name, block_id, task_id)
        .await
        .map_err(|e| {
            if e == "Task not found" {
                return Box::new(std::io::Error::new(std::io::ErrorKind::NotFound, e)) as Box<dyn std::error::Error + Send + Sync>;
            }
            Box::new(std::io::Error::new(std::io::ErrorKind::Other, e)) as Box<dyn std::error::Error + Send + Sync>
        })?;
    
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(serde_json::to_string(&task)?.into())
        .map_err(Box::new)?)
}
