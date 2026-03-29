use lambda_http::{Body, Error, Response};
use aws_sdk_dynamodb::Client as DynamoClient;
use super::model::{User, UserRole, CreateUserPayload, UpdateUserPayload};
use aws_sdk_dynamodb::types::AttributeValue;

/// Convert a string to Title Case (capitalize first letter of each word).
/// Splits on whitespace, dots, underscores, and dashes, then joins with a space.
pub fn to_title_case(s: &str) -> String {
    s.split(|c: char| c.is_whitespace() || c == '.' || c == '_' || c == '-')
        .filter(|w| !w.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().to_string() + &chars.as_str().to_lowercase(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// List all users from DynamoDB
pub async fn list_users(
    client: &DynamoClient,
    table_name: &str,
) -> Result<Response<Body>, Error> {
    let result = client
        .query()
        .table_name(table_name)
        .key_condition_expression("PK = :pk AND begins_with(SK, :sk_prefix)")
        .expression_attribute_values(":pk", AttributeValue::S("USER".to_string()))
        .expression_attribute_values(":sk_prefix", AttributeValue::S("USER#".to_string()))
        .send()
        .await
        .map_err(|e| format!("DynamoDB query error: {}", e))?;

    let mut users = Vec::new();
    for item in result.items() {
        let sk = item.get("SK").and_then(|v| v.as_s().ok()).map(|s| s.to_string()).unwrap_or_default();
        let user_id = sk.strip_prefix("USER#").unwrap_or("").to_string();
        let user_name = item.get("user_name").and_then(|v| v.as_s().ok()).map(|s| s.to_string()).unwrap_or_default();
        let user_email = item.get("user_email").and_then(|v| v.as_s().ok()).map(|s| s.to_string()).unwrap_or_default();
        let user_company = item.get("user_company").and_then(|v| v.as_s().ok()).map(|s| s.to_string());
        let user_role = item.get("user_role").and_then(|v| v.as_s().ok()).map(|s| UserRole::from_str_loose(s)).unwrap_or(UserRole::Annotator);
        let user_created_at = item.get("user_created_at").and_then(|v| v.as_s().ok()).map(|s| s.to_string()).unwrap_or_default();
        let user_last_login = item.get("last_login").and_then(|v| v.as_s().ok()).map(|s| s.to_string());

        users.push(User {
            user_id,
            user_name,
            user_email,
            user_company,
            user_role,
            user_created_at,
            user_last_login,
        });
    }

    let resp = Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(serde_json::to_string(&users)?.into())
        .map_err(Box::new)?;
    Ok(resp)
}

/// Create user in DynamoDB after Cognito signup
/// This is called once after user signs up in Cognito
pub async fn create_user(
    client: &DynamoClient,
    table_name: &str,
    user_id: &str,
    body: &[u8],
) -> Result<Response<Body>, Error> {
    let req: CreateUserPayload = serde_json::from_slice(body)?;
    let title_name = to_title_case(&req.user_name);

    // Resolve role: super-admin always gets Admin, otherwise use requested role
    let role = if let Ok(super_admin) = std::env::var("SUPER_ADMIN_SUB") {
        if user_id == super_admin { UserRole::Admin } else { req.user_role.clone() }
    } else {
        req.user_role.clone()
    };

    let now = chrono::Utc::now().to_rfc3339();
    let sk = format!("USER#{}", user_id);

    // Store user in DynamoDB with PK=USER, SK=USER#cognito-id
    let mut put_request = client
        .put_item()
        .table_name(table_name)
        .item("PK", AttributeValue::S("USER".to_string()))
        .item("SK", AttributeValue::S(sk))
        .item("user_name", AttributeValue::S(title_name.clone()))
        .item("user_email", AttributeValue::S(req.user_email.clone()))
        .item("user_role", AttributeValue::S(role.as_str().to_string()))
        .item("user_created_at", AttributeValue::S(now.clone()));
    
    if let Some(company) = &req.user_company {
        put_request = put_request.item("user_company", AttributeValue::S(company.clone()));
    }
    
    put_request.send().await.map_err(|e| format!("DynamoDB put_item error: {}", e))?;

    let user = User {
        user_id: user_id.to_string(),
        user_name: title_name,
        user_email: req.user_email,
        user_company: req.user_company,
        user_role: role,
        user_created_at: now,
        user_last_login: None,
    };

    let resp = Response::builder()
        .status(201)
        .header("content-type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(serde_json::to_string(&user)?.into())
        .map_err(Box::new)?;
    Ok(resp)
}

/// Get current user from DynamoDB
pub async fn get_user(
    client: &DynamoClient,
    table_name: &str,
    user_id: &str,
) -> Result<Response<Body>, Error> {
    let sk = format!("USER#{}", user_id);

    let result = client
        .get_item()
        .table_name(table_name)
        .key("PK", AttributeValue::S("USER".to_string()))
        .key("SK", AttributeValue::S(sk.clone()))
        .send()
        .await
        .map_err(|e| format!("DynamoDB get_item error: {}", e))?;

    if let Some(item) = result.item() {
        let mut user_name = item.get("user_name").and_then(|v| v.as_s().ok()).map(|s| s.to_string()).unwrap_or_default();
        let user_email = item.get("user_email").and_then(|v| v.as_s().ok()).map(|s| s.to_string()).unwrap_or_default();
        if user_name.trim().is_empty() {
            user_name = to_title_case(user_email.split('@').next().unwrap_or("User"));
        }
        let user_company = item.get("user_company").and_then(|v| v.as_s().ok()).map(|s| s.to_string());
        let user_role = item.get("user_role").and_then(|v| v.as_s().ok()).map(|s| UserRole::from_str_loose(s)).unwrap_or(UserRole::Annotator);
        let user_created_at = item.get("user_created_at").and_then(|v| v.as_s().ok()).map(|s| s.to_string()).unwrap_or_default();
        let _last_login = item.get("user_last_login").and_then(|v| v.as_s().ok()).map(|s| s.to_string());
        
        // Update last_login on every get
        let now = chrono::Utc::now().to_rfc3339();
        let _ = client
            .update_item()
            .table_name(table_name)
            .key("PK", AttributeValue::S("USER".to_string()))
            .key("SK", AttributeValue::S(sk.clone()))
            .update_expression("SET last_login = :login")
            .expression_attribute_values(":login", AttributeValue::S(now.clone()))
            .send()
            .await;

        let user = User {
            user_id: user_id.to_string(),
            user_name: user_name.clone(),
            user_email: user_email.clone(),
            user_company: user_company.clone(),
            user_role: user_role.clone(),
            user_created_at: user_created_at.clone(),
            user_last_login: Some(now.clone()),
        };
        
        let resp = Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::to_string(&user)?.into())
            .map_err(Box::new)?;
        Ok(resp)
    } else {
        let resp = Response::builder()
            .status(404)
            .header("content-type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::json!({"error": "User not found"}).to_string().into())
            .map_err(Box::new)?;
        Ok(resp)
    }
}

/// Update user
pub async fn update_user(
    client: &DynamoClient,
    table_name: &str,
    user_id: &str,
    body: &[u8],
) -> Result<Response<Body>, Error> {
    let req: UpdateUserPayload = serde_json::from_slice(body)?;
    let sk = format!("USER#{}", user_id);

    let mut update_expr = vec![];
    let mut expr_names = std::collections::HashMap::new();
    let mut expr_values = std::collections::HashMap::new();
    
    if let Some(name) = req.user_name {
        update_expr.push("#user_name = :user_name");
        expr_names.insert("#user_name".to_string(), "user_name".to_string());
        expr_values.insert(":user_name".to_string(), AttributeValue::S(name));
    }
    
    if let Some(company) = req.user_company {
        update_expr.push("user_company = :user_company");
        expr_values.insert(":user_company".to_string(), AttributeValue::S(company));
    }
    
    if let Some(role) = req.user_role {
        update_expr.push("#user_role = :user_role");
        expr_names.insert("#user_role".to_string(), "user_role".to_string());
        expr_values.insert(":user_role".to_string(), AttributeValue::S(role.as_str().to_string()));
    }
    
    if !update_expr.is_empty() {
        let mut builder = client
            .update_item()
            .table_name(table_name)
            .key("PK", AttributeValue::S("USER".to_string()))
            .key("SK", AttributeValue::S(sk))
            .update_expression(format!("SET {}", update_expr.join(", ")));
        
        for (k, v) in expr_names {
            builder = builder.expression_attribute_names(k, v);
        }
        
        for (k, v) in expr_values {
            builder = builder.expression_attribute_values(k, v);
        }
        
        builder.send().await.map_err(|e| format!("DynamoDB update_item error: {}", e))?;
    }

    // Return updated user
    get_user(client, table_name, user_id).await
}
