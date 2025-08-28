use axum::{
    body::Body,
    extract::{Request, ConnectInfo},
    middleware::Next,
    response::Response,
};
use futures::Future;
use std::{net::SocketAddr, pin::Pin};
use tracing::{info, debug, warn};

use crate::connection::{
    ConnectionInfo, CONNECTION_REGISTRY,
    extensions::ConnectionExtension,
};

/// Enhanced middleware to monitor client connections and detect disconnections
/// 
/// This middleware:
/// 1. Creates a unique connection identifier for each request
/// 2. Registers the connection in the global registry
/// 3. Monitors for client disconnections during request processing
/// 4. Cleans up connection state when the request completes
pub fn connection_monitor(
    request: Request,
    next: Next,
) -> Pin<Box<dyn Future<Output = Response> + Send>>
{
    Box::pin(async move {
        // Extract remote address from connection info or headers
        let remote_addr = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|connect_info| connect_info.0)
            .or_else(|| {
                // Fallback to headers for proxy scenarios
                request.headers()
                    .get("x-forwarded-for")
                    .and_then(|h| h.to_str().ok())
                    .and_then(|addr_str| addr_str.parse().ok())
                    .or_else(|| {
                        request.headers()
                            .get("x-real-ip")
                            .and_then(|h| h.to_str().ok())
                            .and_then(|addr_str| addr_str.parse().ok())
                    })
            });

        // Create connection info for this request
        let conn_info = ConnectionInfo::new(remote_addr);
        let conn_id = conn_info.id;
        let conn_token = conn_info.cancel_token.clone();

        // Log the new connection
        if let Some(addr) = remote_addr {
            info!("[CONNECTION] New request from {} ({})", addr, conn_id);
        } else {
            info!("[CONNECTION] New request ({})", conn_id);
        }

        // Register the connection
        CONNECTION_REGISTRY.register_connection(conn_info.clone()).await;
        
        // Increment request counter
        let request_num = conn_info.increment_request_count();
        debug!("[CONNECTION] Request #{} for connection {}", request_num, conn_id);

        // Add connection info as an extension to the request
        let mut request = request;
        request.extensions_mut().insert(ConnectionExtension(conn_info));

        // Create a future that monitors for disconnections
        let disconnect_monitor = async {
            conn_token.cancelled().await;
            debug!("[CONNECTION] Connection {} was cancelled", conn_id);
        };

        // Process the request with disconnection monitoring
        let response = tokio::select! {
            response = next.run(request) => {
                debug!("[CONNECTION] Request completed normally for {}", conn_id);
                response
            }
            _ = disconnect_monitor => {
                warn!("[CONNECTION] Client disconnected during request processing ({})", conn_id);
                // Return a 499 Client Closed Request response
                Response::builder()
                    .status(499)
                    .body(Body::from("Client Closed Request"))
                    .unwrap_or_else(|_| Response::new(Body::empty()))
            }
        };

        // Check if this is a streaming response
        let is_streaming_response = response.headers()
            .get("content-type")
            .and_then(|ct| ct.to_str().ok())
            .map(|ct| ct.contains("text/event-stream"))
            .unwrap_or(false);

        if is_streaming_response {
            // For streaming responses, don't cancel the connection token
            // The streaming handler will manage its own lifecycle
            debug!("[CONNECTION] Streaming response detected for {}, deferring connection cleanup", conn_id);
            CONNECTION_REGISTRY.unregister_connection(conn_id).await;
        } else {
            // For non-streaming responses, cancel the connection token immediately
            debug!("[CONNECTION] Non-streaming response for {}, canceling connection", conn_id);
            CONNECTION_REGISTRY.cancel_connection(conn_id).await;
            CONNECTION_REGISTRY.unregister_connection(conn_id).await;
        }
        
        response
    })
}

/// Middleware to extract connection information from request extensions
/// This is a helper for handlers that need access to connection info
pub async fn extract_connection_info(request: &Request) -> Option<ConnectionInfo> {
    request
        .extensions()
        .get::<ConnectionExtension>()
        .map(|ext| ext.0.clone())
}

/// Get the connection cancellation token from the current request
pub fn get_connection_cancel_token(request: &Request) -> Option<tokio_util::sync::CancellationToken> {
    request
        .extensions()
        .get::<ConnectionExtension>()
        .map(|ext| ext.cancel_token.clone())
}

/// Get the connection ID from the current request
pub fn get_connection_id(request: &Request) -> Option<crate::connection::ConnectionId> {
    request
        .extensions()
        .get::<ConnectionExtension>()
        .map(|ext| ext.0.id)
}