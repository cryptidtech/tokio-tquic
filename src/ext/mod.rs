//! TQUIC-specific extensions not available in Quinn.
//!
//! This module is gated behind the `tquic-ext` feature flag.

mod config;
mod connection;

pub use config::TransportConfigExt;
pub use connection::ConnectionExt;

// Re-export TQUIC-specific types
pub use tquic::CongestionControlAlgorithm;
pub use tquic::MultipathAlgorithm;
pub use tquic::PathStats;
pub use tquic::FourTuple;
