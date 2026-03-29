use aws_sdk_dynamodb::{Client as DynamoClient} ;
use doxle_atoms::media;
use lambda_http::{Body, Error, Response, http::StatusCode};
use serde::Deserialize;


#[derive(Debug, Deserialize)]
struct CreateTaskImageRequest {
	image_id: String,
	image_name: String,
	url: String,
}



/// 1. HANDLER: Creates an image (raw internet text into json)
pub async fn create_image_for_task_handler(
	client: &DynamoClient,
	table_name: &str,
	project_id: &str,
	block_id: &str,
	task_id: &str,
	body: &[u8],
	)-> Result<Response<Body>, Error> {

	 // 🔍 LOG 1: raw request coming in
    tracing::info!(
        "📥 create_image_for_task_handler: table={}, block_id={}, task_id={}, raw_body={}",
        table_name,
        block_id,
        task_id,
        String::from_utf8_lossy(body),
    );


	// Step A: Convert Raw Bytes -> Rust Struct
	let  req:CreateTaskImageRequest = serde_json::from_slice(body)?;

	
	// 🔍 LOG 2: parsed payload before overriding task_id
    tracing::info!(
        "📦 Parsed CreateTaskImageRequest url = {}",
        req.url,
    );

    // Step B: call shared media atom
    let result = media::create_image_for_task(
    		client,
    		table_name,
    		project_id,
    		block_id,
    		task_id,
    		req.image_id,
    		req.image_name.clone(),
    		req.url,
    		None, //order
    		None, //width
    		None, //height
    ).await;

    match result {
    	Ok(image) => {
    		tracing::info!(
                "✅ create_image_for_task_handler success: image_id={}, task_id={:?}, block_id={}",
                image.image_id,
                image.task_id,
                image.block_id,
            );

            Ok(Response::builder()
                .status(StatusCode::CREATED)
                .header("Content-Type", "application/json")
                .header("Access-Control-Allow-Origin", "*")
                .body(serde_json::to_string(&image)?.into())
                .map_err(Box::new)?)
    	},
    	Err(e) => {
            tracing::error!(
                "❌ create_image_for_task_handler failed: table={}, block_id={}, task_id={}, error={}",
                table_name,
                block_id,
                task_id,
                e
            );

            Ok(Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header("Content-Type", "application/json")
                .header("Access-Control-Allow-Origin", "*")
                .body(serde_json::json!({ "error": e }).to_string().into())
                .map_err(Box::new)?)
        }
    }
	
	
}



// HTTP handler: GET /blocks/{block_id}/tasks/{task_id}/images
pub async fn list_images_for_task_handler(
    client: &DynamoClient,
    table_name: &str,
    block_id: &str,
    task_id: &str,
) -> Result<Response<Body>, Error> {
    match media::load_images_for_task(client, table_name, block_id, task_id).await {
        Ok(images) => {
            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json")
                .header("Access-Control-Allow-Origin", "*")
                .body(serde_json::to_string(&images)?.into())
                .map_err(Box::new)?)
        }
        Err(e) => {
            tracing::error!(
                "❌ list_images_for_task_handler failed: table={}, block_id={}, task_id={}, error={}",
                table_name,
                block_id,
                task_id,
                e
            );

            Ok(Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header("Content-Type", "application/json")
                .header("Access-Control-Allow-Origin", "*")
                .body(serde_json::json!({ "error": e }).to_string().into())
                .map_err(Box::new)?)
        }
    }
}
