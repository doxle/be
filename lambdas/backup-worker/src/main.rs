use aws_sdk_dynamodb::types::AttributeValue;
use aws_sdk_dynamodb::Client as DynamoClient;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use aws_sdk_s3::Client as S3Client;
use futures::future::join_all;
use futures::stream::{self, StreamExt};
use lambda_runtime::{run, service_fn, Error, LambdaEvent};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Cursor, Write};
use std::time::Duration;
use tokio::time::sleep;
use zip::write::{FileOptions, ZipWriter};

const BACKUP_STATUS_RUNNING: &str = "running";
const BACKUP_STATUS_COMPLETED: &str = "completed";
const BACKUP_STATUS_FAILED: &str = "failed";
const BACKUP_PHASE_PILOT: &str = "pilot_finalize";
const BACKUP_PHASE_COPY_IMAGES: &str = "copy_images";
const BACKUP_PHASE_FINALIZE: &str = "finalize_metadata";
const BACKUP_PHASE_COMPLETED: &str = "completed";
const BACKUP_PHASE_FAILED: &str = "failed";
const PILOT_IMAGE_LIMIT: usize = 1;
const PILOT_ANNOTATION_LIMIT: usize = 8;
const MULTIPART_THRESHOLD_BYTES: usize = 32 * 1024 * 1024;
const MULTIPART_PART_SIZE_BYTES: usize = 8 * 1024 * 1024;
const S3_UPLOAD_RETRIES: usize = 3;

#[derive(Debug, Deserialize)]
struct WorkerInput {
    action: String,
    backup_id: String,
    project_id: String,
    block_id: String,
    #[serde(default)]
    continuation_token: Option<String>,
    #[serde(default)]
    batch_size: Option<i32>,
}

#[derive(Debug, Serialize)]
struct WorkerOutput {
    done: bool,
    include_image_copy: bool,
    copied_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_continuation_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact_s3_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pilot_artifact_s3_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    annotation_count: Option<u64>,
}

#[derive(Debug, Serialize)]
struct ExportBundle {
    format: String,
    exported_at: String,
    project_id: String,
    block_id: String,
    block: serde_json::Value,
    labels: Vec<ExportLabel>,
    tasks: Vec<ExportTask>,
    images: Vec<ExportImage>,
}

#[derive(Debug, Serialize)]
struct ExportLabel {
    label_id: String,
    label_name: String,
    label_color: Option<String>,
}

#[derive(Debug, Serialize)]
struct ExportTask {
    task_id: String,
    task_name: String,
    task_state: Option<String>,
}

#[derive(Debug, Serialize)]
struct ExportAnnotation {
    label_name: String,
    geometry: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct ExportImage {
    image_id: String,
    image_name: String,
    s3_key: String,
    width: Option<u32>,
    height: Option<u32>,
    task_id: Option<String>,
    order: Option<i32>,
    annotations: Vec<ExportAnnotation>,
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
    let bucket = std::env::var("S3_BUCKET_NAME").unwrap_or_else(|_| "doxle-app".to_string());
    let batch_size = input.batch_size.unwrap_or(250).clamp(1, 1000);

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
        "init" => {
            init_backup_job(
                &dynamo,
                &table_name,
                &input.backup_id,
                &input.project_id,
                &input.block_id,
            )
            .await
        }
        "pilot" => {
            run_pilot_finalize(
                &dynamo,
                &s3,
                &table_name,
                &bucket,
                &input.backup_id,
                &input.project_id,
                &input.block_id,
            )
            .await
        }
        "copy_batch" => {
            copy_image_batch(
                &dynamo,
                &s3,
                &table_name,
                &bucket,
                &input.backup_id,
                &input.block_id,
                input.continuation_token,
                batch_size,
            )
            .await
        }
        "finalize" => {
            finalize_backup(
                &dynamo,
                &s3,
                &table_name,
                &bucket,
                &input.backup_id,
                &input.project_id,
                &input.block_id,
            )
            .await
        }
        other => Err(format!("Unknown action '{}'", other)),
    };

    match result {
        Ok(output) => Ok(output),
        Err(err) => {
            let _ = mark_backup_failed(&dynamo, &table_name, &input.backup_id, &err).await;
            Err(err.into())
        }
    }
}

async fn init_backup_job(
    dynamo: &DynamoClient,
    table_name: &str,
    backup_id: &str,
    project_id: &str,
    block_id: &str,
) -> Result<WorkerOutput, String> {
    let job_item = load_backup_job_item(dynamo, table_name, backup_id)
        .await?
        .ok_or_else(|| format!("Backup job '{}' not found", backup_id))?;
    let include_image_copy = attr_bool(&job_item, "include_image_copy").unwrap_or(true);
    let images_total = fetch_block_image_count(dynamo, table_name, project_id, block_id).await?;
    let phase = if include_image_copy {
        BACKUP_PHASE_COPY_IMAGES
    } else {
        BACKUP_PHASE_FINALIZE
    };

    let now = now_rfc3339();
    dynamo
        .update_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(format!("BACKUP_JOB#{}", backup_id)))
        .key("SK", AttributeValue::S("META".to_string()))
        .update_expression("SET #status = :status, #phase = :phase, images_total = :images_total, images_copied = :zero, annotations_total = :zero, annotations_exported = :zero, updated_at = :updated_at REMOVE error_message, artifact_s3_key, image_copy_token")
        .expression_attribute_names("#status", "status")
        .expression_attribute_names("#phase", "phase")
        .expression_attribute_values(":status", AttributeValue::S(BACKUP_STATUS_RUNNING.to_string()))
        .expression_attribute_values(":phase", AttributeValue::S(phase.to_string()))
        .expression_attribute_values(":images_total", AttributeValue::N(images_total.to_string()))
        .expression_attribute_values(":zero", AttributeValue::N("0".to_string()))
        .expression_attribute_values(":updated_at", AttributeValue::S(now))
        .send()
        .await
        .map_err(|e| format!("Failed to update backup job state: {}", e))?;

    Ok(WorkerOutput {
        done: !include_image_copy,
        include_image_copy,
        copied_count: 0,
        next_continuation_token: None,
        artifact_s3_key: None,
        pilot_artifact_s3_key: None,
        image_count: Some(images_total),
        annotation_count: Some(0),
    })
}

async fn run_pilot_finalize(
    dynamo: &DynamoClient,
    s3: &S3Client,
    table_name: &str,
    bucket: &str,
    backup_id: &str,
    project_id: &str,
    block_id: &str,
) -> Result<WorkerOutput, String> {
    let job_item = load_backup_job_item(dynamo, table_name, backup_id)
        .await?
        .ok_or_else(|| format!("Backup job '{}' not found", backup_id))?;
    let include_image_copy = attr_bool(&job_item, "include_image_copy").unwrap_or(true);

    let now = now_rfc3339();
    dynamo
        .update_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(format!("BACKUP_JOB#{}", backup_id)))
        .key("SK", AttributeValue::S("META".to_string()))
        .update_expression("SET #status = :status, #phase = :phase, updated_at = :updated_at REMOVE error_message")
        .expression_attribute_names("#status", "status")
        .expression_attribute_names("#phase", "phase")
        .expression_attribute_values(":status", AttributeValue::S(BACKUP_STATUS_RUNNING.to_string()))
        .expression_attribute_values(":phase", AttributeValue::S(BACKUP_PHASE_PILOT.to_string()))
        .expression_attribute_values(":updated_at", AttributeValue::S(now))
        .send()
        .await
        .map_err(|e| format!("Failed to mark backup pilot phase: {}", e))?;

    let (pilot_zip_bytes, image_count, annotation_count) = build_pilot_backup_zip(
        dynamo,
        table_name,
        project_id,
        block_id,
        backup_id,
        include_image_copy,
    )
    .await?;

    let pilot_key = format!(
        "backups/{}/pilot/doxle_export_pilot_{}.zip",
        backup_id,
        chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
    );

    upload_artifact_with_retries(s3, bucket, &pilot_key, pilot_zip_bytes).await?;

    dynamo
        .update_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(format!("BACKUP_JOB#{}", backup_id)))
        .key("SK", AttributeValue::S("META".to_string()))
        .update_expression("SET pilot_artifact_s3_key = :pilot_key, updated_at = :updated_at")
        .expression_attribute_values(":pilot_key", AttributeValue::S(pilot_key.clone()))
        .expression_attribute_values(":updated_at", AttributeValue::S(now_rfc3339()))
        .send()
        .await
        .map_err(|e| format!("Failed storing pilot artifact key: {}", e))?;

    Ok(WorkerOutput {
        done: true,
        include_image_copy,
        copied_count: 0,
        next_continuation_token: None,
        artifact_s3_key: None,
        pilot_artifact_s3_key: Some(pilot_key),
        image_count: Some(image_count),
        annotation_count: Some(annotation_count),
    })
}

async fn copy_image_batch(
    dynamo: &DynamoClient,
    s3: &S3Client,
    table_name: &str,
    bucket: &str,
    backup_id: &str,
    block_id: &str,
    continuation_token: Option<String>,
    batch_size: i32,
) -> Result<WorkerOutput, String> {
    let job_item = load_backup_job_item(dynamo, table_name, backup_id)
        .await?
        .ok_or_else(|| format!("Backup job '{}' not found", backup_id))?;
    let include_image_copy = attr_bool(&job_item, "include_image_copy").unwrap_or(true);

    if !include_image_copy {
        return Ok(WorkerOutput {
            done: true,
            include_image_copy,
            copied_count: 0,
            next_continuation_token: None,
            artifact_s3_key: None,
            pilot_artifact_s3_key: None,
            image_count: None,
            annotation_count: None,
        });
    }

    let token = continuation_token.or_else(|| attr_s(&job_item, "image_copy_token"));
    let prefix = format!("annotations/blocks/{}/images/", block_id);

    let mut list_request = s3
        .list_objects_v2()
        .bucket(bucket)
        .prefix(&prefix)
        .max_keys(batch_size);
    if let Some(token) = token {
        list_request = list_request.continuation_token(token);
    }

    let list_result = list_request
        .send()
        .await
        .map_err(|e| format!("Failed listing source images for backup copy: {}", e))?;

    let mut copied_count = 0u64;
    for object in list_result.contents() {
        let Some(source_key) = object.key() else {
            continue;
        };
        if source_key.ends_with('/') {
            continue;
        }
        let file_name = source_key.rsplit('/').next().unwrap_or(source_key);
        let backup_image_key = format!("backups/{}/images/{}", backup_id, file_name);
        let copy_source = format!("{}/{}", bucket, source_key);

        s3.copy_object()
            .bucket(bucket)
            .key(&backup_image_key)
            .copy_source(copy_source)
            .send()
            .await
            .map_err(|e| {
                format!(
                    "Failed copying '{}' to backup path '{}': {}",
                    source_key, backup_image_key, e
                )
            })?;

        copied_count += 1;
    }

    let next_token = list_result
        .next_continuation_token()
        .map(|s| s.to_string());
    let done = next_token.is_none();
    let now = now_rfc3339();

    if done {
        dynamo
            .update_item()
            .table_name(table_name)
            .key("PK", AttributeValue::S(format!("BACKUP_JOB#{}", backup_id)))
            .key("SK", AttributeValue::S("META".to_string()))
            .update_expression("SET #phase = :phase, updated_at = :updated_at REMOVE image_copy_token ADD images_copied :copied")
            .expression_attribute_names("#phase", "phase")
            .expression_attribute_values(":phase", AttributeValue::S(BACKUP_PHASE_FINALIZE.to_string()))
            .expression_attribute_values(":updated_at", AttributeValue::S(now))
            .expression_attribute_values(":copied", AttributeValue::N(copied_count.to_string()))
            .send()
            .await
            .map_err(|e| format!("Failed updating backup copy progress: {}", e))?;
    } else {
        let next_token_value = next_token.clone().unwrap_or_default();
        dynamo
            .update_item()
            .table_name(table_name)
            .key("PK", AttributeValue::S(format!("BACKUP_JOB#{}", backup_id)))
            .key("SK", AttributeValue::S("META".to_string()))
            .update_expression("SET #phase = :phase, image_copy_token = :token, updated_at = :updated_at ADD images_copied :copied")
            .expression_attribute_names("#phase", "phase")
            .expression_attribute_values(":phase", AttributeValue::S(BACKUP_PHASE_COPY_IMAGES.to_string()))
            .expression_attribute_values(":token", AttributeValue::S(next_token_value))
            .expression_attribute_values(":updated_at", AttributeValue::S(now))
            .expression_attribute_values(":copied", AttributeValue::N(copied_count.to_string()))
            .send()
            .await
            .map_err(|e| format!("Failed updating backup copy checkpoint: {}", e))?;
    }

    Ok(WorkerOutput {
        done,
        include_image_copy,
        copied_count,
        next_continuation_token: next_token,
        artifact_s3_key: None,
        pilot_artifact_s3_key: None,
        image_count: None,
        annotation_count: None,
    })
}

async fn finalize_backup(
    dynamo: &DynamoClient,
    s3: &S3Client,
    table_name: &str,
    bucket: &str,
    backup_id: &str,
    project_id: &str,
    block_id: &str,
) -> Result<WorkerOutput, String> {
    let job_item = load_backup_job_item(dynamo, table_name, backup_id)
        .await?
        .ok_or_else(|| format!("Backup job '{}' not found", backup_id))?;
    let include_image_copy = attr_bool(&job_item, "include_image_copy").unwrap_or(true);

    let now = now_rfc3339();
    dynamo
        .update_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(format!("BACKUP_JOB#{}", backup_id)))
        .key("SK", AttributeValue::S("META".to_string()))
        .update_expression("SET #status = :status, #phase = :phase, updated_at = :updated_at REMOVE error_message")
        .expression_attribute_names("#status", "status")
        .expression_attribute_names("#phase", "phase")
        .expression_attribute_values(":status", AttributeValue::S(BACKUP_STATUS_RUNNING.to_string()))
        .expression_attribute_values(":phase", AttributeValue::S(BACKUP_PHASE_FINALIZE.to_string()))
        .expression_attribute_values(":updated_at", AttributeValue::S(now))
        .send()
        .await
        .map_err(|e| format!("Failed to mark backup as finalizing: {}", e))?;

    let (zip_bytes, image_count, annotation_count) =
        build_backup_zip(dynamo, table_name, project_id, block_id, backup_id, include_image_copy).await?;

    let artifact_key = format!(
        "backups/{}/doxle_export_{}.zip",
        backup_id,
        chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
    );
    upload_artifact_with_retries(s3, bucket, &artifact_key, zip_bytes).await?;

    let now = now_rfc3339();
    dynamo
        .update_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(format!("BACKUP_JOB#{}", backup_id)))
        .key("SK", AttributeValue::S("META".to_string()))
        .update_expression("SET #status = :status, #phase = :phase, artifact_s3_key = :artifact_key, images_total = :images_total, annotations_total = :annotations_total, annotations_exported = :annotations_exported, updated_at = :updated_at REMOVE error_message, image_copy_token")
        .expression_attribute_names("#status", "status")
        .expression_attribute_names("#phase", "phase")
        .expression_attribute_values(":status", AttributeValue::S(BACKUP_STATUS_COMPLETED.to_string()))
        .expression_attribute_values(":phase", AttributeValue::S(BACKUP_PHASE_COMPLETED.to_string()))
        .expression_attribute_values(":artifact_key", AttributeValue::S(artifact_key.clone()))
        .expression_attribute_values(":images_total", AttributeValue::N(image_count.to_string()))
        .expression_attribute_values(":annotations_total", AttributeValue::N(annotation_count.to_string()))
        .expression_attribute_values(":annotations_exported", AttributeValue::N(annotation_count.to_string()))
        .expression_attribute_values(":updated_at", AttributeValue::S(now))
        .send()
        .await
        .map_err(|e| format!("Failed updating completed backup state: {}", e))?;

    Ok(WorkerOutput {
        done: true,
        include_image_copy,
        copied_count: 0,
        next_continuation_token: None,
        artifact_s3_key: Some(artifact_key),
        pilot_artifact_s3_key: None,
        image_count: Some(image_count),
        annotation_count: Some(annotation_count),
    })
}

async fn build_backup_zip(
    dynamo: &DynamoClient,
    table_name: &str,
    project_id: &str,
    block_id: &str,
    backup_id: &str,
    include_image_copy: bool,
) -> Result<(Vec<u8>, u64, u64), String> {
    let block = fetch_block(dynamo, table_name, project_id, block_id)
        .await?
        .ok_or_else(|| "Block not found while finalizing backup".to_string())?;

    let label_items = fetch_all_json(dynamo, table_name, &format!("BLOCK#{}", block_id), "LABEL#").await?;
    let labels: Vec<ExportLabel> = label_items.iter().filter_map(to_export_label).collect();

    let task_items = fetch_all_json(dynamo, table_name, &format!("BLOCK#{}", block_id), "TASK#").await?;
    let tasks: Vec<ExportTask> = task_items.iter().filter_map(to_export_task).collect();

    let image_items = fetch_all_json(dynamo, table_name, &format!("BLOCK#{}", block_id), "IMAGE#").await?;
    let image_count = image_items.len();

    let image_futures: Vec<_> = image_items
        .iter()
        .enumerate()
        .filter_map(|(idx, item)| {
            let image_id = item
                .get("SK")
                .and_then(|v| v.as_str())
                .and_then(|s| s.strip_prefix("IMAGE#"))
                .unwrap_or_default()
                .to_string();
            if image_id.is_empty() {
                return None;
            }

            let original_s3_key = item
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let mapped_s3_key =
                map_export_s3_key(&original_s3_key, backup_id, include_image_copy);
            let image_name = item
                .get("image_name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let width = item.get("width").and_then(|v| v.as_f64()).map(|n| n as u32);
            let height = item.get("height").and_then(|v| v.as_f64()).map(|n| n as u32);
            let task_id = item
                .get("task_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let order = item.get("order").and_then(|v| v.as_f64()).map(|n| n as i32);
            let dynamo_client = dynamo.clone();
            let table = table_name.to_string();
            let total = image_count;

            Some(async move {
                let ann_items = fetch_all_json(
                    &dynamo_client,
                    &table,
                    &format!("IMAGE#{}", image_id),
                    "ANNOTATION#",
                )
                .await
                .unwrap_or_else(|e| {
                    tracing::error!(
                        "Failed to fetch annotations for image [{} / {}] {}: {}",
                        idx + 1,
                        total,
                        image_name,
                        e
                    );
                    vec![]
                });

                let annotations: Vec<ExportAnnotation> =
                    ann_items.iter().filter_map(to_export_annotation).collect();

                ExportImage {
                    image_id,
                    image_name,
                    s3_key: mapped_s3_key,
                    width,
                    height,
                    task_id,
                    order,
                    annotations,
                }
            })
        })
        .collect();

    const CONCURRENCY_LIMIT: usize = 24;
    let images: Vec<ExportImage> = stream::iter(image_futures)
        .buffer_unordered(CONCURRENCY_LIMIT)
        .collect()
        .await;
    let image_total = images.len() as u64;
    let total_annotations: u64 = images.iter().map(|i| i.annotations.len() as u64).sum();

    let bundle = ExportBundle {
        format: "doxle_export_v1".to_string(),
        exported_at: now_rfc3339(),
        project_id: project_id.to_string(),
        block_id: block_id.to_string(),
        block,
        labels,
        tasks,
        images,
    };

    let json_bytes = serde_json::to_vec(&bundle).map_err(|e| format!("Failed to serialize backup JSON: {}", e))?;
    let zip_bytes = zip_export_json(&json_bytes)?;
    Ok((zip_bytes, image_total, total_annotations))
}

async fn build_pilot_backup_zip(
    dynamo: &DynamoClient,
    table_name: &str,
    project_id: &str,
    block_id: &str,
    backup_id: &str,
    include_image_copy: bool,
) -> Result<(Vec<u8>, u64, u64), String> {
    let block = fetch_block(dynamo, table_name, project_id, block_id)
        .await?
        .ok_or_else(|| "Block not found while building pilot backup".to_string())?;

    let label_items =
        fetch_json_with_limit(dynamo, table_name, &format!("BLOCK#{}", block_id), "LABEL#", 8).await?;
    let labels: Vec<ExportLabel> = label_items.iter().filter_map(to_export_label).collect();

    let task_items =
        fetch_json_with_limit(dynamo, table_name, &format!("BLOCK#{}", block_id), "TASK#", 4).await?;
    let tasks: Vec<ExportTask> = task_items.iter().filter_map(to_export_task).collect();

    let image_items = fetch_json_with_limit(
        dynamo,
        table_name,
        &format!("BLOCK#{}", block_id),
        "IMAGE#",
        PILOT_IMAGE_LIMIT,
    )
    .await?;

    let mut images: Vec<ExportImage> = Vec::new();
    for item in image_items {
        let image_id = item
            .get("SK")
            .and_then(|v| v.as_str())
            .and_then(|s| s.strip_prefix("IMAGE#"))
            .unwrap_or_default()
            .to_string();
        if image_id.is_empty() {
            continue;
        }

        let original_s3_key = item
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let mapped_s3_key = map_export_s3_key(&original_s3_key, backup_id, include_image_copy);
        let image_name = item
            .get("image_name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let width = item.get("width").and_then(|v| v.as_f64()).map(|n| n as u32);
        let height = item.get("height").and_then(|v| v.as_f64()).map(|n| n as u32);
        let task_id = item
            .get("task_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let order = item.get("order").and_then(|v| v.as_f64()).map(|n| n as i32);

        let ann_items = fetch_json_with_limit(
            dynamo,
            table_name,
            &format!("IMAGE#{}", image_id),
            "ANNOTATION#",
            PILOT_ANNOTATION_LIMIT,
        )
        .await?;
        let annotations: Vec<ExportAnnotation> = ann_items.iter().filter_map(to_export_annotation).collect();

        images.push(ExportImage {
            image_id,
            image_name,
            s3_key: mapped_s3_key,
            width,
            height,
            task_id,
            order,
            annotations,
        });
    }

    let image_total = images.len() as u64;
    let total_annotations: u64 = images.iter().map(|i| i.annotations.len() as u64).sum();
    let bundle = ExportBundle {
        format: "doxle_export_v1_pilot".to_string(),
        exported_at: now_rfc3339(),
        project_id: project_id.to_string(),
        block_id: block_id.to_string(),
        block,
        labels,
        tasks,
        images,
    };

    let json_bytes = serde_json::to_vec(&bundle)
        .map_err(|e| format!("Failed to serialize pilot backup JSON: {}", e))?;
    let zip_bytes = zip_export_json(&json_bytes)?;
    Ok((zip_bytes, image_total, total_annotations))
}

async fn upload_artifact_with_retries(
    s3: &S3Client,
    bucket: &str,
    key: &str,
    bytes: Vec<u8>,
) -> Result<(), String> {
    let size = bytes.len();
    for attempt in 1..=S3_UPLOAD_RETRIES {
        let upload_result = if size >= MULTIPART_THRESHOLD_BYTES {
            put_object_multipart(s3, bucket, key, &bytes).await
        } else {
            s3.put_object()
                .bucket(bucket)
                .key(key)
                .body(ByteStream::from(bytes.clone()))
                .content_type("application/zip")
                .send()
                .await
                .map(|_| ())
                .map_err(|e| format!("{:?}", e))
        };

        match upload_result {
            Ok(()) => return Ok(()),
            Err(err) => {
                if attempt == S3_UPLOAD_RETRIES {
                    return Err(format!(
                        "Failed uploading backup artifact to s3://{}/{} ({} bytes) after {} attempts: {}",
                        bucket, key, size, S3_UPLOAD_RETRIES, err
                    ));
                }
                tracing::warn!(
                    "Upload attempt {} failed for s3://{}/{} ({} bytes): {}",
                    attempt,
                    bucket,
                    key,
                    size,
                    err
                );
                sleep(Duration::from_secs((attempt as u64) * 2)).await;
            }
        }
    }

    Err("Unexpected upload retry state".to_string())
}

async fn put_object_multipart(
    s3: &S3Client,
    bucket: &str,
    key: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let create = s3
        .create_multipart_upload()
        .bucket(bucket)
        .key(key)
        .content_type("application/zip")
        .send()
        .await
        .map_err(|e| format!("multipart create failed: {:?}", e))?;

    let upload_id = create
        .upload_id()
        .ok_or_else(|| "multipart create did not return upload_id".to_string())?
        .to_string();

    let mut parts: Vec<CompletedPart> = Vec::new();
    let mut part_number: i32 = 1;
    for chunk in bytes.chunks(MULTIPART_PART_SIZE_BYTES) {
        let upload_part_res = s3
            .upload_part()
            .bucket(bucket)
            .key(key)
            .upload_id(&upload_id)
            .part_number(part_number)
            .body(ByteStream::from(chunk.to_vec()))
            .send()
            .await;

        let upload_part = match upload_part_res {
            Ok(resp) => resp,
            Err(err) => {
                let _ = s3
                    .abort_multipart_upload()
                    .bucket(bucket)
                    .key(key)
                    .upload_id(&upload_id)
                    .send()
                    .await;
                return Err(format!(
                    "multipart upload_part {} failed: {:?}",
                    part_number, err
                ));
            }
        };

        let etag = upload_part
            .e_tag()
            .ok_or_else(|| format!("multipart part {} missing etag", part_number))?;

        parts.push(
            CompletedPart::builder()
                .part_number(part_number)
                .e_tag(etag)
                .build(),
        );
        part_number += 1;
    }

    let completed_upload = CompletedMultipartUpload::builder()
        .set_parts(Some(parts))
        .build();

    s3.complete_multipart_upload()
        .bucket(bucket)
        .key(key)
        .upload_id(upload_id)
        .multipart_upload(completed_upload)
        .send()
        .await
        .map_err(|e| format!("multipart complete failed: {:?}", e))?;

    Ok(())
}

async fn fetch_block(
    client: &DynamoClient,
    table_name: &str,
    project_id: &str,
    block_id: &str,
) -> Result<Option<serde_json::Value>, String> {
    let result = client
        .get_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(format!("PROJECT#{}", project_id)))
        .key("SK", AttributeValue::S(format!("BLOCK#{}", block_id)))
        .send()
        .await
        .map_err(|e| format!("Failed to load block item: {}", e))?;

    Ok(result.item().map(dynamo_item_to_json))
}

async fn fetch_all_json(
    client: &DynamoClient,
    table_name: &str,
    pk: &str,
    sk_prefix: &str,
) -> Result<Vec<serde_json::Value>, String> {
    let mut items = Vec::new();
    let mut last_key: Option<HashMap<String, AttributeValue>> = None;

    loop {
        let mut query = client
            .query()
            .table_name(table_name)
            .key_condition_expression("PK = :pk AND begins_with(SK, :sk)")
            .expression_attribute_values(":pk", AttributeValue::S(pk.to_string()))
            .expression_attribute_values(":sk", AttributeValue::S(sk_prefix.to_string()));

        if let Some(key) = last_key.clone() {
            query = query.set_exclusive_start_key(Some(key));
        }

        let result = query
            .send()
            .await
            .map_err(|e| format!("Failed querying {} / {}: {}", pk, sk_prefix, e))?;

        for item in result.items() {
            items.push(dynamo_item_to_json(item));
        }

        match result.last_evaluated_key() {
            Some(key) => last_key = Some(key.clone()),
            None => break,
        }
    }

    Ok(items)
}

async fn fetch_json_with_limit(
    client: &DynamoClient,
    table_name: &str,
    pk: &str,
    sk_prefix: &str,
    limit: usize,
) -> Result<Vec<serde_json::Value>, String> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let mut items = Vec::new();
    let mut last_key: Option<HashMap<String, AttributeValue>> = None;

    while items.len() < limit {
        let remaining = (limit - items.len()) as i32;
        let mut query = client
            .query()
            .table_name(table_name)
            .key_condition_expression("PK = :pk AND begins_with(SK, :sk)")
            .expression_attribute_values(":pk", AttributeValue::S(pk.to_string()))
            .expression_attribute_values(":sk", AttributeValue::S(sk_prefix.to_string()))
            .limit(remaining);

        if let Some(key) = last_key.clone() {
            query = query.set_exclusive_start_key(Some(key));
        }

        let result = query
            .send()
            .await
            .map_err(|e| format!("Failed querying {} / {} with limit: {}", pk, sk_prefix, e))?;

        for item in result.items() {
            items.push(dynamo_item_to_json(item));
            if items.len() >= limit {
                break;
            }
        }

        match result.last_evaluated_key() {
            Some(key) if items.len() < limit => last_key = Some(key.clone()),
            _ => break,
        }
    }

    Ok(items)
}

async fn fetch_block_image_count(
    client: &DynamoClient,
    table_name: &str,
    project_id: &str,
    block_id: &str,
) -> Result<u64, String> {
    let result = client
        .get_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(format!("PROJECT#{}", project_id)))
        .key("SK", AttributeValue::S(format!("BLOCK#{}", block_id)))
        .send()
        .await
        .map_err(|e| format!("Failed loading block for image_count: {}", e))?;

    let count = result
        .item()
        .and_then(|item| item.get("image_count"))
        .and_then(|value| value.as_n().ok())
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(0);
    Ok(count)
}

async fn load_backup_job_item(
    client: &DynamoClient,
    table_name: &str,
    backup_id: &str,
) -> Result<Option<HashMap<String, AttributeValue>>, String> {
    let result = client
        .get_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(format!("BACKUP_JOB#{}", backup_id)))
        .key("SK", AttributeValue::S("META".to_string()))
        .send()
        .await
        .map_err(|e| format!("Failed loading backup job {}: {}", backup_id, e))?;
    Ok(result.item().cloned())
}

async fn mark_backup_failed(
    client: &DynamoClient,
    table_name: &str,
    backup_id: &str,
    error_message: &str,
) -> Result<(), String> {
    client
        .update_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(format!("BACKUP_JOB#{}", backup_id)))
        .key("SK", AttributeValue::S("META".to_string()))
        .update_expression("SET #status = :status, #phase = :phase, error_message = :error_message, updated_at = :updated_at")
        .expression_attribute_names("#status", "status")
        .expression_attribute_names("#phase", "phase")
        .expression_attribute_values(":status", AttributeValue::S(BACKUP_STATUS_FAILED.to_string()))
        .expression_attribute_values(":phase", AttributeValue::S(BACKUP_PHASE_FAILED.to_string()))
        .expression_attribute_values(":error_message", AttributeValue::S(error_message.to_string()))
        .expression_attribute_values(":updated_at", AttributeValue::S(now_rfc3339()))
        .send()
        .await
        .map_err(|e| format!("Failed to persist backup failure state: {}", e))?;
    Ok(())
}

fn map_export_s3_key(original_key: &str, backup_id: &str, include_image_copy: bool) -> String {
    if !include_image_copy {
        return original_key.to_string();
    }
    let file_name = original_key.rsplit('/').next().unwrap_or(original_key);
    format!("backups/{}/images/{}", backup_id, file_name)
}

fn to_export_label(item: &serde_json::Value) -> Option<ExportLabel> {
    let label_id = item
        .get("SK")
        .and_then(|v| v.as_str())
        .and_then(|sk| sk.strip_prefix("LABEL#"))
        .unwrap_or_default()
        .to_string();
    let label_name = item
        .get("label_name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    if label_name.is_empty() {
        return None;
    }

    Some(ExportLabel {
        label_id,
        label_name,
        label_color: item
            .get("label_color")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    })
}

fn to_export_task(item: &serde_json::Value) -> Option<ExportTask> {
    let task_id = item
        .get("SK")
        .and_then(|v| v.as_str())
        .and_then(|sk| sk.strip_prefix("TASK#"))
        .unwrap_or_default()
        .to_string();
    let task_name = item
        .get("task_name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    if task_name.is_empty() {
        return None;
    }

    Some(ExportTask {
        task_id,
        task_name,
        task_state: item
            .get("task_state")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    })
}

fn to_export_annotation(item: &serde_json::Value) -> Option<ExportAnnotation> {
    let label_name = item
        .get("label_name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    if label_name.is_empty() {
        return None;
    }

    let geometry = match item.get("geometry") {
        Some(serde_json::Value::String(s)) => serde_json::from_str::<serde_json::Value>(s)
            .unwrap_or(serde_json::Value::String(s.clone())),
        Some(other) => other.clone(),
        None => return None,
    };

    Some(ExportAnnotation {
        label_name,
        geometry,
    })
}

fn zip_export_json(json_bytes: &[u8]) -> Result<Vec<u8>, String> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    writer
        .start_file("doxle_export.json", options)
        .map_err(|e| format!("Zip write error: {}", e))?;
    writer
        .write_all(json_bytes)
        .map_err(|e| format!("Zip write error: {}", e))?;

    let cursor = writer
        .finish()
        .map_err(|e| format!("Zip finish error: {}", e))?;
    Ok(cursor.into_inner())
}

fn dynamo_item_to_json(item: &HashMap<String, AttributeValue>) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (key, value) in item {
        if key == "PK" {
            continue;
        }
        map.insert(key.clone(), attribute_to_json(value));
    }
    serde_json::Value::Object(map)
}

fn attribute_to_json(attr: &AttributeValue) -> serde_json::Value {
    match attr {
        AttributeValue::S(s) => serde_json::Value::String(s.clone()),
        AttributeValue::N(n) => {
            if let Ok(i) = n.parse::<i64>() {
                serde_json::Value::Number(i.into())
            } else if let Ok(f) = n.parse::<f64>() {
                serde_json::Number::from_f64(f)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::String(n.clone()))
            } else {
                serde_json::Value::String(n.clone())
            }
        }
        AttributeValue::Bool(b) => serde_json::Value::Bool(*b),
        AttributeValue::M(m) => dynamo_item_to_json(m),
        AttributeValue::L(l) => serde_json::Value::Array(l.iter().map(attribute_to_json).collect()),
        AttributeValue::Null(_) => serde_json::Value::Null,
        _ => serde_json::Value::Null,
    }
}

fn attr_s(item: &HashMap<String, AttributeValue>, key: &str) -> Option<String> {
    item.get(key)
        .and_then(|v| v.as_s().ok())
        .map(|s| s.to_string())
}

fn attr_bool(item: &HashMap<String, AttributeValue>, key: &str) -> Option<bool> {
    item.get(key).and_then(|v| v.as_bool().ok()).copied()
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}
