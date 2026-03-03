use std::net::SocketAddr;

use tokio::sync::oneshot;
use tquic::FourTuple;
use tquic::PathStats;

use crate::bridge::BridgeCommand;
use crate::error::ConnectionError;
use crate::Connection;

/// Extension trait for TQUIC-specific connection features.
///
/// These methods expose multipath QUIC capabilities not available in Quinn,
/// including path management and per-path statistics.
pub trait ConnectionExt {
    /// Add a new network path to the connection.
    ///
    /// This is used for multipath QUIC to add additional network interfaces.
    fn add_path(
        &self,
        local: SocketAddr,
        remote: SocketAddr,
    ) -> impl std::future::Future<Output = Result<(), ConnectionError>> + Send;

    /// Abandon a network path.
    ///
    /// Stops using the specified path for sending data.
    fn abandon_path(
        &self,
        local: SocketAddr,
        remote: SocketAddr,
    ) -> impl std::future::Future<Output = Result<(), ConnectionError>> + Send;

    /// Migrate the connection to a different network path.
    ///
    /// Makes the specified path the active path for the connection.
    fn migrate_path(
        &self,
        local: SocketAddr,
        remote: SocketAddr,
    ) -> impl std::future::Future<Output = Result<(), ConnectionError>> + Send;

    /// Get statistics for a specific path.
    fn path_stats(
        &self,
        local: SocketAddr,
        remote: SocketAddr,
    ) -> impl std::future::Future<Output = Result<PathStats, ConnectionError>> + Send;

    /// List all active paths.
    fn paths(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<FourTuple>, ConnectionError>> + Send;
}

impl ConnectionExt for Connection {
    async fn add_path(
        &self,
        local: SocketAddr,
        remote: SocketAddr,
    ) -> Result<(), ConnectionError> {
        let (tx, rx) = oneshot::channel();
        self.handle.send_cmd(BridgeCommand::AddPath {
            conn_id: self.conn_id,
            local,
            remote,
            respond: tx,
        });
        rx.await
            .unwrap_or(Err(ConnectionError::LocallyClosed))
    }

    async fn abandon_path(
        &self,
        local: SocketAddr,
        remote: SocketAddr,
    ) -> Result<(), ConnectionError> {
        let (tx, rx) = oneshot::channel();
        self.handle.send_cmd(BridgeCommand::AbandonPath {
            conn_id: self.conn_id,
            local,
            remote,
            respond: tx,
        });
        rx.await
            .unwrap_or(Err(ConnectionError::LocallyClosed))
    }

    async fn migrate_path(
        &self,
        local: SocketAddr,
        remote: SocketAddr,
    ) -> Result<(), ConnectionError> {
        let (tx, rx) = oneshot::channel();
        self.handle.send_cmd(BridgeCommand::MigratePath {
            conn_id: self.conn_id,
            local,
            remote,
            respond: tx,
        });
        rx.await
            .unwrap_or(Err(ConnectionError::LocallyClosed))
    }

    async fn path_stats(
        &self,
        local: SocketAddr,
        remote: SocketAddr,
    ) -> Result<PathStats, ConnectionError> {
        let (tx, rx) = oneshot::channel();
        self.handle.send_cmd(BridgeCommand::GetPathStats {
            conn_id: self.conn_id,
            local,
            remote,
            respond: tx,
        });
        rx.await
            .unwrap_or(Err(ConnectionError::LocallyClosed))
    }

    async fn paths(&self) -> Result<Vec<FourTuple>, ConnectionError> {
        let (tx, rx) = oneshot::channel();
        self.handle.send_cmd(BridgeCommand::GetPaths {
            conn_id: self.conn_id,
            respond: tx,
        });
        rx.await
            .unwrap_or(Err(ConnectionError::LocallyClosed))
    }
}
