use aws_sdk_dynamodb::types::AttributeValue;
use aws_sdk_dynamodb::Client as DynamoClient;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client as S3Client;
use lambda_http::{http::StatusCode, Body, Error, Response};
use serde::Serialize;
use std::io::{Cursor, Write};
use zip::write::{FileOptions, ZipWriter};

fn get_bucket_name() -> String {
    std::env::var("S3_BUCKET_NAME").unwrap_or_else(|_| "doxle-app".to_string())
}

#[derive(Serialize)]
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

#[derive(Serialize)]
struct ExportLabel {
    label_id: String,
    label_name: String,
    label_color: Option<String>,
}

#[derive(Serialize)]
struct ExportTask {
    task_id: String,
    task_name: String,
    task_state: Option<String>,
}

#[derive(Serialize)]
struct ExportAnnotation {
    label_name: String,
    geometry: serde_json::Value,
}

#[derive(Serialize)]
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

/// GET /projects/{pid}/blocks/{bid}/export
pub async fn export_block(
    dynamo_client: &DynamoClient,
    s3_client: &S3Client,
    table_name: &str,
    project_id: &str,
    block_id: &str,
) -> Result<Response<Body>, Error> {
    let bucket = get_bucket_name();
    let start = std::time::Instant::now();
    tracing::info!("📦 [1/7] Starting export for block={} bucket={}", block_id, bucket);

    let block = fetch_block(dynamo_client, table_name, project_id, block_id).await?;
    tracing::info!(
        "📦 [2/7] Fetched block metadata: is_null={} ({:.0?})",
        block.is_null(),
        start.elapsed()
    );
    if block.is_null() {
        return ok_json_status(
            serde_json::json!({"error": "Block not found"}),
            StatusCode::NOT_FOUND,
        );
    }

    let label_items = fetch_all(
        dynamo_client,
        table_name,
        &format!("BLOCK#{}", block_id),
        "LABEL#",
    )
    .await?;
    let labels: Vec<ExportLabel> = label_items.iter().filter_map(to_export_label).collect();
    tracing::info!(
        "📦 [3/7] Fetched {} labels ({:.0?})",
        labels.len(),
        start.elapsed()
    );

    let task_items = fetch_all(
        dynamo_client,
        table_name,
        &format!("BLOCK#{}", block_id),
        "TASK#",
    )
    .await?;
    let tasks: Vec<ExportTask> = task_items.iter().filter_map(to_export_task).collect();
    tracing::info!(
        "📦 [4/7] Fetched {} tasks ({:.0?})",
        tasks.len(),
        start.elapsed()
    );

    let image_items = fetch_all(
        dynamo_client,
        table_name,
        &format!("BLOCK#{}", block_id),
        "IMAGE#",
    )
    .await?;
    let image_count = image_items.len();
    tracing::info!(
        "📦 [5/7] Fetched {} image records, loading annotations... ({:.0?})",
        image_count,
        start.elapsed()
    );

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

            let s3_key = item
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
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
            let dynamo = dynamo_client.clone();
            let table = table_name.to_string();
            let total = image_count;

            Some(async move {
                let ann_items = match fetch_all(
                    &dynamo,
                    &table,
                    &format!("IMAGE#{}", image_id),
                    "ANNOTATION#",
                )
                .await
                {
                    Ok(anns) => {
                        tracing::info!(
                            "  📝 annotations [{}/{}] {} -> {} annotations",
                            idx + 1,
                            total,
                            image_name,
                            anns.len()
                        );
                        anns
                    }
                    Err(e) => {
                        tracing::error!(
                            "  ❌ annotations FAILED [{}/{}] {} : {}",
                            idx + 1,
                            total,
                            image_name,
                            e
                        );
                        vec![]
                    }
                };

                let annotations: Vec<ExportAnnotation> =
                    ann_items.iter().filter_map(to_export_annotation).collect();

                ExportImage {
                    image_id,
                    image_name,
                    s3_key,
                    width,
                    height,
                    task_id,
                    order,
                    annotations,
                }
            })
        })
        .collect();

    tracing::info!("📦 Awaiting {} image futures...", image_futures.len());
    let images: Vec<ExportImage> = futures::future::join_all(image_futures).await;
    tracing::info!("📦 [6/7] All images done ({:.0?})", start.elapsed());

    let total_annotations: usize = images.iter().map(|i| i.annotations.len()).sum();
    tracing::info!(
        "📦 SUMMARY: {} labels, {} tasks, {} images, {} total annotations",
        labels.len(),
        tasks.len(),
        images.len(),
        total_annotations
    );

    let response = ExportBundle {
        format: "doxle_export_v1".to_string(),
        exported_at: chrono::Utc::now().to_rfc3339(),
        project_id: project_id.to_string(),
        block_id: block_id.to_string(),
        block,
        labels,
        tasks,
        images,
    };

    tracing::info!("📦 Serializing backup JSON...");
    let json_bytes = serde_json::to_vec(&response)?;
    tracing::info!(
        "📦 JSON size: {} bytes ({:.2} MB)",
        json_bytes.len(),
        json_bytes.len() as f64 / 1_048_576.0
    );

    let zip_bytes = zip_export_json(&json_bytes)
        .map_err(|e| -> Error { format!("Failed to build export zip: {}", e).into() })?;

    let export_key = format!(
        "exports/{}/doxle_export_{}.zip",
        block_id,
        chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
    );
    tracing::info!("📦 [7/7] Uploading to s3://{}/{}", bucket, export_key);

    s3_client
        .put_object()
        .bucket(&bucket)
        .key(&export_key)
        .body(ByteStream::from(zip_bytes))
        .content_type("application/zip")
        .send()
        .await
        .map_err(|e| -> Error { format!("S3 upload failed: {}", e).into() })?;

    let download_url = generate_presigned_download_url(
        s3_client,
        &bucket,
        &export_key,
        "doxle-export-backup.zip",
    )
    .await
    .map_err(|e| -> Error { format!("Presign export failed: {}", e).into() })?;

    tracing::info!("📦 ✅ Export complete ({:.0?})", start.elapsed());
    ok_json_status(serde_json::json!({ "download_url": download_url }), StatusCode::OK)
}

// ─── Helpers ───

async fn fetch_block(
    client: &DynamoClient,
    table_name: &str,
    project_id: &str,
    block_id: &str,
) -> Result<serde_json::Value, Error> {
    let result = client
        .get_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(format!("PROJECT#{}", project_id)))
        .key("SK", AttributeValue::S(format!("BLOCK#{}", block_id)))
        .send()
        .await?;

    match result.item() {
        Some(item) => Ok(dynamo_item_to_json(item)),
        None => Ok(serde_json::Value::Null),
    }
}

async fn fetch_all(
    client: &DynamoClient,
    table_name: &str,
    pk: &str,
    sk_prefix: &str,
) -> Result<Vec<serde_json::Value>, Error> {
    let mut items = Vec::new();
    let mut last_key = None;

    loop {
        let mut query = client
            .query()
            .table_name(table_name)
            .key_condition_expression("PK = :pk AND begins_with(SK, :sk)")
            .expression_attribute_values(":pk", AttributeValue::S(pk.to_string()))
            .expression_attribute_values(":sk", AttributeValue::S(sk_prefix.to_string()));

        if let Some(key) = last_key {
            query = query.set_exclusive_start_key(Some(key));
        }

        let result = query.send().await?;

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

/// Presigned URL that forces browser download instead of displaying
async fn generate_presigned_download_url(
    s3_client: &S3Client,
    bucket: &str,
    key: &str,
    filename: &str,
) -> Result<String, String> {
    let presigned = s3_client
        .get_object()
        .bucket(bucket)
        .key(key)
        .response_content_disposition(format!("attachment; filename=\"{}\"", filename))
        .presigned(
            aws_sdk_s3::presigning::PresigningConfig::expires_in(
                std::time::Duration::from_secs(3600),
            )
            .map_err(|e| format!("Presign config error: {}", e))?,
        )
        .await
        .map_err(|e| format!("Failed to presign: {}", e))?;

    Ok(presigned.uri().to_string())
}

/// Convert a DynamoDB item to a clean JSON object (strips DynamoDB type wrappers)
fn dynamo_item_to_json(item: &std::collections::HashMap<String, AttributeValue>) -> serde_json::Value {
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

fn ok_json_status(value: serde_json::Value, status: StatusCode) -> Result<Response<Body>, Error> {
    Ok(Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(value.to_string().into())
        .map_err(Box::new)?)
}
