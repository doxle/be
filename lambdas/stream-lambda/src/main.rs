use aws_config;
use aws_lambda_events::event::dynamodb::{Event, EventRecord};
use aws_sdk_apigatewaymanagement::Client as ApiGatewayManagementClient;
use aws_sdk_dynamodb::Client as DynamoClient;
use doxle_shared::sockets::broadcast::_broadcast_to_all;
use doxle_shared::sockets::messages::BroadcastMessage;
use lambda_runtime::{run, service_fn, Error, LambdaEvent};

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .without_time()
        .init();

    run(service_fn(function_handler)).await
}

async fn function_handler(event: LambdaEvent<Event>) -> Result<(), Error> {
    tracing::info!("DynamoDB Stream event received with {} records", event.payload.records.len());

    // Initialize AWS clients
    let config = aws_config::load_from_env().await;
    let dynamo_client = DynamoClient::new(&config);

    // Get WebSocket API endpoint from environment
    let ws_endpoint = std::env::var("WS_API_ENDPOINT")
        .expect("WS_API_ENDPOINT must be set for stream handler");

    let api_config = aws_sdk_apigatewaymanagement::config::Builder::from(&config)
        .endpoint_url(ws_endpoint)
        .build();
    let api_gateway_client = ApiGatewayManagementClient::from_conf(api_config);

    let table_name = std::env::var("TABLE_NAME").unwrap_or_else(|_| "doxle-annotations".to_string());

    // Process each record
    for record in event.payload.records {
        if let Err(e) = process_record(&record, &dynamo_client, &api_gateway_client, &table_name).await {
            tracing::error!("Failed to process record: {}", e);
        }
    }

    Ok(())
}

async fn process_record(
    record: &EventRecord,
    dynamo_client: &DynamoClient,
    api_gateway_client: &ApiGatewayManagementClient,
    table_name: &str,
) -> Result<(), Error> {
    let event_name = &record.event_name;

    tracing::info!("Processing {} event", event_name);

    // Determine entity type from PK
    // For REMOVE events, new_image is empty; use old_image instead
    let image = if record.change.new_image.is_empty() {
        &record.change.old_image
    } else {
        &record.change.new_image
    };
    
    let pk = image.get("PK")
        .and_then(|attr| {
            // Convert to string - the AttributeValue should be a String variant
            serde_json::to_value(attr).ok()
                .and_then(|v| v.as_str().map(|s| s.to_string()))
        })
        .ok_or("Missing PK")?;
    
    let pk_str = pk.as_str();

    // Extract SK for entity classification (block items now live under PROJECT# PK)
    let sk = image.get("SK")
        .and_then(|attr| {
            serde_json::to_value(attr).ok()
                .and_then(|v| v.as_str().map(|s| s.to_string()))
        })
        .unwrap_or_default();
    let sk_str = sk.as_str();

    // Skip connection records (they're not data changes)
    if pk_str.starts_with("CONNECTION#") {
        return Ok(());
    }

    // Classify entity type using BOTH PK and SK:
    // - PK=PROJECT, SK=PROJECT# → project event
    // - PK=PROJECT#, SK=BLOCK# → block event (blocks now live under projects)
    // - PK=BLOCK#, SK=TASK/LABEL/IMAGE# → task/label/image event
    // - PK=IMAGE#, SK=ANNOTATION# → annotation event
    let entity_type = if pk_str == "PROJECT" && sk_str.starts_with("PROJECT#") {
        "project"
    } else if pk_str.starts_with("PROJECT#") && sk_str.starts_with("BLOCK#") {
        "block"
    } else if pk_str.starts_with("BLOCK#") {
        if sk_str.starts_with("TASK#") { "task" }
        else if sk_str.starts_with("LABEL#") { "label" }
        else if sk_str.starts_with("IMAGE#") { "image" }
        else { return Ok(()); }
    } else if pk_str.starts_with("IMAGE#") && sk_str.starts_with("ANNOTATION#") {
        "annotation"
    } else if pk_str.starts_with("CLASS#") {
        "class"
    } else {
        return Ok(());
    };

    // Determine entity type and create appropriate broadcast message
    let message = match event_name.as_str() {
        "INSERT" => {
            let msg_type = format!("{}_created", entity_type);
            if entity_type == "project" {
                create_project_broadcast(record, &msg_type)?
            } else {
                create_entity_broadcast(record, &msg_type)?
            }
        }
        "MODIFY" => {
            let msg_type = format!("{}_updated", entity_type);
            if entity_type == "project" {
                create_project_broadcast(record, &msg_type)?
            } else {
                create_entity_broadcast(record, &msg_type)?
            }
        }
        "REMOVE" => {
            let entity_id = extract_id_from_pk(pk_str);
            let msg_type = format!("{}_deleted", entity_type);
            BroadcastMessage::_new(&msg_type, serde_json::json!({ "id": entity_id }))
        }
        _ => return Ok(()),
    };

    // Broadcast to all connected WebSocket clients
    _broadcast_to_all(dynamo_client, api_gateway_client, table_name, &message).await?;

    tracing::info!("Broadcast sent: {}", message.r#type);

    Ok(())
}

fn create_project_broadcast(record: &EventRecord, message_type: &str) -> Result<BroadcastMessage, Error> {
    let new_image = &record.change.new_image;

    // Convert DynamoDB AttributeValue HashMap to JSON
    let json_data = serde_json::to_value(new_image)?;

    Ok(BroadcastMessage::_new(message_type, json_data))
}

fn create_entity_broadcast(record: &EventRecord, message_type: &str) -> Result<BroadcastMessage, Error> {
    let new_image = &record.change.new_image;

    let json_data = serde_json::to_value(new_image)?;

    Ok(BroadcastMessage::_new(message_type, json_data))
}

fn extract_id_from_pk(pk: &str) -> String {
    pk.split('#').nth(1).unwrap_or(pk).to_string()
}
