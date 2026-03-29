use aws_sdk_dynamodb::Client as DynamoClient;
use aws_sdk_dynamodb::types::{AttributeValue, Select};
use super::model::{Project, ProjectStatus, CreateProjectPayload, UpdateProjectPayload};
use std::collections::HashMap;

/// Create a new project
pub async fn create_project(
    client: &DynamoClient,
    table_name: &str,
    owner_id: &str,
    payload: CreateProjectPayload,
) -> Result<Project, String> {
    let project_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let pk = "PROJECT".to_string();
    let sk = format!("PROJECT#{}", project_id);

    let members = payload.project_members.unwrap_or_default();

    let mut builder = client
        .put_item()
        .table_name(table_name)
        .item("PK", AttributeValue::S(pk))
        .item("SK", AttributeValue::S(sk))
        .item("project_name", AttributeValue::S(payload.project_name.clone()))
        .item("project_status", AttributeValue::S(ProjectStatus::Active.as_str().to_string()))
        .item("block_count", AttributeValue::N("0".to_string()))
        .item("project_owner", AttributeValue::S(owner_id.to_string()))
        .item("project_created_at", AttributeValue::S(now.clone()))
        .item("project_updated_at", AttributeValue::S(now.clone()));

    if !members.is_empty() {
        builder = builder.item("project_members", AttributeValue::L(
            members.iter().map(|m| AttributeValue::S(m.clone())).collect()
        ));
    }

    if let Some(v) = &payload.project_company {
        builder = builder.item("project_company", AttributeValue::S(v.clone()));
    }
    if let Some(v) = &payload.project_address {
        builder = builder.item("project_address", AttributeValue::S(v.clone()));
    }
    if let Some(v) = &payload.project_email {
        builder = builder.item("project_email", AttributeValue::S(v.clone()));
    }
    if let Some(v) = &payload.project_start_date {
        builder = builder.item("project_start_date", AttributeValue::S(v.clone()));
    }
    if let Some(v) = &payload.project_end_date {
        builder = builder.item("project_end_date", AttributeValue::S(v.clone()));
    }
    if let Some(v) = &payload.project_description {
        builder = builder.item("project_description", AttributeValue::S(v.clone()));
    }
    if let Some(v) = &payload.project_budget {
        builder = builder.item("project_budget", AttributeValue::S(v.clone()));
    }
    if let Some(v) = &payload.project_client_name {
        builder = builder.item("project_client_name", AttributeValue::S(v.clone()));
    }

    builder.send().await.map_err(|e| format!("DynamoDB put_item error: {}", e))?;

    Ok(Project {
        project_id,
        project_name: payload.project_name,
        block_count: 0,
        project_company: payload.project_company,
        project_status: ProjectStatus::Active,
        project_address: payload.project_address,
        project_email: payload.project_email,
        project_owner: owner_id.to_string(),
        project_members: members,
        project_start_date: payload.project_start_date,
        project_end_date: payload.project_end_date,
        project_description: payload.project_description,
        project_budget: payload.project_budget,
        project_client_name: payload.project_client_name,
        project_created_at: now.clone(),
        project_updated_at: now,
    })
}

/// Get a specific project
pub async fn get_project(
    client: &DynamoClient,
    table_name: &str,
    project_id: &str,
) -> Result<Project, String> {
    let pk = "PROJECT".to_string();
    let sk = format!("PROJECT#{}", project_id);

    let result = client
        .get_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(pk))
        .key("SK", AttributeValue::S(sk))
        .send()
        .await
        .map_err(|e| format!("DynamoDB get_item error: {}", e))?;

    if let Some(item) = result.item() {
        let mut project = parse_project_item(project_id, item)?;
        project.block_count = load_project_block_count(client, table_name, project_id).await?;
        Ok(project)
    } else {
        Err("Project not found".to_string())
    }
}

/// List all projects
pub async fn list_projects(
    client: &DynamoClient,
    table_name: &str,
) -> Result<Vec<Project>, String> {

    let mut projects = Vec::new();
    let mut last_evaluated_key: Option<HashMap<String, AttributeValue>> = None;

    loop {
        let mut query = client
            .query()
            .table_name(table_name)
            .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
            .expression_attribute_values(":pk", AttributeValue::S("PROJECT".to_string()))
            .expression_attribute_values(":sk_prefix", AttributeValue::S("PROJECT#".to_string()));

        if let Some(start_key) = last_evaluated_key.clone() {
            query = query.set_exclusive_start_key(Some(start_key));
        }

        let result = query
            .send()
            .await
            .map_err(|e| format!("DynamoDB query error: {}", e))?;

        for item in result.items() {
            if let Some(sk) = item.get("SK").and_then(|v| v.as_s().ok()) {
                if let Some(project_id) = sk.strip_prefix("PROJECT#") {
                    let mut project = parse_project_item(project_id, item)?;
                    project.block_count = load_project_block_count(client, table_name, project_id).await?;
                    projects.push(project);
                }
            }
        }

        match result.last_evaluated_key() {
            Some(next_key) => last_evaluated_key = Some(next_key.clone()),
            None => break,
        }
    }

    Ok(projects)
}

/// Update a project
pub async fn update_project(
    client: &DynamoClient,
    table_name: &str,
    project_id: &str,
    payload: UpdateProjectPayload,
) -> Result<Project, String> {
    let pk = "PROJECT".to_string();
    let sk = format!("PROJECT#{}", project_id);

    let mut update_expr = vec![];
    let mut expr_names = HashMap::new();
    let mut expr_values = HashMap::new();

    if let Some(v) = payload.project_name {
        update_expr.push("#project_name = :project_name");
        expr_names.insert("#project_name".to_string(), "project_name".to_string());
        expr_values.insert(":project_name".to_string(), AttributeValue::S(v));
    }
    if let Some(v) = payload.project_company {
        update_expr.push("#project_company = :project_company");
        expr_names.insert("#project_company".to_string(), "project_company".to_string());
        expr_values.insert(":project_company".to_string(), AttributeValue::S(v));
    }
    if let Some(v) = payload.project_status {
        update_expr.push("#project_status = :project_status");
        expr_names.insert("#project_status".to_string(), "project_status".to_string());
        expr_values.insert(":project_status".to_string(), AttributeValue::S(v.as_str().to_string()));
    }
    if let Some(v) = payload.project_address {
        update_expr.push("#project_address = :project_address");
        expr_names.insert("#project_address".to_string(), "project_address".to_string());
        expr_values.insert(":project_address".to_string(), AttributeValue::S(v));
    }
    if let Some(v) = payload.project_email {
        update_expr.push("#project_email = :project_email");
        expr_names.insert("#project_email".to_string(), "project_email".to_string());
        expr_values.insert(":project_email".to_string(), AttributeValue::S(v));
    }
    if let Some(v) = payload.project_members {
        update_expr.push("#project_members = :project_members");
        expr_names.insert("#project_members".to_string(), "project_members".to_string());
        expr_values.insert(":project_members".to_string(), AttributeValue::L(
            v.iter().map(|m| AttributeValue::S(m.clone())).collect()
        ));
    }
    if let Some(v) = payload.project_start_date {
        update_expr.push("#project_start_date = :project_start_date");
        expr_names.insert("#project_start_date".to_string(), "project_start_date".to_string());
        expr_values.insert(":project_start_date".to_string(), AttributeValue::S(v));
    }
    if let Some(v) = payload.project_end_date {
        update_expr.push("#project_end_date = :project_end_date");
        expr_names.insert("#project_end_date".to_string(), "project_end_date".to_string());
        expr_values.insert(":project_end_date".to_string(), AttributeValue::S(v));
    }
    if let Some(v) = payload.project_description {
        update_expr.push("#project_description = :project_description");
        expr_names.insert("#project_description".to_string(), "project_description".to_string());
        expr_values.insert(":project_description".to_string(), AttributeValue::S(v));
    }
    if let Some(v) = payload.project_budget {
        update_expr.push("#project_budget = :project_budget");
        expr_names.insert("#project_budget".to_string(), "project_budget".to_string());
        expr_values.insert(":project_budget".to_string(), AttributeValue::S(v));
    }
    if let Some(v) = payload.project_client_name {
        update_expr.push("#project_client_name = :project_client_name");
        expr_names.insert("#project_client_name".to_string(), "project_client_name".to_string());
        expr_values.insert(":project_client_name".to_string(), AttributeValue::S(v));
    }

    // Always update project_updated_at
    update_expr.push("#project_updated_at = :project_updated_at");
    expr_names.insert("#project_updated_at".to_string(), "project_updated_at".to_string());
    expr_values.insert(
        ":project_updated_at".to_string(),
        AttributeValue::S(chrono::Utc::now().to_rfc3339()),
    );

    if !update_expr.is_empty() {
        let update_expression = format!("SET {}", update_expr.join(", "));

        let mut builder = client
            .update_item()
            .table_name(table_name)
            .key("PK", AttributeValue::S(pk))
            .key("SK", AttributeValue::S(sk))
            .update_expression(update_expression);

        for (k, v) in expr_names {
            builder = builder.expression_attribute_names(k, v);
        }
        for (k, v) in expr_values {
            builder = builder.expression_attribute_values(k, v);
        }

        builder.send().await.map_err(|e| format!("DynamoDB update_item error: {}", e))?;
    }

    get_project(client, table_name, project_id).await
}

/// Delete a project
pub async fn delete_project(
    client: &DynamoClient,
    table_name: &str,
    project_id: &str,
) -> Result<(), String> {
    let pk = "PROJECT".to_string();
    let sk = format!("PROJECT#{}", project_id);

    client
        .delete_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(pk))
        .key("SK", AttributeValue::S(sk))
        .send()
        .await
        .map_err(|e| format!("DynamoDB delete_item error: {}", e))?;

    Ok(())
}

// ─── Helpers ───

fn get_str(item: &HashMap<String, AttributeValue>, key: &str) -> String {
    item.get(key).and_then(|v| v.as_s().ok()).map(|s| s.to_string()).unwrap_or_default()
}

fn get_opt_str(item: &HashMap<String, AttributeValue>, key: &str) -> Option<String> {
    item.get(key).and_then(|v| v.as_s().ok()).map(|s| s.to_string())
}

fn get_string_list(item: &HashMap<String, AttributeValue>, key: &str) -> Vec<String> {
    item.get(key)
        .and_then(|v| v.as_l().ok())
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_s().ok().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn get_n_u32(item: &HashMap<String, AttributeValue>, key: &str) -> Option<u32> {
    item.get(key)
        .and_then(|v| v.as_n().ok())
        .and_then(|n| n.parse::<u32>().ok())
}

async fn load_project_block_count(
    client: &DynamoClient,
    table_name: &str,
    project_id: &str,
) -> Result<u32, String> {
    let mut total: u64 = 0;
    let mut last_evaluated_key: Option<HashMap<String, AttributeValue>> = None;

    loop {
        let mut query = client
            .query()
            .table_name(table_name)
            .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
            .expression_attribute_values(":pk", AttributeValue::S(format!("PROJECT#{}", project_id)))
            .expression_attribute_values(":sk_prefix", AttributeValue::S("BLOCK#".to_string()))
            .select(Select::Count);

        if let Some(start_key) = last_evaluated_key.clone() {
            query = query.set_exclusive_start_key(Some(start_key));
        }

        let result = query
            .send()
            .await
            .map_err(|e| format!("DynamoDB block-count query error: {}", e))?;

        total += result.count().max(0) as u64;

        match result.last_evaluated_key() {
            Some(next_key) => last_evaluated_key = Some(next_key.clone()),
            None => break,
        }
    }

    Ok(total.min(u32::MAX as u64) as u32)
}

fn parse_project_item(project_id: &str, item: &HashMap<String, AttributeValue>) -> Result<Project, String> {
    let block_count = get_n_u32(item, "block_count")
        .ok_or_else(|| format!("Project {} is missing required numeric block_count", project_id))?;

    Ok(Project {
        project_id: project_id.to_string(),
        project_name: get_str(item, "project_name"),
        block_count,
        project_company: get_opt_str(item, "project_company"),
        project_status: item
            .get("project_status")
            .and_then(|v| v.as_s().ok())
            .map(|s| ProjectStatus::from_str_loose(s))
            .unwrap_or(ProjectStatus::Active),
        project_address: get_opt_str(item, "project_address"),
        project_email: get_opt_str(item, "project_email"),
        project_owner: get_str(item, "project_owner"),
        project_members: get_string_list(item, "project_members"),
        project_start_date: get_opt_str(item, "project_start_date"),
        project_end_date: get_opt_str(item, "project_end_date"),
        project_description: get_opt_str(item, "project_description"),
        project_budget: get_opt_str(item, "project_budget"),
        project_client_name: get_opt_str(item, "project_client_name"),
        project_created_at: get_str(item, "project_created_at"),
        project_updated_at: get_str(item, "project_updated_at"),
    })
}
