use async_stream::stream;
use axum::{body::Body, extract::State, response::Response};
use bytes::Bytes;
use colored::Colorize;
use futures::{Stream, StreamExt};
use http::header::CONTENT_TYPE;
use serde::Serialize;
use serde_json::json;
use std::{
    pin::Pin,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{select, time::interval};
use tracing::{info, debug};

use crate::{
    SHUTDOWN_TOKEN,
    config::CLEWDR_CONFIG,
    error::ClewdrError,
    gemini_state::{GeminiApiFormat, GeminiState},
    middleware::gemini::{GeminiContext, GeminiOaiPreprocess, GeminiPreprocess},
    utils::enabled,
};

// Convert complete response to streaming chunks
async fn response_to_stream_chunks(
    response: Response,
    ctx: &GeminiContext,
) -> Result<Pin<Box<dyn Stream<Item = Result<Bytes, axum::Error>> + Send>>, ClewdrError> {
    use axum::body::to_bytes;

    // Get the response body as bytes
    let body_bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .map_err(|_e| ClewdrError::UnexpectedNone {
            msg: "Failed to read response body",
        })?;

    // Parse and convert based on format
    match ctx.api_format {
        GeminiApiFormat::OpenAI => {
            // Parse as OpenAI format and create streaming chunks
            let response_text = String::from_utf8_lossy(&body_bytes).to_string();
            let model = ctx.model.clone();
            Ok(Box::pin(convert_openai_to_stream(response_text, model)))
        }
        GeminiApiFormat::Gemini => {
            // Parse as Gemini format and create streaming chunks
            let response_text = String::from_utf8_lossy(&body_bytes).to_string();
            Ok(Box::pin(convert_gemini_to_stream(response_text)))
        }
    }
}

// Convert OpenAI format response to streaming chunks
fn convert_openai_to_stream(
    response_text: String,
    model: String,
) -> impl Stream<Item = Result<Bytes, axum::Error>> {
    stream! {
        if let Ok(response_data) = serde_json::from_str::<serde_json::Value>(&response_text)
            && let Some(choices) = response_data["choices"].as_array()
            && let Some(first_choice) = choices.first()
            && let Some(content) = first_choice["message"]["content"].as_str()
        {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            // Send complete content as a single chunk to preserve formatting
            let chunk_data = json!({
                "id": format!("chatcmpl-{}", timestamp),
                "object": "chat.completion.chunk",
                "created": timestamp,
                "model": model,
                "choices": [{
                    "delta": {"content": content},
                    "index": 0,
                    "finish_reason": null
                }]
            });

            yield Ok(Bytes::from(format!("data: {chunk_data}\n\n")));

            // Send final chunk with finish_reason
            let final_chunk = json!({
                "id": format!("chatcmpl-{}", timestamp),
                "object": "chat.completion.chunk",
                "created": timestamp,
                "model": model,
                "choices": [{
                    "delta": {},
                    "index": 0,
                    "finish_reason": "stop"
                }]
            });

            yield Ok(Bytes::from(format!("data: {final_chunk}\n\n")));
            yield Ok(Bytes::from("data: [DONE]\n\n"));
        }
    }
}

// Convert Gemini format response to streaming chunks
fn convert_gemini_to_stream(
    response_text: String,
) -> impl Stream<Item = Result<Bytes, axum::Error>> {
    stream! {
        if let Ok(response_data) = serde_json::from_str::<serde_json::Value>(&response_text)
            && let Some(candidates) = response_data["candidates"].as_array()
            && let Some(first_candidate) = candidates.first()
            && let Some(content) = first_candidate.get("content")
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.as_array())
                .and_then(|arr| arr.first())
                .and_then(|part| part.get("text"))
                .and_then(|t| t.as_str())
        {
            // Send complete content as a single chunk to preserve formatting
            let chunk_data = json!({
                "candidates": [{
                    "content": {
                        "parts": [{"text": content}],
                        "role": "model"
                    },
                    "finishReason": null,
                    "index": 0
                }]
            });

            yield Ok(Bytes::from(format!("data: {chunk_data}\n\n")));

            // Send final chunk with finishReason
            let final_chunk = json!({
                "candidates": [{
                    "content": {
                        "parts": [{"text": ""}],
                        "role": "model"
                    },
                    "finishReason": "STOP",
                    "index": 0
                }]
            });

            yield Ok(Bytes::from(format!("data: {final_chunk}\n\n")));
        }
    }
}

// Create keep-alive chunk based on API format for client compatibility
fn create_keep_alive_chunk(api_format: &GeminiApiFormat) -> String {
    match api_format {
        GeminiApiFormat::OpenAI => {
            // OpenAI format: use minimal but complete JSON chunk for compatibility
            // Based on real-world OpenAI streaming format requirements
            let keep_alive_data = json!({
                "id": "chatcmpl-keepalive",
                "object": "chat.completion.chunk",
                "created": SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                "model": "keepalive",
                "choices": [{
                    "index": 0,
                    "delta": {"content": ""},
                    "finish_reason": null
                }]
            });
            format!("data: {keep_alive_data}\n\n")
        }
        GeminiApiFormat::Gemini => {
            // Gemini format: use larger, legal JSON data to help penetrate carrier NAT
            // Empty candidates array is legal and won't break client parsing
            let empty_gemini_data = json!({
                "candidates": [],
                "metadata": {
                    "keepalive": true,
                    "timestamp": SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis()
                }
            });
            format!("data: {empty_gemini_data}\n\n")
        }
    }
}

// Create error chunk based on API format
fn create_error_chunk(error: &ClewdrError, ctx: &GeminiContext) -> String {
    match ctx.api_format {
        GeminiApiFormat::OpenAI => {
            let error_data = json!({
                "error": {
                    "message": error.to_string(),
                    "type": "api_error",
                    "code": "internal_error"
                }
            });
            format!("data: {error_data}\n\n")
        }
        GeminiApiFormat::Gemini => {
            let error_data = json!({
                "error": {
                    "message": error.to_string(),
                    "code": 500,
                    "status": "INTERNAL"
                }
            });
            format!("data: {error_data}\n\n")
        }
    }
}

// Fake streaming handler - sends keep-alive messages while processing non-streaming request
fn fake_streaming_handler<T>(
    state: GeminiState,
    body: T,
    ctx: GeminiContext,
    cancellation_token: tokio_util::sync::CancellationToken,
    conn_id: Option<crate::connection::ConnectionId>,
) -> impl Stream<Item = Result<Bytes, axum::Error>>
where
    T: Serialize + Clone + Send + 'static,
{
    let config = CLEWDR_CONFIG.load();
    let keep_alive_interval = Duration::from_secs_f64(config.fake_streaming_interval);

    stream! {
        info!("[FAKE_STREAMING] Handler started");

        // Set up the non-streaming request in the background
        let mut non_streaming_state = state.clone();
        non_streaming_state.stream = false;

        // Fix path: change from streamGenerateContent to generateContent for non-streaming
        if non_streaming_state.path.contains("streamGenerateContent") {
            non_streaming_state.path = non_streaming_state.path.replace("streamGenerateContent", "generateContent");
        }

        // Fix query parameters: remove alt=sse if present to avoid SSE format response
        if let Some(alt) = &non_streaming_state.query.alt
            && alt == "sse"
        {
            non_streaming_state.query.alt = None;
        }

        // Create channels for communication
        let (keep_alive_tx, mut keep_alive_rx) = tokio::sync::mpsc::channel::<Bytes>(100);

        // Clone context data for response conversion to avoid partial moves
        let ctx_for_response = ctx.api_format.clone();

        // Spawn independent keep-alive task that runs completely separately from API calls
        let keep_alive_handle = {
            let tx = keep_alive_tx.clone();
            let api_format = ctx_for_response.clone();
            let cancellation_token = cancellation_token.clone();

            tokio::spawn(async move {
                let mut interval = interval(keep_alive_interval);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

                // Send initial keep-alive immediately
                let initial_msg = Bytes::from(create_keep_alive_chunk(&api_format));
                debug!("[FAKE_STREAMING] Generated initial keep-alive chunk ({:?} format, {} bytes)", api_format, initial_msg.len());
                if tx.send(initial_msg).await.is_err() {
                    debug!("[FAKE_STREAMING] Initial channel send failed, receiver dropped");
                    return; // Receiver dropped
                }
                debug!("[FAKE_STREAMING] Initial keep-alive message sent to internal channel successfully");

                loop {
                    tokio::select! {
                        _ = interval.tick() => {
                            let keep_alive = Bytes::from(create_keep_alive_chunk(&api_format));
                            debug!("[FAKE_STREAMING] Generated keep-alive chunk ({:?} format, {} bytes)", api_format, keep_alive.len());
                            if tx.send(keep_alive).await.is_err() {
                                debug!("[FAKE_STREAMING] Channel send failed, receiver dropped");
                                break; // Receiver dropped, task should stop
                            }
                            debug!("[FAKE_STREAMING] Keep-alive message sent to internal channel successfully");
                        }
                        _ = cancellation_token.cancelled() => {
                            info!("[FAKE_STREAMING] Keep-alive task cancelled");
                            break;
                        }
                    }
                }
            })
        };

        // Remove the connection monitor task - it's not needed
        
        // Create API future directly (no spawn to allow cancellation)
        let cancellation_token_for_api = cancellation_token.clone();
        let api_future = async move {
            tokio::select! {
                result = non_streaming_state.try_chat(body, cancellation_token_for_api.clone()) => result,
                _ = cancellation_token_for_api.cancelled() => {
                    info!("[FAKE_STREAMING] API task cancelled");
                    Err(ClewdrError::RequestCancelled)
                }
            }
        };

        // Main event loop - handles keep-alive messages, API completion, and cancellation
        let mut api_future = Box::pin(api_future);
        loop {
            select! {
                biased; // Ensure fair scheduling

                // Cancellation signal
                _ = cancellation_token.cancelled() => {
                    info!("[FAKE_STREAMING] Main loop cancelled");
                    
                    // Clean up the connection on cancellation
                    if let Some(conn_id) = conn_id {
                        use crate::CONNECTION_REGISTRY;
                        CONNECTION_REGISTRY.cancel_connection(conn_id).await;
                        debug!("[FAKE_STREAMING] Connection {} cleaned up after cancellation", conn_id);
                    }
                    
                    // Stop keep-alive task by dropping the sender
                    drop(keep_alive_tx);
                    keep_alive_handle.abort();
                    yield Err(axum::Error::new("Request cancelled"));
                    break;
                }

                // API future completion
                result = &mut api_future => {
                    // Stop keep-alive task by dropping the sender
                    drop(keep_alive_tx);
                    keep_alive_handle.abort();

                    match result {
                        Ok(response) => {
                            // Convert the complete response to streaming format
                            // Create a context for response conversion with cloned api_format
                            let response_ctx = GeminiContext {
                                api_format: ctx_for_response,
                                stream: ctx.stream,
                                model: ctx.model,
                                vertex: ctx.vertex,
                                path: ctx.path,
                                query: ctx.query,
                            };
                            let chunks = response_to_stream_chunks(response, &response_ctx).await;
                            match chunks {
                                Ok(chunk_stream) => {
                                    let mut chunk_stream = chunk_stream;
                                    while let Some(chunk) = chunk_stream.next().await {
                                        match &chunk {
                                            Ok(bytes) => debug!("[FAKE_STREAMING] Sending API response chunk to client ({} bytes)", bytes.len()),
                                            Err(_) => debug!("[FAKE_STREAMING] Sending API error chunk to client"),
                                        }
                                        yield chunk;
                                    }
                                }
                                Err(e) => {
                                    yield Err(axum::Error::new(format!("Failed to convert response: {e}")));
                                }
                            }
                        }
                        Err(e) => {
                            // Send error in streaming format
                            let error_ctx = GeminiContext {
                                api_format: ctx_for_response,
                                stream: ctx.stream,
                                model: ctx.model,
                                vertex: ctx.vertex,
                                path: ctx.path,
                                query: ctx.query,
                            };
                            let error_chunk = create_error_chunk(&e, &error_ctx);
                            yield Ok(Bytes::from(error_chunk));
                        }
                    }
                    break;
                }

                // Keep-alive messages from independent task
                keep_alive_msg = keep_alive_rx.recv() => {
                    match keep_alive_msg {
                        Some(msg) => {
                            debug!("[FAKE_STREAMING] Received keep-alive from channel ({} bytes)", msg.len());
                            debug!("[FAKE_STREAMING] Sending keep-alive to client: {}", 
                                   String::from_utf8_lossy(&msg[..std::cmp::min(100, msg.len())]));
                            yield Ok(msg);
                            debug!("[FAKE_STREAMING] Keep-alive successfully sent to client");
                        }
                        None => {
                            debug!("[FAKE_STREAMING] Keep-alive channel closed");
                            // Keep-alive task ended (shouldn't happen before API completes)
                        }
                    }
                }
            }
        }

        info!("[FAKE_STREAMING] Handler completed");
        
        // Clean up the connection after streaming completes
        if let Some(conn_id) = conn_id {
            use crate::CONNECTION_REGISTRY;
            CONNECTION_REGISTRY.cancel_connection(conn_id).await;
            debug!("[FAKE_STREAMING] Connection {} cleaned up after stream completion", conn_id);
        }
        
        // Ensure the API task is cleaned up
        // Note: api_task is consumed in the select! above, so we don't need to abort it here
    }
}

// Common handler function to process both Gemini and OpenAI format requests
async fn handle_gemini_request<T: Serialize + Clone + Send + 'static>(
    mut state: GeminiState,
    body: T,
    ctx: GeminiContext,
    conn_token: Option<tokio_util::sync::CancellationToken>,
    conn_id: Option<crate::connection::ConnectionId>,
) -> Result<Response, ClewdrError> {
    state.update_from_ctx(&ctx);
    info!(
        "[REQ] stream: {}, vertex: {}, format: {}, model: {}",
        enabled(ctx.stream),
        enabled(ctx.vertex),
        if ctx.api_format == GeminiApiFormat::Gemini {
            ctx.api_format.to_string().green()
        } else {
            ctx.api_format.to_string().yellow()
        },
        ctx.model.green(),
    );

    let config = CLEWDR_CONFIG.load();
    
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

    // Check if we should use fake streaming
    if ctx.stream && config.fake_streaming {
        let stream = fake_streaming_handler(state, body, ctx, composite_token, conn_id);
        let res = Response::builder()
            .header(CONTENT_TYPE, "text/event-stream")
            .header("Cache-Control", "no-cache")
            .header("Connection", "keep-alive")
            .body(Body::from_stream(stream))?;
        return Ok(res);
    }

    // For non-streaming requests without fake streaming, return directly  
    if !ctx.stream {
        let res = tokio::select! {
            result = state.try_chat(body, composite_token.clone()) => result?,
            _ = composite_token.cancelled() => {
                info!("[CANCELLED] Gemini request cancelled by signal");
                return Err(ClewdrError::RequestCancelled);
            }
        };
        return Ok(res);
    }

    // For real streaming requests, proceed as before
    let res = tokio::select! {
        result = state.try_chat(body, composite_token.clone()) => result?,
        _ = composite_token.cancelled() => {
            info!("[CANCELLED] Gemini streaming request cancelled by signal");
            return Err(ClewdrError::RequestCancelled);
        }
    };
    Ok(res)
}

pub async fn api_post_gemini(
    State(state): State<GeminiState>,
    GeminiPreprocess(body, ctx, conn_token, conn_id): GeminiPreprocess,
) -> Result<Response, ClewdrError> {
    handle_gemini_request(state, body, ctx, conn_token, conn_id).await
}

pub async fn api_post_gemini_oai(
    State(state): State<GeminiState>,
    GeminiOaiPreprocess(body, ctx, conn_token, conn_id): GeminiOaiPreprocess,
) -> Result<Response, ClewdrError> {
    handle_gemini_request(state, body, ctx, conn_token, conn_id).await
}
