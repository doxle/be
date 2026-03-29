use lambda_http::{run, service_fn, tracing, Error, Request};
use aws_sdk_cognitoidentityprovider::Client as CognitoClient;
use aws_sdk_dynamodb::Client as DynamoClient;
use aws_sdk_lambda::Client as LambdaClient;
use aws_sdk_apigatewaymanagement::Client as ApiGatewayManagementClient;
use aws_sdk_s3::Client as S3Client;
use aws_sdk_sesv2::Client as SesClient;
use doxle_shared::AppState;
use std::sync::Arc;

mod http_handler;

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing::init_default_subscriber();
    
    // Initialize AWS clients once at startup
    let config = aws_config::load_from_env().await;
    
    // API Gateway Management client for WebSocket (optional endpoint)
    let api_gateway_client = std::env::var("WS_API_ENDPOINT").ok().map(|endpoint| {
        let api_config = aws_sdk_apigatewaymanagement::config::Builder::from(&config)
            .endpoint_url(endpoint)
            .build();
        ApiGatewayManagementClient::from_conf(api_config)
    });
    
    let state = AppState::new(
        CognitoClient::new(&config),
        DynamoClient::new(&config),
        LambdaClient::new(&config),
        S3Client::new(&config),
        SesClient::new(&config),
        api_gateway_client,
    );
    
    run(service_fn(move |event: Request| {
        let state = Arc::clone(&state);
        async move {
            // For now, assume all events are HTTP until we set up WebSocket API
            // We'll detect WebSocket events by checking if body contains WebSocket message format
            let body_str = std::str::from_utf8(event.body()).unwrap_or("");
            let is_websocket = body_str.contains("\"action\":") && 
                              (body_str.contains("connect") || body_str.contains("disconnect") || body_str.contains("message"));
            
            if is_websocket {
                doxle_shared::sockets::handle_websocket_event(event, state).await
            } else {
                http_handler::function_handler(event, state).await
            }
        }
    })).await
}
