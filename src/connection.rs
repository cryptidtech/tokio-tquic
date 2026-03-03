use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use pin_project_lite::pin_project;
use tokio::sync::oneshot;

use crate::bridge::{BridgeCommand, BridgeHandle};
use crate::error::{ConnectError, ConnectionError};
use crate::recv_stream::RecvStream;
use crate::send_stream::SendStream;
use crate::{Side, VarInt};

pin_project! {
    /// A future for an in-progress connection attempt.
    ///
    /// Awaiting this yields a [`Connection`] once the handshake completes.
    ///
    /// For client-initiated connections, this first resolves the connection ID from the
    /// bridge thread, then waits for the QUIC handshake to complete. For server-accepted
    /// connections, the connection ID is already known.
    pub struct Connecting {
        // For client-initiated: resolves the conn_id from the bridge thread.
        // None for server-accepted connections (conn_id already known).
        id_rx: Option<oneshot::Receiver<Result<u64, ConnectError>>>,
        // Known once id_rx resolves (or immediately for server-accepted).
        conn_id: Option<u64>,
        // The target address (used by remote_address() before conn_id is known).
        remote_addr: SocketAddr,
        pub(crate) handle: Arc<BridgeHandle>,
    }
}

impl std::fmt::Debug for Connecting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connecting")
            .field("conn_id", &self.conn_id)
            .field("remote_addr", &self.remote_addr)
            .finish()
    }
}

impl Connecting {
    /// Create a `Connecting` for a client-initiated connection.
    /// The conn_id will be resolved asynchronously from the bridge.
    pub(crate) fn new_client(
        id_rx: oneshot::Receiver<Result<u64, ConnectError>>,
        remote_addr: SocketAddr,
        handle: Arc<BridgeHandle>,
    ) -> Self {
        Self {
            id_rx: Some(id_rx),
            conn_id: None,
            remote_addr,
            handle,
        }
    }

    /// Create a `Connecting` for a server-accepted connection.
    /// The conn_id is already known from the incoming event.
    pub(crate) fn new_server(
        conn_id: u64,
        remote_addr: SocketAddr,
        handle: Arc<BridgeHandle>,
    ) -> Self {
        Self {
            id_rx: None,
            conn_id: Some(conn_id),
            remote_addr,
            handle,
        }
    }

    /// Attempt to convert into a 0-RTT connection.
    pub fn into_0rtt(self) -> Result<(Connection, ZeroRttAccepted), Self> {
        // TQUIC handles 0-RTT internally via TlsConfig::set_early_data_enabled
        Err(self)
    }

    /// Get the peer's address.
    pub fn remote_address(&self) -> SocketAddr {
        // If we have a conn_id, try to get the live address from the bridge.
        if let Some(conn_id) = self.conn_id {
            if let Some(cn) = self.handle.get_conn_notify(conn_id) {
                return *cn.remote_addr.lock().unwrap();
            }
        }
        self.remote_addr
    }
}

impl Future for Connecting {
    type Output = Result<Connection, ConnectionError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        // Phase 1: Resolve the conn_id if we don't have it yet.
        if this.conn_id.is_none() {
            if let Some(rx) = this.id_rx.as_mut() {
                match Pin::new(rx).poll(cx) {
                    Poll::Ready(Ok(Ok(id))) => {
                        *this.conn_id = Some(id);
                        *this.id_rx = None;
                    }
                    Poll::Ready(Ok(Err(e))) => {
                        return Poll::Ready(Err(ConnectionError::from_connect_error(e)));
                    }
                    Poll::Ready(Err(_)) => {
                        return Poll::Ready(Err(ConnectionError::LocallyClosed));
                    }
                    Poll::Pending => return Poll::Pending,
                }
            } else {
                return Poll::Ready(Err(ConnectionError::LocallyClosed));
            }
        }

        let conn_id = this.conn_id.unwrap();

        // Phase 2: Wait for the QUIC handshake to complete.
        let cn = match this.handle.get_conn_notify(conn_id) {
            Some(cn) => cn,
            None => return Poll::Ready(Err(ConnectionError::LocallyClosed)),
        };

        // Check if already established.
        if cn.is_established.load(Ordering::Acquire) {
            return Poll::Ready(Ok(Connection {
                conn_id,
                handle: this.handle.clone(),
            }));
        }

        // Check if already closed (e.g. handshake failed).
        if let Some(err) = cn.closed_rx.borrow().clone() {
            return Poll::Ready(Err(err));
        }

        // Register our waker so the bridge thread can wake us.
        *cn.establish_waker.lock().unwrap() = Some(cx.waker().clone());

        // Re-check after registering to avoid missed notifications.
        if cn.is_established.load(Ordering::Acquire) {
            return Poll::Ready(Ok(Connection {
                conn_id,
                handle: this.handle.clone(),
            }));
        }
        if let Some(err) = cn.closed_rx.borrow().clone() {
            return Poll::Ready(Err(err));
        }

        Poll::Pending
    }
}

/// A future that resolves when 0-RTT is accepted or rejected.
#[derive(Debug)]
pub struct ZeroRttAccepted {
    _inner: (),
}

impl Future for ZeroRttAccepted {
    type Output = bool;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        Poll::Ready(false)
    }
}

/// A QUIC connection.
///
/// Operations on the connection are proxied to the TQUIC thread via channels.
#[derive(Debug, Clone)]
pub struct Connection {
    pub(crate) conn_id: u64,
    pub(crate) handle: Arc<BridgeHandle>,
}

impl Connection {
    /// Open a bidirectional stream.
    pub fn open_bi(&self) -> OpenBi {
        OpenBi {
            conn: self.clone(),
            rx: None,
        }
    }

    /// Open a unidirectional stream.
    pub fn open_uni(&self) -> OpenUni {
        OpenUni {
            conn: self.clone(),
            rx: None,
        }
    }

    /// Accept the next bidirectional stream opened by the peer.
    pub fn accept_bi(&self) -> AcceptBi {
        let conn_id = self.conn_id;
        let handle = self.handle.clone();
        AcceptBi {
            fut: Box::pin(async move {
                let cn = handle
                    .get_conn_notify(conn_id)
                    .ok_or(ConnectionError::LocallyClosed)?;

                loop {
                    if let Some(err) = cn.closed_rx.borrow().clone() {
                        return Err(err);
                    }

                    // Acquire the async lock so multiple accept_bi() callers
                    // queue up properly instead of racing on poll_recv.
                    let mut rx = cn.accept_bi_rx.lock().await;

                    let mut closed = cn.closed_rx.clone();
                    tokio::select! {
                        stream_id = rx.recv() => {
                            match stream_id {
                                Some(stream_id) => {
                                    let send = SendStream {
                                        conn_id,
                                        stream_id,
                                        handle: handle.clone(),
    
                                        finished: false,
                                    };
                                    let recv = RecvStream {
                                        conn_id,
                                        stream_id,
                                        handle: handle.clone(),
    
                                        all_data_read: false,
                                    };
                                    return Ok((send, recv));
                                }
                                None => return Err(ConnectionError::LocallyClosed),
                            }
                        }
                        _ = closed.changed() => {
                            if let Some(err) = cn.closed_rx.borrow().clone() {
                                return Err(err);
                            }
                            // Loop back to re-check and re-lock.
                        }
                    }
                }
            }),
        }
    }

    /// Accept the next unidirectional stream opened by the peer.
    pub fn accept_uni(&self) -> AcceptUni {
        let conn_id = self.conn_id;
        let handle = self.handle.clone();
        AcceptUni {
            fut: Box::pin(async move {
                let cn = handle
                    .get_conn_notify(conn_id)
                    .ok_or(ConnectionError::LocallyClosed)?;

                loop {
                    if let Some(err) = cn.closed_rx.borrow().clone() {
                        return Err(err);
                    }

                    let mut rx = cn.accept_uni_rx.lock().await;

                    let mut closed = cn.closed_rx.clone();
                    tokio::select! {
                        stream_id = rx.recv() => {
                            match stream_id {
                                Some(stream_id) => {
                                    let recv = RecvStream {
                                        conn_id,
                                        stream_id,
                                        handle: handle.clone(),
    
                                        all_data_read: false,
                                    };
                                    return Ok(recv);
                                }
                                None => return Err(ConnectionError::LocallyClosed),
                            }
                        }
                        _ = closed.changed() => {
                            if let Some(err) = cn.closed_rx.borrow().clone() {
                                return Err(err);
                            }
                        }
                    }
                }
            }),
        }
    }

    /// Read an unreliable datagram.
    pub fn read_datagram(&self) -> ReadDatagram {
        ReadDatagram {
            conn: self.clone(),
        }
    }

    /// Send an unreliable datagram.
    ///
    /// Note: TQUIC does not currently expose a public datagram API.
    pub fn send_datagram(&self, _data: Bytes) -> Result<(), SendDatagramError> {
        Err(SendDatagramError::UnsupportedByPeer)
    }

    /// Send an unreliable datagram, waiting for buffer space if needed.
    pub fn send_datagram_wait(&self, _data: Bytes) -> SendDatagram {
        SendDatagram {
            conn: self.clone(),
        }
    }

    /// Maximum size of datagrams that can be sent.
    pub fn max_datagram_size(&self) -> Option<usize> {
        None
    }

    /// Close the connection immediately.
    pub fn close(&self, error_code: VarInt, reason: &[u8]) {
        self.handle.send_cmd(BridgeCommand::CloseConnection {
            conn_id: self.conn_id,
            error_code: error_code.into_inner(),
            reason: reason.to_vec(),
        });
    }

    /// Wait for the connection to be closed.
    pub async fn closed(&self) -> ConnectionError {
        let cn = match self.handle.get_conn_notify(self.conn_id) {
            Some(cn) => cn,
            None => return ConnectionError::LocallyClosed,
        };

        let mut rx = cn.closed_rx.clone();
        loop {
            // Check current value.
            if let Some(err) = rx.borrow().clone() {
                return err;
            }
            // Wait for change.
            if rx.changed().await.is_err() {
                return ConnectionError::LocallyClosed;
            }
        }
    }

    /// If the connection is closed, return the reason.
    pub fn close_reason(&self) -> Option<ConnectionError> {
        self.handle
            .get_conn_notify(self.conn_id)
            .and_then(|cn| cn.closed_rx.borrow().clone())
    }

    /// Get the peer's address.
    pub fn remote_address(&self) -> SocketAddr {
        self.handle
            .get_conn_notify(self.conn_id)
            .map(|cn| *cn.remote_addr.lock().unwrap())
            .unwrap_or_else(|| "0.0.0.0:0".parse().unwrap())
    }

    /// Current best estimate of round-trip time.
    pub fn rtt(&self) -> Duration {
        let (tx, rx) = oneshot::channel();
        self.handle.send_cmd(BridgeCommand::GetRtt {
            conn_id: self.conn_id,
            respond: tx,
        });
        rx.blocking_recv()
            .ok()
            .flatten()
            .unwrap_or(Duration::from_millis(0))
    }

    /// Whether this connection was initiated locally or by the peer.
    pub fn side(&self) -> Side {
        self.handle
            .get_conn_notify(self.conn_id)
            .map(|cn| {
                if cn.is_server.load(Ordering::Relaxed) {
                    Side::Server
                } else {
                    Side::Client
                }
            })
            .unwrap_or(Side::Client)
    }

    /// A stable identifier for this connection.
    pub fn stable_id(&self) -> usize {
        self.conn_id as usize
    }

    /// Helper: check if the connection is closed and return the error if so.
    fn check_closed(&self) -> Option<ConnectionError> {
        self.handle
            .get_conn_notify(self.conn_id)
            .and_then(|cn| cn.closed_rx.borrow().clone())
    }
}

// --- Stream opening/accepting futures ---

pin_project! {
    /// Future for opening a bidirectional stream.
    pub struct OpenBi {
        conn: Connection,
        rx: Option<oneshot::Receiver<Result<u64, ConnectionError>>>,
    }
}

impl Future for OpenBi {
    type Output = Result<(SendStream, RecvStream), ConnectionError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        // If we haven't sent the command yet, send it now.
        if this.rx.is_none() {
            if let Some(err) = this.conn.check_closed() {
                return Poll::Ready(Err(err));
            }
            let (tx, rx) = oneshot::channel();
            this.conn.handle.send_cmd(BridgeCommand::OpenBiStream {
                conn_id: this.conn.conn_id,
                respond: tx,
            });
            *this.rx = Some(rx);
        }

        // Poll the response.
        let rx = this.rx.as_mut().unwrap();
        match Pin::new(rx).poll(cx) {
            Poll::Ready(Ok(Ok(stream_id))) => {
                let send = SendStream {
                    conn_id: this.conn.conn_id,
                    stream_id,
                    handle: this.conn.handle.clone(),

                    finished: false,
                };
                let recv = RecvStream {
                    conn_id: this.conn.conn_id,
                    stream_id,
                    handle: this.conn.handle.clone(),

                    all_data_read: false,
                };
                Poll::Ready(Ok((send, recv)))
            }
            Poll::Ready(Ok(Err(e))) => Poll::Ready(Err(e)),
            Poll::Ready(Err(_)) => Poll::Ready(Err(ConnectionError::LocallyClosed)),
            Poll::Pending => Poll::Pending,
        }
    }
}

pin_project! {
    /// Future for opening a unidirectional stream.
    pub struct OpenUni {
        conn: Connection,
        rx: Option<oneshot::Receiver<Result<u64, ConnectionError>>>,
    }
}

impl Future for OpenUni {
    type Output = Result<SendStream, ConnectionError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        if this.rx.is_none() {
            if let Some(err) = this.conn.check_closed() {
                return Poll::Ready(Err(err));
            }
            let (tx, rx) = oneshot::channel();
            this.conn.handle.send_cmd(BridgeCommand::OpenUniStream {
                conn_id: this.conn.conn_id,
                respond: tx,
            });
            *this.rx = Some(rx);
        }

        let rx = this.rx.as_mut().unwrap();
        match Pin::new(rx).poll(cx) {
            Poll::Ready(Ok(Ok(stream_id))) => {
                let send = SendStream {
                    conn_id: this.conn.conn_id,
                    stream_id,
                    handle: this.conn.handle.clone(),

                    finished: false,
                };
                Poll::Ready(Ok(send))
            }
            Poll::Ready(Ok(Err(e))) => Poll::Ready(Err(e)),
            Poll::Ready(Err(_)) => Poll::Ready(Err(ConnectionError::LocallyClosed)),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Future for accepting a bidirectional stream from the peer.
pub struct AcceptBi {
    fut: Pin<Box<dyn Future<Output = Result<(SendStream, RecvStream), ConnectionError>> + Send>>,
}

impl Future for AcceptBi {
    type Output = Result<(SendStream, RecvStream), ConnectionError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.fut.as_mut().poll(cx)
    }
}

/// Future for accepting a unidirectional stream from the peer.
pub struct AcceptUni {
    fut: Pin<Box<dyn Future<Output = Result<RecvStream, ConnectionError>> + Send>>,
}

impl Future for AcceptUni {
    type Output = Result<RecvStream, ConnectionError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.fut.as_mut().poll(cx)
    }
}

pin_project! {
    /// Future for reading an unreliable datagram.
    pub struct ReadDatagram {
        conn: Connection,
    }
}

impl Future for ReadDatagram {
    type Output = Result<Bytes, ConnectionError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        // TQUIC doesn't expose a public datagram API, so we wait for connection close.
        if let Some(err) = this.conn.check_closed() {
            return Poll::Ready(Err(err));
        }

        let cn = match this.conn.handle.get_conn_notify(this.conn.conn_id) {
            Some(cn) => cn,
            None => return Poll::Ready(Err(ConnectionError::LocallyClosed)),
        };

        // Register for connection close notification.
        let mut rx = cn.closed_rx.clone();
        let waker = cx.waker().clone();
        tokio::spawn(async move {
            let _ = rx.changed().await;
            waker.wake();
        });

        Poll::Pending
    }
}

pin_project! {
    /// Future for sending an unreliable datagram with back-pressure.
    pub struct SendDatagram {
        conn: Connection,
    }
}

impl Future for SendDatagram {
    type Output = Result<(), SendDatagramError>;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        Poll::Ready(Err(SendDatagramError::UnsupportedByPeer))
    }
}

/// Errors from [`Connection::send_datagram`].
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum SendDatagramError {
    /// Datagrams are not supported by the peer.
    #[error("datagrams not supported by peer")]
    UnsupportedByPeer,
    /// Datagram support is disabled.
    #[error("datagram support disabled")]
    Disabled,
    /// The datagram is too large.
    #[error("datagram too large")]
    TooLarge,
    /// The connection was lost.
    #[error("connection lost: {0}")]
    ConnectionLost(#[from] ConnectionError),
}
