use aws_sdk_dynamodb::Client as DynamoClient;
use aws_sdk_dynamodb::types::AttributeValue;
use super::model::{Task, CreateTaskPayload};
use std::collections::HashMap;

/// Load all tasks for a block (pure domain logic, no HTTP)
/// Images field will be empty - populated by block layer during joins
pub async fn load_tasks_for_block(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
) -> Result<Vec<Task>, String> {
    let pk = format!("BLOCK#{}", block_id);
    
    let result = client
        .query()
        .table_name(table_name)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(pk))
        .expression_attribute_values(":sk_prefix", AttributeValue::S("TASK#".to_string()))
        .send()
        .await
        .map_err(|e| format!("DynamoDB query error: {}", e))?;
    
    let mut tasks = Vec::new();
    for item in result.items() {
        if let Some(sk) = item.get("SK").and_then(|v| v.as_s().ok()) {
            if let Some(task_id) = sk.strip_prefix("TASK#") {
                let task = Task {
                    task_id: task_id.to_string(),
                    block_id: block_id.to_string(),
                    task_name: item
                        .get("task_name")
                        .and_then(|v| v.as_s().ok())
                        .map(|s| s.to_string())
                        .unwrap_or_default(),
                    task_state: item
                        .get("task_state")
                        .and_then(|v| v.as_s().ok())
                        .map(|s| s.to_string())
                        .unwrap_or_default(),
                    assignee: item
                        .get("assignee")
                        .and_then(|v| v.as_s().ok())
                        .map(|s| s.to_string())
                        .unwrap_or_default(),
                    checked_by: item
                        .get("checked_by")
                        .and_then(|v| v.as_s().ok())
                        .map(|s| s.to_string())
                        .unwrap_or_default(),
                    locked: item
                        .get("locked")
                        .and_then(|v| v.as_bool().ok())
                        .copied()
                        .unwrap_or(false),
                    image_count:item
                        .get("image_count")
                        .and_then(|v| v.as_n().ok())
                        .and_then(|n| n.parse().ok())
                        .unwrap_or(0),
                    created_at: item
                        .get("created_at")
                        .and_then(|v| v.as_s().ok())
                        .map(|s| s.to_string())
                        .unwrap_or_default(),
                    images: vec![],  // Filled in later by be/blocks/* when joining with media
                };
                tasks.push(task);
            }
        }
    }
    
    Ok(tasks)
}

/// Create a new task in a block
pub async fn create_task(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    payload: CreateTaskPayload,
) -> Result<Task, String> {
    let task_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let pk = format!("BLOCK#{}", block_id);
    let sk = format!("TASK#{}", task_id);
    
    let mut builder = client
        .put_item()
        .table_name(table_name)
        .item("PK", AttributeValue::S(pk))
        .item("SK", AttributeValue::S(sk))
        .item("task_name", AttributeValue::S(payload.task_name.clone()))
        .item("task_state", AttributeValue::S("todo".to_string()))
        .item("image_count", AttributeValue::N("0".to_string()))
        .item("created_at", AttributeValue::S(now.clone()))
        .item("locked", AttributeValue::Bool(false));

    if let Some(assignee) = &payload.assignee {
        builder = builder.item("assignee", AttributeValue::S(assignee.clone()));
    }
    if let Some(checked_by) = &payload.checked_by {
        builder = builder.item("checked_by", AttributeValue::S(checked_by.clone()));
    }
    
    builder.send().await.map_err(|e| format!("DynamoDB put_item error: {}", e))?;
    
    Ok(Task {
        task_id,
        block_id: block_id.to_string(),
        task_name: payload.task_name,
        task_state: "todo".to_string(),
        assignee: payload.assignee.unwrap_or_default(),
        checked_by: payload.checked_by.unwrap_or_default(),
        locked: false,
        image_count:0,
        images: vec![],
        created_at: now,

    })
}

/// Get a specific task
pub async fn get_task(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    task_id: &str,
) -> Result<Task, String> {
    let pk = format!("BLOCK#{}", block_id);
    let sk = format!("TASK#{}", task_id);
    
    let result = client
        .get_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(pk))
        .key("SK", AttributeValue::S(sk))
        .send()
        .await
        .map_err(|e| format!("DynamoDB get_item error: {}", e))?;
    
    if let Some(item) = result.item() {
        Ok(Task {
            task_id: task_id.to_string(),
            block_id: block_id.to_string(),
            task_name: item
                .get("task_name")
                .and_then(|v| v.as_s().ok())
                .map(|s| s.to_string())
                .unwrap_or_default(),
            task_state: item
                .get("task_state")
                .and_then(|v| v.as_s().ok())
                .map(|s| s.to_string())
                .unwrap_or_default(),
            assignee: item
                .get("assignee")
                .and_then(|v| v.as_s().ok())
                .map(|s| s.to_string())
                .unwrap_or_default(),
            checked_by: item
                .get("checked_by")
                .and_then(|v| v.as_s().ok())
                .map(|s| s.to_string())
                .unwrap_or_default(),
            locked: item
                .get("locked")
                .and_then(|v| v.as_bool().ok())
                .copied()
                .unwrap_or(false),
            image_count:item
                .get("image_count")
                .and_then(|v| v.as_n().ok())
                .and_then(|n| n.parse().ok())
                .unwrap_or(0),
            created_at: item
                .get("created_at")
                .and_then(|v| v.as_s().ok())
                .map(|s| s.to_string())
                .unwrap_or_default(),
            images: vec![],
        })
    } else {
        Err("Task not found".to_string())
    }
}

/// Update a task
pub async fn update_task(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    task_id: &str,
    payload: super::model::UpdateTaskPayload,
) -> Result<Task, String> {
    let pk = format!("BLOCK#{}", block_id);
    let sk = format!("TASK#{}", task_id);

    // This is to update the task count
    let old_task  = get_task(client, table_name, block_id, task_id).await?;
    let old_task_state = old_task.task_state.clone();
    let old_task_image_count = old_task.image_count as i64;


    
    let mut update_expr = vec![];
    let mut expr_names = HashMap::new();
    let mut expr_values = HashMap::new();
    
    if let Some(name) = payload.task_name {
        update_expr.push("#task_name = :task_name");
        expr_names.insert("#task_name".to_string(), "task_name".to_string());
        expr_values.insert(":task_name".to_string(), AttributeValue::S(name));
    }
    
    if let Some(ref state) = payload.task_state {
        update_expr.push("#task_state = :task_state");
        expr_names.insert("#task_state".to_string(), "task_state".to_string());
        expr_values.insert(":task_state".to_string(), AttributeValue::S(state.to_string()));
    }

    if let Some(assignee) = payload.assignee {
        update_expr.push("#assignee = :assignee");
        expr_names.insert("#assignee".to_string(), "assignee".to_string());
        expr_values.insert(":assignee".to_string(), AttributeValue::S(assignee));
    }

    if let Some(checked_by) = payload.checked_by {
        update_expr.push("#checked_by = :checked_by");
        expr_names.insert("#checked_by".to_string(), "checked_by".to_string());
        expr_values.insert(":checked_by".to_string(), AttributeValue::S(checked_by));
    }
    
    if !update_expr.is_empty() {
        let update_expression = format!("SET {}", update_expr.join(", "));

        // Update approved_image_count only when state changes

        if let Some(new_state) = payload.task_state.as_deref(){
            let delta:i64 = match (&*old_task_state, new_state) {
                ("done", "done") => 0,
                ("done", _) => -old_task_image_count,
                (_, "done") => old_task_image_count,
                _=>0,
            };

            if delta != 0 {
                client
                    .update_item()
                    .table_name(table_name)
                    .key("PK", AttributeValue::S("BLOCK".to_string()))
                    .key("SK", AttributeValue::S(format!("BLOCK#{}", block_id)))
                    .update_expression("SET approved_image_count = approved_image_count + :delta")
                    .expression_attribute_values(":delta", AttributeValue::N(delta.to_string()))
                    .send()
                    .await
                    .map_err(|e| format!("DynamoDB update_item error: {}", e))?;
            
            }
        }
        
        let mut builder = client
            .update_item()
            .table_name(table_name)
            .key("PK", AttributeValue::S(pk))
            .key("SK", AttributeValue::S(sk))
            .update_expression(update_expression);
            
        for (k, v) in expr_names {
            builder = builder.expression_attribute_names(k, v);
        }
        
        for (k, v) in expr_values {
            builder = builder.expression_attribute_values(k, v);
        }
        
        builder.send().await.map_err(|e| format!("DynamoDB update_item error: {}", e))?;
    }
    
    get_task(client, table_name, block_id, task_id).await
}

/// Delete a task
pub async fn delete_task(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    task_id: &str,
) -> Result<(), String> {
    
    
    // Del Images for Tasks
    let task_images = crate::media::service::load_images_for_task(client, table_name, block_id, task_id).await?;
    for image in task_images {
         // Del Annotations for Images
         let image_id = image.image_id.as_str();
         crate::media::service::delete_image(client, table_name, block_id, image_id).await?;

    }

    client
        .delete_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(format!("BLOCK#{}", block_id)))
        .key("SK", AttributeValue::S(format!("TASK#{}", task_id)))
        .send()
        .await
        .map_err(|e| format!("DynamoDB delete_item error: {}", e))?;
    
    Ok(())
}
