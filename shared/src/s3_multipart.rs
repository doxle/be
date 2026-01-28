use lambda_http::{Body, Error, Response, http::StatusCode};
use aws_sdk_s3::Client as S3Client;
use serde::{Deserialize, Serialize};
use crate::types::{ImageMetadata, ImageLevel};
use crate::image_processing;

// const BUCKET_NAME: &str = "doxle-annotations";
const MULTIPART_THRESHOLD: usize = 50 * 1024 * 1024; // 50MB

#[derive(Deserialize)]
pub struct InitiateUploadRequest {
    pub block_id: String,
    pub file_name: String,
    pub content_type: String,
    pub file_size: usize,
}
    
#[derive(Serialize)]
pub struct InitiateUploadResponse {
    pub image_id: String,
    pub upload_id: Option<String>, // For multipart
    pub upload_urls: Vec<UploadPart>,
    pub is_multipart: bool,
    pub extension: String,
}

#[derive(Serialize)]
pub struct UploadPart {
    pub part_number: i32,
    pub upload_url: String,
}

#[derive(Deserialize)]
pub struct CompleteMultipartRequest {
    pub block_id: String,
    pub image_id: String,
    pub upload_id: String,
    pub extension: String,
    pub parts: Vec<CompletedPart>,
}

#[derive(Deserialize, Serialize)]
pub struct CompletedPart {
    pub part_number: i32,
    pub etag: String,
}

#[derive(Serialize)]
pub struct UploadCompleteResponse {
    pub image_id: String,
    pub url: String,
}

fn get_bucket_name()->String{
    std::env::var("S3_BUCKET_NAME").unwrap_or_else(|_| "doxle-app".to_string())
}

/// Initiate upload - returns single or multipart presigned URLs
pub async fn initiate_upload(
    s3_client: &S3Client,
    request: InitiateUploadRequest,
) -> Result<Response<Body>, Error> {
    let image_id = uuid::Uuid::new_v4().to_string();
    
    let extension = request.file_name
        .split('.')
        .last()
        .unwrap_or("jpg")
        .to_string();
    
    // Updated S3 key structure: annotations/blocks/{block_id}/images/{image_id}.{ext}
    let s3_key = format!(
        "annotations/blocks/{}/images/{}.{}",
        request.block_id,
        image_id,
        extension
    );
    
    let is_multipart = request.file_size >= MULTIPART_THRESHOLD;
    
    if is_multipart {
        // Multipart upload for files >= 5MB
        let num_parts = (request.file_size as f64 / MULTIPART_THRESHOLD as f64).ceil() as i32;
        
        // Initiate multipart upload
        let create_result = s3_client
            .create_multipart_upload()
            .bucket(&get_bucket_name())
            .key(&s3_key)
            .content_type(&request.content_type)
            .send()
            .await
            .map_err(|e| format!("Failed to initiate multipart upload: {}", e))?;
        
        let upload_id = create_result.upload_id()
            .ok_or("No upload ID returned")?
            .to_string();
        
        // Generate presigned URLs for each part
        let mut upload_parts = Vec::new();
        
        for part_number in 1..=num_parts {
            let presigned = s3_client
                .upload_part()
                .bucket(&get_bucket_name())
                .key(&s3_key)
                .upload_id(&upload_id)
                .part_number(part_number)
                .presigned(
                    aws_sdk_s3::presigning::PresigningConfig::expires_in(
                        std::time::Duration::from_secs(3600)
                    )?
                )
                .await
                .map_err(|e| format!("Failed to generate presigned URL for part {}: {}", part_number, e))?;
            
            upload_parts.push(UploadPart {
                part_number,
                upload_url: presigned.uri().to_string(),
            });
        }
        
        let response = InitiateUploadResponse {
            image_id: image_id.clone(),
            upload_id: Some(upload_id),
            upload_urls: upload_parts,
            is_multipart: true,
            extension: extension.clone(),
        };
        
        Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::to_string(&response)?.into())
            .map_err(Box::new)?)
            
    } else {
        // Single part upload for files < 5MB
        let presigned = s3_client
            .put_object()
            .bucket(&get_bucket_name())
            .key(&s3_key)
            .content_type(&request.content_type)
            .presigned(
                aws_sdk_s3::presigning::PresigningConfig::expires_in(
                    std::time::Duration::from_secs(3600)
                )?
            )
            .await
            .map_err(|e| format!("Failed to generate presigned URL: {}", e))?;
        
        let response = InitiateUploadResponse {
            image_id: image_id.clone(),
            upload_id: None,
            upload_urls: vec![UploadPart {
                part_number: 1,
                upload_url: presigned.uri().to_string(),
            }],
            is_multipart: false,
            extension: extension.clone(),
        };
        
        Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .header("Access-Control-Allow-Origin", "*")
            .body(serde_json::to_string(&response)?.into())
            .map_err(Box::new)?)
    }
}

/// Complete multipart upload
pub async fn complete_multipart_upload(
    s3_client: &S3Client,
    request: CompleteMultipartRequest,
) -> Result<Response<Body>, Error> {
    let s3_key = format!(
        "annotations/blocks/{}/images/{}.{}",
        request.block_id,
        request.image_id,
        request.extension
    );
    
    // Only complete multipart if there are parts (multipart upload)
    // For single-part uploads, parts will be empty and upload_id will be empty
    if !request.parts.is_empty() && !request.upload_id.is_empty() {
        // Build completed parts
        let mut completed_parts = Vec::new();
        for part in &request.parts {
            let completed_part = aws_sdk_s3::types::CompletedPart::builder()
                .part_number(part.part_number)
                .e_tag(&part.etag)
                .build();
            completed_parts.push(completed_part);
        }
        
        let completed_upload = aws_sdk_s3::types::CompletedMultipartUpload::builder()
            .set_parts(Some(completed_parts))
            .build();
        
        // Complete the multipart upload
        s3_client
            .complete_multipart_upload()
            .bucket(&get_bucket_name())
            .key(&s3_key)
            .upload_id(&request.upload_id)
            .multipart_upload(completed_upload)
            .send()
            .await
            .map_err(|e| format!("Failed to complete multipart upload: {}", e))?;
    }
    
    // Process image asynchronously (generate pyramid if needed)
    tracing::info!("🔄 Starting post-upload processing for image: {}", request.image_id);
    //@sk causing slowness match process_uploaded_image(
    //     s3_client,
    //     &request.block_id,
    //     &request.image_id,
    //     &request.extension,
    // ).await {
    //     Ok(metadata) => {
    //         tracing::info!("✅ Image processing complete: {} levels", metadata.levels.len());
    //     }
    //     Err(e) => {
    //         tracing::error!("⚠️ Image processing failed (continuing anyway): {}", e);
    //     }
    // }
    
    // Generate public URL (use first level path)
    let url = format!(
        "https://{}.s3.amazonaws.com/{}",
        &get_bucket_name(),
        s3_key
    );
    
    let response = UploadCompleteResponse {
        image_id: request.image_id.clone(),
        url,
    };
    
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(serde_json::to_string(&response)?.into())
        .map_err(Box::new)?)
}

/// Abort multipart upload (cleanup on failure)
pub async fn abort_multipart_upload(
    s3_client: &S3Client,
    block_id: String,
    image_id: String,
    upload_id: String,
    extension: String,
) -> Result<Response<Body>, Error> {
    let s3_key = format!(
        "annotations/blocks/{}/images/{}.{}",
        block_id,
        image_id,
        extension
    );
    
    s3_client
        .abort_multipart_upload()
        .bucket(&get_bucket_name())
        .key(&s3_key)
        .upload_id(&upload_id)
        .send()
        .await
        .map_err(|e| format!("Failed to abort multipart upload: {}", e))?;
    
    Ok(Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header("Access-Control-Allow-Origin", "*")
        .body(Body::Empty)
        .map_err(Box::new)?)
}

/// Process uploaded image: generate half-width if needed and create metadata
pub async fn process_uploaded_image(
    s3_client: &S3Client,
    block_id: &str,
    image_id: &str,
    extension: &str,
) -> Result<ImageMetadata, String> {
    let original_key = format!(
        "annotations/blocks/{}/images/{}.{}",
        block_id, image_id, extension
    );
    
    // Download original image from S3
    tracing::info!("📥 Downloading image from S3: {}", original_key);
    let result = s3_client
        .get_object()
        .bucket(&get_bucket_name())
        .key(&original_key)
        .send()
        .await
        .map_err(|e| format!("Failed to download image: {}", e))?;
    
    let image_bytes = result
        .body
        .collect()
        .await
        .map_err(|e| format!("Failed to read image bytes: {}", e))?
        .into_bytes()
        .to_vec();
    
    let file_size = image_bytes.len();
    
    // Get dimensions
    let (width, height) = image_processing::get_dimensions(&image_bytes)?;
    
    tracing::info!("📐 Image dimensions: {}x{}, size: {} bytes", width, height, file_size);
    
    // Check if we need half-width version
    let needs_pyramid = image_processing::needs_half_width(file_size, width, height);
    
    let mut levels = vec![];
    
    if needs_pyramid {
        tracing::info!("🔄 Generating half-width version...");
        
        // Generate half-width
        let (half_width, half_height, half_bytes) = image_processing::generate_half_width(&image_bytes)?;
        let half_size = half_bytes.len();
        
        // Upload structure: annotations/blocks/{bid}/images/{img_id}/
        let base_path = format!("annotations/blocks/{}/images/{}", block_id, image_id);
        
        // Upload full resolution (move original to folder)
        let full_key = format!("{}/{}w.{}", base_path, width, extension);
        tracing::info!("📤 Uploading full resolution to: {}", full_key);
        s3_client
            .put_object()
            .bucket(&get_bucket_name())
            .key(&full_key)
            .body(image_bytes.into())
            .send()
            .await
            .map_err(|e| format!("Failed to upload full resolution: {}", e))?;
        
        // Delete old flat file
        s3_client
            .delete_object()
            .bucket(&get_bucket_name())
            .key(&original_key)
            .send()
            .await
            .ok(); // Ignore errors
        
        // Upload half-width (JPEG)
        let half_key = format!("{}/{}w.jpg", base_path, half_width);
        tracing::info!("📤 Uploading half-width to: {}", half_key);
        s3_client
            .put_object()
            .bucket(&get_bucket_name())
            .key(&half_key)
            .body(half_bytes.into())
            .content_type("image/jpeg")
            .send()
            .await
            .map_err(|e| format!("Failed to upload half-width: {}", e))?;
        
        // Build metadata
        levels.push(ImageLevel {
            width,
            height,
            path: format!("{}w.{}", width, extension),
            size: file_size,
            purpose: "full".to_string(),
        });
        
        levels.push(ImageLevel {
            width: half_width,
            height: half_height,
            path: format!("{}w.jpg", half_width),
            size: half_size,
            purpose: "preview".to_string(),
        });
        
        // Upload metadata.json
        let metadata = ImageMetadata {
            original_width: width,
            original_height: height,
            file_size,
            format: extension.to_string(),
            levels: levels.clone(),
        };
        
        let metadata_json = serde_json::to_string(&metadata)
            .map_err(|e| format!("Failed to serialize metadata: {}", e))?;
        
        let metadata_key = format!("{}/metadata.json", base_path);
        tracing::info!("📤 Uploading metadata to: {}", metadata_key);
        s3_client
            .put_object()
            .bucket(&get_bucket_name())
            .key(&metadata_key)
            .body(metadata_json.into_bytes().into())
            .content_type("application/json")
            .send()
            .await
            .map_err(|e| format!("Failed to upload metadata: {}", e))?;
        
        tracing::info!("✅ Image processing complete: pyramid created");
        Ok(metadata)
        
    } else {
        tracing::info!("✅ Image is small enough, no pyramid needed");
        
        // Single level metadata
        levels.push(ImageLevel {
            width,
            height,
            path: format!("{}.{}", image_id, extension),
            size: file_size,
            purpose: "full".to_string(),
        });
        
        Ok(ImageMetadata {
            original_width: width,
            original_height: height,
            file_size,
            format: extension.to_string(),
            levels,
        })
    }
}
