use aws_sdk_dynamodb::types::AttributeValue;
use aws_sdk_dynamodb::Client as DynamoClient;
use doxle_atoms::drawing::model::Geometry;
use lambda_runtime::{run, service_fn, Error, LambdaEvent};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

const RECONCILE_STATUS_RUNNING: &str = "running";
const RECONCILE_STATUS_COMPLETED: &str = "completed";
const RECONCILE_STATUS_FAILED: &str = "failed";
const RECONCILE_PHASE_RECONCILING: &str = "reconciling";
const RECONCILE_PHASE_COMPLETED: &str = "completed";
const RECONCILE_PHASE_FAILED: &str = "failed";

#[derive(Debug, Deserialize)]
struct WorkerInput {
    action: String,
    reconcile_job_id: String,
    project_id: String,
    block_id: String,
    #[serde(default)]
    force: Option<bool>,
}

#[derive(Debug, Serialize)]
struct WorkerOutput {
    done: bool,
    images_total: u64,
    images_processed: u64,
    annotations_total: u64,
    block_annotation_count: u64,
}

struct WorkerState {
    dynamo: DynamoClient,
    table_name: String,
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .without_time()
        .init();

    let timeout_config = aws_config::timeout::TimeoutConfig::builder()
        .operation_timeout(Duration::from_secs(900))
        .operation_attempt_timeout(Duration::from_secs(300))
        .build();

    let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .timeout_config(timeout_config)
        .load()
        .await;
    let table_name = std::env::var("TABLE_NAME").unwrap_or_else(|_| "doxle".to_string());
    let state = Arc::new(WorkerState {
        dynamo: DynamoClient::new(&config),
        table_name,
    });

    run(service_fn(move |event: LambdaEvent<WorkerInput>| {
        let state = Arc::clone(&state);
        async move { function_handler(event, state).await }
    }))
    .await
}

async fn function_handler(
    event: LambdaEvent<WorkerInput>,
    state: Arc<WorkerState>,
) -> Result<WorkerOutput, Error> {
    let input = event.payload;

    let result = match input.action.as_str() {
        "reconcile" => reconcile_counts(&state.dynamo, &state.table_name, &input).await,
        other => Err(format!("Unknown action '{}'", other)),
    };

    match result {
        Ok(output) => {
            let _ = release_reconcile_lock(
                &state.dynamo,
                &state.table_name,
                &input.project_id,
                &input.block_id,
            )
            .await;
            Ok(output)
        }
        Err(err) => {
            let _ = mark_reconcile_failed(
                &state.dynamo,
                &state.table_name,
                &input.reconcile_job_id,
                &err,
            )
            .await;
            let _ = release_reconcile_lock(
                &state.dynamo,
                &state.table_name,
                &input.project_id,
                &input.block_id,
            )
            .await;
            Err(err.into())
        }
    }
}

async fn reconcile_counts(
    dynamo: &DynamoClient,
    table_name: &str,
    input: &WorkerInput,
) -> Result<WorkerOutput, String> {
    let _force = input.force.unwrap_or(false);
    let job_item = load_reconcile_job_item(dynamo, table_name, &input.reconcile_job_id)
        .await?
        .ok_or_else(|| format!("Reconcile job '{}' not found", input.reconcile_job_id))?;

    let job_project_id = attr_s(&job_item, "project_id").unwrap_or_default();
    let job_block_id = attr_s(&job_item, "block_id").unwrap_or_default();
    if job_project_id != input.project_id || job_block_id != input.block_id {
        return Err("Reconcile job scope mismatch".to_string());
    }

    let images = load_block_image_items(dynamo, table_name, &input.block_id).await?;
    let images_total = images.len() as u64;
    let approved_task_ids = load_approved_task_ids(dynamo, table_name, &input.block_id).await?;
    let approved_image_count = images
        .iter()
        .filter(|item| {
            item.get("task_id")
                .and_then(|v| v.as_s().ok())
                .map(|task_id| approved_task_ids.contains(task_id))
                .unwrap_or(false)
        })
        .count() as u64;

    update_reconcile_progress(
        dynamo,
        table_name,
        &input.reconcile_job_id,
        images_total,
        0,
        0,
        0,
    )
    .await?;

    let mut images_processed: u64 = 0;
    let mut annotations_total: u64 = 0;

    for image_item in images {
        let sk = image_item
            .get("SK")
            .and_then(|v| v.as_s().ok())
            .ok_or("Image item missing SK")?;
        let image_id = sk
            .strip_prefix("IMAGE#")
            .ok_or_else(|| format!("Unexpected image SK '{}'", sk))?;

        let (annotation_count, labels_count, bbox_count, polygon_count) =
            load_image_annotation_counts(dynamo, table_name, image_id).await?;

        update_image_counters(
            dynamo,
            table_name,
            &input.block_id,
            image_id,
            annotation_count,
            &labels_count,
            &bbox_count,
            &polygon_count,
        )
        .await?;

        images_processed += 1;
        annotations_total += annotation_count;

        if images_processed % 25 == 0 || images_processed == images_total {
            update_reconcile_progress(
                dynamo,
                table_name,
                &input.reconcile_job_id,
                images_total,
                images_processed,
                annotations_total,
                annotations_total,
            )
            .await?;
        }
    }

    update_block_annotation_count(
        dynamo,
        table_name,
        &input.project_id,
        &input.block_id,
        annotations_total,
        images_total,
        approved_image_count,
    )
    .await?;

    mark_reconcile_completed(
        dynamo,
        table_name,
        &input.reconcile_job_id,
        images_total,
        images_processed,
        annotations_total,
        annotations_total,
    )
    .await?;

    Ok(WorkerOutput {
        done: true,
        images_total,
        images_processed,
        annotations_total,
        block_annotation_count: annotations_total,
    })
}

async fn load_block_image_items(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
) -> Result<Vec<HashMap<String, AttributeValue>>, String> {
    let mut items = Vec::new();
    let mut last_evaluated_key: Option<HashMap<String, AttributeValue>> = None;

    loop {
        let mut query = client
            .query()
            .table_name(table_name)
            .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
            .expression_attribute_values(":pk", AttributeValue::S(format!("BLOCK#{}", block_id)))
            .expression_attribute_values(":sk_prefix", AttributeValue::S("IMAGE#".to_string()));

        if let Some(start_key) = last_evaluated_key.clone() {
            query = query.set_exclusive_start_key(Some(start_key));
        }

        let result = query
            .send()
            .await
            .map_err(|e| format!("Failed querying block images: {}", e))?;

        for item in result.items() {
            items.push(item.clone());
        }

        match result.last_evaluated_key() {
            Some(next_key) => last_evaluated_key = Some(next_key.clone()),
            None => break,
        }
    }

    Ok(items)
}

async fn load_approved_task_ids(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
) -> Result<HashSet<String>, String> {
    let mut approved_task_ids: HashSet<String> = HashSet::new();
    let mut last_evaluated_key: Option<HashMap<String, AttributeValue>> = None;

    loop {
        let mut query = client
            .query()
            .table_name(table_name)
            .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
            .expression_attribute_values(":pk", AttributeValue::S(format!("BLOCK#{}", block_id)))
            .expression_attribute_values(":sk_prefix", AttributeValue::S("TASK#".to_string()));

        if let Some(start_key) = last_evaluated_key.clone() {
            query = query.set_exclusive_start_key(Some(start_key));
        }

        let result = query
            .send()
            .await
            .map_err(|e| format!("Failed querying block tasks: {}", e))?;

        for item in result.items() {
            let state = item
                .get("task_state")
                .and_then(|v| v.as_s().ok())
                .map(|s| s.to_lowercase())
                .unwrap_or_else(|| "todo".to_string());

            if state != "approved" {
                continue;
            }

            let Some(sk) = item.get("SK").and_then(|v| v.as_s().ok()) else {
                continue;
            };
            if let Some(task_id) = sk.strip_prefix("TASK#") {
                approved_task_ids.insert(task_id.to_string());
            }
        }

        match result.last_evaluated_key() {
            Some(next_key) => last_evaluated_key = Some(next_key.clone()),
            None => break,
        }
    }

    Ok(approved_task_ids)
}

async fn load_image_annotation_counts(
    client: &DynamoClient,
    table_name: &str,
    image_id: &str,
) -> Result<
    (
        u64,
        HashMap<String, u64>,
        HashMap<String, u64>,
        HashMap<String, u64>,
    ),
    String,
> {
    let mut annotation_count: u64 = 0;
    let mut labels_count: HashMap<String, u64> = HashMap::new();
    let mut bbox_count: HashMap<String, u64> = HashMap::new();
    let mut polygon_count: HashMap<String, u64> = HashMap::new();
    let mut last_evaluated_key: Option<HashMap<String, AttributeValue>> = None;
    let pk = format!("IMAGE#{}", image_id);

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
            .map_err(|e| format!("Failed querying annotations for image {}: {}", image_id, e))?;

        for item in result.items() {
            let sk = item
                .get("SK")
                .and_then(|v| v.as_s().ok())
                .map(|s| s.as_str())
                .unwrap_or("");
            let annotation_id = sk.strip_prefix("ANNOTATION#").unwrap_or_default();

            let label_name = item
                .get("label_name")
                .and_then(|v| v.as_s().ok())
                .map(|s| s.to_string())
                .or_else(|| {
                    item.get("label_id")
                        .and_then(|v| v.as_s().ok())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| "unknown".to_string());

            let geometry_raw = item
                .get("geometry")
                .and_then(|v| v.as_s().ok())
                .ok_or_else(|| {
                    format!(
                        "Annotation {} on image {} is missing geometry",
                        annotation_id, image_id
                    )
                })?;

            let geometry: Geometry = serde_json::from_str(geometry_raw).map_err(|e| {
                format!(
                    "Annotation {} on image {} has invalid geometry: {}",
                    annotation_id, image_id, e
                )
            })?;

            annotation_count += 1;
            *labels_count.entry(label_name.clone()).or_insert(0) += 1;
            match geometry {
                Geometry::BBox { .. } => {
                    *bbox_count.entry(label_name).or_insert(0) += 1;
                }
                Geometry::Polygon { .. } => {
                    *polygon_count.entry(label_name).or_insert(0) += 1;
                }
            }
        }

        match result.last_evaluated_key() {
            Some(next_key) => last_evaluated_key = Some(next_key.clone()),
            None => break,
        }
    }

    Ok((annotation_count, labels_count, bbox_count, polygon_count))
}

async fn update_image_counters(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    image_id: &str,
    annotation_count: u64,
    labels_count: &HashMap<String, u64>,
    bbox_count: &HashMap<String, u64>,
    polygon_count: &HashMap<String, u64>,
) -> Result<(), String> {
    client
        .update_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(format!("BLOCK#{}", block_id)))
        .key("SK", AttributeValue::S(format!("IMAGE#{}", image_id)))
        .update_expression("SET annotation_count = :annotation_count, labels_count = :labels_count, bbox_count = :bbox_count, polygon_count = :polygon_count")
        .expression_attribute_values(
            ":annotation_count",
            AttributeValue::N(annotation_count.to_string()),
        )
        .expression_attribute_values(":labels_count", number_map_attribute(labels_count))
        .expression_attribute_values(":bbox_count", number_map_attribute(bbox_count))
        .expression_attribute_values(":polygon_count", number_map_attribute(polygon_count))
        .send()
        .await
        .map_err(|e| format!("Failed updating counters for image {}: {}", image_id, e))?;

    Ok(())
}

async fn update_block_annotation_count(
    client: &DynamoClient,
    table_name: &str,
    project_id: &str,
    block_id: &str,
    annotation_count: u64,
    image_count: u64,
    approved_image_count: u64,
) -> Result<(), String> {
    client
        .update_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(format!("PROJECT#{}", project_id)))
        .key("SK", AttributeValue::S(format!("BLOCK#{}", block_id)))
        .update_expression("SET annotation_count = :annotation_count, image_count = :image_count, approved_image_count = :approved_image_count, block_updated_at = :updated_at")
        .expression_attribute_values(
            ":annotation_count",
            AttributeValue::N(annotation_count.to_string()),
        )
        .expression_attribute_values(":image_count", AttributeValue::N(image_count.to_string()))
        .expression_attribute_values(
            ":approved_image_count",
            AttributeValue::N(approved_image_count.to_string()),
        )
        .expression_attribute_values(":updated_at", AttributeValue::S(now_rfc3339()))
        .send()
        .await
        .map_err(|e| format!("Failed updating block counters: {}", e))?;

    Ok(())
}

async fn update_reconcile_progress(
    client: &DynamoClient,
    table_name: &str,
    reconcile_job_id: &str,
    images_total: u64,
    images_processed: u64,
    annotations_total: u64,
    annotations_processed: u64,
) -> Result<(), String> {
    client
        .update_item()
        .table_name(table_name)
        .key(
            "PK",
            AttributeValue::S(format!("RECONCILE_JOB#{}", reconcile_job_id)),
        )
        .key("SK", AttributeValue::S("META".to_string()))
        .update_expression("SET #status = :status, #phase = :phase, images_total = :images_total, images_processed = :images_processed, annotations_total = :annotations_total, annotations_processed = :annotations_processed, block_annotation_count = :block_annotation_count, updated_at = :updated_at REMOVE error_message")
        .expression_attribute_names("#status", "status")
        .expression_attribute_names("#phase", "phase")
        .expression_attribute_values(
            ":status",
            AttributeValue::S(RECONCILE_STATUS_RUNNING.to_string()),
        )
        .expression_attribute_values(
            ":phase",
            AttributeValue::S(RECONCILE_PHASE_RECONCILING.to_string()),
        )
        .expression_attribute_values(":images_total", AttributeValue::N(images_total.to_string()))
        .expression_attribute_values(
            ":images_processed",
            AttributeValue::N(images_processed.to_string()),
        )
        .expression_attribute_values(
            ":annotations_total",
            AttributeValue::N(annotations_total.to_string()),
        )
        .expression_attribute_values(
            ":annotations_processed",
            AttributeValue::N(annotations_processed.to_string()),
        )
        .expression_attribute_values(
            ":block_annotation_count",
            AttributeValue::N(annotations_processed.to_string()),
        )
        .expression_attribute_values(":updated_at", AttributeValue::S(now_rfc3339()))
        .send()
        .await
        .map_err(|e| format!("Failed updating reconcile progress: {}", e))?;
    Ok(())
}

async fn mark_reconcile_completed(
    client: &DynamoClient,
    table_name: &str,
    reconcile_job_id: &str,
    images_total: u64,
    images_processed: u64,
    annotations_total: u64,
    annotations_processed: u64,
) -> Result<(), String> {
    client
        .update_item()
        .table_name(table_name)
        .key(
            "PK",
            AttributeValue::S(format!("RECONCILE_JOB#{}", reconcile_job_id)),
        )
        .key("SK", AttributeValue::S("META".to_string()))
        .update_expression("SET #status = :status, #phase = :phase, images_total = :images_total, images_processed = :images_processed, annotations_total = :annotations_total, annotations_processed = :annotations_processed, block_annotation_count = :block_annotation_count, updated_at = :updated_at REMOVE error_message")
        .expression_attribute_names("#status", "status")
        .expression_attribute_names("#phase", "phase")
        .expression_attribute_values(
            ":status",
            AttributeValue::S(RECONCILE_STATUS_COMPLETED.to_string()),
        )
        .expression_attribute_values(
            ":phase",
            AttributeValue::S(RECONCILE_PHASE_COMPLETED.to_string()),
        )
        .expression_attribute_values(":images_total", AttributeValue::N(images_total.to_string()))
        .expression_attribute_values(
            ":images_processed",
            AttributeValue::N(images_processed.to_string()),
        )
        .expression_attribute_values(
            ":annotations_total",
            AttributeValue::N(annotations_total.to_string()),
        )
        .expression_attribute_values(
            ":annotations_processed",
            AttributeValue::N(annotations_processed.to_string()),
        )
        .expression_attribute_values(
            ":block_annotation_count",
            AttributeValue::N(annotations_processed.to_string()),
        )
        .expression_attribute_values(":updated_at", AttributeValue::S(now_rfc3339()))
        .send()
        .await
        .map_err(|e| format!("Failed marking reconcile complete: {}", e))?;
    Ok(())
}

async fn load_reconcile_job_item(
    client: &DynamoClient,
    table_name: &str,
    reconcile_job_id: &str,
) -> Result<Option<HashMap<String, AttributeValue>>, String> {
    let result = client
        .get_item()
        .table_name(table_name)
        .key(
            "PK",
            AttributeValue::S(format!("RECONCILE_JOB#{}", reconcile_job_id)),
        )
        .key("SK", AttributeValue::S("META".to_string()))
        .send()
        .await
        .map_err(|e| format!("Failed loading reconcile job {}: {}", reconcile_job_id, e))?;
    Ok(result.item().cloned())
}

async fn mark_reconcile_failed(
    client: &DynamoClient,
    table_name: &str,
    reconcile_job_id: &str,
    error_message: &str,
) -> Result<(), String> {
    client
        .update_item()
        .table_name(table_name)
        .key(
            "PK",
            AttributeValue::S(format!("RECONCILE_JOB#{}", reconcile_job_id)),
        )
        .key("SK", AttributeValue::S("META".to_string()))
        .update_expression("SET #status = :status, #phase = :phase, error_message = :error_message, updated_at = :updated_at")
        .expression_attribute_names("#status", "status")
        .expression_attribute_names("#phase", "phase")
        .expression_attribute_values(
            ":status",
            AttributeValue::S(RECONCILE_STATUS_FAILED.to_string()),
        )
        .expression_attribute_values(
            ":phase",
            AttributeValue::S(RECONCILE_PHASE_FAILED.to_string()),
        )
        .expression_attribute_values(":error_message", AttributeValue::S(error_message.to_string()))
        .expression_attribute_values(":updated_at", AttributeValue::S(now_rfc3339()))
        .send()
        .await
        .map_err(|e| format!("Failed to persist reconcile failure state: {}", e))?;
    Ok(())
}

async fn release_reconcile_lock(
    client: &DynamoClient,
    table_name: &str,
    project_id: &str,
    block_id: &str,
) -> Result<(), String> {
    client
        .delete_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(format!("PROJECT#{}", project_id)))
        .key(
            "SK",
            AttributeValue::S(format!("BLOCK#{}#RECONCILE_LOCK", block_id)),
        )
        .send()
        .await
        .map_err(|e| format!("Failed releasing reconcile lock: {}", e))?;
    Ok(())
}

fn number_map_attribute(values: &HashMap<String, u64>) -> AttributeValue {
    let mut map: HashMap<String, AttributeValue> = HashMap::new();
    for (k, v) in values {
        map.insert(k.clone(), AttributeValue::N(v.to_string()));
    }
    AttributeValue::M(map)
}

fn attr_s(item: &HashMap<String, AttributeValue>, key: &str) -> Option<String> {
    item.get(key)
        .and_then(|v| v.as_s().ok())
        .map(|s| s.to_string())
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}
