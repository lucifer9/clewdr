/// Authentication, request processing, and response transformation middleware
///
/// This module contains middleware components that handle various aspects of
/// request processing and response transformation in the Clewdr proxy service:
///
/// - Authentication: Verify API keys for different authentication methods (admin, OpenAI, Claude)
/// - Request preprocessing: Normalize requests from different API formats
/// - Response transformation: Convert between different response formats and handle streaming
/// - Connection monitoring: Track client connections for disconnect detection
mod auth;
pub mod claude;
pub mod gemini;
pub mod connection;

pub use auth::{RequireAdminAuth, RequireBearerAuth, RequireQueryKeyAuth, RequireXApiKeyAuth};
pub use connection::{connection_monitor, extract_connection_info, get_connection_cancel_token, get_connection_id};
