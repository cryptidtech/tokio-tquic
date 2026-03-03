use std::sync::Arc;
use std::time::Duration;

use crate::error::ConnectError;

/// Configuration for a QUIC endpoint.
///
/// Maps to TQUIC's `Config` endpoint-level settings (CID generation, stateless reset,
/// connection limits, retry, token management).
#[derive(Debug, Clone)]
pub struct EndpointConfig {
    /// Connection ID length (0-20 bytes).
    pub(crate) cid_len: usize,
    /// Enable stateless reset.
    pub(crate) stateless_reset: bool,
    /// Stateless reset token key (64 bytes). `None` uses TQUIC's random default.
    pub(crate) reset_token_key: Option<[u8; 64]>,
    /// Address token lifetime in seconds.
    pub(crate) address_token_lifetime: u64,
    /// Maximum number of packets sent per batch.
    pub(crate) send_batch_size: usize,
}

impl Default for EndpointConfig {
    fn default() -> Self {
        Self {
            cid_len: 8,
            stateless_reset: true,
            reset_token_key: None,
            address_token_lifetime: 86400,
            send_batch_size: 64,
        }
    }
}

impl EndpointConfig {
    /// Create a new endpoint configuration with defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the connection ID length (0-20 bytes).
    pub fn cid_len(&mut self, len: usize) -> &mut Self {
        self.cid_len = len;
        self
    }

    /// Enable or disable stateless reset.
    pub fn stateless_reset(&mut self, enabled: bool) -> &mut Self {
        self.stateless_reset = enabled;
        self
    }

    /// Set the stateless reset token key (64 bytes).
    pub fn reset_token_key(&mut self, key: [u8; 64]) -> &mut Self {
        self.reset_token_key = Some(key);
        self
    }

    /// Set the address token lifetime in seconds.
    pub fn address_token_lifetime(&mut self, seconds: u64) -> &mut Self {
        self.address_token_lifetime = seconds;
        self
    }

    /// Set the maximum number of packets sent per batch.
    pub fn send_batch_size(&mut self, size: usize) -> &mut Self {
        self.send_batch_size = size;
        self
    }
}

/// Transport-level configuration shared by client and server.
///
/// Maps to TQUIC's `Config` transport parameter settings. These control flow control
/// windows, stream concurrency limits, timeouts, and MTU discovery.
#[derive(Debug, Clone)]
pub struct TransportConfig {
    // Timeouts
    pub(crate) max_idle_timeout: Option<Duration>,
    pub(crate) max_handshake_timeout: Option<Duration>,

    // Flow control
    pub(crate) initial_max_data: u64,
    pub(crate) initial_max_stream_data_bidi_local: u64,
    pub(crate) initial_max_stream_data_bidi_remote: u64,
    pub(crate) initial_max_stream_data_uni: u64,
    pub(crate) max_connection_window: u64,
    pub(crate) max_stream_window: u64,

    // Stream limits
    pub(crate) initial_max_streams_bidi: u64,
    pub(crate) initial_max_streams_uni: u64,

    // MTU
    pub(crate) enable_dplpmtud: bool,
    pub(crate) send_udp_payload_size: Option<usize>,

    // ACK behavior
    pub(crate) ack_delay_exponent: Option<u64>,
    pub(crate) max_ack_delay: Option<u64>,
    pub(crate) ack_eliciting_threshold: Option<u64>,

    // Connection limits
    pub(crate) max_concurrent_conns: u32,

    // Recovery
    pub(crate) initial_rtt: Option<Duration>,

    // Congestion control algorithm (stored for tquic-ext, applied internally)
    #[cfg(feature = "tquic-ext")]
    pub(crate) congestion_control_algorithm: Option<tquic::CongestionControlAlgorithm>,
    #[cfg(feature = "tquic-ext")]
    pub(crate) initial_congestion_window: Option<u64>,
    #[cfg(feature = "tquic-ext")]
    pub(crate) min_congestion_window: Option<u64>,

    // Pacing
    #[cfg(feature = "tquic-ext")]
    pub(crate) enable_pacing: Option<bool>,

    // Multipath
    #[cfg(feature = "tquic-ext")]
    pub(crate) enable_multipath: Option<bool>,
    #[cfg(feature = "tquic-ext")]
    pub(crate) multipath_algorithm: Option<tquic::MultipathAlgorithm>,

    // BBR tuning
    #[cfg(feature = "tquic-ext")]
    pub(crate) bbr_probe_rtt_duration: Option<u64>,
    #[cfg(feature = "tquic-ext")]
    pub(crate) bbr_probe_rtt_based_on_bdp: Option<bool>,
    #[cfg(feature = "tquic-ext")]
    pub(crate) bbr_probe_rtt_cwnd_gain: Option<f64>,
    #[cfg(feature = "tquic-ext")]
    pub(crate) bbr_rtprop_filter_len: Option<u64>,
    #[cfg(feature = "tquic-ext")]
    pub(crate) bbr_probe_bw_cwnd_gain: Option<f64>,

    // COPA tuning
    #[cfg(feature = "tquic-ext")]
    pub(crate) copa_slow_start_delta: Option<f64>,
    #[cfg(feature = "tquic-ext")]
    pub(crate) copa_steady_delta: Option<f64>,
    #[cfg(feature = "tquic-ext")]
    pub(crate) copa_use_standing_rtt: Option<bool>,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            max_idle_timeout: Some(Duration::from_secs(30)),
            max_handshake_timeout: None,
            initial_max_data: 10_485_760,
            initial_max_stream_data_bidi_local: 5_242_880,
            initial_max_stream_data_bidi_remote: 2_097_152,
            initial_max_stream_data_uni: 1_048_576,
            max_connection_window: 15 * 1024 * 1024,
            max_stream_window: 6 * 1024 * 1024,
            initial_max_streams_bidi: 200,
            initial_max_streams_uni: 100,
            enable_dplpmtud: true,
            send_udp_payload_size: None,
            ack_delay_exponent: None,
            max_ack_delay: None,
            ack_eliciting_threshold: None,
            max_concurrent_conns: 1_000_000,
            initial_rtt: None,
            #[cfg(feature = "tquic-ext")]
            congestion_control_algorithm: None,
            #[cfg(feature = "tquic-ext")]
            initial_congestion_window: None,
            #[cfg(feature = "tquic-ext")]
            min_congestion_window: None,
            #[cfg(feature = "tquic-ext")]
            enable_pacing: None,
            #[cfg(feature = "tquic-ext")]
            enable_multipath: None,
            #[cfg(feature = "tquic-ext")]
            multipath_algorithm: None,
            #[cfg(feature = "tquic-ext")]
            bbr_probe_rtt_duration: None,
            #[cfg(feature = "tquic-ext")]
            bbr_probe_rtt_based_on_bdp: None,
            #[cfg(feature = "tquic-ext")]
            bbr_probe_rtt_cwnd_gain: None,
            #[cfg(feature = "tquic-ext")]
            bbr_rtprop_filter_len: None,
            #[cfg(feature = "tquic-ext")]
            bbr_probe_bw_cwnd_gain: None,
            #[cfg(feature = "tquic-ext")]
            copa_slow_start_delta: None,
            #[cfg(feature = "tquic-ext")]
            copa_steady_delta: None,
            #[cfg(feature = "tquic-ext")]
            copa_use_standing_rtt: None,
        }
    }
}

impl TransportConfig {
    /// Create a new transport configuration with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Maximum duration of inactivity before the connection is closed.
    ///
    /// `None` disables the idle timeout. Maps to TQUIC's `set_max_idle_timeout`.
    pub fn max_idle_timeout(&mut self, timeout: Option<Duration>) -> &mut Self {
        self.max_idle_timeout = timeout;
        self
    }

    /// Maximum duration for the handshake to complete.
    ///
    /// `None` uses TQUIC's default. Maps to TQUIC's `set_max_handshake_timeout`.
    pub fn max_handshake_timeout(&mut self, timeout: Option<Duration>) -> &mut Self {
        self.max_handshake_timeout = timeout;
        self
    }

    /// Maximum bytes the peer may transmit across all streams before receiving an update.
    ///
    /// Maps to TQUIC's `set_initial_max_data`.
    pub fn receive_window(&mut self, bytes: u64) -> &mut Self {
        self.initial_max_data = bytes;
        self
    }

    /// Maximum bytes the peer may transmit on each bidirectional stream we open.
    ///
    /// Maps to TQUIC's `set_initial_max_stream_data_bidi_local`.
    pub fn stream_receive_window_bidi_local(&mut self, bytes: u64) -> &mut Self {
        self.initial_max_stream_data_bidi_local = bytes;
        self
    }

    /// Maximum bytes the peer may transmit on each bidirectional stream they open.
    ///
    /// Maps to TQUIC's `set_initial_max_stream_data_bidi_remote`.
    pub fn stream_receive_window_bidi_remote(&mut self, bytes: u64) -> &mut Self {
        self.initial_max_stream_data_bidi_remote = bytes;
        self
    }

    /// Maximum bytes the peer may transmit on each unidirectional stream.
    ///
    /// Maps to TQUIC's `set_initial_max_stream_data_uni`.
    pub fn stream_receive_window_uni(&mut self, bytes: u64) -> &mut Self {
        self.initial_max_stream_data_uni = bytes;
        self
    }

    /// Maximum aggregate receive window across all streams.
    ///
    /// Maps to TQUIC's `set_max_connection_window`.
    pub fn max_connection_window(&mut self, bytes: u64) -> &mut Self {
        self.max_connection_window = bytes;
        self
    }

    /// Maximum receive window for a single stream.
    ///
    /// Maps to TQUIC's `set_max_stream_window`.
    pub fn max_stream_window(&mut self, bytes: u64) -> &mut Self {
        self.max_stream_window = bytes;
        self
    }

    /// Maximum number of bidirectional streams the peer may open concurrently.
    ///
    /// Maps to TQUIC's `set_initial_max_streams_bidi`.
    pub fn max_concurrent_bidi_streams(&mut self, count: u64) -> &mut Self {
        self.initial_max_streams_bidi = count;
        self
    }

    /// Maximum number of unidirectional streams the peer may open concurrently.
    ///
    /// Maps to TQUIC's `set_initial_max_streams_uni`.
    pub fn max_concurrent_uni_streams(&mut self, count: u64) -> &mut Self {
        self.initial_max_streams_uni = count;
        self
    }

    /// Whether to use DPLPMTUD for path MTU discovery.
    ///
    /// Maps to TQUIC's `enable_dplpmtud`.
    pub fn enable_mtu_discovery(&mut self, enabled: bool) -> &mut Self {
        self.enable_dplpmtud = enabled;
        self
    }

    /// Initial RTT estimate. `None` uses TQUIC's default (333ms).
    ///
    /// Maps to TQUIC's `set_initial_rtt`.
    pub fn initial_rtt(&mut self, rtt: Duration) -> &mut Self {
        self.initial_rtt = Some(rtt);
        self
    }

    /// Maximum number of concurrent connections the endpoint will accept.
    ///
    /// Maps to TQUIC's `set_max_concurrent_conns`.
    pub fn max_concurrent_connections(&mut self, count: u32) -> &mut Self {
        self.max_concurrent_conns = count;
        self
    }
}

/// Server-specific configuration.
///
/// Wraps TQUIC's `TlsConfig` (server mode) and transport configuration. Provides
/// TLS certificate/key, ALPN protocols, and server-specific options like retry.
#[derive(Clone)]
pub struct ServerConfig {
    pub(crate) transport: Arc<TransportConfig>,
    pub(crate) cert_file: String,
    pub(crate) key_file: String,
    pub(crate) alpn_protocols: Vec<Vec<u8>>,
    pub(crate) enable_early_data: bool,
    pub(crate) enable_retry: bool,
}

impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerConfig")
            .field("alpn_protocols", &self.alpn_protocols)
            .field("enable_early_data", &self.enable_early_data)
            .field("enable_retry", &self.enable_retry)
            .finish_non_exhaustive()
    }
}

impl ServerConfig {
    /// Create a new server configuration.
    ///
    /// # Arguments
    /// * `cert_file` - Path to the PEM-encoded certificate chain file
    /// * `key_file` - Path to the PEM-encoded private key file
    /// * `alpn_protocols` - Application-Layer Protocol Negotiation protocols
    pub fn new(cert_file: String, key_file: String, alpn_protocols: Vec<Vec<u8>>) -> Self {
        Self {
            transport: Arc::new(TransportConfig::default()),
            cert_file,
            key_file,
            alpn_protocols,
            enable_early_data: false,
            enable_retry: false,
        }
    }

    /// Set the transport configuration.
    pub fn transport_config(&mut self, config: Arc<TransportConfig>) -> &mut Self {
        self.transport = config;
        self
    }

    /// Enable 0-RTT data acceptance.
    pub fn enable_early_data(&mut self, enabled: bool) -> &mut Self {
        self.enable_early_data = enabled;
        self
    }

    /// Enable stateless retry for address validation.
    ///
    /// Maps to TQUIC's `Config::enable_retry`. This is a global setting;
    /// per-connection retry (as in Quinn's `Incoming::retry()`) is not supported.
    pub fn enable_retry(&mut self, enabled: bool) -> &mut Self {
        self.enable_retry = enabled;
        self
    }
}

/// Client-specific configuration.
///
/// Wraps TQUIC's `TlsConfig` (client mode) and transport configuration. Provides
/// ALPN protocols, certificate verification options, and 0-RTT settings.
#[derive(Clone)]
pub struct ClientConfig {
    pub(crate) transport: Arc<TransportConfig>,
    pub(crate) alpn_protocols: Vec<Vec<u8>>,
    pub(crate) enable_early_data: bool,
    pub(crate) verify: bool,
    pub(crate) ca_certs: Option<String>,
}

impl std::fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientConfig")
            .field("alpn_protocols", &self.alpn_protocols)
            .field("enable_early_data", &self.enable_early_data)
            .field("verify", &self.verify)
            .finish_non_exhaustive()
    }
}

impl ClientConfig {
    /// Create a new client configuration.
    ///
    /// # Arguments
    /// * `alpn_protocols` - Application-Layer Protocol Negotiation protocols
    pub fn new(alpn_protocols: Vec<Vec<u8>>) -> Self {
        Self {
            transport: Arc::new(TransportConfig::default()),
            alpn_protocols,
            enable_early_data: false,
            verify: true,
            ca_certs: None,
        }
    }

    /// Set the transport configuration.
    pub fn transport_config(&mut self, config: Arc<TransportConfig>) -> &mut Self {
        self.transport = config;
        self
    }

    /// Enable 0-RTT data sending.
    pub fn enable_early_data(&mut self, enabled: bool) -> &mut Self {
        self.enable_early_data = enabled;
        self
    }

    /// Whether to verify the server's TLS certificate.
    pub fn verify(&mut self, verify: bool) -> &mut Self {
        self.verify = verify;
        self
    }

    /// Set the path to the CA certificate file for server verification.
    pub fn ca_certs(&mut self, path: String) -> &mut Self {
        self.ca_certs = Some(path);
        self
    }
}

// ---------------------------------------------------------------------------
// Internal: build a tquic::Config from our config types
// ---------------------------------------------------------------------------

/// Build a `tquic::TlsConfig` for a server from a `ServerConfig`.
pub(crate) fn build_server_tls_config(
    config: &ServerConfig,
) -> Result<tquic::TlsConfig, ConnectError> {
    tquic::TlsConfig::new_server_config(
        &config.cert_file,
        &config.key_file,
        config.alpn_protocols.clone(),
        config.enable_early_data,
    )
    .map_err(|e| ConnectError::InvalidDnsName(format!("TLS config error: {e}")))
}

/// Build a `tquic::TlsConfig` for a client from a `ClientConfig`.
pub(crate) fn build_client_tls_config(
    config: &ClientConfig,
) -> Result<tquic::TlsConfig, ConnectError> {
    let mut tls = tquic::TlsConfig::new_client_config(
        config.alpn_protocols.clone(),
        config.enable_early_data,
    )
    .map_err(|e| ConnectError::InvalidDnsName(format!("TLS config error: {e}")))?;

    tls.set_verify(config.verify);

    if let Some(ref ca_path) = config.ca_certs {
        tls.set_ca_certs(ca_path)
            .map_err(|e| ConnectError::InvalidDnsName(format!("CA cert error: {e}")))?;
    }

    Ok(tls)
}

/// Merge an `EndpointConfig` and `TransportConfig` into a `tquic::Config`.
///
/// Also applies the TLS config and server-specific options from `ServerConfig` if present.
pub(crate) fn build_tquic_config(
    endpoint: &EndpointConfig,
    transport: &TransportConfig,
    tls_config: tquic::TlsConfig,
    server_config: Option<&ServerConfig>,
) -> Result<tquic::Config, ConnectError> {
    let mut config = tquic::Config::new()
        .map_err(|e| ConnectError::InvalidDnsName(format!("config error: {e}")))?;

    // --- Endpoint-level settings ---
    config.set_cid_len(endpoint.cid_len);
    config.enable_stateless_reset(endpoint.stateless_reset);
    config.set_address_token_lifetime(endpoint.address_token_lifetime);
    config.set_send_batch_size(endpoint.send_batch_size);

    if let Some(key) = endpoint.reset_token_key {
        config.set_reset_token_key(key);
    }

    // --- Transport parameters ---

    // Timeouts
    let idle_ms = transport
        .max_idle_timeout
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    config.set_max_idle_timeout(idle_ms);

    if let Some(handshake) = transport.max_handshake_timeout {
        config.set_max_handshake_timeout(handshake.as_millis() as u64);
    }

    // Flow control
    config.set_initial_max_data(transport.initial_max_data);
    config.set_initial_max_stream_data_bidi_local(transport.initial_max_stream_data_bidi_local);
    config.set_initial_max_stream_data_bidi_remote(transport.initial_max_stream_data_bidi_remote);
    config.set_initial_max_stream_data_uni(transport.initial_max_stream_data_uni);
    config.set_max_connection_window(transport.max_connection_window);
    config.set_max_stream_window(transport.max_stream_window);

    // Stream limits
    config.set_initial_max_streams_bidi(transport.initial_max_streams_bidi);
    config.set_initial_max_streams_uni(transport.initial_max_streams_uni);

    // MTU discovery
    config.enable_dplpmtud(transport.enable_dplpmtud);
    if let Some(size) = transport.send_udp_payload_size {
        config.set_send_udp_payload_size(size);
    }

    // ACK behavior
    if let Some(v) = transport.ack_delay_exponent {
        config.set_ack_delay_exponent(v);
    }
    if let Some(v) = transport.max_ack_delay {
        config.set_max_ack_delay(v);
    }
    if let Some(v) = transport.ack_eliciting_threshold {
        config.set_ack_eliciting_threshold(v);
    }

    // Connection limits
    config.set_max_concurrent_conns(transport.max_concurrent_conns);

    // Recovery
    if let Some(rtt) = transport.initial_rtt {
        config.set_initial_rtt(rtt.as_millis() as u64);
    }

    // --- tquic-ext settings ---
    #[cfg(feature = "tquic-ext")]
    {
        if let Some(algo) = transport.congestion_control_algorithm {
            config.set_congestion_control_algorithm(algo);
        }
        if let Some(v) = transport.initial_congestion_window {
            config.set_initial_congestion_window(v);
        }
        if let Some(v) = transport.min_congestion_window {
            config.set_min_congestion_window(v);
        }
        if let Some(v) = transport.enable_pacing {
            config.enable_pacing(v);
        }
        if let Some(v) = transport.enable_multipath {
            config.enable_multipath(v);
        }
        if let Some(algo) = transport.multipath_algorithm {
            config.set_multipath_algorithm(algo);
        }
        if let Some(v) = transport.bbr_probe_rtt_duration {
            config.set_bbr_probe_rtt_duration(v);
        }
        if let Some(v) = transport.bbr_probe_rtt_based_on_bdp {
            config.enable_bbr_probe_rtt_based_on_bdp(v);
        }
        if let Some(v) = transport.bbr_probe_rtt_cwnd_gain {
            config.set_bbr_probe_rtt_cwnd_gain(v);
        }
        if let Some(v) = transport.bbr_rtprop_filter_len {
            config.set_bbr_rtprop_filter_len(v);
        }
        if let Some(v) = transport.bbr_probe_bw_cwnd_gain {
            config.set_bbr_probe_bw_cwnd_gain(v);
        }
        if let Some(v) = transport.copa_slow_start_delta {
            config.set_copa_slow_start_delta(v);
        }
        if let Some(v) = transport.copa_steady_delta {
            config.set_copa_steady_delta(v);
        }
        if let Some(v) = transport.copa_use_standing_rtt {
            config.enable_copa_use_standing_rtt(v);
        }
    }

    // --- Server-specific options ---
    if let Some(sc) = server_config {
        config.enable_retry(sc.enable_retry);
    }

    // --- TLS ---
    config.set_tls_config(tls_config);

    Ok(config)
}
