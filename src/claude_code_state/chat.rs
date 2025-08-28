use colored::Colorize;
use snafu::ResultExt;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, error, info};

use crate::{
    claude_code_state::{ClaudeCodeState, TokenStatus},
    config::CLEWDR_CONFIG,
    error::{CheckClaudeErr, ClewdrError, WreqSnafu},
    types::claude::CreateMessageParams,
    utils::forward_response,
};

impl ClaudeCodeState {
    /// Attempts to send a chat message to Claude API with retry mechanism
    ///
    /// This method handles the complete chat flow including:
    /// - Request preparation and logging
    /// - Cookie management for authentication
    /// - Executing the chat request with automatic retries on failure
    /// - Response transformation according to the specified API format
    /// - Error handling and cleanup
    ///
    /// The method implements a sophisticated retry mechanism to handle transient failures,
    /// and manages conversation cleanup to prevent resource leaks. It also includes
    /// performance tracking to measure response times.
    ///
    /// # Arguments
    /// * `p` - The client request body containing messages and configuration
    /// * `cancellation_token` - Token to cancel the operation when signalled
    ///
    /// # Returns
    /// * `Result<axum::response::Response, ClewdrError>` - Formatted response or error
    pub async fn try_chat(
        &mut self,
        p: CreateMessageParams,
        cancellation_token: CancellationToken,
    ) -> Result<axum::response::Response, ClewdrError> {
        for i in 0..CLEWDR_CONFIG.load().max_retries + 1 {
            if i > 0 {
                info!("[RETRY] attempt: {}", i.to_string().green());
            }
            let mut state = self.to_owned();
            let p = p.to_owned();

            let cookie = state.request_cookie().await?;
            let retry = async {
                match state.check_token() {
                    TokenStatus::None => {
                        info!("No token found, requesting new token");
                        let org = state.get_organization().await?;
                        let code_res = state.exchange_code(&org).await?;
                        state.exchange_token(code_res).await?;
                        state.return_cookie(None).await;
                    }
                    TokenStatus::Expired => {
                        info!("Token expired, refreshing token");
                        state.refresh_token().await?;
                        state.return_cookie(None).await;
                    }
                    TokenStatus::Valid => {
                        info!("Token is valid, proceeding with request");
                    }
                }
                let Some(access_token) = state.cookie.as_ref().and_then(|c| c.token.to_owned())
                else {
                    return Err(ClewdrError::UnexpectedNone {
                        msg: "No access token found in cookie",
                    });
                };
                state
                    .send_chat(access_token.access_token.to_owned(), p)
                    .await
            }
            .instrument(tracing::info_span!(
                "claude_code",
                "cookie" = cookie.cookie.ellipse()
            ));

            let result = tokio::select! {
                res = retry => res,
                _ = cancellation_token.cancelled() => {
                    info!("[CANCELLED] Claude Code request cancelled");
                    return Err(ClewdrError::RequestCancelled);
                }
            };

            match result {
                Ok(res) => {
                    return Ok(res);
                }
                Err(e) => {
                    error!(
                        "[{}] {}",
                        state.cookie.as_ref().unwrap().cookie.ellipse().green(),
                        e
                    );
                    // 429 error
                    if let ClewdrError::InvalidCookie { reason } = e {
                        state.return_cookie(Some(reason.to_owned())).await;
                        continue;
                    }
                    return Err(e);
                }
            }
        }
        Err(ClewdrError::TooManyRetries)
    }

    pub async fn send_chat(
        &mut self,
        access_token: String,
        mut p: CreateMessageParams,
    ) -> Result<axum::response::Response, ClewdrError> {
        // Check if model is 1M context version and prepare for API
        let beta_header = if let Some(model) = p.model.strip_suffix("-1M") {
            // Remove -1M suffix before sending to API
            p.model = model.to_string();
            "oauth-2025-04-20,context-1m-2025-08-07"
        } else {
            "oauth-2025-04-20"
        };

        let api_res = self
            .client
            .post(format!("{}/v1/messages", self.endpoint))
            .bearer_auth(access_token)
            .header("anthropic-beta", beta_header)
            .header("anthropic-version", "2023-06-01")
            .json(&p)
            .send()
            .await
            .context(WreqSnafu {
                msg: "Failed to send chat message",
            })?
            .check_claude()
            .await?;
        forward_response(api_res)
    }
}
