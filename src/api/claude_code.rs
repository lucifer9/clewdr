use std::time::Instant;

use axum::{Extension, extract::State, response::Response};
use colored::Colorize;
use tracing::info;

use crate::{
    SHUTDOWN_TOKEN,
    claude_code_state::ClaudeCodeState,
    error::ClewdrError,
    middleware::claude::{ClaudeApiFormat, ClaudeCodePreprocess, ClaudeContext},
    utils::{enabled, print_out_json},
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
    let format_display = match f.api_format() {
        ClaudeApiFormat::Claude => ClaudeApiFormat::Claude.to_string().green(),
        ClaudeApiFormat::OpenAI => ClaudeApiFormat::OpenAI.to_string().yellow(),
    };
    info!(
        "[REQ] stream: {}, msgs: {}, model: {}, think: {}, format: {}",
        enabled(state.stream),
        p.messages.len().to_string().green(),
        p.model.green(),
        enabled(p.thinking.is_some()),
        format_display
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
    info!(
        "[FIN] elapsed: {}s",
        format!("{}", elapsed.as_secs_f32()).green()
    );

    res.map(|r| (Extension(f), r))
}
