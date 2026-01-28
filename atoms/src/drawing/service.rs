use aws_sdk_dynamodb::Client as DynamoClient;
use aws_sdk_dynamodb::types::AttributeValue;
use super::model::{Annotation, Geometry, CreateAnnotationPayload, UpdateAnnotationPayload};

/// Create a new annotation
pub async fn create_annotation(
    client: &DynamoClient,
    table_name: &str,
    block_id:&str,
    image_id: &str,
    user_id: &str,
    payload: CreateAnnotationPayload,
) -> Result<Annotation, String> {
    let annotation_id = uuid::Uuid::new_v4().to_string();
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
        .item("geometry", AttributeValue::S(geometry_json))
        .item("created_by", AttributeValue::S(user_id.to_string()))
        .item("created_at", AttributeValue::S(now.clone()))
        .send()
        .await
        .map_err(|e| format!("DynamoDB put_item error: {}", e))?;


    // Increment BLOCK- annotation_count
    client
        .update_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S("BLOCK".to_string()))
        .key("SK", AttributeValue::S(format!("BLOCK#{}", block_id)))
        .update_expression("SET annotation_count = annotation_count + :one")
        .expression_attribute_values(":one", AttributeValue::N("1".to_string()))
        .send()
        .await
        .map_err(|e| format!("DynamoDB update_item error: {}", e))?;


    // Increment IMAGE - annotation_count
    client
        .update_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(format!("BLOCK#{}", block_id)))
        .key("SK", AttributeValue::S(format!("IMAGE#{}", image_id)))
        .update_expression("SET annotation_count = annotation_count + :one")
        .expression_attribute_values(":one", AttributeValue::N("1".to_string()))
        .send()
        .await
        .map_err(|e| format!("DynamoDB update_item error: {}", e))?;





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
    
    let result = client
        .query()
        .table_name(table_name)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(pk))
        .expression_attribute_values(":sk_prefix", AttributeValue::S("ANNOTATION#".to_string()))
        .send()
        .await
        .map_err(|e| format!("DynamoDB query error: {}", e))?;
        
    let mut annotations = Vec::new();
    
    for item in result.items() {
        if let Some(sk) = item.get("SK").and_then(|v| v.as_s().ok()) {
            if let Some(annotation_id) = sk.strip_prefix("ANNOTATION#") {
                let geometry_str = item.get("geometry")
                    .and_then(|v| v.as_s().ok())
                    .ok_or("Missing geometry")?;
                    
                let geometry: Geometry = serde_json::from_str(geometry_str)
                    .map_err(|e| format!("Failed to parse geometry: {}", e))?;
                    
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
    
    Ok(annotations)
}

/// Update annotation label
pub async fn update_annotation(
    client:&DynamoClient,
    table_name:&str,
    image_id:&str,
    annotation_id:&str,
    payload:UpdateAnnotationPayload
    ) -> Result <(), String> {

    let pk = format!("IMAGE#{}", image_id);
    let sk = format!("ANNOTATION#{}", annotation_id);
    let now = chrono::Utc::now().to_rfc3339();

    // Build dynamic // Start with just timestamp
    let mut update_parts:Vec<&str> = vec!["updated_at = :updated_at"];
    let mut expr_values:Vec<(String, AttributeValue)> = vec![(":updated_at".to_string(), AttributeValue::S(now))];

    // If label_id exists, add it
    if let Some(label_id) = &payload.label_id {
        update_parts.push("label_id = :label_id");
        expr_values.push((":label_id".to_string(), AttributeValue::S(label_id.clone())));
    }
    // Now: update_parts = ["updated_at = :updated_at", "label_id = :label_id"]

    // If geometry exists, add it
    // expr_values.push(":geometry" → "{\"type\":\"polygon\",\"points\":[...]}");
    if let Some(geometry) = &payload.geometry {
        let geometry_json = serde_json::to_string(geometry).map_err(|e| format!("Faled to serialize geometry: {}", e))?;
        update_parts.push("geometry = :geometry");
        expr_values.push((":geometry".to_string(), AttributeValue::S(geometry_json)));

    }

    // If nothing to update besides timestamp, that's fine
    let update_expression = format!("SET {}", update_parts.join(", "));


    // UPDATE SET updated_at = :updated_at, label_id = :label_id, geometry = :geometry
    // WHERE PK = "IMAGE#123" AND SK = "ANNOTATION#456"

    // -- With values:
    // -- :updated_at = "2024-01-11T00:51:21Z"
    // -- :label_id = "cat"
    // -- :geometry = "{\"type\":\"polygon\",...}"
    let mut update_builder = client
                                .update_item()
                                .table_name(table_name)
                                .key("PK", AttributeValue::S(pk))
                                .key("SK", AttributeValue::S(sk))
                                .update_expression(&update_expression);

                                
    for (name,value) in expr_values {
        update_builder = update_builder.expression_attribute_values(name, value);
    }

    update_builder
        .send()
        .await
        .map_err(|e| format!("DynamoDB update error: {}", e))?;

    Ok(())
}


/// Delete annotation
pub async fn delete_annotation(
    client: &DynamoClient,
    table_name: &str,
    block_id:&str,
    image_id: &str,
    annotation_id: &str,
) -> Result<(), String> {
    let pk = format!("IMAGE#{}", image_id);
    let sk = format!("ANNOTATION#{}", annotation_id);
    
    client
        .delete_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(pk))
        .key("SK", AttributeValue::S(sk))
        .send()
        .await
        .map_err(|e| format!("DynamoDB delete_item error: {}", e))?;

    // Decrement BLOCK- annotation_count
    client
        .update_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S("BLOCK".to_string()))
        .key("SK", AttributeValue::S(format!("BLOCK#{}", block_id)))
        .update_expression("SET annotation_count = annotation_count - :one")
        .expression_attribute_values(":one", AttributeValue::N("1".to_string()))
        .send()
        .await
        .map_err(|e| format!("DynamoDB update_item error: {}", e))?;
        

    // Decrement image annotation count
    client
        .update_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(format!("BLOCK#{}", block_id)))
        .key("SK", AttributeValue::S(format!("IMAGE#{}", image_id)))
        .update_expression("SET annotation_count = annotation_count - :one")
        .expression_attribute_values(":one", AttributeValue::N("1".to_string()))
        .send()
        .await
        .map_err(|e| format!("DynamoDB delete_item error: {}", e))?;




        
    Ok(())
}


