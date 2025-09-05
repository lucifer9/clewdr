use std::time::Instant;

use axum::{Extension, extract::State, response::Response};
use tracing::info;

use crate::{
    SHUTDOWN_TOKEN,
    claude_code_state::ClaudeCodeState,
    error::ClewdrError,
    middleware::claude::{ClaudeCodePreprocess, ClaudeContext},
    utils::{enabled_plain, print_out_json},
};

pub async fn api_claude_code(
    State(mut state): State<ClaudeCodeState>,
    ClaudeCodePreprocess(p, f, conn_token): ClaudeCodePreprocess,
) -> Result<(Extension<ClaudeContext>, Response), ClewdrError> {
    state.system_prompt_hash = f.system_prompt_hash();
    state.stream = p.stream.unwrap_or_default();
    state.api_format = f.api_format();
    state.usage = f.usage().to_owned();
    print_out_json(&p, "claude_code_client_req.json");
    info!(
        stream = %enabled_plain(state.stream),
        msgs = %p.messages.len(),
        model = %p.model,
        thinking = %enabled_plain(p.thinking.is_some()),
        format = %f.api_format(),
        "Claude Code request received"
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
