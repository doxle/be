use lambda_http::{Body, Error, Response, http::StatusCode};
use aws_sdk_s3::{Client as S3Client};

/// Proxy an image from S3 through Lambda
/// This streams the image directly from S3 to the response
pub async fn proxy_image(
    s3_client: &S3Client,
    bucket: &str,
    key: &str,
) -> Result<Response<Body>, Error> {
    // Fetch object from S3 with detailed error logging
    let result = s3_client
        .get_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .map_err(|e| {
              // Log full error for debugging; Display usually includes code/message
            tracing::error!(
                "S3 get_object error for bucket={} key={}: {}",
                bucket,
                key,
                e
            );
            // And also log debug form for extra context
            tracing::error!(
                "S3 get_object error (debug) for bucket={} key={}: {:?}",
                bucket,
                key,
                e
            );
            format!("Failed to get object from S3: {:?}", e)
        })?;

    // Get content type
    let content_type = result
        .content_type()
        .unwrap_or("application/octet-stream")
        .to_string();

    // Get the body bytes
    let body_bytes = result
        .body
        .collect()
        .await
        .map_err(|e| format!("Failed to read S3 body: {}", e))?
        .into_bytes();

    // Return image with proper headers
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", content_type)
        .header("Access-Control-Allow-Origin", "*")
        .header("Cache-Control", "public, max-age=31536000, immutable") // Cache for 1 year
        .body(body_bytes.to_vec().into())
        .map_err(Box::new)?)
}
