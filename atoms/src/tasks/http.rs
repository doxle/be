use aws_sdk_dynamodb::Client as DynamoClient;
use lambda_http::{Body, Error, Response, http::StatusCode};
use std::collections::HashMap;
use super::service;

// Import media service for the join
use crate::media::service::load_images_for_block;

/// List all tasks for a block with their images (Backend Join)
pub async fn list_block_tasks(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
) -> Result<Response<Body>, Error> {
    // 1. Fetch Tasks and Images in parallel
    let (tasks_result, images_result) = tokio::join!(
        service::load_tasks_for_block(client, table_name, block_id),
        load_images_for_block(client, table_name, block_id)
    );
    
    // Handle errors (if either fails, we return 500)
    let mut tasks = tasks_result.map_err(|e| Box::new(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
    let images = images_result.map_err(|e| Box::new(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
    
    // 2. Index images by task_id
    let mut task_images: HashMap<String, Vec<crate::media::Image>> = HashMap::new();
    for image in images {
        if let Some(tid) = &image.task_id {
            task_images.entry(tid.clone()).or_default().push(image);
        }
    }
    
    // 3. Attach images to tasks
    for task in &mut tasks {
        if let Some(mut imgs) = task_images.remove(&task.task_id) {
             // Sort images by order
             imgs.sort_by(|a, b| match (a.order, b.order) {
                (Some(a_order), Some(b_order)) => a_order.cmp(&b_order),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            });
            task.images = imgs;
        }
    }
    
    // 4. Return Response
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(serde_json::to_string(&tasks)?.into())
        .map_err(Box::new)?)
}
