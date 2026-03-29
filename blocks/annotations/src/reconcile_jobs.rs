use aws_sdk_dynamodb::types::AttributeValue;
use aws_sdk_dynamodb::Client as DynamoClient;
use lambda_http::{http::StatusCode, Body, Error, Response};
use serde::{Deserialize, Serialize};

const RECONCILE_STATUS_QUEUED: &str = "queued";
const RECONCILE_STATUS_FAILED: &str = "failed";
const RECONCILE_PHASE_PENDING: &str = "pending";
const RECONCILE_PHASE_FAILED: &str = "failed";
const RECONCILE_LOCK_TTL_SECONDS: i64 = 60 * 60;
const RECONCILE_JOB_TTL_SECONDS: i64 = 7 * 24 * 60 * 60;

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct StartReconcileJobRequest {
    pub force: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct ReconcileJobStatusResponse {
    pub reconcile_job_id: String,
    pub project_id: String,
    pub block_id: String,
    pub status: String,
    pub phase: String,
    pub force: bool,
    pub images_total: u64,
    pub images_processed: u64,
    pub annotations_total: u64,
    pub annotations_processed: u64,
    pub block_annotation_count: u64,
    pub execution_arn: Option<String>,
    pub error_message: Option<String>,
    pub requested_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// POST /projects/{pid}/blocks/{bid}/reconcile-counts
pub async fn start_reconcile_job(
    client: &DynamoClient,
    table_name: &str,
    project_id: &str,
    block_id: &str,
    user_id: &str,
    body: &[u8],
) -> Result<Response<Body>, Error> {
    let req: StartReconcileJobRequest = if body.is_empty() {
        StartReconcileJobRequest::default()
    } else {
        serde_json::from_slice(body)?
    };
    let force = req.force.unwrap_or(false);

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

    let reconcile_job_id = uuid::Uuid::new_v4().to_string();
    let now_dt = chrono::Utc::now();
    let now = now_dt.to_rfc3339();
    let now_unix = now_dt.timestamp();
    let reconcile_job_ttl = now_unix + RECONCILE_JOB_TTL_SECONDS;
    let lock_expires_at = now_unix + RECONCILE_LOCK_TTL_SECONDS;

    let active_reconcile_job_id = acquire_reconcile_lock(
        client,
        table_name,
        project_id,
        block_id,
        &reconcile_job_id,
        now_unix,
        lock_expires_at,
        &now,
    )
    .await?;
    if let Some(active_reconcile_job_id) = active_reconcile_job_id {
        let mut value =
            serde_json::json!({ "error": "A reconcile job is already running for this block" });
        if !active_reconcile_job_id.trim().is_empty() {
            value["reconcile_job_id"] = serde_json::Value::String(active_reconcile_job_id);
        }
        return ok_json_status(
            value,
            StatusCode::CONFLICT,
        );
    }

    let mut put = client
        .put_item()
        .table_name(table_name)
        .item(
            "PK",
            AttributeValue::S(format!("RECONCILE_JOB#{}", reconcile_job_id)),
        )
        .item("SK", AttributeValue::S("META".to_string()))
        .item(
            "reconcile_job_id",
            AttributeValue::S(reconcile_job_id.clone()),
        )
        .item("project_id", AttributeValue::S(project_id.to_string()))
        .item("block_id", AttributeValue::S(block_id.to_string()))
        .item(
            "status",
            AttributeValue::S(RECONCILE_STATUS_QUEUED.to_string()),
        )
        .item("phase", AttributeValue::S(RECONCILE_PHASE_PENDING.to_string()))
        .item("force", AttributeValue::Bool(force))
        .item("images_total", AttributeValue::N("0".to_string()))
        .item("images_processed", AttributeValue::N("0".to_string()))
        .item("annotations_total", AttributeValue::N("0".to_string()))
        .item("annotations_processed", AttributeValue::N("0".to_string()))
        .item("block_annotation_count", AttributeValue::N("0".to_string()))
        .item("ttl", AttributeValue::N(reconcile_job_ttl.to_string()))
        .item("created_at", AttributeValue::S(now.clone()))
        .item("updated_at", AttributeValue::S(now.clone()));

    if !user_id.trim().is_empty() {
        put = put.item("requested_by", AttributeValue::S(user_id.to_string()));
    }

    if let Err(err) = put.send().await {
        let _ = release_reconcile_lock(client, table_name, project_id, block_id).await;
        return Err(Box::new(err));
    }

    let state_machine_arn = std::env::var("RECONCILE_STATE_MACHINE_ARN").ok();
    let Some(state_machine_arn) = state_machine_arn.filter(|v| !v.trim().is_empty()) else {
        let msg = "RECONCILE_STATE_MACHINE_ARN is not configured".to_string();
        set_job_failed(client, table_name, &reconcile_job_id, &msg).await?;
        let _ = release_reconcile_lock(client, table_name, project_id, block_id).await;
        return ok_json_status(
            serde_json::json!({ "error": msg, "reconcile_job_id": reconcile_job_id }),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    };

    let aws_cfg = aws_config::load_from_env().await;
    let sfn = aws_sdk_sfn::Client::new(&aws_cfg);
    let execution_input = serde_json::json!({
        "reconcile_job_id": reconcile_job_id.clone(),
        "project_id": project_id,
        "block_id": block_id,
        "force": force
    });
    let execution_name = format!("reconcile-{}", reconcile_job_id.replace('-', ""));

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
            let msg = format!("Failed to start reconcile workflow: {}", err);
            set_job_failed(client, table_name, &reconcile_job_id, &msg).await?;
            let _ = release_reconcile_lock(client, table_name, project_id, block_id).await;
            return ok_json_status(
                serde_json::json!({ "error": msg, "reconcile_job_id": reconcile_job_id }),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    if let Some(exec_arn) = execution_arn.as_ref() {
        client
            .update_item()
            .table_name(table_name)
            .key(
                "PK",
                AttributeValue::S(format!("RECONCILE_JOB#{}", reconcile_job_id)),
            )
            .key("SK", AttributeValue::S("META".to_string()))
            .update_expression(
                "SET execution_arn = :execution_arn, updated_at = :updated_at",
            )
            .expression_attribute_values(
                ":execution_arn",
                AttributeValue::S(exec_arn.clone()),
            )
            .expression_attribute_values(
                ":updated_at",
                AttributeValue::S(chrono::Utc::now().to_rfc3339()),
            )
            .send()
            .await?;
    }

    let response = ReconcileJobStatusResponse {
        reconcile_job_id,
        project_id: project_id.to_string(),
        block_id: block_id.to_string(),
        status: RECONCILE_STATUS_QUEUED.to_string(),
        phase: RECONCILE_PHASE_PENDING.to_string(),
        force,
        images_total: 0,
        images_processed: 0,
        annotations_total: 0,
        annotations_processed: 0,
        block_annotation_count: 0,
        execution_arn: None,
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

/// GET /projects/{pid}/blocks/{bid}/reconcile-counts/{reconcile_job_id}
pub async fn get_reconcile_job_status(
    client: &DynamoClient,
    table_name: &str,
    project_id: &str,
    block_id: &str,
    reconcile_job_id: &str,
) -> Result<Response<Body>, Error> {
    let result = client
        .get_item()
        .table_name(table_name)
        .key(
            "PK",
            AttributeValue::S(format!("RECONCILE_JOB#{}", reconcile_job_id)),
        )
        .key("SK", AttributeValue::S("META".to_string()))
        .send()
        .await?;

    let Some(item) = result.item() else {
        return ok_json_status(
            serde_json::json!({ "error": "Reconcile job not found" }),
            StatusCode::NOT_FOUND,
        );
    };

    let item_project_id = attr_s(item, "project_id").unwrap_or_default();
    let item_block_id = attr_s(item, "block_id").unwrap_or_default();

    if item_project_id != project_id || item_block_id != block_id {
        return ok_json_status(
            serde_json::json!({ "error": "Reconcile job not found" }),
            StatusCode::NOT_FOUND,
        );
    }

    let response = ReconcileJobStatusResponse {
        reconcile_job_id: attr_s(item, "reconcile_job_id")
            .unwrap_or_else(|| reconcile_job_id.to_string()),
        project_id: item_project_id,
        block_id: item_block_id,
        status: attr_s(item, "status").unwrap_or_else(|| RECONCILE_STATUS_QUEUED.to_string()),
        phase: attr_s(item, "phase").unwrap_or_else(|| RECONCILE_PHASE_PENDING.to_string()),
        force: attr_bool(item, "force").unwrap_or(false),
        images_total: attr_n_u64(item, "images_total").unwrap_or(0),
        images_processed: attr_n_u64(item, "images_processed").unwrap_or(0),
        annotations_total: attr_n_u64(item, "annotations_total").unwrap_or(0),
        annotations_processed: attr_n_u64(item, "annotations_processed").unwrap_or(0),
        block_annotation_count: attr_n_u64(item, "block_annotation_count").unwrap_or(0),
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

async fn acquire_reconcile_lock(
    client: &DynamoClient,
    table_name: &str,
    project_id: &str,
    block_id: &str,
    reconcile_job_id: &str,
    now_unix: i64,
    lock_expires_at: i64,
    now_rfc3339: &str,
) -> Result<Option<String>, Error> {
    let lock_pk = format!("PROJECT#{}", project_id);
    let lock_sk = reconcile_lock_sort_key(block_id);

    let acquire_result = client
        .put_item()
        .table_name(table_name)
        .item("PK", AttributeValue::S(lock_pk.clone()))
        .item("SK", AttributeValue::S(lock_sk.clone()))
        .item("project_id", AttributeValue::S(project_id.to_string()))
        .item("block_id", AttributeValue::S(block_id.to_string()))
        .item(
            "reconcile_job_id",
            AttributeValue::S(reconcile_job_id.to_string()),
        )
        .item(
            "lock_expires_at",
            AttributeValue::N(lock_expires_at.to_string()),
        )
        .item("ttl", AttributeValue::N(lock_expires_at.to_string()))
        .item("updated_at", AttributeValue::S(now_rfc3339.to_string()))
        .condition_expression("attribute_not_exists(lock_expires_at) OR lock_expires_at < :now")
        .expression_attribute_values(":now", AttributeValue::N(now_unix.to_string()))
        .send()
        .await;

    match acquire_result {
        Ok(_) => Ok(None),
        Err(err) => {
            if !is_conditional_check_failed(&err.to_string()) {
                return Err(Box::new(err));
            }
            let existing = client
                .get_item()
                .table_name(table_name)
                .key("PK", AttributeValue::S(lock_pk))
                .key("SK", AttributeValue::S(lock_sk))
                .send()
                .await?;
            let active_job_id = existing
                .item()
                .and_then(|item| attr_s(item, "reconcile_job_id"))
                .unwrap_or_default();
            Ok(Some(active_job_id))
        }
    }
}

async fn release_reconcile_lock(
    client: &DynamoClient,
    table_name: &str,
    project_id: &str,
    block_id: &str,
) -> Result<(), Error> {
    client
        .delete_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(format!("PROJECT#{}", project_id)))
        .key("SK", AttributeValue::S(reconcile_lock_sort_key(block_id)))
        .send()
        .await?;
    Ok(())
}

fn reconcile_lock_sort_key(block_id: &str) -> String {
    format!("BLOCK#{}#RECONCILE_LOCK", block_id)
}

fn is_conditional_check_failed(err: &str) -> bool {
    err.contains("ConditionalCheckFailed")
}

async fn set_job_failed(
    client: &DynamoClient,
    table_name: &str,
    reconcile_job_id: &str,
    message: &str,
) -> Result<(), Error> {
    client
        .update_item()
        .table_name(table_name)
        .key(
            "PK",
            AttributeValue::S(format!("RECONCILE_JOB#{}", reconcile_job_id)),
        )
        .key("SK", AttributeValue::S("META".to_string()))
        .update_expression(
            "SET #status = :status, #phase = :phase, error_message = :error, updated_at = :updated_at",
        )
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
        .expression_attribute_values(":error", AttributeValue::S(message.to_string()))
        .expression_attribute_values(
            ":updated_at",
            AttributeValue::S(chrono::Utc::now().to_rfc3339()),
        )
        .send()
        .await?;
    Ok(())
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
