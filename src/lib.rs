//! A Tokio-friendly async wrapper around TQUIC with a Quinn-compatible API.
//!
//! This crate provides an async QUIC implementation powered by [TQUIC](https://tquic.net)
//! with an API surface that matches [Quinn](https://docs.rs/quinn). This allows developers
//! to swap between Quinn and TQUIC backends with minimal code changes.
//!
//! # Architecture
//!
//! TQUIC's core types (`Endpoint`, `Connection`) are `!Send` due to internal use of
//! `Rc<RefCell<...>>`. This crate bridges TQUIC's callback-based, single-threaded model
//! to Tokio's multi-threaded async world using a dedicated I/O thread with channel-based
//! communication.
//!
//! # Feature Flags
//!
//! - `tquic-ext`: Expose TQUIC-specific extensions (multipath, COPA, BBRv3, path management)
//!   through `ConnectionExt` and `TransportConfigExt` traits.

mod bridge;
mod config;
mod connection;
mod endpoint;
mod error;
mod incoming;
mod recv_stream;
mod send_stream;

#[cfg(feature = "tquic-ext")]
#[cfg_attr(docsrs, doc(cfg(feature = "tquic-ext")))]
pub mod ext;

// Primary types
pub use endpoint::{Accept, Endpoint, EndpointStats};
pub use incoming::{Incoming, IncomingFuture, RetryError};
pub use connection::{
    AcceptBi, AcceptUni, Connecting, Connection, OpenBi, OpenUni, ReadDatagram, SendDatagram,
    SendDatagramError, ZeroRttAccepted,
};
pub use send_stream::{ClosedStream, SendStream, StoppedError, WriteError};
pub use recv_stream::{ReadError, ReadExactError, ReadToEndError, RecvStream, ResetError};
pub use error::{ConnectError, ConnectionError};
pub use config::{ClientConfig, EndpointConfig, ServerConfig, TransportConfig};

// Re-export useful tquic types
pub use tquic::ConnectionId;

/// A QUIC variable-length integer.
///
/// Thin wrapper around `u64` matching Quinn's `VarInt` API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VarInt(u64);

impl VarInt {
    /// The largest representable value.
    pub const MAX: Self = Self((1 << 62) - 1);

    /// Construct a `VarInt` from a `u64`, returning `None` if the value is too large.
    pub const fn from_u64(v: u64) -> Option<Self> {
        if v <= Self::MAX.0 {
            Some(Self(v))
        } else {
            None
        }
    }

    /// Extract the integer value.
    pub const fn into_inner(self) -> u64 {
        self.0
    }
}

impl From<u32> for VarInt {
    fn from(v: u32) -> Self {
        Self(v as u64)
    }
}

impl From<u16> for VarInt {
    fn from(v: u16) -> Self {
        Self(v as u64)
    }
}

impl From<u8> for VarInt {
    fn from(v: u8) -> Self {
        Self(v as u64)
    }
}

impl std::fmt::Display for VarInt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// QUIC stream identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StreamId(u64);

impl StreamId {
    /// Extract the raw integer value.
    pub const fn into_inner(self) -> u64 {
        self.0
    }

    /// Create from a raw integer value.
    pub const fn from_raw(v: u64) -> Self {
        Self(v)
    }
}

impl std::fmt::Display for StreamId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Which side of a connection is referenced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    /// The client side.
    Client,
    /// The server side.
    Server,
}

/// Direction of a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dir {
    /// Bidirectional stream.
    Bi,
    /// Unidirectional stream.
    Uni,
}
