use annotations_block::bulk_import;
use aws_sdk_dynamodb::types::AttributeValue;
use aws_sdk_dynamodb::Client as DynamoClient;
use aws_sdk_s3::Client as S3Client;
use lambda_http::{http::StatusCode, Body};
use lambda_runtime::{run, service_fn, Error, LambdaEvent};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

const IMPORT_STATUS_RUNNING: &str = "running";
const IMPORT_STATUS_COMPLETED: &str = "completed";
const IMPORT_STATUS_FAILED: &str = "failed";
const IMPORT_PHASE_PARSE: &str = "parse";
const IMPORT_PHASE_LABELS: &str = "labels";
const IMPORT_PHASE_COMPLETED: &str = "completed";
const IMPORT_PHASE_FAILED: &str = "failed";

#[derive(Debug, Deserialize)]
struct WorkerInput {
    action: String,
    import_job_id: String,
    project_id: String,
    block_id: String,
    import_id: String,
    s3_key: String,
    #[serde(default)]
    phase: Option<String>,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    max_tasks: Option<usize>,
}

#[derive(Debug, Serialize)]
struct WorkerOutput {
    done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    processed: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_offset: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    labels_total: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tasks_total: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    images_total: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    annotations_total: Option<usize>,
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .without_time()
        .init();

    run(service_fn(function_handler)).await
}

async fn function_handler(event: LambdaEvent<WorkerInput>) -> Result<WorkerOutput, Error> {
    let input = event.payload;
    let table_name = std::env::var("TABLE_NAME").unwrap_or_else(|_| "doxle".to_string());

    let timeout_config = aws_config::timeout::TimeoutConfig::builder()
        .operation_timeout(Duration::from_secs(900))
        .operation_attempt_timeout(Duration::from_secs(300))
        .build();

    let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .timeout_config(timeout_config)
        .load()
        .await;
    let dynamo = DynamoClient::new(&config);
    let s3 = S3Client::new(&config);

    let result = match input.action.as_str() {
        "init" => init_import_job(&dynamo, &table_name, &input.import_job_id).await,
        "parse" => parse_import_manifest(&dynamo, &s3, &table_name, &input).await,
        "process_batch" => process_import_batch(&dynamo, &s3, &table_name, &input).await,
        "cleanup" => cleanup_import_job(&dynamo, &s3, &table_name, &input).await,
        other => Err(format!("Unknown action '{}'", other)),
    };

    match result {
        Ok(output) => Ok(output),
        Err(err) => {
            let _ = mark_import_failed(&dynamo, &table_name, &input.import_job_id, &err).await;
            Err(err.into())
        }
    }
}

async fn init_import_job(
    dynamo: &DynamoClient,
    table_name: &str,
    import_job_id: &str,
) -> Result<WorkerOutput, String> {
    let _ = load_import_job_item(dynamo, table_name, import_job_id)
        .await?
        .ok_or_else(|| format!("Import job '{}' not found", import_job_id))?;

    dynamo
        .update_item()
        .table_name(table_name)
        .key(
            "PK",
            AttributeValue::S(format!("IMPORT_JOB#{}", import_job_id)),
        )
        .key("SK", AttributeValue::S("META".to_string()))
        .update_expression(
            "SET #status = :status, #phase = :phase, updated_at = :updated_at REMOVE error_message",
        )
        .expression_attribute_names("#status", "status")
        .expression_attribute_names("#phase", "phase")
        .expression_attribute_values(":status", AttributeValue::S(IMPORT_STATUS_RUNNING.to_string()))
        .expression_attribute_values(":phase", AttributeValue::S(IMPORT_PHASE_PARSE.to_string()))
        .expression_attribute_values(":updated_at", AttributeValue::S(now_rfc3339()))
        .send()
        .await
        .map_err(|e| format!("Failed to mark import job initialized: {}", e))?;

    Ok(WorkerOutput {
        done: true,
        phase: Some(IMPORT_PHASE_PARSE.to_string()),
        processed: None,
        total: None,
        next_offset: None,
        labels_total: None,
        tasks_total: None,
        images_total: None,
        annotations_total: None,
    })
}

async fn parse_import_manifest(
    dynamo: &DynamoClient,
    s3: &S3Client,
    table_name: &str,
    input: &WorkerInput,
) -> Result<WorkerOutput, String> {
    let job_item = load_import_job_item(dynamo, table_name, &input.import_job_id)
        .await?
        .ok_or_else(|| format!("Import job '{}' not found", input.import_job_id))?;

    let max_tasks = input
        .max_tasks
        .or_else(|| attr_n_usize(&job_item, "max_tasks"));

    let parse_body = serde_json::to_vec(&serde_json::json!({
        "import_id": input.import_id,
        "s3_key": input.s3_key,
        "max_tasks": max_tasks
    }))
    .map_err(|e| format!("Failed to serialize parse request: {}", e))?;

    let parse_resp = bulk_import::parse_import(s3, &input.block_id, &parse_body)
        .await
        .map_err(|e| format!("Parse import call failed: {}", e))?;

    if parse_resp.status() != StatusCode::OK {
        return Err(format!(
            "Parse import failed ({}): {}",
            parse_resp.status(),
            response_body_to_string(parse_resp.body())
        ));
    }

    let parsed: bulk_import::ParseImportResponse =
        serde_json::from_slice(&response_body_to_bytes(parse_resp.body()))
            .map_err(|e| format!("Failed to parse parse_import response JSON: {}", e))?;

    dynamo
        .update_item()
        .table_name(table_name)
        .key(
            "PK",
            AttributeValue::S(format!("IMPORT_JOB#{}", input.import_job_id)),
        )
        .key("SK", AttributeValue::S("META".to_string()))
        .update_expression("SET #status = :status, #phase = :phase, labels_total = :labels_total, tasks_total = :tasks_total, images_total = :images_total, annotations_total = :annotations_total, updated_at = :updated_at REMOVE error_message")
        .expression_attribute_names("#status", "status")
        .expression_attribute_names("#phase", "phase")
        .expression_attribute_values(":status", AttributeValue::S(IMPORT_STATUS_RUNNING.to_string()))
        .expression_attribute_values(":phase", AttributeValue::S(IMPORT_PHASE_LABELS.to_string()))
        .expression_attribute_values(":labels_total", AttributeValue::N(parsed.total_labels.to_string()))
        .expression_attribute_values(":tasks_total", AttributeValue::N(parsed.total_tasks.to_string()))
        .expression_attribute_values(":images_total", AttributeValue::N(parsed.total_images.to_string()))
        .expression_attribute_values(":annotations_total", AttributeValue::N(parsed.total_annotations.to_string()))
        .expression_attribute_values(":updated_at", AttributeValue::S(now_rfc3339()))
        .send()
        .await
        .map_err(|e| format!("Failed updating import totals: {}", e))?;

    Ok(WorkerOutput {
        done: true,
        phase: Some(IMPORT_PHASE_LABELS.to_string()),
        processed: None,
        total: None,
        next_offset: None,
        labels_total: Some(parsed.total_labels),
        tasks_total: Some(parsed.total_tasks),
        images_total: Some(parsed.total_images),
        annotations_total: Some(parsed.total_annotations),
    })
}

async fn process_import_batch(
    dynamo: &DynamoClient,
    s3: &S3Client,
    table_name: &str,
    input: &WorkerInput,
) -> Result<WorkerOutput, String> {
    let phase = input
        .phase
        .as_ref()
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(|| "process_batch requires phase".to_string())?
        .to_string();
    let offset = input.offset.unwrap_or(0);
    let limit = input.limit.unwrap_or(50).max(1);

    let batch_body = serde_json::to_vec(&serde_json::json!({
        "s3_key": input.s3_key,
        "phase": phase,
        "offset": offset,
        "limit": limit
    }))
    .map_err(|e| format!("Failed to serialize process_batch request: {}", e))?;

    let batch_resp = bulk_import::process_batch(
        s3,
        dynamo,
        table_name,
        &input.project_id,
        &input.block_id,
        &batch_body,
    )
    .await
    .map_err(|e| format!("Process batch call failed: {}", e))?;

    if batch_resp.status() != StatusCode::OK {
        return Err(format!(
            "Process batch failed ({}): {}",
            batch_resp.status(),
            response_body_to_string(batch_resp.body())
        ));
    }

    let parsed: bulk_import::ProcessBatchResponse =
        serde_json::from_slice(&response_body_to_bytes(batch_resp.body()))
            .map_err(|e| format!("Failed to parse process_batch response JSON: {}", e))?;

    let mut labels_processed = 0usize;
    let mut tasks_processed = 0usize;
    let mut images_processed = 0usize;
    let mut annotations_processed = 0usize;

    match parsed.phase.as_str() {
        "labels" => labels_processed = parsed.processed,
        "tasks" => tasks_processed = parsed.processed,
        "images" => {
            images_processed = parsed.processed;
            annotations_processed = parsed.annotations_created;
        }
        other => {
            return Err(format!("Unsupported process phase in response: {}", other));
        }
    }

    dynamo
        .update_item()
        .table_name(table_name)
        .key(
            "PK",
            AttributeValue::S(format!("IMPORT_JOB#{}", input.import_job_id)),
        )
        .key("SK", AttributeValue::S("META".to_string()))
        .update_expression("SET #status = :status, #phase = :phase, updated_at = :updated_at REMOVE error_message ADD labels_processed :labels_processed, tasks_processed :tasks_processed, images_processed :images_processed, annotations_processed :annotations_processed, labels_created :labels_created, tasks_created :tasks_created, images_created :images_created, annotations_created :annotations_created")
        .expression_attribute_names("#status", "status")
        .expression_attribute_names("#phase", "phase")
        .expression_attribute_values(":status", AttributeValue::S(IMPORT_STATUS_RUNNING.to_string()))
        .expression_attribute_values(":phase", AttributeValue::S(parsed.phase.clone()))
        .expression_attribute_values(":updated_at", AttributeValue::S(now_rfc3339()))
        .expression_attribute_values(":labels_processed", AttributeValue::N(labels_processed.to_string()))
        .expression_attribute_values(":tasks_processed", AttributeValue::N(tasks_processed.to_string()))
        .expression_attribute_values(":images_processed", AttributeValue::N(images_processed.to_string()))
        .expression_attribute_values(":annotations_processed", AttributeValue::N(annotations_processed.to_string()))
        .expression_attribute_values(":labels_created", AttributeValue::N(parsed.labels_created.to_string()))
        .expression_attribute_values(":tasks_created", AttributeValue::N(parsed.tasks_created.to_string()))
        .expression_attribute_values(":images_created", AttributeValue::N(parsed.images_created.to_string()))
        .expression_attribute_values(":annotations_created", AttributeValue::N(parsed.annotations_created.to_string()))
        .send()
        .await
        .map_err(|e| format!("Failed updating import batch progress: {}", e))?;

    let next_offset = offset + parsed.processed;
    let done = next_offset >= parsed.total;

    Ok(WorkerOutput {
        done,
        phase: Some(parsed.phase),
        processed: Some(parsed.processed),
        total: Some(parsed.total),
        next_offset: Some(next_offset),
        labels_total: None,
        tasks_total: None,
        images_total: None,
        annotations_total: None,
    })
}

async fn cleanup_import_job(
    dynamo: &DynamoClient,
    s3: &S3Client,
    table_name: &str,
    input: &WorkerInput,
) -> Result<WorkerOutput, String> {
    let cleanup_body = serde_json::to_vec(&serde_json::json!({
        "s3_key": input.s3_key
    }))
    .map_err(|e| format!("Failed to serialize cleanup request: {}", e))?;

    let cleanup_resp = bulk_import::cleanup_import(s3, &cleanup_body)
        .await
        .map_err(|e| format!("Cleanup call failed: {}", e))?;

    if cleanup_resp.status() != StatusCode::OK {
        return Err(format!(
            "Cleanup failed ({}): {}",
            cleanup_resp.status(),
            response_body_to_string(cleanup_resp.body())
        ));
    }

    dynamo
        .update_item()
        .table_name(table_name)
        .key(
            "PK",
            AttributeValue::S(format!("IMPORT_JOB#{}", input.import_job_id)),
        )
        .key("SK", AttributeValue::S("META".to_string()))
        .update_expression(
            "SET #status = :status, #phase = :phase, updated_at = :updated_at REMOVE error_message",
        )
        .expression_attribute_names("#status", "status")
        .expression_attribute_names("#phase", "phase")
        .expression_attribute_values(":status", AttributeValue::S(IMPORT_STATUS_COMPLETED.to_string()))
        .expression_attribute_values(":phase", AttributeValue::S(IMPORT_PHASE_COMPLETED.to_string()))
        .expression_attribute_values(":updated_at", AttributeValue::S(now_rfc3339()))
        .send()
        .await
        .map_err(|e| format!("Failed updating import completed state: {}", e))?;

    Ok(WorkerOutput {
        done: true,
        phase: Some(IMPORT_PHASE_COMPLETED.to_string()),
        processed: None,
        total: None,
        next_offset: None,
        labels_total: None,
        tasks_total: None,
        images_total: None,
        annotations_total: None,
    })
}

async fn load_import_job_item(
    client: &DynamoClient,
    table_name: &str,
    import_job_id: &str,
) -> Result<Option<HashMap<String, AttributeValue>>, String> {
    let result = client
        .get_item()
        .table_name(table_name)
        .key(
            "PK",
            AttributeValue::S(format!("IMPORT_JOB#{}", import_job_id)),
        )
        .key("SK", AttributeValue::S("META".to_string()))
        .send()
        .await
        .map_err(|e| format!("Failed loading import job {}: {}", import_job_id, e))?;
    Ok(result.item().cloned())
}

async fn mark_import_failed(
    client: &DynamoClient,
    table_name: &str,
    import_job_id: &str,
    error_message: &str,
) -> Result<(), String> {
    client
        .update_item()
        .table_name(table_name)
        .key(
            "PK",
            AttributeValue::S(format!("IMPORT_JOB#{}", import_job_id)),
        )
        .key("SK", AttributeValue::S("META".to_string()))
        .update_expression("SET #status = :status, #phase = :phase, error_message = :error_message, updated_at = :updated_at")
        .expression_attribute_names("#status", "status")
        .expression_attribute_names("#phase", "phase")
        .expression_attribute_values(":status", AttributeValue::S(IMPORT_STATUS_FAILED.to_string()))
        .expression_attribute_values(":phase", AttributeValue::S(IMPORT_PHASE_FAILED.to_string()))
        .expression_attribute_values(":error_message", AttributeValue::S(error_message.to_string()))
        .expression_attribute_values(":updated_at", AttributeValue::S(now_rfc3339()))
        .send()
        .await
        .map_err(|e| format!("Failed to persist import failure state: {}", e))?;
    Ok(())
}

fn attr_n_usize(item: &HashMap<String, AttributeValue>, key: &str) -> Option<usize> {
    item.get(key)
        .and_then(|v| v.as_n().ok())
        .and_then(|n| n.parse::<usize>().ok())
}

fn response_body_to_bytes(body: &Body) -> Vec<u8> {
    match body {
        Body::Empty => Vec::new(),
        Body::Text(text) => text.as_bytes().to_vec(),
        Body::Binary(bin) => bin.clone(),
    }
}

fn response_body_to_string(body: &Body) -> String {
    match body {
        Body::Empty => String::new(),
        Body::Text(text) => text.clone(),
        Body::Binary(bin) => String::from_utf8_lossy(bin).to_string(),
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}
