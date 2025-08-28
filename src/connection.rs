use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, atomic::{AtomicU64, Ordering}},
    time::Instant,
};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Unique identifier for each connection
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionId(Uuid);

impl Default for ConnectionId {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl std::fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.simple())
    }
}

/// Information about an active connection
#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub id: ConnectionId,
    pub remote_addr: Option<SocketAddr>,
    pub created_at: Instant,
    pub cancel_token: CancellationToken,
    pub request_count: Arc<AtomicU64>,
}

impl ConnectionInfo {
    pub fn new(remote_addr: Option<SocketAddr>) -> Self {
        Self {
            id: ConnectionId::new(),
            remote_addr,
            created_at: Instant::now(),
            cancel_token: CancellationToken::new(),
            request_count: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn increment_request_count(&self) -> u64 {
        self.request_count.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn get_request_count(&self) -> u64 {
        self.request_count.load(Ordering::SeqCst)
    }

    pub fn duration(&self) -> std::time::Duration {
        self.created_at.elapsed()
    }
}

/// Global registry for tracking active connections
pub struct ConnectionRegistry {
    connections: RwLock<HashMap<ConnectionId, ConnectionInfo>>,
}

impl ConnectionRegistry {
    pub fn new() -> Self {
        Self {
            connections: RwLock::new(HashMap::new()),
        }
    }

    pub async fn register_connection(&self, conn_info: ConnectionInfo) {
        let mut connections = self.connections.write().await;
        connections.insert(conn_info.id, conn_info);
    }

    pub async fn unregister_connection(&self, conn_id: ConnectionId) {
        let mut connections = self.connections.write().await;
        connections.remove(&conn_id);
        // Note: We don't cancel the token here anymore
        // This allows streaming responses to manage their own lifecycle
    }

    /// Cancel a specific connection's token
    pub async fn cancel_connection(&self, conn_id: ConnectionId) {
        let connections = self.connections.read().await;
        if let Some(conn_info) = connections.get(&conn_id) {
            conn_info.cancel_token.cancel();
        }
    }

    pub async fn get_connection(&self, conn_id: ConnectionId) -> Option<ConnectionInfo> {
        let connections = self.connections.read().await;
        connections.get(&conn_id).cloned()
    }

    pub async fn cancel_all_connections(&self) {
        let connections = self.connections.read().await;
        for conn_info in connections.values() {
            conn_info.cancel_token.cancel();
        }
    }

    pub async fn active_connection_count(&self) -> usize {
        let connections = self.connections.read().await;
        connections.len()
    }

    pub async fn get_all_connections(&self) -> Vec<ConnectionInfo> {
        let connections = self.connections.read().await;
        connections.values().cloned().collect()
    }

    /// Cleanup connections that have been cancelled
    pub async fn cleanup_cancelled_connections(&self) {
        let mut connections = self.connections.write().await;
        connections.retain(|_, conn_info| !conn_info.cancel_token.is_cancelled());
    }
}

impl Default for ConnectionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Extension trait to add connection info to axum requests
pub trait ConnectionExt {
    fn connection_info(&self) -> Option<&ConnectionInfo>;
    fn connection_id(&self) -> Option<ConnectionId>;
    fn connection_cancel_token(&self) -> Option<CancellationToken>;
}

/// Connection-related axum extensions
pub mod extensions {
    use super::*;

    /// Extension that holds connection information for the current request
    #[derive(Debug, Clone)]
    pub struct ConnectionExtension(pub ConnectionInfo);

    impl std::ops::Deref for ConnectionExtension {
        type Target = ConnectionInfo;
        
        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }
}

/// Global connection registry instance
use std::sync::LazyLock;
pub static CONNECTION_REGISTRY: LazyLock<ConnectionRegistry> = LazyLock::new(ConnectionRegistry::new);