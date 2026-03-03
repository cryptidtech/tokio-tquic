use std::future::{Future, IntoFuture};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use pin_project_lite::pin_project;

use crate::bridge::{BridgeCommand, BridgeHandle};
use crate::connection::{Connecting, Connection};
use crate::error::ConnectionError;

/// An incoming connection for which the server has not yet decided whether to accept, reject,
/// or ignore.
pub struct Incoming {
    pub(crate) incoming_id: u64,
    pub(crate) remote_addr: SocketAddr,
    pub(crate) handle: Arc<BridgeHandle>,
}

impl std::fmt::Debug for Incoming {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Incoming")
            .field("incoming_id", &self.incoming_id)
            .field("remote_addr", &self.remote_addr)
            .finish_non_exhaustive()
    }
}

impl Incoming {
    /// Accept this connection using the default server configuration.
    ///
    /// In TQUIC, connections are auto-accepted on the server side. This method
    /// notifies the bridge and returns a `Connecting` future that completes
    /// when the handshake finishes.
    pub fn accept(self) -> Result<Connecting, ConnectionError> {
        self.handle
            .send_cmd(BridgeCommand::AcceptIncoming);

        Ok(Connecting::new_server(
            self.incoming_id,
            self.remote_addr,
            self.handle,
        ))
    }

    /// Accept this connection using a custom server configuration.
    ///
    /// Note: TQUIC does not support per-connection server config changes after endpoint
    /// creation. The custom config is ignored; the endpoint's original config is used.
    pub fn accept_with(self, _server_config: Arc<crate::config::ServerConfig>) -> Result<Connecting, ConnectionError> {
        self.accept()
    }

    /// Reject this connection.
    pub fn refuse(self) {
        self.handle
            .send_cmd(BridgeCommand::RefuseIncoming { conn_id: self.incoming_id });
    }

    /// Respond with a retry packet.
    ///
    /// Note: TQUIC handles retry globally via `Config::enable_retry`, not per-connection.
    /// This method always returns `Err(RetryError)`.
    pub fn retry(self) -> Result<(), RetryError> {
        Err(RetryError { incoming: self })
    }

    /// Ignore this connection attempt, not sending any packet in response.
    pub fn ignore(self) {
        // Close the connection silently on the bridge side.
        self.handle
            .send_cmd(BridgeCommand::RefuseIncoming { conn_id: self.incoming_id });
    }

    /// The peer's UDP address.
    pub fn remote_address(&self) -> SocketAddr {
        self.remote_addr
    }

    /// Whether the peer's address has been validated.
    pub fn remote_address_validated(&self) -> bool {
        false
    }

    /// Whether retry is possible.
    pub fn may_retry(&self) -> bool {
        false
    }
}

impl IntoFuture for Incoming {
    type Output = Result<Connection, ConnectionError>;
    type IntoFuture = IncomingFuture;

    fn into_future(self) -> Self::IntoFuture {
        match self.accept() {
            Ok(connecting) => IncomingFuture {
                connecting: Some(connecting),
                error: None,
            },
            Err(e) => IncomingFuture {
                connecting: None,
                error: Some(e),
            },
        }
    }
}

pin_project! {
    /// Future for an incoming connection that has been accepted.
    pub struct IncomingFuture {
        connecting: Option<Connecting>,
        error: Option<ConnectionError>,
    }
}

impl Future for IncomingFuture {
    type Output = Result<Connection, ConnectionError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        // Return any error from the accept step.
        if let Some(err) = this.error.take() {
            return Poll::Ready(Err(err));
        }

        // Poll the inner Connecting future.
        if let Some(connecting) = this.connecting.as_mut() {
            // Connecting is Unpin (all fields are u64 and Arc), so Pin::new is safe.
            match Pin::new(connecting).poll(cx) {
                Poll::Ready(result) => {
                    *this.connecting = None;
                    Poll::Ready(result)
                }
                Poll::Pending => Poll::Pending,
            }
        } else {
            Poll::Ready(Err(ConnectionError::LocallyClosed))
        }
    }
}

/// Error returned when calling [`Incoming::retry`].
///
/// TQUIC does not support per-connection retry; use `Config::enable_retry` instead.
#[derive(Debug)]
pub struct RetryError {
    incoming: Incoming,
}

impl RetryError {
    /// Recover the [`Incoming`] to take a different action.
    pub fn into_incoming(self) -> Incoming {
        self.incoming
    }
}

impl std::fmt::Display for RetryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "retry() is not supported; use Config::enable_retry instead")
    }
}

impl std::error::Error for RetryError {}
