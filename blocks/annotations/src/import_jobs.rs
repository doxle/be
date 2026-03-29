use aws_sdk_dynamodb::types::AttributeValue;
use aws_sdk_dynamodb::Client as DynamoClient;
use lambda_http::{http::StatusCode, Body, Error, Response};
use serde::{Deserialize, Serialize};

const IMPORT_STATUS_QUEUED: &str = "queued";
const IMPORT_STATUS_FAILED: &str = "failed";
const IMPORT_PHASE_PENDING: &str = "pending";
const IMPORT_PHASE_FAILED: &str = "failed";

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct StartImportJobRequest {
    pub import_id: String,
    pub s3_key: String,
    pub max_tasks: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct ImportJobStatusResponse {
    pub import_job_id: String,
    pub project_id: String,
    pub block_id: String,
    pub import_id: String,
    pub s3_key: String,
    pub status: String,
    pub phase: String,
    pub labels_total: u64,
    pub labels_processed: u64,
    pub tasks_total: u64,
    pub tasks_processed: u64,
    pub images_total: u64,
    pub images_processed: u64,
    pub annotations_total: u64,
    pub annotations_processed: u64,
    pub labels_created: u64,
    pub tasks_created: u64,
    pub images_created: u64,
    pub annotations_created: u64,
    pub execution_arn: Option<String>,
    pub error_message: Option<String>,
    pub requested_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// POST /projects/{pid}/blocks/{bid}/imports
pub async fn start_import_job(
    client: &DynamoClient,
    table_name: &str,
    project_id: &str,
    block_id: &str,
    user_id: &str,
    body: &[u8],
) -> Result<Response<Body>, Error> {
    let req: StartImportJobRequest = if body.is_empty() {
        StartImportJobRequest::default()
    } else {
        serde_json::from_slice(body)?
    };

    if req.import_id.trim().is_empty() || req.s3_key.trim().is_empty() {
        return ok_json_status(
            serde_json::json!({
                "error": "import_id and s3_key are required"
            }),
            StatusCode::BAD_REQUEST,
        );
    }

    // Ensure block exists under the given project before creating import job.
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

    let import_job_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    let mut put = client
        .put_item()
        .table_name(table_name)
        .item(
            "PK",
            AttributeValue::S(format!("IMPORT_JOB#{}", import_job_id)),
        )
        .item("SK", AttributeValue::S("META".to_string()))
        .item("import_job_id", AttributeValue::S(import_job_id.clone()))
        .item("project_id", AttributeValue::S(project_id.to_string()))
        .item("block_id", AttributeValue::S(block_id.to_string()))
        .item("import_id", AttributeValue::S(req.import_id.clone()))
        .item("s3_key", AttributeValue::S(req.s3_key.clone()))
        .item(
            "status",
            AttributeValue::S(IMPORT_STATUS_QUEUED.to_string()),
        )
        .item("phase", AttributeValue::S(IMPORT_PHASE_PENDING.to_string()))
        .item("labels_total", AttributeValue::N("0".to_string()))
        .item("labels_processed", AttributeValue::N("0".to_string()))
        .item("tasks_total", AttributeValue::N("0".to_string()))
        .item("tasks_processed", AttributeValue::N("0".to_string()))
        .item("images_total", AttributeValue::N("0".to_string()))
        .item("images_processed", AttributeValue::N("0".to_string()))
        .item("annotations_total", AttributeValue::N("0".to_string()))
        .item("annotations_processed", AttributeValue::N("0".to_string()))
        .item("labels_created", AttributeValue::N("0".to_string()))
        .item("tasks_created", AttributeValue::N("0".to_string()))
        .item("images_created", AttributeValue::N("0".to_string()))
        .item("annotations_created", AttributeValue::N("0".to_string()))
        .item("created_at", AttributeValue::S(now.clone()))
        .item("updated_at", AttributeValue::S(now.clone()));

    if !user_id.trim().is_empty() {
        put = put.item("requested_by", AttributeValue::S(user_id.to_string()));
    }
    if let Some(max_tasks) = req.max_tasks {
        put = put.item("max_tasks", AttributeValue::N(max_tasks.to_string()));
    }

    put.send().await?;

    // Trigger Step Functions execution for async orchestration.
    let state_machine_arn = std::env::var("IMPORT_STATE_MACHINE_ARN").ok();
    let Some(state_machine_arn) = state_machine_arn.filter(|v| !v.trim().is_empty()) else {
        let msg = "IMPORT_STATE_MACHINE_ARN is not configured".to_string();
        set_job_failed(client, table_name, &import_job_id, &msg).await?;
        return ok_json_status(
            serde_json::json!({ "error": msg, "import_job_id": import_job_id }),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    };

    let aws_cfg = aws_config::load_from_env().await;
    let sfn = aws_sdk_sfn::Client::new(&aws_cfg);
    let execution_input = serde_json::json!({
        "import_job_id": import_job_id.clone(),
        "project_id": project_id,
        "block_id": block_id,
        "import_id": req.import_id.clone(),
        "s3_key": req.s3_key.clone(),
        "max_tasks": req.max_tasks
    });
    let execution_name = format!("import-{}", import_job_id.replace('-', ""));

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
            let msg = format!("Failed to start import workflow: {}", err);
            set_job_failed(client, table_name, &import_job_id, &msg).await?;
            return ok_json_status(
                serde_json::json!({ "error": msg, "import_job_id": import_job_id }),
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
                AttributeValue::S(format!("IMPORT_JOB#{}", import_job_id)),
            )
            .key("SK", AttributeValue::S("META".to_string()))
            .update_expression(
                "SET execution_arn = :execution_arn, updated_at = :updated_at REMOVE error_message",
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

    let response = ImportJobStatusResponse {
        import_job_id,
        project_id: project_id.to_string(),
        block_id: block_id.to_string(),
        import_id: req.import_id,
        s3_key: req.s3_key,
        status: IMPORT_STATUS_QUEUED.to_string(),
        phase: IMPORT_PHASE_PENDING.to_string(),
        labels_total: 0,
        labels_processed: 0,
        tasks_total: 0,
        tasks_processed: 0,
        images_total: 0,
        images_processed: 0,
        annotations_total: 0,
        annotations_processed: 0,
        labels_created: 0,
        tasks_created: 0,
        images_created: 0,
        annotations_created: 0,
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

/// GET /projects/{pid}/blocks/{bid}/imports/{import_job_id}
pub async fn get_import_job_status(
    client: &DynamoClient,
    table_name: &str,
    project_id: &str,
    block_id: &str,
    import_job_id: &str,
) -> Result<Response<Body>, Error> {
    let result = client
        .get_item()
        .table_name(table_name)
        .key(
            "PK",
            AttributeValue::S(format!("IMPORT_JOB#{}", import_job_id)),
        )
        .key("SK", AttributeValue::S("META".to_string()))
        .send()
        .await?;

    let Some(item) = result.item() else {
        return ok_json_status(
            serde_json::json!({ "error": "Import job not found" }),
            StatusCode::NOT_FOUND,
        );
    };

    let item_project_id = attr_s(item, "project_id").unwrap_or_default();
    let item_block_id = attr_s(item, "block_id").unwrap_or_default();

    if item_project_id != project_id || item_block_id != block_id {
        return ok_json_status(
            serde_json::json!({ "error": "Import job not found" }),
            StatusCode::NOT_FOUND,
        );
    }

    let response = ImportJobStatusResponse {
        import_job_id: attr_s(item, "import_job_id").unwrap_or_else(|| import_job_id.to_string()),
        project_id: item_project_id,
        block_id: item_block_id,
        import_id: attr_s(item, "import_id").unwrap_or_default(),
        s3_key: attr_s(item, "s3_key").unwrap_or_default(),
        status: attr_s(item, "status").unwrap_or_else(|| IMPORT_STATUS_QUEUED.to_string()),
        phase: attr_s(item, "phase").unwrap_or_else(|| IMPORT_PHASE_PENDING.to_string()),
        labels_total: attr_n_u64(item, "labels_total").unwrap_or(0),
        labels_processed: attr_n_u64(item, "labels_processed").unwrap_or(0),
        tasks_total: attr_n_u64(item, "tasks_total").unwrap_or(0),
        tasks_processed: attr_n_u64(item, "tasks_processed").unwrap_or(0),
        images_total: attr_n_u64(item, "images_total").unwrap_or(0),
        images_processed: attr_n_u64(item, "images_processed").unwrap_or(0),
        annotations_total: attr_n_u64(item, "annotations_total").unwrap_or(0),
        annotations_processed: attr_n_u64(item, "annotations_processed").unwrap_or(0),
        labels_created: attr_n_u64(item, "labels_created").unwrap_or(0),
        tasks_created: attr_n_u64(item, "tasks_created").unwrap_or(0),
        images_created: attr_n_u64(item, "images_created").unwrap_or(0),
        annotations_created: attr_n_u64(item, "annotations_created").unwrap_or(0),
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
    import_job_id: &str,
    message: &str,
) -> Result<(), Error> {
    client
        .update_item()
        .table_name(table_name)
        .key(
            "PK",
            AttributeValue::S(format!("IMPORT_JOB#{}", import_job_id)),
        )
        .key("SK", AttributeValue::S("META".to_string()))
        .update_expression(
            "SET #status = :status, #phase = :phase, error_message = :error, updated_at = :updated_at",
        )
        .expression_attribute_names("#status", "status")
        .expression_attribute_names("#phase", "phase")
        .expression_attribute_values(":status", AttributeValue::S(IMPORT_STATUS_FAILED.to_string()))
        .expression_attribute_values(":phase", AttributeValue::S(IMPORT_PHASE_FAILED.to_string()))
        .expression_attribute_values(":error", AttributeValue::S(message.to_string()))
        .expression_attribute_values(":updated_at", AttributeValue::S(chrono::Utc::now().to_rfc3339()))
        .send()
        .await?;
    Ok(())
}

fn attr_s(item: &std::collections::HashMap<String, AttributeValue>, key: &str) -> Option<String> {
    item.get(key)
        .and_then(|v| v.as_s().ok())
        .map(|s| s.to_string())
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
