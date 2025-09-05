use std::time::Instant;

use axum::{Extension, extract::State, response::Response};
use tracing::info;

use crate::{
    SHUTDOWN_TOKEN,
    claude_web_state::ClaudeWebState,
    error::ClewdrError,
    middleware::claude::{ClaudeContext, ClaudeWebPreprocess},
    utils::{enabled_plain, print_out_json},
};
/// Axum handler for the API messages
/// Main API endpoint for handling message requests to Claude
/// Processes messages, handles retries, and returns responses in stream or non-stream mode
///
/// # Arguments
/// * `XApiKey(_)` - API key authentication
/// * `state` - Application state containing client information
/// * `p` - Request body containing messages and configuration
///
/// # Returns
/// * `Response` - Stream or JSON response from Claude
pub async fn api_claude_web(
    State(mut state): State<ClaudeWebState>,
    ClaudeWebPreprocess(p, f, conn_token): ClaudeWebPreprocess,
) -> Result<(Extension<ClaudeContext>, Response), ClewdrError> {
    let stream = p.stream.unwrap_or_default();
    print_out_json(&p, "claude_web_client_req.json");
    state.api_format = f.api_format();
    state.stream = stream;
    state.usage = f.usage().to_owned();
    info!(
        stream = %enabled_plain(stream),
        msgs = %p.messages.len(),
        model = %p.model,
        thinking = %enabled_plain(p.thinking.is_some()),
        format = %f.api_format(),
        "Claude Web request received"
    );
    let stopwatch = Instant::now();
    
    // Create a child token for this request from the global shutdown token
    let global_request_token = SHUTDOWN_TOKEN.child_token();
    
    // Create a composite token that responds to both global shutdown and connection disconnection
    let composite_token = if let Some(conn_token) = conn_token {
        let composite = global_request_token.child_token();
        
        // Create a task that cancels the composite token if either parent is cancelled
        let composite_clone = composite.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = global_request_token.cancelled() => {
                    info!("[CANCEL] Global shutdown signal received");
                    composite_clone.cancel();
                }
                _ = conn_token.cancelled() => {
                    info!("[CANCEL] Connection disconnect signal received");
                    composite_clone.cancel();
                }
            }
        });
        
        composite
    } else {
        global_request_token
    };
    
    let res = tokio::select! {
        result = state.try_chat(p, composite_token.clone()) => result,
        _ = composite_token.cancelled() => {
            info!("[CANCELLED] Request cancelled by signal");
            Err(ClewdrError::RequestCancelled)
        }
    };

    let elapsed = stopwatch.elapsed();
    info!(elapsed_secs = %elapsed.as_secs_f32(), "[FIN] elapsed");

    res.map(|r| (Extension(f), r))
}
