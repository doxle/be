use aws_sdk_sesv2::Client as SesClient;
use lambda_http::{http::StatusCode, Body, Error, Response};
use serde::{Deserialize, Serialize};

use crate::email::send_contact_email;

#[derive(Deserialize)]
pub struct ContactRequest {
    pub email: String,
    pub message: String,
}

#[derive(Serialize)]
struct ContactResponse {
    message: String,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
    message: String,
}

/// Handle contact form submission
pub async fn handle_contact(
    ses_client: &SesClient,
    body: &Body,
) -> Result<Response<Body>, Error> {
    let body_str = match body {
        Body::Text(text) => text,
        Body::Binary(bytes) => std::str::from_utf8(bytes).unwrap_or(""),
        Body::Empty => "",
    };

    tracing::info!("Contact form submission received");

    let contact_request: ContactRequest = match serde_json::from_str(body_str) {
        Ok(req) => req,
        Err(e) => {
            tracing::error!("Failed to parse contact request: {}", e);
            let error = ErrorResponse {
                error: "InvalidRequest".to_string(),
                message: format!("Invalid request body: {}", e),
            };
            return Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header("Content-Type", "application/json")
                .header("Access-Control-Allow-Origin", "*")
                .body(serde_json::to_string(&error)?.into())
                .map_err(Box::new)?);
        }
    };

    // Basic validation
    if contact_request.email.is_empty() || !contact_request.email.contains('@') {
        let error = ErrorResponse {
            error: "InvalidEmail".to_string(),
            message: "Please provide a valid email address".to_string(),
        };
        return Ok(Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::to_string(&error)?.into())
            .map_err(Box::new)?);
    }

    if contact_request.message.is_empty() {
        let error = ErrorResponse {
            error: "InvalidMessage".to_string(),
            message: "Please provide a message".to_string(),
        };
        return Ok(Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::to_string(&error)?.into())
            .map_err(Box::new)?);
    }

    // Send email
    match send_contact_email(ses_client, &contact_request.email, &contact_request.message).await {
        Ok(_) => {
            tracing::info!("Contact email sent successfully from: {}", contact_request.email);
            let response = ContactResponse {
                message: "Message sent successfully".to_string(),
            };
            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json")
                .header("Access-Control-Allow-Origin", "*")
                .body(serde_json::to_string(&response)?.into())
                .map_err(Box::new)?)
        }
        Err(e) => {
            tracing::error!("Failed to send contact email: {}", e);
            let error = ErrorResponse {
                error: "EmailFailed".to_string(),
                message: "Failed to send message. Please try again later.".to_string(),
            };
            Ok(Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header("Content-Type", "application/json")
                .header("Access-Control-Allow-Origin", "*")
                .body(serde_json::to_string(&error)?.into())
                .map_err(Box::new)?)
        }
    }
}
