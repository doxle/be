
use aws_sdk_dynamodb::Client as DynamoClient;
use aws_sdk_dynamodb::types::AttributeValue;
use super::model::{Image, CreateImagePayload, UpdateImagePayload};
use std::collections::HashMap;
use std::cmp::Ordering;

/// Load all images for a block (pure domain logic, no HTTP)
/// Used by blocks layer to perform joins with tasks
pub async fn load_images_for_block(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
) -> Result<Vec<Image>, String> {
    let pk = format!("BLOCK#{}", block_id);
    
    let result = client
        .query()
        .table_name(table_name)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(pk))
        .expression_attribute_values(":sk_prefix", AttributeValue::S("IMAGE#".to_string()))
        .send()
        .await
        .map_err(|e| format!("DynamoDB query error: {}", e))?;
    
    let mut images = Vec::new();
    for item in result.items() {
        if let Some(sk) = item.get("SK").and_then(|v| v.as_s().ok()) {
            if let Some(image_id) = sk.strip_prefix("IMAGE#") {
                let image = Image {
                    image_id: image_id.to_string(),
                    block_id: block_id.to_string(),
                    task_id: item.get("task_id").and_then(|v| v.as_s().ok()).map(|s| s.to_string()),
                    url: item.get("url").and_then(|v| v.as_s().ok()).map(|s| s.to_string()).unwrap_or_default(),
                    locked: item.get("locked").and_then(|v| v.as_bool().ok()).copied().unwrap_or(false),
                    order: item.get("order").and_then(|v| v.as_n().ok()).and_then(|n| n.parse().ok()),
                    annotation_count: item.get("annotation_count").and_then(|v| v.as_n().ok()).and_then(|n| n.parse().ok()).unwrap_or(0),
                    uploaded_at: item.get("uploaded_at").and_then(|v| v.as_s().ok()).map(|s| s.to_string()).unwrap_or_default(),
                };
                images.push(image);
            }
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



/// List images for a specific task
pub async fn load_images_for_task(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    task_id: &str,
) -> Result<Vec<Image>, String> {
    let all_images = load_images_for_block(client, table_name, block_id).await?;
    
    let task_images: Vec<Image> = all_images
        .into_iter()
        .filter(|img| img.task_id.as_deref() == Some(task_id))
        .collect();
    
    Ok(task_images)
}

/// Create a new image in a block
pub async fn create_image(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    payload: CreateImagePayload,
) -> Result<Image, String> {
    let image_id = uuid::Uuid::new_v4().to_string();
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
        .item("uploaded_at", AttributeValue::S(now.clone()));

    
    // Since there is conditional logic for task, we need to use builder    
    if let Some(task_id) = &payload.task_id {
        builder = builder.item("task_id", AttributeValue::S(task_id.clone()));
    }    

    // Since there is conditional logic, we need to use builder    
    if let Some(order) = payload.order {
        builder = builder.item("order", AttributeValue::N(order.to_string()));
    }

    builder.send().await.map_err(|e| format!("DynamoDB put_item error: {}", e))?;

    // Increment BLOCK image count
    client
        .update_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S("BLOCK".to_string()))
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

        if task_image_count > 0 && task_state == "done" {
            client
                .update_item()
                .table_name(table_name)
                .key("PK", AttributeValue::S("BLOCK".to_string()))
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
        url: payload.url,
        locked: false,
        order: payload.order,
        annotation_count:0,
        uploaded_at: now,
    })
}

/// Create image for a specific task (convenience function)
pub async fn create_image_for_task(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    task_id: &str,
    url: String,
    order: Option<i32>,
) -> Result<Image, String> {
    let payload = CreateImagePayload {
        url,
        task_id: Some(task_id.to_string()),
        order,
    };
    
    create_image(client, table_name, block_id, payload).await
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
            url: item.get("url").and_then(|v| v.as_s().ok()).map(|s| s.to_string()).unwrap_or_default(),
            locked: item.get("locked").and_then(|v| v.as_bool().ok()).copied().unwrap_or(false),
            order: item.get("order").and_then(|v| v.as_n().ok()).and_then(|n| n.parse().ok()),
            annotation_count: item.get("annotation_count").and_then(|v| v.as_n().ok()).and_then(|n| n.parse().ok()).unwrap_or(0),
            uploaded_at: item.get("uploaded_at").and_then(|v| v.as_s().ok()).map(|s| s.to_string()).unwrap_or_default(),
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

/// Delete an image
pub async fn delete_image(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    image_id: &str,
) -> Result<(), String> {
    let pk = format!("BLOCK#{}", block_id);
    let sk = format!("IMAGE#{}", image_id);
    let image = get_image(client, table_name, block_id, image_id).await?;
    let _annotation_count = image.annotation_count;


    // Decrement BLOCK image count
    client
        .update_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S("BLOCK".to_string()))
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
        
         // Decrement BLOCKS - approved_image_count
        // Get the task via task id
        let task = crate::tasks::service::get_task(client,table_name,block_id, &task_id).await?;
        if task.task_state == "done" {
            // Decrement BLOCKS - approved_image_count
            client
                .update_item()
                .table_name(table_name)
                .key("PK", AttributeValue::S("BLOCK".to_string()))
                .key("SK", AttributeValue::S(format!("BLOCK#{}", block_id)))
                .update_expression("SET approved_image_count = approved_image_count - :one")
                .expression_attribute_values(":one", AttributeValue::N("1".to_string()))
                .send()
                .await
                .map_err(|e| format!("DynamoDB update_item error: {}", e))?;

        }
     }

     // // Decrement BLOCK annotation count
     // client
     //    .update_item()
     //    .table_name(table_name)
     //    .key("PK", AttributeValue::S("BLOCK".to_string()))
     //    .key("SK", AttributeValue::S(format!("BLOCK#{}", block_id)))
     //    .update_expression("SET annotation_count = annotation_count - :image_annotation_count")
     //    .expression_attribute_values(":image_annotation_count", AttributeValue::N(annotation_count.to_string()))
     //    .send()
     //    .await
     //    .map_err(|e| format!("DynamoDB update_item error: {}", e))?;
    

    // Delete orphaned annotations when an image is deleted
    let annotations = crate::drawing::service::list_annotations(client, table_name, image_id).await?;
    for annotation in annotations{
        crate::drawing::service::delete_annotation(client, table_name, block_id, image_id, annotation.annotation_id.as_str()).await?;
    }



    client
        .delete_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(pk))
        .key("SK", AttributeValue::S(sk))
        .send()
        .await
        .map_err(|e| format!("DynamoDB delete_item error: {}", e))?;

    Ok(())
}


