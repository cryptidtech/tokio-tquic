use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::task::{Context, Poll};

use pin_project_lite::pin_project;
use tokio::sync::oneshot;

use crate::bridge::{Bridge, BridgeCommand, BridgeHandle};
use crate::config::{ClientConfig, EndpointConfig, ServerConfig};
use crate::connection::Connecting;
use crate::error::ConnectError;
use crate::incoming::Incoming;
use crate::VarInt;

/// A QUIC endpoint.
///
/// An endpoint corresponds to a single UDP socket, may host many connections, and may act
/// as both client and server for different connections.
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub(crate) handle: Arc<BridgeHandle>,
}

impl Endpoint {
    /// Create an endpoint for outgoing connections only.
    pub fn client(addr: SocketAddr) -> io::Result<Self> {
        let config = EndpointConfig::default();
        Self::new(config, None, addr)
    }

    /// Create an endpoint for incoming and outgoing connections.
    pub fn server(config: ServerConfig, addr: SocketAddr) -> io::Result<Self> {
        let endpoint_config = EndpointConfig::default();
        Self::new(endpoint_config, Some(config), addr)
    }

    /// Create an endpoint with custom configuration.
    pub fn new(
        config: EndpointConfig,
        server_config: Option<ServerConfig>,
        addr: SocketAddr,
    ) -> io::Result<Self> {
        let handle = Bridge::spawn(addr, config, server_config, None)?;
        Ok(Self { handle })
    }

    /// Accept the next incoming connection.
    pub fn accept(&self) -> Accept<'_> {
        Accept { endpoint: self }
    }

    /// Initiate an outgoing connection.
    pub fn connect(
        &self,
        addr: SocketAddr,
        server_name: &str,
    ) -> Result<Connecting, ConnectError> {
        if self.handle.endpoint_closed.load(Ordering::Relaxed) {
            return Err(ConnectError::EndpointStopping);
        }

        let (tx, rx) = oneshot::channel();
        self.handle.send_cmd(BridgeCommand::Connect {
            addr,
            server_name: server_name.to_string(),
            client_config: None,
            respond: tx,
        });

        Ok(Connecting::new_client(rx, addr, self.handle.clone()))
    }

    /// Initiate an outgoing connection with custom configuration.
    pub fn connect_with(
        &self,
        config: ClientConfig,
        addr: SocketAddr,
        server_name: &str,
    ) -> Result<Connecting, ConnectError> {
        if self.handle.endpoint_closed.load(Ordering::Relaxed) {
            return Err(ConnectError::EndpointStopping);
        }

        let (tx, rx) = oneshot::channel();
        self.handle.send_cmd(BridgeCommand::Connect {
            addr,
            server_name: server_name.to_string(),
            client_config: Some(config),
            respond: tx,
        });

        Ok(Connecting::new_client(rx, addr, self.handle.clone()))
    }

    /// Set the default client configuration for future outgoing connections.
    pub fn set_default_client_config(&self, config: ClientConfig) {
        self.handle
            .send_cmd(BridgeCommand::SetDefaultClientConfig { config });
    }

    /// Replace the server configuration.
    pub fn set_server_config(&self, server_config: Option<ServerConfig>) {
        self.handle
            .send_cmd(BridgeCommand::SetServerConfig { config: server_config });
    }

    /// Get the local address this endpoint is bound to.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.handle.local_addr)
    }

    /// Get the number of active connections.
    pub fn open_connections(&self) -> usize {
        self.handle.open_connections.load(Ordering::Relaxed) as usize
    }

    /// Close all connections immediately.
    pub fn close(&self, error_code: VarInt, reason: &[u8]) {
        self.handle.send_cmd(BridgeCommand::CloseEndpoint {
            error_code: error_code.into_inner(),
            reason: reason.to_vec(),
        });
    }

    /// Wait for all connections to be cleanly shut down.
    pub async fn wait_idle(&self) {
        loop {
            if self.handle.open_connections.load(Ordering::Relaxed) == 0 {
                return;
            }
            self.handle.idle_notify.notified().await;
            if self.handle.open_connections.load(Ordering::Relaxed) == 0 {
                return;
            }
        }
    }

    /// Get endpoint statistics.
    pub fn stats(&self) -> EndpointStats {
        EndpointStats::default()
    }
}

pin_project! {
    /// Future yielding the next incoming connection.
    pub struct Accept<'a> {
        endpoint: &'a Endpoint,
    }
}

impl Future for Accept<'_> {
    type Output = Option<Incoming>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        if this.endpoint.handle.endpoint_closed.load(Ordering::Relaxed) {
            return Poll::Ready(None);
        }

        // Try to receive from the incoming channel.
        let mut rx = match this.endpoint.handle.incoming_rx.try_lock() {
            Ok(rx) => rx,
            Err(_) => {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
        };

        match rx.poll_recv(cx) {
            Poll::Ready(Some(data)) => Poll::Ready(Some(Incoming {
                incoming_id: data.conn_id,
                remote_addr: data.remote_addr,
                handle: this.endpoint.handle.clone(),
            })),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Statistics about an endpoint.
#[non_exhaustive]
#[derive(Debug, Default, Copy, Clone)]
pub struct EndpointStats {
    /// Number of handshakes accepted as a server.
    pub accepted_handshakes: u64,
    /// Number of handshakes initiated as a client.
    pub outgoing_handshakes: u64,
    /// Number of handshakes refused.
    pub refused_handshakes: u64,
    /// Number of handshakes ignored.
    pub ignored_handshakes: u64,
}
