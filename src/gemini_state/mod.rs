use std::sync::LazyLock;

use axum::response::Response;
use chrono::Local;
use http::header::CONTENT_TYPE;
use hyper_util::client::legacy::connect::HttpConnector;
use serde::Serialize;
use serde_json::Value;
use snafu::ResultExt;
use strum::Display;
use tokio::spawn;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use wreq::{Client, ClientBuilder, header::AUTHORIZATION};
use yup_oauth2::{CustomHyperClientBuilder, ServiceAccountAuthenticator, ServiceAccountKey};

use crate::{
    config::{CLEWDR_CONFIG, GEMINI_ENDPOINT, KeyStatus},
    error::{CheckGeminiErr, ClewdrError, InvalidUriSnafu, WreqSnafu},
    middleware::gemini::*,
    services::key_actor::KeyActorHandle,
    types::gemini::{
        request::Part,
        response::{FinishReason, GeminiResponse},
    },
    utils::{forward_response, validate_required_tags},
};

#[derive(Clone, Display, PartialEq, Eq, Debug)]
pub enum GeminiApiFormat {
    Gemini,
    OpenAI,
}

static DUMMY_CLIENT: LazyLock<Client> = LazyLock::new(Client::new);

// TODO: replace yup-oauth2 with oauth2 crate
async fn get_token(sa_key: ServiceAccountKey) -> Result<String, ClewdrError> {
    const SCOPES: [&str; 1] = ["https://www.googleapis.com/auth/cloud-platform"];
    let token = if let Some(proxy) = CLEWDR_CONFIG.load().proxy.to_owned() {
        let proxy = proxy
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .trim_start_matches("socks5://");
        let proxy = format!("http://{proxy}");
        let proxy_uri = proxy.parse().context(InvalidUriSnafu {
            uri: proxy.to_owned(),
        })?;
        let proxy = hyper_http_proxy::Proxy::new(hyper_http_proxy::Intercept::All, proxy_uri);
        let connector = HttpConnector::new();
        let proxy_connector = hyper_http_proxy::ProxyConnector::from_proxy(connector, proxy)?;
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .pool_max_idle_per_host(0)
                .build(proxy_connector);
        let client_builder = CustomHyperClientBuilder::from(client);
        let auth = ServiceAccountAuthenticator::with_client(sa_key, client_builder)
            .build()
            .await?;
        auth.token(&SCOPES).await?
    } else {
        let auth = ServiceAccountAuthenticator::builder(sa_key).build().await?;
        auth.token(&SCOPES).await?
    };
    let token = token.token().ok_or(ClewdrError::UnexpectedNone {
        msg: "Oauth token is None",
    })?;
    Ok(token.into())
}

#[derive(Clone)]
pub struct GeminiState {
    pub model: String,
    pub vertex: bool,
    pub path: String,
    pub key: Option<KeyStatus>,
    pub stream: bool,
    pub query: GeminiArgs,
    pub key_handle: KeyActorHandle,
    pub api_format: GeminiApiFormat,
    pub client: Client,
}

impl GeminiState {
    /// Create a new AppState instance
    pub fn new(tx: KeyActorHandle) -> Self {
        GeminiState {
            model: String::new(),
            vertex: false,
            path: String::new(),
            query: GeminiArgs::default(),
            stream: false,
            key: None,
            key_handle: tx,
            api_format: GeminiApiFormat::Gemini,
            client: DUMMY_CLIENT.to_owned(),
        }
    }

    pub async fn report_403(&self) -> Result<(), ClewdrError> {
        if let Some(key) = self.key.to_owned() {
            info!(
                key = %key.key.ellipse(),
                "Removing 403-failed key from pool"
            );
            self.key_handle.delete_key(key).await?;
        }
        Ok(())
    }

    pub async fn report_400(&self) -> Result<(), ClewdrError> {
        if let Some(key) = self.key.to_owned() {
            info!(
                key = %key.key.ellipse(),
                "Removing 400-failed key from pool"
            );
            self.key_handle.delete_key(key).await?;
        }
        Ok(())
    }

    pub async fn report_429(&self) -> Result<(), ClewdrError> {
        if let Some(mut key) = self.key.to_owned() {
            info!(
                key = %key.key.ellipse(),
                "Setting 429 cooldown for key"
            );
            let old_cooldown = key.cooldown_until;
            key.set_429_cooldown();
            info!(
                ?old_cooldown,
                ?key.cooldown_until,
                "Cooldown updated"
            );
            match self.key_handle.return_key(key).await {
                Ok(_) => info!("[KEY_MGMT] Key returned to pool successfully after 429"),
                Err(e) => {
                    error!("[KEY_MGMT] Failed to return key to pool after 429: {}", e);
                    return Err(e);
                }
            }
        } else {
            warn!("[KEY_MGMT] No key available to set 429 cooldown");
        }
        Ok(())
    }

    pub async fn report_success(&self) -> Result<(), ClewdrError> {
        if let Some(key) = self.key.to_owned() {
            // 成功请求时直接返回key，无需修改状态
            self.key_handle.return_key(key).await?;
        }
        Ok(())
    }

    pub async fn request_key(&mut self) -> Result<(), ClewdrError> {
        info!("[REQUEST_KEY] Requesting key from key pool...");
        let key = match self.key_handle.request().await {
            Ok(key) => {
                info!(
                    key = %key.key.ellipse(),
                    "Key obtained successfully from pool"
                );
                key
            }
            Err(e) => {
                error!("[REQUEST_KEY] Failed to obtain key from pool: {}", e);
                return Err(e);
            }
        };
        self.key = Some(key.to_owned());
        let client = ClientBuilder::new()
            .timeout(std::time::Duration::from_secs(300)) // 5 minutes
            .connect_timeout(std::time::Duration::from_secs(30)); // 30 seconds
        let client = if let Some(proxy) = CLEWDR_CONFIG.load().proxy.to_owned() {
            client.proxy(proxy)
        } else {
            client
        };
        self.client = client.build().context(WreqSnafu {
            msg: "Failed to build Gemini client",
        })?;
        Ok(())
    }

    pub fn update_from_ctx(&mut self, ctx: &GeminiContext) {
        self.path = ctx.path.to_owned();
        self.stream = ctx.stream.to_owned();
        self.query = ctx.query.to_owned();
        self.model = ctx.model.to_owned();
        self.vertex = ctx.vertex.to_owned();
        self.api_format = ctx.api_format.to_owned();
    }

    async fn vertex_response(
        &mut self,
        p: impl Sized + Serialize,
    ) -> Result<wreq::Response, ClewdrError> {
        let client = ClientBuilder::new()
            .timeout(std::time::Duration::from_secs(300)) // 5 minutes
            .connect_timeout(std::time::Duration::from_secs(30)); // 30 seconds
        let client = if let Some(proxy) = CLEWDR_CONFIG.load().proxy.to_owned() {
            client.proxy(proxy)
        } else {
            client
        };
        self.client = client.build().context(WreqSnafu {
            msg: "Failed to build Gemini client",
        })?;
        let method = if self.stream {
            "streamGenerateContent"
        } else {
            "generateContent"
        };

        // Get an access token
        let Some(cred) = CLEWDR_CONFIG.load().vertex.credential.to_owned() else {
            return Err(ClewdrError::BadRequest {
                msg: "Vertex credential not found",
            });
        };

        let access_token = get_token(cred.to_owned()).await?;
        let bearer = format!("Bearer {access_token}");
        let res = match self.api_format {
            GeminiApiFormat::Gemini => {
                let endpoint = format!(
                    "https://aiplatform.googleapis.com/v1/projects/{}/locations/global/publishers/google/models/{}:{method}",
                    cred.project_id.unwrap_or_default(),
                    self.model
                );
                let query_vec = self.query.to_vec();
                self
                    .client
                    .post(endpoint)
                    .query(&query_vec)
                    .header(AUTHORIZATION, bearer)
                    .json(&p)
                    .send()
                    .await
                    .context(WreqSnafu {
                        msg: "Failed to send request to Gemini Vertex API",
                    })?
            }
            GeminiApiFormat::OpenAI => {
                self.client
                    .post(format!(
                        "https://aiplatform.googleapis.com/v1beta1/projects/{}/locations/global/endpoints/openapi/chat/completions",
                        cred.project_id.unwrap_or_default(),
                    ))
                    .header(AUTHORIZATION, bearer)
                    .json(&p)
                    .send()
                    .await
                    .context(WreqSnafu {
                        msg: "Failed to send request to Gemini Vertex OpenAI API",
                    })?
            }
        };
        let res = res.check_gemini().await?;
        Ok(res)
    }

    pub async fn send_chat(
        &mut self,
        p: impl Sized + Serialize,
    ) -> Result<wreq::Response, ClewdrError> {
        if self.vertex {
            let res = self.vertex_response(p).await?;
            return Ok(res);
        }

        self.request_key().await?;
        let Some(key) = self.key.to_owned() else {
            return Err(ClewdrError::UnexpectedNone {
                msg: "Key is None, did you request a key?",
            });
        };

        let key = key.key.to_string();
        let res = match self.api_format {
            GeminiApiFormat::Gemini => {
                let mut query_vec = self.query.to_vec();
                query_vec.push(("key", key.as_str()));
                let endpoint = format!("{}/v1beta/{}", GEMINI_ENDPOINT, self.path);

                self.client
                    .post(endpoint)
                    .query(&query_vec)
                    .json(&p)
                    .send()
                    .await
                    .context(WreqSnafu {
                        msg: "Failed to send request to Gemini API",
                    })?
            }
            GeminiApiFormat::OpenAI => {
                let endpoint = format!("{GEMINI_ENDPOINT}/v1beta/openai/chat/completions");
                self.client
                    .post(endpoint)
                    .header(AUTHORIZATION, format!("Bearer {key}"))
                    .json(&p)
                    .send()
                    .await
                    .context(WreqSnafu {
                        msg: "Failed to send request to Gemini OpenAI API",
                    })?
            }
        };
        let res = res.check_gemini().await?;
        Ok(res)
    }

    pub async fn try_chat(
        &mut self, 
        p: impl Serialize + Clone,
        cancellation_token: CancellationToken,
    ) -> Result<Response, ClewdrError> {
        let mut err = None;
        let max_retries = CLEWDR_CONFIG.load().max_retries;
        info!(
            "[TRY_CHAT] Starting - max_retries configured: {}",
            max_retries
        );

        for i in 0..max_retries + 1 {
            if i > 0 {
                info!(attempt = %i, "Retry attempt");
            }
            let p = p.to_owned();

            let send_chat_task = async {
                let mut temp_state = self.to_owned();
                let result = temp_state.send_chat(p).await;
                (temp_state, result)
            };

            let result = tokio::select! {
                (temp_state, res) = send_chat_task => (temp_state, res),
                _ = cancellation_token.cancelled() => {
                    info!("[CANCELLED] Gemini request cancelled during send_chat");
                    return Err(ClewdrError::RequestCancelled);
                }
            };

            match result.1 {
                Ok(resp) => {
                    let check_state = self.to_owned();
                    match check_state.check_empty_choices(resp).await {
                        Ok(resp) => {
                            // 成功处理请求，更新密钥状态
                            let success_state = result.0;
                            spawn(async move {
                                success_state.report_success().await.unwrap_or_else(|e| {
                                    error!("Failed to report success: {}", e);
                                });
                            });
                            return Ok(resp);
                        }
                        Err(e) => {
                            error!("Failed to check empty choices: {}", e);
                            err = Some(e);
                            continue;
                        }
                    }
                },
                Err(e) => {
                    let error_state = result.0;
                    if let Some(key) = error_state.key.to_owned() {
                        error!(key = %key.key.ellipse(), error = %e, "Request failed with key");
                    } else {
                        error!("{}", e);
                    }
                    match e {
                        ClewdrError::GeminiHttpError { code, .. } => {
                            match code.as_u16() {
                                400 => {
                                    spawn(async move {
                                        error_state.report_400().await.unwrap_or_else(|e| {
                                            error!("Failed to report 400: {}", e);
                                        });
                                    });
                                }
                                403 => {
                                    spawn(async move {
                                        error_state.report_403().await.unwrap_or_else(|e| {
                                            error!("Failed to report 403: {}", e);
                                        });
                                    });
                                }
                                429 => {
                                    spawn(async move {
                                        error_state.report_429().await.unwrap_or_else(|e| {
                                            error!("Failed to report 429: {}", e);
                                        });
                                    });
                                }
                                _ => {}
                            }
                            err = Some(e);
                            continue;
                        }
                        e => {
                            error!("[TRY_CHAT] Non-retryable error encountered: {}", e);
                            return Err(e);
                        }
                    }
                }
            }
        }
        error!(
            "[TRY_CHAT] Max retries exceeded - configured: {}, attempted: {}",
            max_retries,
            max_retries + 1
        );
        if let Some(e) = err {
            error!("[TRY_CHAT] Returning last error: {}", e);
            return Err(e);
        }
        error!("[TRY_CHAT] No specific error, returning TooManyRetries");
        Err(ClewdrError::TooManyRetries)
    }

    async fn check_empty_choices(&self, resp: wreq::Response) -> Result<Response, ClewdrError> {
        info!(
            "[CHECK_EMPTY] Starting check - stream={}, api_format={:?}",
            self.stream, self.api_format
        );

        if self.stream {
            info!("[CHECK_EMPTY] Streaming response - forwarding directly");
            return forward_response(resp);
        }

        let bytes = resp.bytes().await.context(WreqSnafu {
            msg: "Failed to get bytes from Gemini response",
        })?;

        info!("[CHECK_EMPTY] Response body length: {} bytes", bytes.len());

        // Check configuration and save content if enabled
        let config = CLEWDR_CONFIG.load();
        if config.save_response_before_tag_check {
            let timestamp = Local::now().format("%Y%m%d%H%M%S%3f");
            let filename = format!("response-{}.txt", timestamp);
            
            // Try to parse response to extract content
            match self.api_format {
                GeminiApiFormat::Gemini => {
                    if let Ok(res) = serde_json::from_slice::<GeminiResponse>(&bytes)
                        && let Some(candidate) = res.candidates.first() {
                            if let Some(content) = &candidate.content {
                                if let Some(parts) = &content.parts {
                                    // Extract all text content
                                    let mut text_content = String::new();
                                    for part in parts {
                                        if let Part::Text { text, .. } = part {
                                            text_content.push_str(text);
                                        }
                                    }
                                    if !text_content.is_empty() {
                                        if let Err(e) = tokio::fs::write(&filename, &text_content).await {
                                            error!("Failed to save content to {}: {}", filename, e);
                                        } else {
                                            info!("Content saved to {}", filename);
                                        }
                                    } else {
                                        info!("No text content to save");
                                    }
                                } else {
                                    info!("No parts in content (empty response)");
                                }
                            } else {
                                // content为空，打印原因
                                info!("Content is empty, finishReason: {:?}", candidate.finishReason);
                            }
                        }
                }
                GeminiApiFormat::OpenAI => {
                    if let Ok(res) = serde_json::from_slice::<Value>(&bytes) {
                        if let Some(content) = res["choices"].get(0)
                            .and_then(|c| c["message"]["content"].as_str()) {
                            if !content.is_empty() {
                                if let Err(e) = tokio::fs::write(&filename, content).await {
                                    error!("Failed to save content to {}: {}", filename, e);
                                } else {
                                    info!("Content saved to {}", filename);
                                }
                            } else {
                                info!("Content is empty");
                            }
                        } else {
                            let finish_reason = res["choices"].get(0)
                                .and_then(|c| c["finish_reason"].as_str());
                            info!("No content field, finish_reason: {:?}", finish_reason);
                        }
                    }
                }
            }
        }

        match self.api_format {
            GeminiApiFormat::Gemini => {
                info!("[CHECK_EMPTY] Attempting to parse as Gemini format");
                let res = match serde_json::from_slice::<GeminiResponse>(&bytes) {
                    Ok(res) => {
                        info!(
                            "[CHECK_EMPTY] Gemini JSON parsed successfully - candidates count: {}",
                            res.candidates.len()
                        );
                        res
                    }
                    Err(e) => {
                        error!("[CHECK_EMPTY] Gemini JSON parse failed - error: {}", e);
                        error!(
                            "[CHECK_EMPTY] Failed bytes (first 500): {}",
                            String::from_utf8_lossy(&bytes[..500.min(bytes.len())])
                        );
                        return Err(ClewdrError::from(e));
                    }
                };
                // Check for candidates that should trigger retry
                if res.candidates.is_empty() {
                    return Err(ClewdrError::EmptyChoices);
                }

                // Unified retry logic: retry if no content unless it's a STOP finish reason
                if let Some(candidate) = res.candidates.first() {
                    if candidate.content.is_none()
                        && candidate.finishReason != Some(FinishReason::STOP)
                    {
                        info!(
                            "[CHECK_EMPTY] No content with finishReason {:?} - will retry",
                            candidate.finishReason
                        );
                        return Err(ClewdrError::EmptyChoices);
                    }

                    // Check tag validation for streaming responses
                    let config = CLEWDR_CONFIG.load();
                    if !config.required_tags.trim().is_empty() {
                        // Use JSON parsing to extract text content safely
                        if let Ok(json_value) = serde_json::to_value(&res)
                            && let Some(candidates) = json_value["candidates"].as_array()
                            && let Some(first_candidate) = candidates.first()
                            && let Some(content) = first_candidate["content"].as_object()
                            && let Some(parts) = content.get("parts").and_then(|v| v.as_array())
                        {
                            for part in parts {
                                // Handle Part enum's JSON structure correctly
                                // Part::Text serializes to {"text": "..."} so we need to access nested text field
                                if let Some(text_obj) = part.as_object()
                                    && let Some(text) = text_obj.get("text").and_then(|t| t.as_str())
                                    && let Err(error) = validate_required_tags(text, &config.required_tags) {
                                        info!(
                                            "[TAG_VALIDATION] Content validation failed: {} - will retry",
                                            error
                                        );
                                        return Err(ClewdrError::EmptyChoices);
                                    }
                            }
                        }
                    }
                }
            }
            GeminiApiFormat::OpenAI => {
                info!("[CHECK_EMPTY] Attempting to parse as OpenAI format");
                let res = match serde_json::from_slice::<Value>(&bytes) {
                    Ok(res) => {
                        info!("[CHECK_EMPTY] OpenAI JSON parsed successfully");
                        res
                    }
                    Err(e) => {
                        error!("[CHECK_EMPTY] OpenAI JSON parse failed - error: {}", e);
                        error!(
                            "[CHECK_EMPTY] Failed bytes (first 500): {}",
                            String::from_utf8_lossy(&bytes[..500.min(bytes.len())])
                        );
                        return Err(ClewdrError::from(e));
                    }
                };
                if res["choices"].as_array().is_some_and(|v| v.is_empty()) {
                    return Err(ClewdrError::EmptyChoices);
                }
                if res["choices"].get(0)
                    .and_then(|c| c["finish_reason"].as_str()) == Some("OTHER") {
                    return Err(ClewdrError::EmptyChoices);
                }

                // Check tag validation for non-streaming responses
                let config = CLEWDR_CONFIG.load();
                if !config.required_tags.trim().is_empty()
                    && let Some(message_content) = res["choices"].get(0)
                        .and_then(|c| c["message"]["content"].as_str())
                    && let Err(error) = validate_required_tags(message_content, &config.required_tags) {
                        info!("[TAG_VALIDATION] Content validation failed: {} - will retry", error);
                        return Err(ClewdrError::EmptyChoices);
                    }
            }
        }
        Ok(Response::builder()
            .header(CONTENT_TYPE, "application/json")
            .body(bytes.into())?)
    }
}
