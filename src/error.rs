/// Errors from initiating a connection.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ConnectError {
    /// The endpoint has been stopped.
    #[error("endpoint stopping")]
    EndpointStopping,
    /// No more connection IDs are available.
    #[error("CIDs exhausted")]
    CidsExhausted,
    /// The DNS name was invalid.
    #[error("invalid DNS name: {0}")]
    InvalidDnsName(String),
    /// The remote address is invalid.
    #[error("invalid remote address: {0}")]
    InvalidRemoteAddress(std::net::SocketAddr),
    /// No default client configuration has been set.
    #[error("no default client config")]
    NoDefaultClientConfig,
}

/// Errors indicating a connection was lost.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ConnectionError {
    /// The peer doesn't support any compatible QUIC version.
    #[error("version mismatch")]
    VersionMismatch,
    /// The peer violated the QUIC specification.
    #[error("transport error: code {code}, {reason}")]
    TransportError {
        /// The QUIC error code.
        code: u64,
        /// Human-readable reason.
        reason: String,
    },
    /// The peer closed the connection.
    #[error("connection closed by peer: error {error_code}")]
    ConnectionClosed {
        /// The error code supplied by the peer.
        error_code: u64,
        /// The reason supplied by the peer.
        reason: Vec<u8>,
    },
    /// The peer closed the connection at the application layer.
    #[error("application closed: error {error_code}")]
    ApplicationClosed {
        /// The error code supplied by the peer.
        error_code: u64,
        /// The reason supplied by the peer.
        reason: Vec<u8>,
    },
    /// The connection was reset.
    #[error("reset by peer")]
    Reset,
    /// The connection timed out.
    #[error("timed out")]
    TimedOut,
    /// The connection was closed locally.
    #[error("locally closed")]
    LocallyClosed,
    /// No more connection IDs are available.
    #[error("CIDs exhausted")]
    CidsExhausted,
}

impl ConnectionError {
    /// Create a `ConnectionError` from a TQUIC `ConnectionError` (from CONNECTION_CLOSE frame).
    pub(crate) fn from_tquic_conn_error(err: &tquic::error::ConnectionError) -> Self {
        if err.is_app {
            ConnectionError::ApplicationClosed {
                error_code: err.error_code,
                reason: err.reason.clone(),
            }
        } else {
            ConnectionError::ConnectionClosed {
                error_code: err.error_code,
                reason: err.reason.clone(),
            }
        }
    }

    /// Create a `ConnectionError` from a TQUIC `Error`.
    #[allow(dead_code)]
    pub(crate) fn from_tquic_error(err: &tquic::Error) -> Self {
        match err {
            tquic::Error::ConnectionRefused => ConnectionError::ConnectionClosed {
                error_code: 0x02,
                reason: b"connection refused".to_vec(),
            },
            tquic::Error::NoViablePath => ConnectionError::TimedOut,
            tquic::Error::ConnectionIdLimitError => ConnectionError::CidsExhausted,
            tquic::Error::UnknownVersion => ConnectionError::VersionMismatch,
            _ => ConnectionError::TransportError {
                code: 0,
                reason: err.to_string(),
            },
        }
    }

    /// Convert a `ConnectError` into a `ConnectionError`.
    pub(crate) fn from_connect_error(err: ConnectError) -> Self {
        match err {
            ConnectError::EndpointStopping => ConnectionError::LocallyClosed,
            ConnectError::CidsExhausted => ConnectionError::CidsExhausted,
            ConnectError::InvalidDnsName(s) => ConnectionError::TransportError {
                code: 0,
                reason: s,
            },
            ConnectError::InvalidRemoteAddress(addr) => ConnectionError::TransportError {
                code: 0,
                reason: format!("invalid remote address: {addr}"),
            },
            ConnectError::NoDefaultClientConfig => ConnectionError::TransportError {
                code: 0,
                reason: "no default client config".to_string(),
            },
        }
    }
}
