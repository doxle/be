use aws_sdk_dynamodb::Client as DynamoClient;
use aws_sdk_dynamodb::types::AttributeValue;
use super::model::{Annotation, Geometry, CreateAnnotationPayload, UpdateAnnotationPayload};

/// Create a new annotation
pub async fn create_annotation(
    client: &DynamoClient,
    table_name: &str,
    project_id: Option<&str>,
    block_id:&str,
    image_id: &str,
    user_id: &str,
    payload: CreateAnnotationPayload,
) -> Result<Annotation, String> {
    let annotation_id = payload.annotation_id;
    let now = chrono::Utc::now().to_rfc3339();
    
    let pk = format!("IMAGE#{}", image_id);
    let sk = format!("ANNOTATION#{}", annotation_id);
    
    let geometry_json = serde_json::to_string(&payload.geometry)
        .map_err(|e| format!("Failed to serialize geometry: {}", e))?;

    client
        .put_item()
        .table_name(table_name)
        .item("PK", AttributeValue::S(pk))
        .item("SK", AttributeValue::S(sk))
        .item("label_id", AttributeValue::S(payload.label_id.clone()))
        .item("label_name", AttributeValue::S(payload.label_name.clone()))
        .item("geometry", AttributeValue::S(geometry_json))
        .item("created_by", AttributeValue::S(user_id.to_string()))
        .item("created_at", AttributeValue::S(now.clone()))
        .send()
        .await
        .map_err(|e| format!("DynamoDB put_item error: {}", e))?;

    let geom_key = match &payload.geometry {
        Geometry::BBox { .. } => "bbox_count",
        Geometry::Polygon { .. } => "polygon_count",
    };

    // Increment LABEL - label_count.{label_name}
    client
        .update_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(format!("BLOCK#{}", block_id)))
        .key("SK", AttributeValue::S(format!("IMAGE#{}", image_id)))
        .update_expression(format!(
            "SET annotation_count = if_not_exists(annotation_count, :zero) + :one, labels_count.#ln = if_not_exists(labels_count.#ln, :zero) + :one, {}.#ln = if_not_exists({}.#ln, :zero) + :one",
              geom_key, geom_key
            ))
        .expression_attribute_names("#ln", &payload.label_name)
        .expression_attribute_values(":one", AttributeValue::N("1".to_string()))
        .expression_attribute_values(":zero", AttributeValue::N("0".to_string()))
        .send()
        .await
        .map_err(|e| format!("DynamoDB update_item error: {}", e))?;
    if let Some(project_id) = project_id.filter(|pid| !pid.trim().is_empty()) {
        client
            .update_item()
            .table_name(table_name)
            .key("PK", AttributeValue::S(format!("PROJECT#{}", project_id)))
            .key("SK", AttributeValue::S(format!("BLOCK#{}", block_id)))
            .update_expression("SET annotation_count = if_not_exists(annotation_count, :zero) + :one, block_updated_at = :updated_at")
            .expression_attribute_values(":one", AttributeValue::N("1".to_string()))
            .expression_attribute_values(":zero", AttributeValue::N("0".to_string()))
            .expression_attribute_values(":updated_at", AttributeValue::S(chrono::Utc::now().to_rfc3339()))
            .send()
            .await
            .map_err(|e| format!("DynamoDB update_item error: {}", e))?;
    }
    

    Ok(Annotation {
        annotation_id,
        image_id: image_id.to_string(),
        label_id: payload.label_id,
        geometry: payload.geometry,
        created_by: user_id.to_string(),
        created_at: now,
        updated_at: None,
    })
}

/// List annotations for an image
pub async fn list_annotations(
    client: &DynamoClient,
    table_name: &str,
    image_id: &str,
) -> Result<Vec<Annotation>, String> {
    let pk = format!("IMAGE#{}", image_id);
    let mut annotations = Vec::new();
    let mut last_evaluated_key: Option<std::collections::HashMap<String, AttributeValue>> = None;

    loop {
        let mut query = client
            .query()
            .table_name(table_name)
            .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
            .expression_attribute_values(":pk", AttributeValue::S(pk.clone()))
            .expression_attribute_values(":sk_prefix", AttributeValue::S("ANNOTATION#".to_string()));

        if let Some(start_key) = last_evaluated_key.clone() {
            query = query.set_exclusive_start_key(Some(start_key));
        }

        let result = query
            .send()
            .await
            .map_err(|e| format!("DynamoDB query error: {}", e))?;

        for item in result.items() {
            if let Some(sk) = item.get("SK").and_then(|v| v.as_s().ok()) {
                if let Some(annotation_id) = sk.strip_prefix("ANNOTATION#") {
                    let geometry_str = match item.get("geometry").and_then(|v| v.as_s().ok()) {
                        Some(s) => s,
                        None => {
                            let keys: Vec<String> = item.keys().cloned().collect();
                            eprintln!("🔴 CORRUPT annotation {} — missing geometry. Fields present: {:?}", annotation_id, keys);
                            return Err(format!("Corrupt annotation {}: missing geometry field. Fields: {:?}", annotation_id, keys));
                        }
                    };
                        
                    let geometry: Geometry = serde_json::from_str(geometry_str)
                        .map_err(|e| {
                            eprintln!("🔴 CORRUPT annotation {} — invalid geometry JSON: {}. Raw: {}", annotation_id, e, geometry_str);
                            format!("Corrupt annotation {}: invalid geometry: {}", annotation_id, e)
                        })?;
                        
                    annotations.push(Annotation {
                        annotation_id: annotation_id.to_string(),
                        image_id: image_id.to_string(),
                        label_id: item.get("label_id").and_then(|v| v.as_s().ok()).unwrap_or(&"default".to_string()).to_string(),
                        geometry,
                        created_by: item.get("created_by").and_then(|v| v.as_s().ok()).unwrap_or(&"".to_string()).to_string(),
                        created_at: item.get("created_at").and_then(|v| v.as_s().ok()).unwrap_or(&"".to_string()).to_string(),
                        updated_at: item.get("updated_at").and_then(|v| v.as_s().ok()).map(|s| s.to_string()),
                    });
                }
            }
        }

        match result.last_evaluated_key() {
            Some(next_key) => last_evaluated_key = Some(next_key.clone()),
            None => break,
        }
    }    
    
    Ok(annotations)
}

/// Update annotation label
pub async fn update_annotation(
    client:&DynamoClient,
    table_name:&str,
    block_id:&str,
    image_id:&str,
    annotation_id:&str,
    payload:UpdateAnnotationPayload
    ) -> Result <(), String> {
    if payload.label_id.is_none() && payload.geometry.is_none() {
        return Ok(());
    }

    let pk = format!("IMAGE#{}", image_id);
    let sk = format!("ANNOTATION#{}", annotation_id);

    let current = client
        .get_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(pk.clone()))
        .key("SK", AttributeValue::S(sk.clone()))
        .send()
        .await
        .map_err(|e| format!("DynamoDB get_item error: {}", e))?;

    let item = current.item().ok_or("Annotation not found")?;
    let old_label_id = item
        .get("label_id")
        .and_then(|v| v.as_s().ok())
        .map(|s| s.to_string())
        .ok_or("Missing label_id on annotation")?;
    let old_label_name = item
        .get("label_name")
        .and_then(|v| v.as_s().ok())
        .map(|s| s.to_string())
        .ok_or("Missing label_name on annotation")?;
    let old_geometry_raw = item
        .get("geometry")
        .and_then(|v| v.as_s().ok())
        .ok_or("Missing geometry on annotation")?;
    let old_geometry: Geometry = serde_json::from_str(old_geometry_raw)
        .map_err(|e| format!("Failed to parse existing geometry: {}", e))?;

    let new_label_id = payload
        .label_id
        .clone()
        .unwrap_or_else(|| old_label_id.clone());
    let new_label_name = if payload.label_id.is_some() {
        load_label_name_for_label_id(client, table_name, block_id, &new_label_id).await?
    } else {
        old_label_name.clone()
    };
    let new_geometry = payload.geometry.unwrap_or_else(|| old_geometry.clone());
    let new_geometry_raw = serde_json::to_string(&new_geometry)
        .map_err(|e| format!("Failed to serialize new geometry: {}", e))?;

    client
        .update_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(pk.clone()))
        .key("SK", AttributeValue::S(sk))
        .update_expression("SET label_id = :label_id, label_name = :label_name, geometry = :geometry, updated_at = :updated_at")
        .expression_attribute_values(":label_id", AttributeValue::S(new_label_id))
        .expression_attribute_values(":label_name", AttributeValue::S(new_label_name.clone()))
        .expression_attribute_values(":geometry", AttributeValue::S(new_geometry_raw))
        .expression_attribute_values(":updated_at", AttributeValue::S(chrono::Utc::now().to_rfc3339()))
        .send()
        .await
        .map_err(|e| format!("DynamoDB update error: {}", e))?;

    let old_geom_key = geometry_counter_key(&old_geometry);
    let new_geom_key = geometry_counter_key(&new_geometry);
    let counts_changed = old_label_name != new_label_name || old_geom_key != new_geom_key;
    if counts_changed {
        client
            .update_item()
            .table_name(table_name)
            .key("PK", AttributeValue::S(format!("BLOCK#{}", block_id)))
            .key("SK", AttributeValue::S(format!("IMAGE#{}", image_id)))
            .update_expression(format!(
                "SET labels_count.#ln = if_not_exists(labels_count.#ln, :zero) - :one, {}.#ln = if_not_exists({}.#ln, :zero) - :one",
                old_geom_key, old_geom_key
            ))
            .expression_attribute_names("#ln", old_label_name)
            .expression_attribute_values(":one", AttributeValue::N("1".to_string()))
            .expression_attribute_values(":zero", AttributeValue::N("0".to_string()))
            .send()
            .await
            .map_err(|e| format!("DynamoDB update_item error: {}", e))?;

        client
            .update_item()
            .table_name(table_name)
            .key("PK", AttributeValue::S(format!("BLOCK#{}", block_id)))
            .key("SK", AttributeValue::S(format!("IMAGE#{}", image_id)))
            .update_expression(format!(
                "SET labels_count.#ln = if_not_exists(labels_count.#ln, :zero) + :one, {}.#ln = if_not_exists({}.#ln, :zero) + :one",
                new_geom_key, new_geom_key
            ))
            .expression_attribute_names("#ln", new_label_name)
            .expression_attribute_values(":one", AttributeValue::N("1".to_string()))
            .expression_attribute_values(":zero", AttributeValue::N("0".to_string()))
            .send()
            .await
            .map_err(|e| format!("DynamoDB update_item error: {}", e))?;
    }

    Ok(())
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

fn geometry_counter_key(geometry: &Geometry) -> &'static str {
    match geometry {
        Geometry::BBox { .. } => "bbox_count",
        Geometry::Polygon { .. } => "polygon_count",
    }
}

async fn load_label_name_for_label_id(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    label_id: &str,
) -> Result<String, String> {
    let result = client
        .get_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(format!("BLOCK#{}", block_id)))
        .key("SK", AttributeValue::S(format!("LABEL#{}", label_id)))
        .send()
        .await
        .map_err(|e| format!("DynamoDB get_item error: {}", e))?;

    let item = result.item().ok_or("Target label not found")?;
    let label_name = item
        .get("label_name")
        .and_then(|v| v.as_s().ok())
        .map(|s| s.to_string())
        .ok_or("Target label is missing label_name")?;

    if label_name.trim().is_empty() {
        return Err("Target label_name is empty".to_string());
    }

    Ok(label_name)
}


/// Bulk-delete all annotations for an image being deleted.
/// Uses BatchWriteItem (25 per batch) and decrements block annotation_count once.
/// Skips image counter updates since the image itself is being deleted.
pub async fn bulk_delete_annotations_for_image(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    image_id: &str,
) -> Result<usize, String> {
    let annotations = list_annotations(client, table_name, image_id).await?;
    let total = annotations.len();
    if total == 0 {
        return Ok(0);
    }

    // Batch delete annotations 25 at a time (DynamoDB BatchWriteItem limit)
    for chunk in annotations.chunks(25) {
        let delete_requests: Vec<aws_sdk_dynamodb::types::WriteRequest> = chunk
            .iter()
            .map(|ann| {
                let pk = AttributeValue::S(format!("IMAGE#{}", image_id));
                let sk = AttributeValue::S(format!("ANNOTATION#{}", ann.annotation_id));
                aws_sdk_dynamodb::types::WriteRequest::builder()
                    .delete_request(
                        aws_sdk_dynamodb::types::DeleteRequest::builder()
                            .key("PK", pk)
                            .key("SK", sk)
                            .build()
                            .expect("valid delete request"),
                    )
                    .build()
            })
            .collect();

        client
            .batch_write_item()
            .request_items(table_name, delete_requests)
            .send()
            .await
            .map_err(|e| format!("DynamoDB batch_write_item error: {}", e))?;
    }

    Ok(total)
}

/// Delete annotation
pub async fn delete_annotation(
    client: &DynamoClient,
    table_name: &str,
    project_id: Option<&str>,
    block_id:&str,
    image_id: &str,
    annotation_id: &str,
) -> Result<(), String> {
    let pk = format!("IMAGE#{}", image_id);
    let sk = format!("ANNOTATION#{}", annotation_id);

    // Delete and return old item in one call (saves a get_item round-trip)
    let delete_result = client
        .delete_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(pk))
        .key("SK", AttributeValue::S(sk))
        .return_values(aws_sdk_dynamodb::types::ReturnValue::AllOld)
        .send()
        .await
        .map_err(|e| format!("DynamoDB delete_item error: {}", e))?;

    let item = delete_result.attributes().ok_or("Annotation not found")?;

    let label_name = item.get("label_name")
        .and_then(|v| v.as_s().ok())
        .map(|s| s.to_string())
        .ok_or("Missing label_name on annotation")?;

    let geometry_str = item.get("geometry")
        .and_then(|v| v.as_s().ok())
        .ok_or("Missing geometry")?;

    let geometry: Geometry = serde_json::from_str(geometry_str)
        .map_err(|e| format!("Failed to parse geometry: {}", e))?;

    let geom_key = match &geometry {
        Geometry::BBox { .. } => "bbox_count",
        Geometry::Polygon { .. } => "polygon_count",
    };

    // Decrement IMAGE labels_count and geom count, guarded to avoid negative counters.
    let image_update = client
        .update_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(format!("BLOCK#{}", block_id)))
        .key("SK", AttributeValue::S(format!("IMAGE#{}", image_id)))
        .update_expression(format!(
            "SET annotation_count = annotation_count - :one, labels_count.#ln = labels_count.#ln - :one, {}.#ln = {}.#ln - :one",
            geom_key, geom_key
        ))
        .condition_expression(format!(
            "annotation_count >= :one AND labels_count.#ln >= :one AND {}.#ln >= :one",
            geom_key
        ))
        .expression_attribute_names("#ln", &label_name)
        .expression_attribute_values(":one", AttributeValue::N("1".to_string()))
        .send()
        .await;
    match image_update {
        Ok(_) => {}
        Err(err) => {
            let err_str = err.to_string();
            if !err_str.contains("ConditionalCheckFailed") {
                return Err(format!("DynamoDB update_item error: {}", err_str));
            }
            // Fallback: still decrement only annotation_count when safe; leave maps unchanged.
            let total_only_update = client
                .update_item()
                .table_name(table_name)
                .key("PK", AttributeValue::S(format!("BLOCK#{}", block_id)))
                .key("SK", AttributeValue::S(format!("IMAGE#{}", image_id)))
                .update_expression("SET annotation_count = annotation_count - :one")
                .condition_expression("annotation_count >= :one")
                .expression_attribute_values(":one", AttributeValue::N("1".to_string()))
                .send()
                .await;
            if let Err(total_only_err) = total_only_update {
                let total_only_err_str = total_only_err.to_string();
                if !total_only_err_str.contains("ConditionalCheckFailed") {
                    return Err(format!(
                        "DynamoDB update_item fallback error: {}",
                        total_only_err_str
                    ));
                }
            }
        }
    }
    if let Some(project_id) = project_id.filter(|pid| !pid.trim().is_empty()) {
        decrement_project_block_annotation_count_safely(
            client,
            table_name,
            project_id,
            block_id,
            1,
        )
        .await?;
    }

    Ok(())
}



