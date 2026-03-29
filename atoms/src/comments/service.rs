use aws_sdk_dynamodb::Client as DynamoClient;
use aws_sdk_dynamodb::types::AttributeValue;
use super::model::{CommentThread, Comment, CreateThreadPayload, CreateCommentPayload, UpdateThreadPayload};

/// Create a new thread under a parent resource
pub async fn create_thread(
    client: &DynamoClient,
    table_name: &str,
    parent_id: &str,
    user_id: &str,
    user_name: &str,
    payload: CreateThreadPayload,
) -> Result<CommentThread, String> {
    let thread_id = payload.thread_id.clone();
    let now = chrono::Utc::now().to_rfc3339();

    let pk = format!("PARENT#{}", parent_id);
    let sk = format!("THREAD#{}", thread_id);

    let mut put = client
        .put_item()
        .table_name(table_name)
        .item("PK", AttributeValue::S(pk))
        .item("SK", AttributeValue::S(sk))
        .item("parent_id", AttributeValue::S(parent_id.to_string()))
        .item("resolved", AttributeValue::Bool(false))
        .item("created_by", AttributeValue::S(user_id.to_string()))
        .item("created_at", AttributeValue::S(now.clone()));

    if let Some(ref meta) = payload.metadata {
        put = put.item("metadata", AttributeValue::S(meta.clone()));
    }

    put.send()
        .await
        .map_err(|e| format!("DynamoDB put_item error: {}", e))?;

    // Always create first comment with the thread
    let comment = create_comment_internal(
        client, table_name, &thread_id, user_id, user_name, &payload.text,
    ).await?;
    let comments = vec![comment];

    Ok(CommentThread {
        thread_id,
        parent_id: parent_id.to_string(),
        metadata: payload.metadata,
        resolved: false,
        created_by: user_id.to_string(),
        created_at: now,
        comments,
    })
}

/// List all threads for a parent resource, each with their comments
pub async fn list_threads(
    client: &DynamoClient,
    table_name: &str,
    parent_id: &str,
) -> Result<Vec<CommentThread>, String> {
    let pk = format!("PARENT#{}", parent_id);

    let result = client
        .query()
        .table_name(table_name)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(pk))
        .expression_attribute_values(":sk_prefix", AttributeValue::S("THREAD#".to_string()))
        .send()
        .await
        .map_err(|e| format!("DynamoDB query error: {}", e))?;

    let mut threads = Vec::new();

    for item in result.items() {
        if let Some(sk) = item.get("SK").and_then(|v| v.as_s().ok()) {
            if let Some(thread_id) = sk.strip_prefix("THREAD#") {
                // Fetch comments for this thread
                let comments = list_comments(client, table_name, thread_id).await?;

                threads.push(CommentThread {
                    thread_id: thread_id.to_string(),
                    parent_id: parent_id.to_string(),
                    metadata: item.get("metadata").and_then(|v| v.as_s().ok()).map(|s| s.to_string()),
                    resolved: item.get("resolved").and_then(|v| v.as_bool().ok()).copied().unwrap_or(false),
                    created_by: item.get("created_by").and_then(|v| v.as_s().ok()).unwrap_or(&String::new()).to_string(),
                    created_at: item.get("created_at").and_then(|v| v.as_s().ok()).unwrap_or(&String::new()).to_string(),
                    comments,
                });
            }
        }
    }

    Ok(threads)
}

/// List comments for a thread
async fn list_comments(
    client: &DynamoClient,
    table_name: &str,
    thread_id: &str,
) -> Result<Vec<Comment>, String> {
    let pk = format!("THREAD#{}", thread_id);

    let result = client
        .query()
        .table_name(table_name)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S(pk))
        .expression_attribute_values(":sk_prefix", AttributeValue::S("COMMENT#".to_string()))
        .send()
        .await
        .map_err(|e| format!("DynamoDB query error: {}", e))?;

    let mut comments = Vec::new();

    for item in result.items() {
        if let Some(sk) = item.get("SK").and_then(|v| v.as_s().ok()) {
            if let Some(comment_id) = sk.strip_prefix("COMMENT#") {
                comments.push(Comment {
                    comment_id: comment_id.to_string(),
                    thread_id: thread_id.to_string(),
                    user_id: item.get("user_id").and_then(|v| v.as_s().ok()).unwrap_or(&String::new()).to_string(),
                    user_name: item.get("user_name").and_then(|v| v.as_s().ok()).unwrap_or(&String::new()).to_string(),
                    text: item.get("text").and_then(|v| v.as_s().ok()).unwrap_or(&String::new()).to_string(),
                    created_at: item.get("created_at").and_then(|v| v.as_s().ok()).unwrap_or(&String::new()).to_string(),
                });
            }
        }
    }

    Ok(comments)
}

/// Add a comment to an existing thread
pub async fn add_comment(
    client: &DynamoClient,
    table_name: &str,
    thread_id: &str,
    user_id: &str,
    user_name: &str,
    payload: CreateCommentPayload,
) -> Result<Comment, String> {
    create_comment_internal(client, table_name, thread_id, user_id, user_name, &payload.text).await
}

/// Internal helper to create a comment row
async fn create_comment_internal(
    client: &DynamoClient,
    table_name: &str,
    thread_id: &str,
    user_id: &str,
    user_name: &str,
    text: &str,
) -> Result<Comment, String> {
    let comment_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    let pk = format!("THREAD#{}", thread_id);
    let sk = format!("COMMENT#{}", comment_id);

    client
        .put_item()
        .table_name(table_name)
        .item("PK", AttributeValue::S(pk))
        .item("SK", AttributeValue::S(sk))
        .item("user_id", AttributeValue::S(user_id.to_string()))
        .item("user_name", AttributeValue::S(user_name.to_string()))
        .item("text", AttributeValue::S(text.to_string()))
        .item("created_at", AttributeValue::S(now.clone()))
        .send()
        .await
        .map_err(|e| format!("DynamoDB put_item error: {}", e))?;

    Ok(Comment {
        comment_id,
        thread_id: thread_id.to_string(),
        user_id: user_id.to_string(),
        user_name: user_name.to_string(),
        text: text.to_string(),
        created_at: now,
    })
}

/// Update a thread (resolve/unresolve, update metadata)
pub async fn update_thread(
    client: &DynamoClient,
    table_name: &str,
    parent_id: &str,
    thread_id: &str,
    payload: UpdateThreadPayload,
) -> Result<(), String> {
    let pk = format!("PARENT#{}", parent_id);
    let sk = format!("THREAD#{}", thread_id);

    let mut update_parts: Vec<&str> = Vec::new();
    let mut expr_values: Vec<(String, AttributeValue)> = Vec::new();

    if let Some(resolved) = payload.resolved {
        update_parts.push("resolved = :resolved");
        expr_values.push((":resolved".to_string(), AttributeValue::Bool(resolved)));
    }

    if let Some(ref metadata) = payload.metadata {
        update_parts.push("metadata = :metadata");
        expr_values.push((":metadata".to_string(), AttributeValue::S(metadata.clone())));
    }

    if update_parts.is_empty() {
        return Ok(());
    }

    let update_expression = format!("SET {}", update_parts.join(", "));

    let mut update_builder = client
        .update_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(pk))
        .key("SK", AttributeValue::S(sk))
        .update_expression(&update_expression);

    for (name, value) in expr_values {
        update_builder = update_builder.expression_attribute_values(name, value);
    }

    update_builder
        .send()
        .await
        .map_err(|e| format!("DynamoDB update error: {}", e))?;

    Ok(())
}

/// Delete a thread and all its comments
pub async fn delete_thread(
    client: &DynamoClient,
    table_name: &str,
    parent_id: &str,
    thread_id: &str,
) -> Result<(), String> {
    // 1. Delete all comments for this thread
    let comments = list_comments(client, table_name, thread_id).await?;
    for comment in &comments {
        let pk = format!("THREAD#{}", thread_id);
        let sk = format!("COMMENT#{}", comment.comment_id);
        client
            .delete_item()
            .table_name(table_name)
            .key("PK", AttributeValue::S(pk))
            .key("SK", AttributeValue::S(sk))
            .send()
            .await
            .map_err(|e| format!("DynamoDB delete comment error: {}", e))?;
    }

    // 2. Delete the thread itself
    let pk = format!("PARENT#{}", parent_id);
    let sk = format!("THREAD#{}", thread_id);
    client
        .delete_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S(pk))
        .key("SK", AttributeValue::S(sk))
        .send()
        .await
        .map_err(|e| format!("DynamoDB delete thread error: {}", e))?;

    Ok(())
}

/// Delete all threads and comments for a parent resource
/// Uses BatchWriteItem for bulk deletion instead of one-by-one.
pub async fn delete_threads_for_parent(
    client: &DynamoClient,
    table_name:&str,
    parent_id:&str,
    ) -> Result<usize, String> {

    let threads = list_threads(client, table_name, parent_id).await?;
    if threads.is_empty() {
        return Ok(0);
    }

    // Collect all delete requests: thread records + their comment records
    let mut all_deletes: Vec<aws_sdk_dynamodb::types::WriteRequest> = Vec::new();

    for thread in &threads {
        // Delete each comment in this thread
        for comment in &thread.comments {
            all_deletes.push(
                aws_sdk_dynamodb::types::WriteRequest::builder()
                    .delete_request(
                        aws_sdk_dynamodb::types::DeleteRequest::builder()
                            .key("PK", AttributeValue::S(format!("THREAD#{}", thread.thread_id)))
                            .key("SK", AttributeValue::S(format!("COMMENT#{}", comment.comment_id)))
                            .build()
                            .expect("valid delete request"),
                    )
                    .build(),
            );
        }
        // Delete the thread record itself
        all_deletes.push(
            aws_sdk_dynamodb::types::WriteRequest::builder()
                .delete_request(
                    aws_sdk_dynamodb::types::DeleteRequest::builder()
                        .key("PK", AttributeValue::S(format!("PARENT#{}", parent_id)))
                        .key("SK", AttributeValue::S(format!("THREAD#{}", thread.thread_id)))
                        .build()
                        .expect("valid delete request"),
                )
                .build(),
        );
    }

    let total = all_deletes.len();

    // Batch delete 25 at a time
    for chunk in all_deletes.chunks(25) {
        client
            .batch_write_item()
            .request_items(table_name, chunk.to_vec())
            .send()
            .await
            .map_err(|e| format!("DynamoDB batch_write_item error: {}", e))?;
    }

    Ok(total)
}
