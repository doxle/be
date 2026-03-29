
use aws_sdk_dynamodb::Client as DynamoClient;
use aws_sdk_dynamodb::types::AttributeValue;
use super::model::{CreateImagePayload, Image, MarkupRect, UpdateImagePayload};
use std::collections::HashMap;
use std::cmp::Ordering;

fn parse_markup_rects(item: &HashMap<String, AttributeValue>) -> Vec<MarkupRect> {
    item.get("markup_rects")
        .and_then(|v| v.as_s().ok())
        .and_then(|s| serde_json::from_str::<Vec<MarkupRect>>(s).ok())
        .unwrap_or_default()
}

/// Load all images for a block (pure domain logic, no HTTP)
/// Used by blocks layer to perform joins with tasks
pub async fn load_images_for_block(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
) -> Result<Vec<Image>, String> {
    let pk = format!("BLOCK#{}", block_id);
    let mut images = Vec::new();
    let mut last_evaluated_key: Option<HashMap<String, AttributeValue>> = None;

    loop {
        let mut query = client
            .query()
            .table_name(table_name)
            .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
            .expression_attribute_values(":pk", AttributeValue::S(pk.clone()))
            .expression_attribute_values(":sk_prefix", AttributeValue::S("IMAGE#".to_string()));

        if let Some(start_key) = last_evaluated_key.clone() {
            query = query.set_exclusive_start_key(Some(start_key));
        }

        let result = query
            .send()
            .await
            .map_err(|e| format!("DynamoDB query error: {}", e))?;

        for item in result.items() {
            if let Some(sk) = item.get("SK").and_then(|v| v.as_s().ok()) {
                if let Some(image_id) = sk.strip_prefix("IMAGE#") {
                    let image = Image {
                        image_id: image_id.to_string(),
                        block_id: block_id.to_string(),
                        task_id: item.get("task_id").and_then(|v| v.as_s().ok()).map(|s| s.to_string()),
                        image_name: item.get("image_name").and_then(|v| v.as_s().ok()).map(|s| s.to_string()).unwrap_or_default(),
                        url: item.get("url").and_then(|v| v.as_s().ok()).map(|s| s.to_string()).unwrap_or_default(),
                        locked: item.get("locked").and_then(|v| v.as_bool().ok()).copied().unwrap_or(false),
                        order: item.get("order").and_then(|v| v.as_n().ok()).and_then(|n| n.parse().ok()),
                        annotation_count: item.get("annotation_count").and_then(|v| v.as_n().ok()).and_then(|n| n.parse().ok()).unwrap_or(0),
                        labels_count: item.get("labels_count")
                            .and_then(|v| v.as_m().ok())
                            .map(|m| {
                                m.iter()
                                    .filter_map(|(k,v)|
                                        v.as_n().ok()
                                            .and_then(|n| n.parse::<u32>().ok())
                                            .map(|n| (k.clone(), n))
                                        )
                                    .collect()
                            })
                            .unwrap_or_default(),
                        bbox_count: item.get("bbox_count")
                            .and_then(|v| v.as_m().ok())
                            .map(|m| m.iter().filter_map(|(k,v)| v.as_n().ok().and_then(|n| n.parse::<u32>().ok()).map(|n| (k.clone(), n))).collect())
                            .unwrap_or_default(),
                        polygon_count: item.get("polygon_count")
                            .and_then(|v| v.as_m().ok())
                            .map(|m| m.iter().filter_map(|(k,v)| v.as_n().ok().and_then(|n| n.parse::<u32>().ok()).map(|n| (k.clone(), n))).collect())
                            .unwrap_or_default(),
                        uploaded_at: item.get("uploaded_at").and_then(|v| v.as_s().ok()).map(|s| s.to_string()).unwrap_or_default(),
                        media_type: item.get("media_type").and_then(|v| v.as_s().ok()).map(|s| s.to_string()).unwrap_or_else(|| "image".to_string()),
                        markup_rects: parse_markup_rects(item),
                        width: item.get("width").and_then(|v| v.as_n().ok()).and_then(|n| n.parse().ok()),
                        height: item.get("height").and_then(|v| v.as_n().ok()).and_then(|n| n.parse().ok()),
                    };
                    images.push(image);
                }
            }
        }

        match result.last_evaluated_key() {
            Some(next_key) => last_evaluated_key = Some(next_key.clone()),
            None => break,
        }
    }
    
    // Sort by order
    images.sort_by(|a, b| match (a.order, b.order) {
        (Some(a_order), Some(b_order)) => a_order.cmp(&b_order),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    });
    
    Ok(images)
}



/// List images for a specific task using DynamoDB FilterExpression.
/// Queries all IMAGE# items under the block PK but filters server-side by task_id,
/// so only matching items are returned over the wire.
pub async fn load_images_for_task(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    task_id: &str,
) -> Result<Vec<Image>, String> {
    let pk = format!("BLOCK#{}", block_id);
    let mut images = Vec::new();
    let mut last_evaluated_key: Option<HashMap<String, AttributeValue>> = None;

    loop {
        let mut query = client
            .query()
            .table_name(table_name)
            .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
            .filter_expression("task_id = :tid")
            .expression_attribute_values(":pk", AttributeValue::S(pk.clone()))
            .expression_attribute_values(":sk_prefix", AttributeValue::S("IMAGE#".to_string()))
            .expression_attribute_values(":tid", AttributeValue::S(task_id.to_string()));

        if let Some(start_key) = last_evaluated_key.clone() {
            query = query.set_exclusive_start_key(Some(start_key));
        }

        let result = query
            .send()
            .await
            .map_err(|e| format!("DynamoDB query error: {}", e))?;

        for item in result.items() {
            if let Some(sk) = item.get("SK").and_then(|v| v.as_s().ok()) {
                if let Some(image_id) = sk.strip_prefix("IMAGE#") {
                    let image = Image {
                        image_id: image_id.to_string(),
                        block_id: block_id.to_string(),
                        task_id: item.get("task_id").and_then(|v| v.as_s().ok()).map(|s| s.to_string()),
                        image_name: item.get("image_name").and_then(|v| v.as_s().ok()).map(|s| s.to_string()).unwrap_or_default(),
                        url: item.get("url").and_then(|v| v.as_s().ok()).map(|s| s.to_string()).unwrap_or_default(),
                        locked: item.get("locked").and_then(|v| v.as_bool().ok()).copied().unwrap_or(false),
                        order: item.get("order").and_then(|v| v.as_n().ok()).and_then(|n| n.parse().ok()),
                        annotation_count: item.get("annotation_count").and_then(|v| v.as_n().ok()).and_then(|n| n.parse().ok()).unwrap_or(0),
                        labels_count: item.get("labels_count")
                            .and_then(|v| v.as_m().ok())
                            .map(|m| m.iter().filter_map(|(k,v)| v.as_n().ok().and_then(|n| n.parse::<u32>().ok()).map(|n| (k.clone(), n))).collect())
                            .unwrap_or_default(),
                        bbox_count: item.get("bbox_count")
                            .and_then(|v| v.as_m().ok())
                            .map(|m| m.iter().filter_map(|(k,v)| v.as_n().ok().and_then(|n| n.parse::<u32>().ok()).map(|n| (k.clone(), n))).collect())
                            .unwrap_or_default(),
                        polygon_count: item.get("polygon_count")
                            .and_then(|v| v.as_m().ok())
                            .map(|m| m.iter().filter_map(|(k,v)| v.as_n().ok().and_then(|n| n.parse::<u32>().ok()).map(|n| (k.clone(), n))).collect())
                            .unwrap_or_default(),
                        uploaded_at: item.get("uploaded_at").and_then(|v| v.as_s().ok()).map(|s| s.to_string()).unwrap_or_default(),
                        media_type: item.get("media_type").and_then(|v| v.as_s().ok()).map(|s| s.to_string()).unwrap_or_else(|| "image".to_string()),
                        markup_rects: parse_markup_rects(item),
                        width: item.get("width").and_then(|v| v.as_n().ok()).and_then(|n| n.parse().ok()),
                        height: item.get("height").and_then(|v| v.as_n().ok()).and_then(|n| n.parse().ok()),
                    };
                    images.push(image);
                }
            }
        }

        match result.last_evaluated_key() {
            Some(next_key) => last_evaluated_key = Some(next_key.clone()),
            None => break,
        }
    }

    // Sort by order
    images.sort_by(|a, b| match (a.order, b.order) {
        (Some(a_order), Some(b_order)) => a_order.cmp(&b_order),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });

    Ok(images)
}

/// Create a new image in a block
pub async fn create_image(
    client: &DynamoClient,
    table_name: &str,
    project_id: &str,
    block_id: &str,
    payload: CreateImagePayload,
) -> Result<Image, String> {
    let image_id = payload.image_id.clone();
    let media_type = payload.media_type.clone().unwrap_or_else(|| "image".to_string());
    let markup_rects = payload.markup_rects.clone().unwrap_or_default();
    let now = chrono::Utc::now().to_rfc3339();
    let pk = format!("BLOCK#{}", block_id);
    let sk = format!("IMAGE#{}", image_id);

    let mut builder = client
        .put_item()
        .table_name(table_name)
        .item("PK", AttributeValue::S(pk.clone()))
        .item("SK", AttributeValue::S(sk.clone()))
        .item("url", AttributeValue::S(payload.url.clone()))
        .item("locked", AttributeValue::Bool(false))
        .item("annotation_count", AttributeValue::N(0.to_string()))
        .item("labels_count", AttributeValue::M(HashMap::new()))
        .item("bbox_count", AttributeValue::M(HashMap::new()))
        .item("polygon_count", AttributeValue::M(HashMap::new()))
        .item("media_type", AttributeValue::S(media_type.clone()))
        .item("uploaded_at", AttributeValue::S(now.clone()));

    builder = builder.item("image_name", AttributeValue::S(payload.image_name.clone()));

    
    // Since there is conditional logic for task, we need to use builder    
    if let Some(task_id) = &payload.task_id {
        builder = builder.item("task_id", AttributeValue::S(task_id.clone()));
    }    

    // Since there is conditional logic, we need to use builder    
    if let Some(order) = payload.order {
        builder = builder.item("order", AttributeValue::N(order.to_string()));
    }

    if let Some(w) = payload.width {
        builder = builder.item("width", AttributeValue::N(w.to_string()));
    }
    if let Some(h) = payload.height {
        builder = builder.item("height", AttributeValue::N(h.to_string()));
    }
    if payload.markup_rects.is_some() {
        builder = builder.item(
            "markup_rects",
            AttributeValue::S(
                serde_json::to_string(&markup_rects)
                    .map_err(|e| format!("Failed to serialize markup_rects: {}", e))?,
            ),
        );
    }

    builder.send().await.map_err(|e| format!("DynamoDB put_item error: {}", e))?;

    // Increment BLOCK image count
    client
        .update_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(format!("PROJECT#{}", project_id)))
        .key("SK", AttributeValue::S(format!("BLOCK#{}", block_id)))
        .update_expression("SET image_count = image_count + :one")
        .expression_attribute_values(":one", AttributeValue::N("1".to_string()))
        .send()
        .await
        .map_err(|e| format!("DynamoDB update_item error: {}", e))?;

    // Increment TASK image_count if task exists
    if let Some(task_id) = &payload.task_id {
        client
            .update_item()
            .table_name(table_name)
            .key("PK", AttributeValue::S(format!("BLOCK#{}", block_id)))
            .key("SK", AttributeValue::S(format!("TASK#{}", task_id)))
            .update_expression("SET image_count = image_count + :one")
            .expression_attribute_values(":one", AttributeValue::N("1".to_string()))
            .send()
            .await
            .map_err(|e| format!("DynamoDB update_item error: {}", e))?;

    
        // Increment BLOCK approved_image_count if task exists
        let task = crate::tasks::service::get_task(client, table_name, block_id, task_id).await?;
        let task_state = task.task_state;
        let task_image_count = task.image_count;

        if task_image_count > 0 && task_state == crate::tasks::model::TaskState::Approved {
            client
                .update_item()
                .table_name(table_name)
                .key("PK", AttributeValue::S(format!("PROJECT#{}", project_id)))
                .key("SK", AttributeValue::S(format!("BLOCK#{}", block_id)))
                .update_expression("SET approved_image_count = approved_image_count + :one")
                .expression_attribute_values(":one", AttributeValue::N("1".to_string()))
                .send()
                .await
                .map_err(|e| format!("DynamoDB update_item error: {}", e))?;
        }
        

    }


    Ok(Image {
        image_id,
        block_id: block_id.to_string(),
        task_id: payload.task_id,
        image_name: payload.image_name.clone(),
        url: payload.url,
        locked: false,
        order: payload.order,
        annotation_count:0,
        labels_count:HashMap::new(),
        bbox_count:HashMap::new(),
        polygon_count:HashMap::new(),
        uploaded_at: now,
        media_type,
        markup_rects,
        width: payload.width,
        height: payload.height,
    })
}

/// Create image for a specific task (convenience function)
pub async fn create_image_for_task(
    client: &DynamoClient,
    table_name: &str,
    project_id: &str,
    block_id: &str,
    task_id: &str,
    image_id: String,
    image_name: String,
    url: String,
    order: Option<i32>,
    width: Option<u32>,
    height: Option<u32>,
) -> Result<Image, String> {
    let payload = CreateImagePayload {
        image_id,
        image_name,
        url,
        task_id: Some(task_id.to_string()),
        order,
        media_type: None,
        markup_rects: None,
        width,
        height,
    };
    
    create_image(client, table_name, project_id, block_id, payload).await
}

/// Get a specific image
pub async fn get_image(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    image_id: &str,
) -> Result<Image, String> {
    let pk = format!("BLOCK#{}", block_id);
    let sk = format!("IMAGE#{}", image_id);

    let result = client
        .get_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(pk))
        .key("SK", AttributeValue::S(sk))
        .send()
        .await
        .map_err(|e| format!("DynamoDB get_item error: {}", e))?;

    if let Some(item) = result.item() {
        Ok(Image {
            image_id: image_id.to_string(),
            block_id: block_id.to_string(),
            task_id: item.get("task_id").and_then(|v| v.as_s().ok()).map(|s| s.to_string()),
            image_name: item.get("image_name").and_then(|v| v.as_s().ok()).map(|s| s.to_string()).unwrap_or_default(),
            url: item.get("url").and_then(|v| v.as_s().ok()).map(|s| s.to_string()).unwrap_or_default(),
            locked: item.get("locked").and_then(|v| v.as_bool().ok()).copied().unwrap_or(false),
            order: item.get("order").and_then(|v| v.as_n().ok()).and_then(|n| n.parse().ok()),
            annotation_count: item.get("annotation_count").and_then(|v| v.as_n().ok()).and_then(|n| n.parse().ok()).unwrap_or(0),
            labels_count: item.get("labels_count").and_then(|v| v.as_m().ok())
                          .map(|m| 
                                m.iter()
                                .filter_map(|(k,v)|
                                        v.as_n().ok()
                                        .and_then(|n| n.parse::<u32>().ok()
                                        )
                                .map(|n| (k.clone(), n)))
                                .collect()
                            ).unwrap_or_default(),
            bbox_count: item.get("bbox_count")
                        .and_then(|v| v.as_m().ok())
                        .map(|m| m.iter().filter_map(|(k,v)| v.as_n().ok().and_then(|n| n.parse::<u32>().ok()).map(|n| (k.clone(), n))).collect())
                        .unwrap_or_default(),             
            polygon_count: item.get("polygon_count")
                        .and_then(|v| v.as_m().ok())
                        .map(|m| m.iter().filter_map(|(k,v)| v.as_n().ok().and_then(|n| n.parse::<u32>().ok()).map(|n| (k.clone(), n))).collect())
                        .unwrap_or_default(),
            uploaded_at: item.get("uploaded_at").and_then(|v| v.as_s().ok()).map(|s| s.to_string()).unwrap_or_default(),
            media_type: item.get("media_type").and_then(|v| v.as_s().ok()).map(|s| s.to_string()).unwrap_or_else(|| "image".to_string()),
            markup_rects: parse_markup_rects(item),
            width: item.get("width").and_then(|v| v.as_n().ok()).and_then(|n| n.parse().ok()),
            height: item.get("height").and_then(|v| v.as_n().ok()).and_then(|n| n.parse().ok()),
        })
    } else {
        Err("Image not found".to_string())
    }
}

/// Update an image
pub async fn update_image(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    image_id: &str,
    payload: UpdateImagePayload,
) -> Result<Image, String> {
    let pk = format!("BLOCK#{}", block_id);
    let sk = format!("IMAGE#{}", image_id);

    let mut update_expr = vec![];
    let mut expr_names = HashMap::new();
    let mut expr_values = HashMap::new();

    if let Some(locked) = payload.locked {
        update_expr.push("#locked = :locked");
        expr_names.insert("#locked".to_string(), "locked".to_string());
        expr_values.insert(":locked".to_string(), AttributeValue::Bool(locked));
    }

    if let Some(order) = payload.order {
        update_expr.push("#order = :order");
        expr_names.insert("#order".to_string(), "order".to_string());
        expr_values.insert(":order".to_string(), AttributeValue::N(order.to_string()));
    }
    if let Some(media_type) = payload.media_type {
        update_expr.push("#media_type = :media_type");
        expr_names.insert("#media_type".to_string(), "media_type".to_string());
        expr_values.insert(":media_type".to_string(), AttributeValue::S(media_type));
    }
    if let Some(markup_rects) = payload.markup_rects {
        let serialized = serde_json::to_string(&markup_rects)
            .map_err(|e| format!("Failed to serialize markup_rects: {}", e))?;
        update_expr.push("#markup_rects = :markup_rects");
        expr_names.insert("#markup_rects".to_string(), "markup_rects".to_string());
        expr_values.insert(":markup_rects".to_string(), AttributeValue::S(serialized));
    }

    if !update_expr.is_empty() {
        let update_expression = format!("SET {}", update_expr.join(", "));

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

    get_image(client, table_name, block_id, image_id).await
}

/// Result of deleting an image, with stats for the FE.
#[derive(serde::Serialize)]
pub struct DeleteImageResult {
    pub annotations_deleted: usize,
    pub threads_deleted: usize,
}

/// Delete an image using bulk operations for speed.
pub async fn delete_image(
    client: &DynamoClient,
    table_name: &str,
    project_id: &str,
    block_id: &str,
    image_id: &str,
) -> Result<DeleteImageResult, String> {
    let pk = format!("BLOCK#{}", block_id);
    let sk = format!("IMAGE#{}", image_id);
    let image = get_image(client, table_name, block_id, image_id).await?;

    // Decrement BLOCK image count
    client
        .update_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(format!("PROJECT#{}", project_id)))
        .key("SK", AttributeValue::S(format!("BLOCK#{}", block_id)))
        .update_expression("SET image_count = image_count - :one")
        .expression_attribute_values(":one", AttributeValue::N("1".to_string()))
        .send()
        .await
        .map_err(|e| format!("DynamoDB update_item error: {}", e))?;

     // Get the task id via image
     if let Some(task_id) = image.task_id {
        // Decrement TASKs - image_count
        client
            .update_item()
            .table_name(table_name)
            .key("PK", AttributeValue::S(format!("BLOCK#{}", block_id)))
            .key("SK", AttributeValue::S(format!("TASK#{}", task_id)))
            .update_expression("SET image_count = image_count - :one")
            .expression_attribute_values(":one", AttributeValue::N("1".to_string()))
            .send()
            .await
            .map_err(|e| format!("DynamoDB update_item error: {}", e))?;
        
        let task = crate::tasks::service::get_task(client,table_name,block_id, &task_id).await?;
        if task.task_state == crate::tasks::model::TaskState::Approved {
            client
                .update_item()
                .table_name(table_name)
                .key("PK", AttributeValue::S(format!("PROJECT#{}", project_id)))
                .key("SK", AttributeValue::S(format!("BLOCK#{}", block_id)))
                .update_expression("SET approved_image_count = approved_image_count - :one")
                .expression_attribute_values(":one", AttributeValue::N("1".to_string()))
                .send()
                .await
                .map_err(|e| format!("DynamoDB update_item error: {}", e))?;
        }
     }

    // Bulk-delete annotations (BatchWriteItem, decrement block count once)
    let annotations_deleted = crate::drawing::service::bulk_delete_annotations_for_image(
        client, table_name, block_id, image_id,
    ).await?;
    if annotations_deleted > 0 && !project_id.trim().is_empty() {
        decrement_project_block_annotation_count_safely(
            client,
            table_name,
            project_id,
            block_id,
            annotations_deleted as u64,
        )
        .await?;
    }

    // Bulk-delete comment threads + comments
    let threads_deleted = crate::comments::service::delete_threads_for_parent(
        client, table_name, image_id,
    ).await?;

    // Delete the image record itself
    client
        .delete_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(pk))
        .key("SK", AttributeValue::S(sk))
        .send()
        .await
        .map_err(|e| format!("DynamoDB delete_item error: {}", e))?;

    Ok(DeleteImageResult { annotations_deleted, threads_deleted })
}
async fn decrement_project_block_annotation_count_safely(
    client: &DynamoClient,
    table_name: &str,
    project_id: &str,
    block_id: &str,
    delta: u64,
) -> Result<(), String> {
    if delta == 0 {
        return Ok(());
    }
    let delta_str = delta.to_string();
    let decrement = client
        .update_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(format!("PROJECT#{}", project_id)))
        .key("SK", AttributeValue::S(format!("BLOCK#{}", block_id)))
        .update_expression("SET annotation_count = annotation_count - :delta, block_updated_at = :updated_at")
        .condition_expression("annotation_count >= :delta")
        .expression_attribute_values(":delta", AttributeValue::N(delta_str.clone()))
        .expression_attribute_values(":updated_at", AttributeValue::S(chrono::Utc::now().to_rfc3339()))
        .send()
        .await;
    match decrement {
        Ok(_) => Ok(()),
        Err(err) => {
            let err_str = err.to_string();
            if !err_str.contains("ConditionalCheckFailed") {
                return Err(format!("DynamoDB update_item error: {}", err_str));
            }
            let clamp_to_zero = client
                .update_item()
                .table_name(table_name)
                .key("PK", AttributeValue::S(format!("PROJECT#{}", project_id)))
                .key("SK", AttributeValue::S(format!("BLOCK#{}", block_id)))
                .update_expression("SET annotation_count = :zero, block_updated_at = :updated_at")
                .condition_expression("attribute_exists(annotation_count) AND annotation_count < :delta")
                .expression_attribute_values(":zero", AttributeValue::N("0".to_string()))
                .expression_attribute_values(":delta", AttributeValue::N(delta_str))
                .expression_attribute_values(":updated_at", AttributeValue::S(chrono::Utc::now().to_rfc3339()))
                .send()
                .await;
            match clamp_to_zero {
                Ok(_) => Ok(()),
                Err(clamp_err) => {
                    let clamp_err_str = clamp_err.to_string();
                    if clamp_err_str.contains("ConditionalCheckFailed") {
                        Ok(())
                    } else {
                        Err(format!("DynamoDB update_item clamp error: {}", clamp_err_str))
                    }
                }
            }
        }
    }
}


