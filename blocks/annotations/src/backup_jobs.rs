use aws_sdk_dynamodb::types::AttributeValue;
use aws_sdk_dynamodb::Client as DynamoClient;
use aws_sdk_s3::Client as S3Client;
use lambda_http::{http::StatusCode, Body, Error, Response};
use serde::{Deserialize, Serialize};

const BACKUP_MODE_FULL: &str = "full";
const BACKUP_STATUS_QUEUED: &str = "queued";
const BACKUP_STATUS_FAILED: &str = "failed";
const BACKUP_PHASE_PENDING: &str = "pending";
const BACKUP_PHASE_FAILED: &str = "failed";

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct StartBackupRequest {
    pub mode: Option<String>,
    pub include_image_copy: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct BackupJobStatusResponse {
    pub backup_id: String,
    pub project_id: String,
    pub block_id: String,
    pub status: String,
    pub phase: String,
    pub mode: String,
    pub include_image_copy: bool,
    pub images_total: u64,
    pub images_copied: u64,
    pub annotations_total: u64,
    pub annotations_exported: u64,
    pub artifact_s3_key: Option<String>,
    pub download_url: Option<String>,
    pub execution_arn: Option<String>,
    pub error_message: Option<String>,
    pub requested_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// POST /projects/{pid}/blocks/{bid}/backups
pub async fn start_backup_job(
    client: &DynamoClient,
    table_name: &str,
    project_id: &str,
    block_id: &str,
    user_id: &str,
    body: &[u8],
) -> Result<Response<Body>, Error> {
    let req: StartBackupRequest = if body.is_empty() {
        StartBackupRequest::default()
    } else {
        serde_json::from_slice(body)?
    };

    // Ensure block exists under the given project before creating backup job.
    let block_exists = client
        .get_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(format!("PROJECT#{}", project_id)))
        .key("SK", AttributeValue::S(format!("BLOCK#{}", block_id)))
        .send()
        .await?;

    if block_exists.item().is_none() {
        return ok_json_status(
            serde_json::json!({ "error": "Block not found" }),
            StatusCode::NOT_FOUND,
        );
    }

    let backup_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let mode = req
        .mode
        .unwrap_or_else(|| BACKUP_MODE_FULL.to_string())
        .trim()
        .to_string();
    let mode = if mode.is_empty() {
        BACKUP_MODE_FULL.to_string()
    } else {
        mode
    };
    let include_image_copy = req.include_image_copy.unwrap_or(true);

    let mut put = client
        .put_item()
        .table_name(table_name)
        .item("PK", AttributeValue::S(format!("BACKUP_JOB#{}", backup_id)))
        .item("SK", AttributeValue::S("META".to_string()))
        .item("backup_id", AttributeValue::S(backup_id.clone()))
        .item("project_id", AttributeValue::S(project_id.to_string()))
        .item("block_id", AttributeValue::S(block_id.to_string()))
        .item("status", AttributeValue::S(BACKUP_STATUS_QUEUED.to_string()))
        .item("phase", AttributeValue::S(BACKUP_PHASE_PENDING.to_string()))
        .item("mode", AttributeValue::S(mode.clone()))
        .item("include_image_copy", AttributeValue::Bool(include_image_copy))
        .item("images_total", AttributeValue::N("0".to_string()))
        .item("images_copied", AttributeValue::N("0".to_string()))
        .item("annotations_total", AttributeValue::N("0".to_string()))
        .item("annotations_exported", AttributeValue::N("0".to_string()))
        .item("created_at", AttributeValue::S(now.clone()))
        .item("updated_at", AttributeValue::S(now.clone()));

    if !user_id.trim().is_empty() {
        put = put.item("requested_by", AttributeValue::S(user_id.to_string()));
    }

    put.send().await?;

    // Trigger Step Functions execution for async orchestration.
    let state_machine_arn = std::env::var("BACKUP_STATE_MACHINE_ARN").ok();
    let Some(state_machine_arn) = state_machine_arn.filter(|v| !v.trim().is_empty()) else {
        let msg = "BACKUP_STATE_MACHINE_ARN is not configured".to_string();
        set_job_failed(client, table_name, &backup_id, &msg).await?;
        return ok_json_status(
            serde_json::json!({ "error": msg, "backup_id": backup_id }),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    };

    let aws_cfg = aws_config::load_from_env().await;
    let sfn = aws_sdk_sfn::Client::new(&aws_cfg);
    let execution_input = serde_json::json!({
        "backup_id": backup_id.clone(),
        "project_id": project_id,
        "block_id": block_id
    });
    let execution_name = format!("backup-{}", backup_id.replace('-', ""));

    let execution_arn = match sfn
        .start_execution()
        .state_machine_arn(&state_machine_arn)
        .name(execution_name)
        .input(execution_input.to_string())
        .send()
        .await
    {
        Ok(resp) => Some(resp.execution_arn().to_string()),
        Err(err) => {
            let msg = format!("Failed to start backup workflow: {}", err);
            set_job_failed(client, table_name, &backup_id, &msg).await?;
            return ok_json_status(
                serde_json::json!({ "error": msg, "backup_id": backup_id }),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    if let Some(exec_arn) = execution_arn.as_ref() {
        client
            .update_item()
            .table_name(table_name)
            .key("PK", AttributeValue::S(format!("BACKUP_JOB#{}", backup_id)))
            .key("SK", AttributeValue::S("META".to_string()))
            .update_expression("SET execution_arn = :execution_arn, updated_at = :updated_at REMOVE error_message")
            .expression_attribute_values(":execution_arn", AttributeValue::S(exec_arn.clone()))
            .expression_attribute_values(":updated_at", AttributeValue::S(chrono::Utc::now().to_rfc3339()))
            .send()
            .await?;
    }

    let response = BackupJobStatusResponse {
        backup_id: backup_id.clone(),
        project_id: project_id.to_string(),
        block_id: block_id.to_string(),
        status: BACKUP_STATUS_QUEUED.to_string(),
        phase: BACKUP_PHASE_PENDING.to_string(),
        mode,
        include_image_copy,
        images_total: 0,
        images_copied: 0,
        annotations_total: 0,
        annotations_exported: 0,
        artifact_s3_key: None,
        download_url: None,
        execution_arn,
        error_message: None,
        requested_by: if user_id.trim().is_empty() {
            None
        } else {
            Some(user_id.to_string())
        },
        created_at: now.clone(),
        updated_at: now,
    };

    Ok(Response::builder()
        .status(StatusCode::ACCEPTED)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(serde_json::to_string(&response)?.into())
        .map_err(Box::new)?)
}

/// GET /projects/{pid}/blocks/{bid}/backups/{backup_id}
pub async fn get_backup_job_status(
    client: &DynamoClient,
    s3_client: &S3Client,
    table_name: &str,
    project_id: &str,
    block_id: &str,
    backup_id: &str,
) -> Result<Response<Body>, Error> {
    let result = client
        .get_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(format!("BACKUP_JOB#{}", backup_id)))
        .key("SK", AttributeValue::S("META".to_string()))
        .send()
        .await?;

    let Some(item) = result.item() else {
        return ok_json_status(
            serde_json::json!({ "error": "Backup job not found" }),
            StatusCode::NOT_FOUND,
        );
    };

    let item_project_id = attr_s(item, "project_id").unwrap_or_default();
    let item_block_id = attr_s(item, "block_id").unwrap_or_default();

    // Scope protection: only expose jobs for matching project/block path.
    if item_project_id != project_id || item_block_id != block_id {
        return ok_json_status(
            serde_json::json!({ "error": "Backup job not found" }),
            StatusCode::NOT_FOUND,
        );
    }

    let artifact_s3_key = attr_s(item, "artifact_s3_key");
    let download_url = if let Some(key) = artifact_s3_key.as_ref() {
        generate_presigned_download_url(s3_client, &get_bucket_name(), key).await.ok()
    } else {
        None
    };

    let response = BackupJobStatusResponse {
        backup_id: attr_s(item, "backup_id").unwrap_or_else(|| backup_id.to_string()),
        project_id: item_project_id,
        block_id: item_block_id,
        status: attr_s(item, "status").unwrap_or_else(|| BACKUP_STATUS_QUEUED.to_string()),
        phase: attr_s(item, "phase").unwrap_or_else(|| BACKUP_PHASE_PENDING.to_string()),
        mode: attr_s(item, "mode").unwrap_or_else(|| BACKUP_MODE_FULL.to_string()),
        include_image_copy: attr_bool(item, "include_image_copy").unwrap_or(true),
        images_total: attr_n_u64(item, "images_total").unwrap_or(0),
        images_copied: attr_n_u64(item, "images_copied").unwrap_or(0),
        annotations_total: attr_n_u64(item, "annotations_total").unwrap_or(0),
        annotations_exported: attr_n_u64(item, "annotations_exported").unwrap_or(0),
        artifact_s3_key,
        download_url,
        execution_arn: attr_s(item, "execution_arn"),
        error_message: attr_s(item, "error_message"),
        requested_by: attr_s(item, "requested_by"),
        created_at: attr_s(item, "created_at").unwrap_or_default(),
        updated_at: attr_s(item, "updated_at").unwrap_or_default(),
    };

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(serde_json::to_string(&response)?.into())
        .map_err(Box::new)?)
}

async fn set_job_failed(
    client: &DynamoClient,
    table_name: &str,
    backup_id: &str,
    message: &str,
) -> Result<(), Error> {
    client
        .update_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(format!("BACKUP_JOB#{}", backup_id)))
        .key("SK", AttributeValue::S("META".to_string()))
        .update_expression(
            "SET #status = :status, #phase = :phase, error_message = :error, updated_at = :updated_at",
        )
        .expression_attribute_names("#status", "status")
        .expression_attribute_names("#phase", "phase")
        .expression_attribute_values(":status", AttributeValue::S(BACKUP_STATUS_FAILED.to_string()))
        .expression_attribute_values(":phase", AttributeValue::S(BACKUP_PHASE_FAILED.to_string()))
        .expression_attribute_values(":error", AttributeValue::S(message.to_string()))
        .expression_attribute_values(":updated_at", AttributeValue::S(chrono::Utc::now().to_rfc3339()))
        .send()
        .await?;
    Ok(())
}

async fn generate_presigned_download_url(
    s3_client: &S3Client,
    bucket: &str,
    key: &str,
) -> Result<String, String> {
    let presigned = s3_client
        .get_object()
        .bucket(bucket)
        .key(key)
        .response_content_disposition("attachment; filename=\"doxle-export-backup.zip\"")
        .presigned(
            aws_sdk_s3::presigning::PresigningConfig::expires_in(std::time::Duration::from_secs(
                3600,
            ))
            .map_err(|e| format!("Presign config error: {}", e))?,
        )
        .await
        .map_err(|e| format!("Failed to presign backup artifact: {}", e))?;
    Ok(presigned.uri().to_string())
}

fn get_bucket_name() -> String {
    std::env::var("S3_BUCKET_NAME").unwrap_or_else(|_| "doxle-app".to_string())
}

fn attr_s(item: &std::collections::HashMap<String, AttributeValue>, key: &str) -> Option<String> {
    item.get(key)
        .and_then(|v| v.as_s().ok())
        .map(|s| s.to_string())
}

fn attr_bool(
    item: &std::collections::HashMap<String, AttributeValue>,
    key: &str,
) -> Option<bool> {
    item.get(key).and_then(|v| v.as_bool().ok()).copied()
}

fn attr_n_u64(item: &std::collections::HashMap<String, AttributeValue>, key: &str) -> Option<u64> {
    item.get(key)
        .and_then(|v| v.as_n().ok())
        .and_then(|n| n.parse::<u64>().ok())
}

fn ok_json_status(value: serde_json::Value, status: StatusCode) -> Result<Response<Body>, Error> {
    Ok(Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(value.to_string().into())
        .map_err(Box::new)?)
}
