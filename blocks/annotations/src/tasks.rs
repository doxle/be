use lambda_http::{Body, Error, Response, http::StatusCode};
use aws_sdk_dynamodb::Client as DynamoClient;
use aws_sdk_s3::Client as S3Client;
use doxle_atoms::{tasks, media};
use doxle_atoms::users::model::UserRole;
use doxle_atoms::tasks::model::TaskState;

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

/// List all tasks for a block.
/// Uses materialized counters from task items — no image join.
/// Annotators only see tasks assigned to them.
pub async fn list_block_tasks(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    user_id: &str,
    user_role: &UserRole,
) -> Result<Response<Body>, Error> {
    // 1) Load tasks via domain service (images empty, counts from DynamoDB)
    let mut task_rows = tasks::service::load_tasks_for_block(client, table_name, block_id)
        .await
        .map_err(|e| Box::new(std::io::Error::new(std::io::ErrorKind::Other, e)) as Box<dyn std::error::Error + Send + Sync>)?;

    // Filter for annotators: only show tasks assigned to them
    if user_role == &UserRole::Annotator {
        task_rows.retain(|t| t.assignee == user_id);
    }

    // Sort by created_at desc (newest first)
    task_rows.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    let json = serde_json::to_string(&task_rows)?;
    tracing::info!("📋 Tasks list: {} tasks, {} bytes ({:.2} MB)", task_rows.len(), json.len(), json.len() as f64 / 1_048_576.0);

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(json.into())
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
    
    let task = tasks::service::update_task(client, table_name, "", block_id, task_id, payload)
        .await
        .map_err(|e| Box::new(std::io::Error::new(std::io::ErrorKind::Other, e)) as Box<dyn std::error::Error + Send + Sync>)?;
    
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(serde_json::to_string(&task)?.into())
        .map_err(Box::new)?)
}

/// Update a task with role-based validation.
/// Annotators can only change task_state (with validated transitions) on tasks assigned to them.
/// Admins can update anything.
pub async fn update_task_with_role(
    client: &DynamoClient,
    table_name: &str,
    project_id: &str,
    block_id: &str,
    task_id: &str,
    body: &[u8],
    user_id: &str,
    user_role: &UserRole,
) -> Result<Response<Body>, Error> {
    let payload: tasks::model::UpdateTaskPayload = serde_json::from_slice(body)?;

    if user_role == &UserRole::Annotator {
        // Annotators can only change task_state, nothing else
        if payload.task_name.is_some() || payload.assignee.is_some() || payload.checked_by.is_some() {
            return Ok(Response::builder()
                .status(StatusCode::FORBIDDEN)
                .header("Content-Type", "application/json")
                .header("Access-Control-Allow-Origin", "*")
                .body(serde_json::json!({"error": "Forbidden", "message": "Annotators can only update task state"}).to_string().into())
                .map_err(Box::new)?);
        }

        // Verify the task is assigned to this annotator
        let current_task = tasks::service::get_task(client, table_name, block_id, task_id)
            .await
            .map_err(|e| Box::new(std::io::Error::new(std::io::ErrorKind::Other, e)) as Box<dyn std::error::Error + Send + Sync>)?;

        if current_task.assignee != user_id {
            return Ok(Response::builder()
                .status(StatusCode::FORBIDDEN)
                .header("Content-Type", "application/json")
                .header("Access-Control-Allow-Origin", "*")
                .body(serde_json::json!({"error": "Forbidden", "message": "You are not assigned to this task"}).to_string().into())
                .map_err(Box::new)?);
        }

        // Validate state transition
        if let Some(ref new_state) = payload.task_state {
            if !TaskState::can_transition(&current_task.task_state, new_state, user_role) {
                return Ok(Response::builder()
                    .status(StatusCode::FORBIDDEN)
                    .header("Content-Type", "application/json")
                    .header("Access-Control-Allow-Origin", "*")
                    .body(serde_json::json!({
                        "error": "Forbidden",
                        "message": format!("Cannot transition from {} to {}", current_task.task_state.as_str(), new_state.as_str())
                    }).to_string().into())
                    .map_err(Box::new)?);
            }
        }
    }

    let task = tasks::service::update_task(client, table_name, project_id, block_id, task_id, payload)
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
    s3_client: &S3Client,
    table_name: &str,
    project_id: &str,
    block_id: &str,
    task_id: &str,
) -> Result<Response<Body>, Error> {

    // Get images from deletion to delete from S3
    let images = media::service::load_images_for_task(client, table_name, block_id, task_id)
            .await
            .map_err(|e| format!("Failed to load images: {}", e))?;

    // Delete from S3
    let bucket_name = std::env::var("S3_BUCKET_NAME").unwrap_or_else(|_| "doxle_app".to_string());

    for image in images {
        // Extract extension from URL path (after last /)
        let ext = image.url.rsplit('/').next().and_then(|filename| filename.rsplit('.').next()).unwrap_or("jpg");
        let s3_key = format!("annotations/blocks/{}/images/{}.{}", block_id, image.image_id, ext); // we do this replace the original filename with image_id
        let _ = s3_client.delete_object().bucket(&bucket_name).key(&s3_key).send().await;

    }


    // Delete from DynamoDB
    tasks::service::delete_task(client, table_name, project_id, block_id, task_id)
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
   
    let mut task = tasks::service::get_task(client, table_name, block_id, task_id).await?;
    let images = media::service::load_images_for_task(client, table_name, block_id, task_id).await?;
    task.annotation_count = images.iter().map(|img| img.annotation_count).sum();
    task.images = images;

    
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(serde_json::to_string(&task)?.into())
        .map_err(Box::new)?)
}
