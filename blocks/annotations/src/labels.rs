use lambda_http::{Body, Error, Response, http::StatusCode};
use aws_sdk_dynamodb::Client as DynamoClient;
use crate::types::{Label, CreateLabelPayload, UpdateLabelPayload};
use aws_sdk_dynamodb::types::AttributeValue;
use std::collections::HashMap;

const FLOOR_PLAN_ORDER: [&str; 20] = [
    "fp-outside",
    "fp-inside",
    "ewalls",
    "windows",
    "iwalls",
    "doors",
    "cav-slider",
    "stairs",
    "robes",
    "toilet",
    "vanity",
    "shower",
    "bathtub",
    "sink",
    "outbuilding",
    "scale",
    "dims",
    "area",
    "title",
    "legend",
];

const ELEVATION_ORDER: [&str; 15] = [
    "gf-wall",
    "gf-window",
    "gf-roof",
    "ff-wall",
    "ff-window",
    "ff-roof",
    "sf-wall",
    "sf-window",
    "sf-roof",
    "skylight",
    "fence",
    "dims",
    "area",
    "title",
    "legend",
];

const ELECTRICAL_PLAN_ORDER: [&str; 3] = [
    "downlight",
    "gpo-single",
    "gpo-double",
];

const ROOF_PLAN_ORDER: [&str; 1] = [
    "box-gutter",
];

/// Sorts the block labels in order
fn order_index_for(block_type:&str, label_name:&str) -> Option<u32> {
    let list: &[&str] = match block_type {
        "floor" => &FLOOR_PLAN_ORDER,
        "elevation" => &ELEVATION_ORDER,
        "electrical" => &ELECTRICAL_PLAN_ORDER,
        "roof" => &ROOF_PLAN_ORDER,
        _ => return None,
    };
    
    list.iter()
        .position(|&name| name == label_name)
        .map(|pos| pos as u32)
}

/// Create a new label for a block
pub async fn create_label(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    body: &[u8],
) -> Result<Response<Body>, Error> {
    let req: CreateLabelPayload = serde_json::from_slice(body)?;
    
    let label_id = uuid::Uuid::new_v4().to_string();
    let pk = format!("BLOCK#{}", block_id);
    let sk = format!("LABEL#{}", label_id);
    
    let mut builder = client
        .put_item()
        .table_name(table_name)
        .item("PK", AttributeValue::S(pk))
        .item("SK", AttributeValue::S(sk))
        .item("label_name", AttributeValue::S(req.label_name.clone()))
        .item("label_count", AttributeValue::N("0".to_string()))
        .item("label_color", AttributeValue::S(req.label_color.clone()));
    
    if let Some(label_properties) = &req.label_properties {
        builder = builder.item("label_properties", AttributeValue::S(serde_json::to_string(label_properties)?));
    }
    
    builder.send().await?;
    
    let label = Label {
        label_id: label_id.clone(),
        block_id: block_id.to_string(),
        label_name: req.label_name,
        label_color: req.label_color,
        label_properties: req.label_properties,
        label_count: 0,
    };
    
    Ok(Response::builder()
        .status(StatusCode::CREATED)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(serde_json::to_string(&label)?.into())
        .map_err(Box::new)?)
}

/// Get a specific label
pub async fn get_label(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    label_id: &str,
) -> Result<Response<Body>, Error> {
     let pk = format!("BLOCK#{}", block_id);
     let sk = format!("LABEL#{}", label_id);
    
    let result = client
        .get_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(pk))
        .key("SK", AttributeValue::S(sk))
        .send()
        .await?;
    
    if let Some(item) = result.item() {
        let label = Label {
            label_id: label_id.to_string(),
            block_id: block_id.to_string(),
            label_name: item.get("label_name").and_then(|v| v.as_s().ok()).map(|s| s.to_string()).unwrap_or_default(),
            label_color: item.get("label_color")
                .and_then(|v| v.as_s().ok())
                .map(|s| s.to_string())
                .expect("label_color missing"),
            label_properties: item.get("label_properties")
                .and_then(|v| v.as_s().ok())
                .and_then(|s| serde_json::from_str(s).ok()),
            label_count: item.get("label_count").and_then(|v| v.as_n().ok()).and_then(|n| n.parse().ok()).unwrap_or(0),
        };
        
        Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::to_string(&label)?.into())
            .map_err(Box::new)?)
    } else {
        Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::json!({"error": " Label not found"}).to_string().into())
            .map_err(Box::new)?)
    }
}

/// Load labels for a block (internal helper for block list/get)
pub async fn fetch_labels_for_block(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
) -> Result<Vec<Label>, Error> {
    let pk = format!("BLOCK#{}", block_id);
    
    let result = client
        .query()
        .table_name(table_name)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(pk))
        .expression_attribute_values(":sk_prefix", AttributeValue::S("LABEL#".to_string()))
        .send()
        .await?;
    
    let mut labels = Vec::new();
    
    for item in result.items() {
            if let Some(sk) = item.get("SK").and_then(|v| v.as_s().ok()) {
                if let Some(label_id) = sk.strip_prefix("LABEL#") {
                    let label = Label {
                        label_id: label_id.to_string(),
                        block_id: block_id.to_string(),
                        label_name: item.get("label_name").and_then(|v| v.as_s().ok()).map(|s| s.to_string()).unwrap_or_default(),
                        label_color: item.get("label_color")
                            .and_then(|v| v.as_s().ok())
                            .map(|s| s.to_string())
                            .expect("label_color missing"),
                        label_properties: item.get("label_properties")
                            .and_then(|v| v.as_s().ok())
                            .and_then(|s| serde_json::from_str(s).ok()),
                        label_count: item.get("label_count").and_then(|v| v.as_n().ok()).and_then(|n| n.parse().ok()).unwrap_or(0),
                    };
                    labels.push(label);
                }
            }
    }

    // Sort the labels
    if let Some(block_type) = get_block_type(client, table_name, block_id).await? {
        labels.sort_by(|a, b| {
            let a_idx = order_index_for(&block_type, &a.label_name);
            let b_idx = order_index_for(&block_type, &b.label_name);
            match (a_idx, b_idx) {
                (Some(a_order), Some(b_order)) => a_order.cmp(&b_order),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.label_name.cmp(&b.label_name),
            }
        });
    }
    
    Ok(labels)
}

/// List all labels for a block
pub async fn list_block_labels(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
) -> Result<Response<Body>, Error> {
    let labels = fetch_labels_for_block(client, table_name, block_id).await?;
    
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(serde_json::to_string(&labels)?.into())
        .map_err(Box::new)?)
}

async fn get_block_type(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
) -> Result<Option<String>, Error> {
    let pk = "BLOCK".to_string();
    let sk = format!("BLOCK#{}", block_id);

    let result = client
        .get_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(pk))
        .key("SK", AttributeValue::S(sk))
        .send()
        .await?;

    Ok(result
        .item()
        .and_then(|item| item.get("block_type"))
        .and_then(|v| v.as_s().ok())
        .map(|s| s.to_string()))
}

/// Update a label
pub async fn update_label(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    label_id: &str,
    body: &[u8],
) -> Result<Response<Body>, Error> {
    let req: UpdateLabelPayload = serde_json::from_slice(body)?;
     let pk = format!("BLOCK#{}", block_id);
     let sk = format!("LABEL#{}", label_id);
    
    let mut update_expr = vec![];
    let mut expr_names = HashMap::new();
    let mut expr_values = HashMap::new();
    
    if let Some(name) = req.label_name {
        update_expr.push("#label_name = :label_name");
        expr_names.insert("#label_name".to_string(), "name".to_string());
        expr_values.insert(":label_name".to_string(), AttributeValue::S(name));
    }
    
    if let Some(color) = req.label_color {
        update_expr.push("#label_color = :label_color");
        expr_names.insert("#label_color".to_string(), "label_color".to_string());
        expr_values.insert(":label_color".to_string(), AttributeValue::S(color));
    }
    
    if let Some(properties) = req.label_properties {
        update_expr.push("#label_properties = :label_properties");
        expr_names.insert("#label_properties".to_string(), "label_properties".to_string());
        expr_values.insert(":label_properties".to_string(), 
            AttributeValue::S(serde_json::to_string(&properties)?));
    }
    
    if !update_expr.is_empty() {
        let mut builder = client
            .update_item()
            .table_name(table_name)
            .key("PK", AttributeValue::S(pk.clone()))
            .key("SK", AttributeValue::S(sk.clone()))
            .update_expression(format!("SET {}", update_expr.join(", ")));
        
        for (k, v) in expr_names {
            builder = builder.expression_attribute_names(k, v);
        }
        
        for (k, v) in expr_values {
            builder = builder.expression_attribute_values(k, v);
        }
        
        builder.send().await?;
    }
    
    get_label(client, table_name, block_id, label_id).await
}

/// Delete a label
pub async fn delete_label(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    label_id: &str,
) -> Result<Response<Body>, Error> {
    let pk = format!("BLOCK#{}", block_id);
    let sk = format!("LABEL#{}", label_id);
    
    client
        .delete_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(pk))
        .key("SK", AttributeValue::S(sk))
        .send()
        .await?;
    
    Ok(Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header("Access-Control-Allow-Origin", "*")
        .body(Body::Empty)
        .map_err(Box::new)?)
}

/// Increment label count (when annotations are added/removed)
pub async fn increment_label_count(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    label_id: &str,
    delta: i32,
) -> Result<(), Error> {
    let pk = format!("BLOCK#{}", block_id);
    let sk = format!("LABEL#{}", label_id);
    
    client
        .update_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(pk))
        .key("SK", AttributeValue::S(sk))
        .update_expression("SET label_count =  label_count + :delta")
        .expression_attribute_values(":delta", AttributeValue::N(delta.to_string()))
        .send()
        .await?;
    
    Ok(())
}
