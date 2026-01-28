use doxle_atoms::blocks::model::{Block, CreateBlockPayload, UpdateBlockPayload};
use aws_sdk_dynamodb::Client as DynamoClient;
use aws_sdk_s3::Client as S3Client;
use lambda_http::{http::StatusCode, Body, Error, Response};
use aws_sdk_dynamodb::types::{AttributeValue, WriteRequest, DeleteRequest};
use std::collections::HashMap;
use tokio::time::{sleep, Duration};
use crate::types::AnnotationBlock;
use crate::labels::fetch_labels_for_block;

/// Create a new block:
/// PK = "BLOCK"
/// SK = "BLOCK#{block_id}"
pub async fn create_block(
    client: &DynamoClient,
    table_name: &str,
    body: &[u8],
) -> Result<Response<Body>, Error> {
    let req: CreateBlockPayload = serde_json::from_slice(body)?;

    let block_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let pk = "BLOCK".to_string();
    let sk = format!("BLOCK#{}", block_id);
    
    let mut builder = client
        .put_item()
        .table_name(table_name)
        .item("PK", AttributeValue::S(pk.clone()))
        .item("SK", AttributeValue::S(sk.clone()))
        .item("block_name", AttributeValue::S(req.block_name.clone()))
        .item("block_type", AttributeValue::S(req.block_type.clone()))
        .item("block_state", AttributeValue::S("draft".to_string()))
        .item("block_locked", AttributeValue::Bool(false))
        .item("image_count", AttributeValue::N(0.to_string()))
        .item("approved_image_count", AttributeValue::N(0.to_string()))
        .item("annotation_count", AttributeValue::N(0.to_string()))
        .item("block_created_at", AttributeValue::S(now.clone()));

    if let Some(comp) = &req.block_company {
        builder = builder.item("block_company", AttributeValue::S(comp.clone()));
    }
        
    builder.send().await?;

    let block = Block {
        block_id,
        block_name: req.block_name,
        block_type: req.block_type,
        block_company: req.block_company,
        block_state: "draft".to_string(),
        block_locked: false,
        image_count: 0,
        approved_image_count: 0,
        annotation_count: 0,
        block_created_at: now,
    };

    let response = AnnotationBlock {
        block,
        labels: Vec::new(),
    };

    Ok(Response::builder()
        .status(StatusCode::CREATED)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(serde_json::to_string(&response)?.into())
        .map_err(Box::new)?)
}

/// Get a specific block
pub async fn get_block(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
) -> Result<Response<Body>, Error> {
    let pk = "BLOCK".to_string();
    let sk = format!("BLOCK#{}", block_id);

    let result = client
        .get_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(pk))
        .key("SK", AttributeValue::S(sk))
        .send()
        .await?;

    if let Some(item) = result.item() {
        let labels = fetch_labels_for_block(client, table_name, block_id).await?;
        
        let block = Block {
            block_id: block_id.to_string(),
            block_name: item
                .get("block_name")
                .and_then(|v| v.as_s().ok())
                .map(|s| s.to_string())
                .unwrap_or_default(),
            block_type: item
                .get("block_type")
                .and_then(|v| v.as_s().ok())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "annotation".to_string()),
            block_company: item
                .get("block_company")
                .and_then(|v| v.as_s().ok())
                .map(|s| s.to_string()),
            block_state: item
                .get("block_state")
                .and_then(|v| v.as_s().ok())
                .map(|s| s.to_string())
                .unwrap_or_default(),
            block_locked: item
                .get("block_locked")
                .and_then(|v| v.as_bool().ok())
                .copied()
                .unwrap_or(false),
            image_count: item
                .get("image_count")
                .and_then(|v| v.as_n().ok())
                .and_then(|n| n.parse().ok())
                .unwrap_or(0),
            approved_image_count: item
                .get("approved_image_count")
                .and_then(|v| v.as_n().ok())
                .and_then(|n| n.parse().ok())
                .unwrap_or(0),
            annotation_count: item
               .get("annotation_count")
                .and_then(|v| v.as_n().ok())
                .and_then(|n| n.parse().ok())
                .unwrap_or(0),
            block_created_at: item
                .get("block_created_at")
                .and_then(|v| v.as_s().ok())
                .map(|s| s.to_string())
                .unwrap_or_default(),
        };

        let response = AnnotationBlock { block, labels };

        Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::to_string(&response)?.into())
            .map_err(Box::new)?)
    } else {
        Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(
                serde_json::json!({"error": "Block not found"})
                    .to_string()
                    .into(),
            )
            .map_err(Box::new)?)
    }
}

/// List all blocks (annotation blocks)
pub async fn list_blocks(
    client: &DynamoClient,
    table_name: &str,
) -> Result<Response<Body>, Error> {
    let pk = "BLOCK".to_string();

    let result = match client
        .query()
        .table_name(table_name)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(pk))
        .expression_attribute_values(
            ":sk_prefix",
            AttributeValue::S("BLOCK#".to_string()),
        )
        .send()
        .await
    {
        Ok(res) => res,
        Err(e) => {
            tracing::error!(
                "DynamoDB list_blocks query failed for table {}: {:?}",
                table_name,
                e
            );
            return Err(Box::new(e));
        }
    };

    let mut blocks = Vec::new();

    for item in result.items() {
        if let Some(sk) = item.get("SK").and_then(|v| v.as_s().ok()) {
            if let Some(block_id) = sk.strip_prefix("BLOCK#") {
                let labels = match fetch_labels_for_block(client, table_name, block_id).await {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::error!("Failed to load labels for block {}: {:?}", block_id, e);
                        vec![]
                    }
                };
                
                let block = Block {
                    block_id: block_id.to_string(),
                    block_name: item
                        .get("block_name")
                        .and_then(|v| v.as_s().ok())
                        .map(|s| s.to_string())
                        .unwrap_or_default(),
                    block_type: item
                        .get("block_type")
                        .and_then(|v| v.as_s().ok())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "annotation".to_string()),
                    block_company: item
                        .get("block_company")
                        .and_then(|v| v.as_s().ok())
                        .map(|s| s.to_string()),
                    block_state: item
                        .get("block_state")
                        .and_then(|v| v.as_s().ok())
                        .map(|s| s.to_string())
                        .unwrap_or_default(),
                    block_locked: item
                        .get("block_locked")
                        .and_then(|v| v.as_bool().ok())
                        .copied()
                        .unwrap_or(false),
                    image_count: item
                        .get("image_count")
                        .and_then(|v| v.as_n().ok())
                        .and_then(|n| n.parse().ok())
                        .unwrap_or(0),
                    approved_image_count: item
                        .get("approved_image_count")
                        .and_then(|v| v.as_n().ok())
                        .and_then(|n| n.parse().ok())
                        .unwrap_or(0),
                    annotation_count: item
                       .get("annotation_count")
                        .and_then(|v| v.as_n().ok())
                        .and_then(|n| n.parse().ok())
                        .unwrap_or(0),
                    block_created_at: item
                        .get("block_created_at")
                        .and_then(|v| v.as_s().ok())
                        .map(|s| s.to_string())
                        .unwrap_or_default(),
                };
                blocks.push(AnnotationBlock { block, labels });
            }
        }
    }

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(serde_json::to_string(&blocks)?.into())
        .map_err(Box::new)?)
}

/// Update a block
pub async fn update_block(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    body: &[u8],
) -> Result<Response<Body>, Error> {
    let req: UpdateBlockPayload = serde_json::from_slice(body)?;
    let pk = "BLOCK".to_string();
    let sk = format!("BLOCK#{}", block_id);

    let mut update_expr = vec![];
    let mut expr_names = std::collections::HashMap::new();
    let mut expr_values = std::collections::HashMap::new();

    if let Some(block_name) = req.block_name {
        update_expr.push("#block_name = :block_name");
        expr_names.insert("#block_name".to_string(), "block_name".to_string());
        expr_values.insert(
            ":block_name".to_string(),
            aws_sdk_dynamodb::types::AttributeValue::S(block_name),
        );
    }

    if let Some(block_state) = req.block_state {
        update_expr.push("#block_state = :block_state");
        expr_names.insert("#block_state".to_string(), "block_state".to_string());
        expr_values.insert(
            ":block_state".to_string(),
            aws_sdk_dynamodb::types::AttributeValue::S(block_state),
        );
    }

    if let Some(block_locked) = req.block_locked {
        update_expr.push("#block_locked = :block_locked");
        expr_names.insert("#block_locked".to_string(), "block_locked".to_string());
        expr_values.insert(
            ":block_locked".to_string(),
            aws_sdk_dynamodb::types::AttributeValue::Bool(block_locked),
        );
    }

    if !update_expr.is_empty() {
        let mut builder = client
            .update_item()
            .table_name(table_name)
            .key("PK", aws_sdk_dynamodb::types::AttributeValue::S(pk))
            .key("SK", aws_sdk_dynamodb::types::AttributeValue::S(sk))
            .update_expression(format!("SET {}", update_expr.join(", ")));

        for (k, v) in expr_names {
            builder = builder.expression_attribute_names(k, v);
        }

        for (k, v) in expr_values {
            builder = builder.expression_attribute_values(k, v);
        }

        builder.send().await?;
    }

    get_block(client, table_name, block_id).await
}

/// Delete a block and associated records (images, annotations, links)
pub async fn delete_block(
    client: &DynamoClient,
    s3_client: &S3Client,
    table_name: &str,
    block_id: &str,
) -> Result<Response<Body>, Error> {
   let block_pk = format!("BLOCK#{}", block_id);
   let mut delete_keys:Vec<HashMap<String, AttributeValue>> = vec![]; //collection to delete

    // STEP 1: Delete tasks and their images
    delete_tasks(&client, table_name, &block_pk, &mut delete_keys).await?;

    // STEP 2: Delete labels
    delete_labels(&client, table_name, &block_pk, &mut delete_keys).await?;

    // STEP 3: Delete block images and their annotations
    delete_block_images(&client, table_name, &block_pk, &mut delete_keys).await?;

    // STEP 4: Delete the block itself
    delete_block_record(&block_pk, block_id, &mut delete_keys);

    // STEP 5: Batch delete all records
    batch_delete_items(&client, table_name, &delete_keys).await?;

    // STEP 6: Delete S3 files
    delete_s3_prefix(s3_client, block_id).await.ok();

    Ok(Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header("Access-Control-Allow-Origin", "*")
        .body(Body::Empty)
        .map_err(Box::new)?)
}

// PRIVATE FUNCTIONS 

/// Delete all tasks for a block and their associated images
async fn delete_tasks(
    client: &DynamoClient,
    table_name: &str,
    block_pk: &str,
    delete_keys: &mut Vec<HashMap<String, AttributeValue>>,
) -> Result<(), Error> {
    let tasks_result = client
        .query()
        .table_name(table_name)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(block_pk.to_string()))
        .expression_attribute_values(":sk_prefix", AttributeValue::S("TASK#".to_string()))
        .send()
        .await?;

    let mut task_pks: Vec<String> = Vec::new();
    for item in tasks_result.items() {
        if let Some(sk) = item.get("SK").and_then(|v| v.as_s().ok()) {
            if sk.starts_with("TASK#") {
                task_pks.push(sk.to_string());
                add_delete_key(delete_keys, block_pk, sk);
            }
        }
    }

    for task_pk in &task_pks {
        delete_task_images(client, table_name, task_pk, delete_keys).await?;
        add_delete_key(delete_keys, task_pk, task_pk);
    }

    Ok(())
}

/// Delete all images for a task
async fn delete_task_images(
    client: &DynamoClient,
    table_name: &str,
    task_pk: &str,
    delete_keys: &mut Vec<HashMap<String, AttributeValue>>,
) -> Result<(), Error> {
    let task_images_result = client
        .query()
        .table_name(table_name)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(task_pk.to_string()))
        .expression_attribute_values(":sk_prefix", AttributeValue::S("IMAGE#".to_string()))
        .send()
        .await?;

    for item in task_images_result.items() {
        if let Some(sk) = item.get("SK").and_then(|v| v.as_s().ok()) {
            add_delete_key(delete_keys, task_pk, sk);
        }
    }

    Ok(())
}

/// Delete all labels for a block
async fn delete_labels(
    client: &DynamoClient,
    table_name: &str,
    block_pk: &str,
    delete_keys: &mut Vec<HashMap<String, AttributeValue>>,
) -> Result<(), Error> {
    let labels_result = client
        .query()
        .table_name(table_name)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(block_pk.to_string()))
        .expression_attribute_values(":sk_prefix", AttributeValue::S("LABEL#".to_string()))
        .send()
        .await?;

    for item in labels_result.items() {
        if let Some(sk) = item.get("SK").and_then(|v| v.as_s().ok()) {
            add_delete_key(delete_keys, block_pk, sk);
        }
    }

    Ok(())
}

// Delete all images for a block and their annotations
async fn delete_block_images(
    client: &DynamoClient,
    table_name: &str,
    block_pk: &str,
    delete_keys: &mut Vec<HashMap<String, AttributeValue>>,
) -> Result<(), Error> {
    let images_result = client
        .query()
        .table_name(table_name)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(block_pk.to_string()))
        .expression_attribute_values(":sk_prefix", AttributeValue::S("IMAGE#".to_string()))
        .send()
        .await?;

    let mut image_pks: Vec<String> = Vec::new();
    for item in images_result.items() {
        if let Some(sk) = item.get("SK").and_then(|v| v.as_s().ok()) {
            if sk.starts_with("IMAGE#") {
                image_pks.push(sk.to_string());
                add_delete_key(delete_keys, block_pk, sk);
            }
        }
    }

    for image_pk in &image_pks {
        delete_annotations(client, table_name, image_pk, delete_keys).await?;
        add_delete_key(delete_keys, image_pk, image_pk);
    }

    Ok(())
}

/// Delete all annotations for an image
async fn delete_annotations(
    client:&DynamoClient,
    table_name:&str,
    image_pk:&str,
    delete_keys: &mut Vec<HashMap<String, AttributeValue>>,
    )->Result<(),Error>{
    let annotations_result = client
            .query()
            .table_name(table_name)
            .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
            .expression_attribute_values(":pk", AttributeValue::S(image_pk.to_string()))
            .expression_attribute_values(":sk_prefix", AttributeValue::S("ANNOTATION#".to_string()))
            .send()
            .await?;

    for item in annotations_result.items() {
        if let Some(sk) = item.get("SK").and_then(|v| v.as_s().ok()) {
            add_delete_key(delete_keys, image_pk, sk);
        }
    }

    Ok(())
}

/// Add the block self-record to delete keys
fn delete_block_record(
    _block_pk: &str,
    block_id: &str,
    delete_keys: &mut Vec<HashMap<String, AttributeValue>>,
) {
    let mut key = HashMap::new();
    key.insert("PK".to_string(), AttributeValue::S("BLOCK".to_string()));
    key.insert("SK".to_string(), AttributeValue::S(format!("BLOCK#{}", block_id)));
    delete_keys.push(key);
}

/// Batch delete items from DynamoDB (25 items per request with retry logic)
async fn batch_delete_items(
    client: &DynamoClient,
    table_name: &str,
    delete_keys: &[HashMap<String, AttributeValue>],
) -> Result<(), Error> {
    for chunk in delete_keys.chunks(25) {
        let write_reqs: Vec<_> = chunk
            .iter()
            .map(|k| {
                WriteRequest::builder()
                    .delete_request(
                        DeleteRequest::builder()
                            .set_key(Some(k.clone()))
                            .build()
                            .unwrap(),
                    )
                    .build()
            })
            .collect();

        let mut unprocessed = Some(write_reqs);
        let mut attempts = 0;
        while let Some(reqs) = unprocessed {
            attempts += 1;
            let result = client
                .batch_write_item()
                .request_items(table_name, reqs)
                .send()
                .await?;

            unprocessed = result
                .unprocessed_items()
                .and_then(|m| m.get(table_name))
                .map(|v| v.clone());

            if unprocessed.is_some() && attempts < 5 {
                sleep(Duration::from_millis(100 * attempts)).await;
            } else {
                break;
            }
        }
    }

    Ok(())
}

/// Helper to add a delete key to the list
fn add_delete_key(
    delete_keys: &mut Vec<HashMap<String, AttributeValue>>,
    pk: &str,
    sk: &str,
) {
    let mut key = HashMap::new();
    key.insert("PK".to_string(), AttributeValue::S(pk.to_string()));
    key.insert("SK".to_string(), AttributeValue::S(sk.to_string()));
    delete_keys.push(key);
}

/// S3 helper: del everything under block/{block_id}/
async fn delete_s3_prefix(
    s3_client: &S3Client,
    block_id: &str,
) -> Result<(), Error> {
    let bucket_name = std::env::var("S3_BUCKET_NAME").unwrap_or_else(|_| "doxle-app".to_string());
    // Match the upload prefix structure: annotations/blocks/{block_id}/
    let prefix = format!("annotations/blocks/{}/", block_id);

    let mut continuation: Option<String> = None;
    loop {
        let mut req = s3_client
            .list_objects_v2()
            .bucket(&bucket_name)
            .prefix(&prefix);
        if let Some(token) = continuation.as_ref() {
            req = req.continuation_token(token);
        }
        let resp = req.send().await.map_err(|e| {
            tracing::error!("S3 list_objects_v2 failed for prefix {}: {}", prefix, e);
            format!("S3 list failed: {}", e)
        })?;

        let contents = resp.contents();
        let objects: Vec<_> = contents
            .iter()
            .filter_map(|o| o.key())
            .filter_map(|k| {
                aws_sdk_s3::types::ObjectIdentifier::builder()
                    .key(k)
                    .build()
                    .ok()
            })
            .collect();
        if objects.is_empty() {
            if resp.is_truncated().unwrap_or(false) {
                continuation = resp.next_continuation_token().map(|s| s.to_string());
                continue;
            } else {
                break;
            }
        }

        let delete_payload = aws_sdk_s3::types::Delete::builder()
            .set_objects(Some(objects))
            .build()
            .map_err(|e| format!("Failed to build S3 delete payload: {:?}", e))?;

        let _ = s3_client
            .delete_objects()
            .bucket(&bucket_name)
            .delete(delete_payload)
            .send()
            .await;

        if resp.is_truncated().unwrap_or(false) {
            continuation = resp.next_continuation_token().map(|s| s.to_string());
        } else {
            break;
        }
    }
    Ok(())
}
