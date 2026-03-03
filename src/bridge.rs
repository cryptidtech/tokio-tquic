use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::{mpsc, oneshot, watch, Notify};

use std::task::Waker;
use tracing::{debug, error, trace, warn};

use tquic::{PacketInfo, PacketSendHandler, TransportHandler};

use crate::config::{
    build_client_tls_config, build_server_tls_config, build_tquic_config, ClientConfig,
    EndpointConfig, ServerConfig,
};
use crate::error::{ConnectError, ConnectionError};

// ---------------------------------------------------------------------------
// Commands: async API → TQUIC thread
// ---------------------------------------------------------------------------

pub(crate) enum BridgeCommand {
    // -- Endpoint --
    Connect {
        addr: SocketAddr,
        server_name: String,
        client_config: Option<ClientConfig>,
        respond: oneshot::Sender<Result<u64, ConnectError>>,
    },
    SetServerConfig {
        config: Option<ServerConfig>,
    },
    SetDefaultClientConfig {
        config: ClientConfig,
    },
    CloseEndpoint {
        error_code: u64,
        reason: Vec<u8>,
    },

    // -- Incoming --
    AcceptIncoming,
    RefuseIncoming {
        conn_id: u64,
    },

    // -- Connection --
    CloseConnection {
        conn_id: u64,
        error_code: u64,
        reason: Vec<u8>,
    },

    // -- Streams --
    OpenBiStream {
        conn_id: u64,
        respond: oneshot::Sender<Result<u64, ConnectionError>>,
    },
    OpenUniStream {
        conn_id: u64,
        respond: oneshot::Sender<Result<u64, ConnectionError>>,
    },
    StreamWrite {
        conn_id: u64,
        stream_id: u64,
        data: Bytes,
        fin: bool,
        respond: oneshot::Sender<Result<usize, crate::send_stream::WriteError>>,
    },
    StreamRead {
        conn_id: u64,
        stream_id: u64,
        max_len: usize,
        respond: oneshot::Sender<Result<(Vec<u8>, bool), crate::recv_stream::ReadError>>,
    },
    StreamShutdownRead {
        conn_id: u64,
        stream_id: u64,
        error_code: u64,
    },
    StreamShutdownWrite {
        conn_id: u64,
        stream_id: u64,
        error_code: u64,
    },
    StreamSetPriority {
        conn_id: u64,
        stream_id: u64,
        urgency: u8,
    },

    // -- Queries --
    GetRtt {
        conn_id: u64,
        respond: oneshot::Sender<Option<Duration>>,
    },

    // -- Path management (tquic-ext) --
    #[cfg(feature = "tquic-ext")]
    AddPath {
        conn_id: u64,
        local: SocketAddr,
        remote: SocketAddr,
        respond: oneshot::Sender<Result<(), ConnectionError>>,
    },
    #[cfg(feature = "tquic-ext")]
    AbandonPath {
        conn_id: u64,
        local: SocketAddr,
        remote: SocketAddr,
        respond: oneshot::Sender<Result<(), ConnectionError>>,
    },
    #[cfg(feature = "tquic-ext")]
    MigratePath {
        conn_id: u64,
        local: SocketAddr,
        remote: SocketAddr,
        respond: oneshot::Sender<Result<(), ConnectionError>>,
    },
    #[cfg(feature = "tquic-ext")]
    GetPathStats {
        conn_id: u64,
        local: SocketAddr,
        remote: SocketAddr,
        respond: oneshot::Sender<Result<tquic::PathStats, ConnectionError>>,
    },
    #[cfg(feature = "tquic-ext")]
    GetPaths {
        conn_id: u64,
        respond: oneshot::Sender<Result<Vec<tquic::FourTuple>, ConnectionError>>,
    },
}

impl std::fmt::Debug for BridgeCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect { addr, server_name, .. } => {
                write!(f, "Connect({addr}, {server_name})")
            }
            Self::CloseEndpoint { .. } => write!(f, "CloseEndpoint"),
            Self::CloseConnection { conn_id, .. } => write!(f, "CloseConnection({conn_id})"),
            Self::OpenBiStream { conn_id, .. } => write!(f, "OpenBiStream({conn_id})"),
            Self::OpenUniStream { conn_id, .. } => write!(f, "OpenUniStream({conn_id})"),
            Self::StreamWrite { conn_id, stream_id, .. } => {
                write!(f, "StreamWrite({conn_id}, {stream_id})")
            }
            Self::StreamRead { conn_id, stream_id, .. } => {
                write!(f, "StreamRead({conn_id}, {stream_id})")
            }
            _ => write!(f, "{}", std::any::type_name::<Self>()),
        }
    }
}

// ---------------------------------------------------------------------------
// BridgeHandle: the Send+Sync handle held by async types
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) struct BridgeHandle {
    pub(crate) cmd_tx: mpsc::UnboundedSender<BridgeCommand>,
    pub(crate) incoming_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<IncomingData>>,
    pub(crate) local_addr: SocketAddr,
    pub(crate) conn_states: dashmap::DashMap<u64, Arc<ConnNotify>>,
    pub(crate) open_connections: AtomicU64,
    pub(crate) endpoint_closed: AtomicBool,
    pub(crate) idle_notify: Notify,
}

impl BridgeHandle {
    pub(crate) fn send_cmd(&self, cmd: BridgeCommand) {
        if self.cmd_tx.send(cmd).is_err() {
            debug!("bridge command channel closed");
        }
    }

    pub(crate) fn get_conn_notify(&self, conn_id: u64) -> Option<Arc<ConnNotify>> {
        self.conn_states.get(&conn_id).map(|v| v.value().clone())
    }
}

/// Data for a new incoming connection delivered to the async side.
#[derive(Debug)]
pub(crate) struct IncomingData {
    pub(crate) conn_id: u64,
    pub(crate) remote_addr: SocketAddr,
}

/// Per-connection notification channels (Send+Sync).
pub(crate) struct ConnNotify {
    /// Set to `true` when the handshake completes.
    pub(crate) is_established: AtomicBool,
    /// Waker registered by `Connecting::poll` to be woken on handshake completion.
    pub(crate) establish_waker: std::sync::Mutex<Option<Waker>>,
    /// Fires when connection is closed; holds the error.
    pub(crate) closed_tx: watch::Sender<Option<ConnectionError>>,
    pub(crate) closed_rx: watch::Receiver<Option<ConnectionError>>,
    /// New peer-initiated bi streams become available.
    pub(crate) accept_bi_tx: mpsc::UnboundedSender<u64>,
    pub(crate) accept_bi_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<u64>>,
    /// New peer-initiated uni streams become available.
    pub(crate) accept_uni_tx: mpsc::UnboundedSender<u64>,
    pub(crate) accept_uni_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<u64>>,
    /// Per-stream readability notifications.
    pub(crate) stream_readable: dashmap::DashMap<u64, Arc<Notify>>,
    /// Per-stream writability notifications.
    pub(crate) stream_writable: dashmap::DashMap<u64, Arc<Notify>>,
    /// Whether this is a server-side connection.
    pub(crate) is_server: AtomicBool,
    /// Remote address.
    pub(crate) remote_addr: std::sync::Mutex<SocketAddr>,
}

impl std::fmt::Debug for ConnNotify {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnNotify")
            .field("is_established", &self.is_established)
            .finish_non_exhaustive()
    }
}

impl ConnNotify {
    fn new(remote_addr: SocketAddr) -> Self {
        let (closed_tx, closed_rx) = watch::channel(None);
        let (accept_bi_tx, accept_bi_rx) = mpsc::unbounded_channel();
        let (accept_uni_tx, accept_uni_rx) = mpsc::unbounded_channel();
        Self {
            is_established: AtomicBool::new(false),
            establish_waker: std::sync::Mutex::new(None),
            closed_tx,
            closed_rx,
            accept_bi_tx,
            accept_bi_rx: tokio::sync::Mutex::new(accept_bi_rx),
            accept_uni_tx,
            accept_uni_rx: tokio::sync::Mutex::new(accept_uni_rx),
            stream_readable: dashmap::DashMap::new(),
            stream_writable: dashmap::DashMap::new(),
            is_server: AtomicBool::new(false),
            remote_addr: std::sync::Mutex::new(remote_addr),
        }
    }

    pub(crate) fn get_or_create_readable(&self, stream_id: u64) -> Arc<Notify> {
        self.stream_readable
            .entry(stream_id)
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone()
    }

    pub(crate) fn get_or_create_writable(&self, stream_id: u64) -> Arc<Notify> {
        self.stream_writable
            .entry(stream_id)
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone()
    }
}

// ---------------------------------------------------------------------------
// PacketSender: implements PacketSendHandler, sends via tokio UdpSocket
// ---------------------------------------------------------------------------

struct PacketSender {
    socket: Rc<std::cell::RefCell<Option<tokio::net::UdpSocket>>>,
}

impl PacketSendHandler for PacketSender {
    fn on_packets_send(&self, pkts: &[(Vec<u8>, PacketInfo)]) -> tquic::Result<usize> {
        let socket_ref = self.socket.borrow();
        let socket = socket_ref.as_ref().ok_or(tquic::Error::Done)?;
        let mut count = 0;
        for (pkt, info) in pkts {
            match socket.try_send_to(pkt, info.dst) {
                Ok(_) => {
                    trace!("sent {} bytes to {}", pkt.len(), info.dst);
                    count += 1;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    debug!("socket send would block after {} packets", count);
                    return Ok(count);
                }
                Err(e) => {
                    return Err(tquic::Error::IoError(format!("send_to: {e}")));
                }
            }
        }
        Ok(count)
    }
}

// ---------------------------------------------------------------------------
// BridgeTransportHandler: implements TransportHandler, forwards to channels
// ---------------------------------------------------------------------------

struct BridgeTransportHandler {
    /// Handle to the Send+Sync shared state.
    handle: Arc<BridgeHandle>,
    /// Whether this endpoint is a server.
    is_server: bool,
    /// Channel for delivering new incoming connections (server only).
    incoming_tx: mpsc::UnboundedSender<IncomingData>,
}

impl TransportHandler for BridgeTransportHandler {
    fn on_conn_created(&mut self, conn: &mut tquic::Connection) {
        let conn_id = conn.index().unwrap_or(0);
        let remote = conn
            .get_active_path()
            .map(|p| p.remote_addr())
            .unwrap_or_else(|_| "0.0.0.0:0".parse().unwrap());

        debug!("on_conn_created: conn_id={conn_id}, remote={remote}");

        let notify = Arc::new(ConnNotify::new(remote));
        notify.is_server.store(conn.is_server(), Ordering::Relaxed);
        self.handle.conn_states.insert(conn_id, notify);
        self.handle.open_connections.fetch_add(1, Ordering::Relaxed);

        // For servers, notify the accept loop about the new incoming connection.
        if self.is_server {
            let _ = self.incoming_tx.send(IncomingData {
                conn_id,
                remote_addr: remote,
            });
        }
    }

    fn on_conn_established(&mut self, conn: &mut tquic::Connection) {
        let conn_id = conn.index().unwrap_or(0);
        debug!("on_conn_established: conn_id={conn_id}");

        if let Some(notify) = self.handle.conn_states.get(&conn_id) {
            notify.is_established.store(true, Ordering::Release);
            if let Some(waker) = notify.establish_waker.lock().unwrap().take() {
                waker.wake();
            }
        }
    }

    fn on_conn_closed(&mut self, conn: &mut tquic::Connection) {
        let conn_id = conn.index().unwrap_or(0);
        debug!("on_conn_closed: conn_id={conn_id}");

        let error = if let Some(err) = conn.peer_error() {
            Some(ConnectionError::from_tquic_conn_error(err))
        } else if let Some(err) = conn.local_error() {
            Some(ConnectionError::from_tquic_conn_error(err))
        } else if conn.is_idle_timeout() || conn.is_handshake_timeout() {
            Some(ConnectionError::TimedOut)
        } else if conn.is_reset() {
            Some(ConnectionError::Reset)
        } else {
            Some(ConnectionError::LocallyClosed)
        };

        if let Some(notify) = self.handle.conn_states.get(&conn_id) {
            let _ = notify.closed_tx.send(error);
            // Also wake the establish waker in case connection closed before handshake.
            if let Some(waker) = notify.establish_waker.lock().unwrap().take() {
                waker.wake();
            }
        }

        self.handle.open_connections.fetch_sub(1, Ordering::Relaxed);
        if self.handle.open_connections.load(Ordering::Relaxed) == 0 {
            self.handle.idle_notify.notify_waiters();
        }
    }

    fn on_stream_created(&mut self, conn: &mut tquic::Connection, stream_id: u64) {
        let conn_id = conn.index().unwrap_or(0);
        debug!("on_stream_created: conn_id={conn_id}, stream_id={stream_id}");

        if let Some(notify) = self.handle.conn_states.get(&conn_id) {
            // Pre-create notification entries.
            notify.get_or_create_readable(stream_id);
            notify.get_or_create_writable(stream_id);

            // If this is a peer-initiated stream, notify the accept futures.
            let is_server = notify.is_server.load(Ordering::Relaxed);
            let peer_initiated = if is_server {
                // Server: peer (client) initiates streams with even IDs (bit 0 = 0)
                stream_id & 0x1 == 0
            } else {
                // Client: peer (server) initiates streams with odd IDs (bit 0 = 1)
                stream_id & 0x1 == 1
            };

            if peer_initiated {
                let is_bidi = stream_id & 0x2 == 0;
                if is_bidi {
                    let _ = notify.accept_bi_tx.send(stream_id);
                } else {
                    let _ = notify.accept_uni_tx.send(stream_id);
                }
            }
        }
    }

    fn on_stream_readable(&mut self, conn: &mut tquic::Connection, stream_id: u64) {
        let conn_id = conn.index().unwrap_or(0);
        trace!("on_stream_readable: conn_id={conn_id}, stream_id={stream_id}");

        if let Some(notify) = self.handle.conn_states.get(&conn_id) {
            let n = notify.get_or_create_readable(stream_id);
            // Use notify_one() to store a permit if no waiter is registered yet.
            // This avoids a race where wait_readable() hasn't called notified() yet.
            n.notify_one();
        }
    }

    fn on_stream_writable(&mut self, conn: &mut tquic::Connection, stream_id: u64) {
        let conn_id = conn.index().unwrap_or(0);
        trace!("on_stream_writable: conn_id={conn_id}, stream_id={stream_id}");

        if let Some(notify) = self.handle.conn_states.get(&conn_id) {
            let n = notify.get_or_create_writable(stream_id);
            // Use notify_one() to store a permit if no waiter is registered yet.
            n.notify_one();
        }
    }

    fn on_stream_closed(&mut self, conn: &mut tquic::Connection, stream_id: u64) {
        let conn_id = conn.index().unwrap_or(0);
        debug!("on_stream_closed: conn_id={conn_id}, stream_id={stream_id}");

        if let Some(notify) = self.handle.conn_states.get(&conn_id) {
            // Wake any waiters so they can see the stream is done.
            if let Some(n) = notify.stream_readable.get(&stream_id) {
                n.notify_one();
            }
            if let Some(n) = notify.stream_writable.get(&stream_id) {
                n.notify_one();
            }
        }
    }

    fn on_new_token(&mut self, conn: &mut tquic::Connection, _token: Vec<u8>) {
        let conn_id = conn.index().unwrap_or(0);
        debug!("on_new_token: conn_id={conn_id}");
        // Token management is handled internally by TQUIC.
    }
}

// ---------------------------------------------------------------------------
// Bridge: spawns the dedicated TQUIC I/O thread
// ---------------------------------------------------------------------------

pub(crate) struct Bridge;

impl Bridge {
    /// Spawn the TQUIC I/O thread and return a `BridgeHandle`.
    pub(crate) fn spawn(
        bind_addr: SocketAddr,
        endpoint_config: EndpointConfig,
        server_config: Option<ServerConfig>,
        default_client_config: Option<ClientConfig>,
    ) -> Result<Arc<BridgeHandle>, std::io::Error> {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();

        let is_server = server_config.is_some();

        // We bind the socket on the current (caller) thread to report errors immediately.
        let std_socket = std::net::UdpSocket::bind(bind_addr)?;
        std_socket.set_nonblocking(true)?;
        let local_addr = std_socket.local_addr()?;

        let handle = Arc::new(BridgeHandle {
            cmd_tx,
            incoming_rx: tokio::sync::Mutex::new(incoming_rx),
            local_addr,
            conn_states: dashmap::DashMap::new(),
            open_connections: AtomicU64::new(0),
            endpoint_closed: AtomicBool::new(false),
            idle_notify: Notify::new(),
        });

        let handle_clone = handle.clone();

        std::thread::Builder::new()
            .name("tquic-io".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                        return;
                    }
                };

                let local = tokio::task::LocalSet::new();
                local.block_on(&rt, async move {
                    if let Err(e) = run_bridge(
                        std_socket,
                        local_addr,
                        endpoint_config,
                        server_config,
                        default_client_config,
                        is_server,
                        handle_clone,
                        incoming_tx,
                        cmd_rx,
                        ready_tx,
                    )
                    .await
                    {
                        error!("bridge event loop error: {e}");
                    }
                });
            })
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        // Wait for the thread to signal readiness.
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(handle),
            Ok(Err(e)) => Err(std::io::Error::new(std::io::ErrorKind::Other, e)),
            Err(_) => Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "bridge thread died during startup",
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// The event loop running on the dedicated TQUIC thread
// ---------------------------------------------------------------------------

async fn run_bridge(
    std_socket: std::net::UdpSocket,
    local_addr: SocketAddr,
    endpoint_config: EndpointConfig,
    server_config: Option<ServerConfig>,
    default_client_config: Option<ClientConfig>,
    is_server: bool,
    handle: Arc<BridgeHandle>,
    incoming_tx: mpsc::UnboundedSender<IncomingData>,
    mut cmd_rx: mpsc::UnboundedReceiver<BridgeCommand>,
    ready_tx: std::sync::mpsc::Sender<Result<(), String>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Convert std socket to tokio socket (must happen on the local runtime).
    let socket = tokio::net::UdpSocket::from_std(std_socket)?;
    let socket_rc = Rc::new(std::cell::RefCell::new(Some(socket)));

    // Build TQUIC config.
    let transport = server_config
        .as_ref()
        .map(|sc| (*sc.transport).clone())
        .or_else(|| default_client_config.as_ref().map(|cc| (*cc.transport).clone()))
        .unwrap_or_default();

    let tls_config = if let Some(ref sc) = server_config {
        build_server_tls_config(sc).map_err(|e| e.to_string())?
    } else if let Some(ref cc) = default_client_config {
        build_client_tls_config(cc).map_err(|e| e.to_string())?
    } else {
        // Create a minimal client TLS config.
        tquic::TlsConfig::new_client_config(vec![], false).map_err(|e| e.to_string())?
    };

    let tquic_config = build_tquic_config(&endpoint_config, &transport, tls_config, server_config.as_ref())
        .map_err(|e| e.to_string())?;

    // Create the handler and packet sender.
    let handler = BridgeTransportHandler {
        handle: handle.clone(),
        is_server,
        incoming_tx,
    };

    let sender = Rc::new(PacketSender {
        socket: socket_rc.clone(),
    });

    // Create the TQUIC endpoint.
    let mut endpoint = tquic::Endpoint::new(
        Box::new(tquic_config),
        is_server,
        Box::new(handler),
        sender,
    );

    // Store the current default client config for connect_with.
    let mut current_client_config = default_client_config;

    // Signal that the bridge is ready.
    let _ = ready_tx.send(Ok(()));

    // Recv buffer.
    let mut recv_buf = vec![0u8; 65536];

    loop {
        // Get the socket ref for recv.
        let socket_ref = socket_rc.borrow();
        let socket = match socket_ref.as_ref() {
            Some(s) => s,
            None => break,
        };

        // Calculate timeout.
        let timeout_dur = endpoint.timeout().unwrap_or(Duration::from_millis(100));
        let sleep = tokio::time::sleep(timeout_dur);
        tokio::pin!(sleep);

        tokio::select! {
            // Receive UDP packets
            result = socket.recv_from(&mut recv_buf) => {
                drop(socket_ref); // Release borrow before mutating endpoint
                match result {
                    Ok((len, peer)) => {
                        let info = PacketInfo {
                            src: peer,
                            dst: local_addr,
                            time: Instant::now(),
                        };
                        if let Err(e) = endpoint.recv(&mut recv_buf[..len], &info) {
                            debug!("endpoint.recv error: {e:?}");
                        }
                    }
                    Err(e) => {
                        warn!("socket recv error: {e}");
                    }
                }
            }

            // Process commands from async API
            cmd = cmd_rx.recv() => {
                drop(socket_ref); // Release borrow before mutating endpoint
                match cmd {
                    Some(cmd) => {
                        process_command(
                            &mut endpoint,
                            cmd,
                            &handle,
                            &endpoint_config,
                            &mut current_client_config,
                            local_addr,
                        );
                    }
                    None => {
                        debug!("command channel closed, shutting down bridge");
                        break;
                    }
                }
            }

            // Handle timeout
            _ = &mut sleep => {
                drop(socket_ref);
                endpoint.on_timeout(Instant::now());
            }
        }

        // Always process connections after any event.
        if let Err(e) = endpoint.process_connections() {
            error!("process_connections error: {e:?}");
        }
    }

    // Cleanup: close the endpoint.
    endpoint.close(true);
    let _ = endpoint.process_connections();

    Ok(())
}

// ---------------------------------------------------------------------------
// Command processing
// ---------------------------------------------------------------------------

fn process_command(
    endpoint: &mut tquic::Endpoint,
    cmd: BridgeCommand,
    handle: &Arc<BridgeHandle>,
    endpoint_config: &EndpointConfig,
    current_client_config: &mut Option<ClientConfig>,
    local_addr: SocketAddr,
) {
    match cmd {
        BridgeCommand::Connect {
            addr,
            server_name,
            client_config,
            respond,
        } => {
            let config = client_config
                .as_ref()
                .or(current_client_config.as_ref());

            let result = match config {
                Some(cc) => {
                    // Build a per-connection TQUIC config if a custom client config is provided.
                    match build_client_tls_config(cc) {
                        Ok(tls) => match build_tquic_config(
                            endpoint_config,
                            &cc.transport,
                            tls,
                            None,
                        ) {
                            Ok(cfg) => endpoint.connect(
                                local_addr,
                                addr,
                                Some(&server_name),
                                None,
                                None,
                                Some(&cfg),
                            ),
                            Err(e) => {
                                let _ = respond.send(Err(e));
                                return;
                            }
                        },
                        Err(e) => {
                            let _ = respond.send(Err(e));
                            return;
                        }
                    }
                }
                None => {
                    // Use the endpoint's default config.
                    endpoint.connect(
                        local_addr,
                        addr,
                        Some(&server_name),
                        None,
                        None,
                        None,
                    )
                }
            };

            let _ = respond.send(result.map_err(|e| match e {
                tquic::Error::InvalidConfig(s) => ConnectError::InvalidDnsName(s),
                _ => ConnectError::EndpointStopping,
            }));
        }

        BridgeCommand::SetServerConfig { config: _config } => {
            // TQUIC doesn't support changing server config after creation.
            // This is a limitation vs Quinn.
            debug!("SetServerConfig: not supported after endpoint creation");
        }

        BridgeCommand::SetDefaultClientConfig { config } => {
            *current_client_config = Some(config);
        }

        BridgeCommand::CloseEndpoint {
            error_code,
            reason,
        } => {
            handle.endpoint_closed.store(true, Ordering::Relaxed);
            // Close all connections.
            // Iterate known connections and close them.
            for entry in handle.conn_states.iter() {
                let conn_id = *entry.key();
                if let Some(conn) = endpoint.conn_get_mut(conn_id) {
                    let _ = conn.close(true, error_code, &reason);
                }
            }
            endpoint.close(false);
        }

        BridgeCommand::AcceptIncoming => {
            // In TQUIC, connections are auto-accepted on the server side.
            // The on_conn_created callback has already fired.
        }

        BridgeCommand::RefuseIncoming { conn_id } => {
            if let Some(conn) = endpoint.conn_get_mut(conn_id) {
                let _ = conn.close(false, 0x02, b"refused");
            }
        }

        BridgeCommand::CloseConnection {
            conn_id,
            error_code,
            reason,
        } => {
            if let Some(conn) = endpoint.conn_get_mut(conn_id) {
                let _ = conn.close(true, error_code, &reason);
            }
        }

        BridgeCommand::OpenBiStream { conn_id, respond } => {
            let result = if let Some(conn) = endpoint.conn_get_mut(conn_id) {
                conn.stream_bidi_new(0, false)
                    .map_err(|_| ConnectionError::LocallyClosed)
            } else {
                Err(ConnectionError::LocallyClosed)
            };
            let _ = respond.send(result);
        }

        BridgeCommand::OpenUniStream { conn_id, respond } => {
            let result = if let Some(conn) = endpoint.conn_get_mut(conn_id) {
                conn.stream_uni_new(0, false)
                    .map_err(|_| ConnectionError::LocallyClosed)
            } else {
                Err(ConnectionError::LocallyClosed)
            };
            let _ = respond.send(result);
        }

        BridgeCommand::StreamWrite {
            conn_id,
            stream_id,
            data,
            fin,
            respond,
        } => {
            let result = if let Some(conn) = endpoint.conn_get_mut(conn_id) {
                match conn.stream_write(stream_id, data, fin) {
                    Ok(n) => Ok(n),
                    Err(tquic::Error::Done) => Ok(0),
                    Err(tquic::Error::StreamStopped(code)) => {
                        Err(crate::send_stream::WriteError::Stopped(crate::VarInt::from(
                            code as u32,
                        )))
                    }
                    Err(_) => Err(crate::send_stream::WriteError::ClosedStream),
                }
            } else {
                Err(crate::send_stream::WriteError::ConnectionLost(
                    ConnectionError::LocallyClosed,
                ))
            };
            let _ = respond.send(result);
        }

        BridgeCommand::StreamRead {
            conn_id,
            stream_id,
            max_len,
            respond,
        } => {
            let result = if let Some(conn) = endpoint.conn_get_mut(conn_id) {
                let mut buf = vec![0u8; max_len];
                match conn.stream_read(stream_id, &mut buf) {
                    Ok((n, fin)) => {
                        buf.truncate(n);
                        Ok((buf, fin))
                    }
                    Err(tquic::Error::Done) => Ok((vec![], false)),
                    Err(tquic::Error::StreamReset(code)) => {
                        Err(crate::recv_stream::ReadError::Reset(crate::VarInt::from(
                            code as u32,
                        )))
                    }
                    Err(_) => Err(crate::recv_stream::ReadError::ClosedStream),
                }
            } else {
                Err(crate::recv_stream::ReadError::ConnectionLost(
                    ConnectionError::LocallyClosed,
                ))
            };
            let _ = respond.send(result);
        }

        BridgeCommand::StreamShutdownRead {
            conn_id,
            stream_id,
            error_code,
        } => {
            if let Some(conn) = endpoint.conn_get_mut(conn_id) {
                let _ = conn.stream_shutdown(stream_id, tquic::Shutdown::Read, error_code);
            }
        }

        BridgeCommand::StreamShutdownWrite {
            conn_id,
            stream_id,
            error_code,
        } => {
            if let Some(conn) = endpoint.conn_get_mut(conn_id) {
                let _ = conn.stream_shutdown(stream_id, tquic::Shutdown::Write, error_code);
            }
        }

        BridgeCommand::StreamSetPriority {
            conn_id,
            stream_id,
            urgency,
        } => {
            if let Some(conn) = endpoint.conn_get_mut(conn_id) {
                let _ = conn.stream_set_priority(stream_id, urgency, false);
            }
        }

        BridgeCommand::GetRtt { conn_id, respond } => {
            let rtt = if let Some(conn) = endpoint.conn_get_mut(conn_id) {
                let paths: Vec<_> = conn.paths_iter().collect();
                if let Some(four_tuple) = paths.first() {
                    conn.get_path_stats(four_tuple.local, four_tuple.remote)
                        .ok()
                        .map(|stats| Duration::from_micros(stats.srtt))
                } else {
                    Some(Duration::from_millis(0))
                }
            } else {
                None
            };
            let _ = respond.send(rtt);
        }

        #[cfg(feature = "tquic-ext")]
        BridgeCommand::AddPath {
            conn_id,
            local,
            remote,
            respond,
        } => {
            let result = if let Some(conn) = endpoint.conn_get_mut(conn_id) {
                conn.add_path(local, remote)
                    .map(|_| ())
                    .map_err(|e| ConnectionError::TransportError {
                        code: 0,
                        reason: format!("add_path: {e}"),
                    })
            } else {
                Err(ConnectionError::LocallyClosed)
            };
            let _ = respond.send(result);
        }

        #[cfg(feature = "tquic-ext")]
        BridgeCommand::AbandonPath {
            conn_id,
            local,
            remote,
            respond,
        } => {
            let result = if let Some(conn) = endpoint.conn_get_mut(conn_id) {
                conn.abandon_path(local, remote)
                    .map_err(|e| ConnectionError::TransportError {
                        code: 0,
                        reason: format!("abandon_path: {e}"),
                    })
            } else {
                Err(ConnectionError::LocallyClosed)
            };
            let _ = respond.send(result);
        }

        #[cfg(feature = "tquic-ext")]
        BridgeCommand::MigratePath {
            conn_id,
            local,
            remote,
            respond,
        } => {
            let result = if let Some(conn) = endpoint.conn_get_mut(conn_id) {
                conn.migrate_path(local, remote)
                    .map_err(|e| ConnectionError::TransportError {
                        code: 0,
                        reason: format!("migrate_path: {e}"),
                    })
            } else {
                Err(ConnectionError::LocallyClosed)
            };
            let _ = respond.send(result);
        }

        #[cfg(feature = "tquic-ext")]
        BridgeCommand::GetPathStats {
            conn_id,
            local,
            remote,
            respond,
        } => {
            let result = if let Some(conn) = endpoint.conn_get_mut(conn_id) {
                conn.get_path_stats(local, remote)
                    .map(|s| copy_path_stats(s))
                    .map_err(|e| ConnectionError::TransportError {
                        code: 0,
                        reason: format!("get_path_stats: {e}"),
                    })
            } else {
                Err(ConnectionError::LocallyClosed)
            };
            let _ = respond.send(result);
        }

        #[cfg(feature = "tquic-ext")]
        BridgeCommand::GetPaths { conn_id, respond } => {
            let result = if let Some(conn) = endpoint.conn_get_mut(conn_id) {
                Ok(conn.paths_iter().collect())
            } else {
                Err(ConnectionError::LocallyClosed)
            };
            let _ = respond.send(result);
        }
    }
}

/// Copy a `PathStats` field-by-field (tquic's PathStats doesn't derive Clone).
#[cfg(feature = "tquic-ext")]
fn copy_path_stats(s: &tquic::PathStats) -> tquic::PathStats {
    tquic::PathStats {
        recv_count: s.recv_count,
        recv_bytes: s.recv_bytes,
        sent_count: s.sent_count,
        sent_bytes: s.sent_bytes,
        lost_count: s.lost_count,
        lost_bytes: s.lost_bytes,
        acked_count: s.acked_count,
        acked_bytes: s.acked_bytes,
        init_cwnd: s.init_cwnd,
        final_cwnd: s.final_cwnd,
        max_cwnd: s.max_cwnd,
        min_cwnd: s.min_cwnd,
        max_inflight: s.max_inflight,
        loss_event_count: s.loss_event_count,
        cwnd_limited_count: s.cwnd_limited_count,
        cwnd_limited_duration: s.cwnd_limited_duration,
        min_rtt: s.min_rtt,
        max_rtt: s.max_rtt,
        srtt: s.srtt,
        rttvar: s.rttvar,
        in_slow_start: s.in_slow_start,
        pacing_rate: s.pacing_rate,
        min_pacing_rate: s.min_pacing_rate,
        pto_count: s.pto_count,
    }
}
