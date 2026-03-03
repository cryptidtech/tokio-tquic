#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tokio_tquic::{ClientConfig, Endpoint, EndpointConfig, ServerConfig, TransportConfig};

/// Holds temporary cert/key files on disk. Files are deleted when this is dropped.
pub struct TestCerts {
    _dir: TempDir,
    pub cert_path: String,
    pub key_path: String,
}

/// Generate a self-signed certificate and write to temporary PEM files.
pub fn generate_test_certs() -> TestCerts {
    let dir = TempDir::new().expect("failed to create temp dir");
    let certified_key =
        rcgen::generate_simple_self_signed(vec!["localhost".into()])
            .expect("failed to generate self-signed cert");

    let cert_path = dir.path().join("cert.pem");
    let key_path = dir.path().join("key.pem");

    std::fs::write(&cert_path, certified_key.cert.pem()).expect("write cert");
    std::fs::write(&key_path, certified_key.key_pair.serialize_pem()).expect("write key");

    TestCerts {
        _dir: dir,
        cert_path: cert_path.to_str().unwrap().to_string(),
        key_path: key_path.to_str().unwrap().to_string(),
    }
}

/// ALPN protocol used in tests.
pub const TEST_ALPN: &[u8] = b"test-proto";

/// Create a server config for tests (no cert verification, no retry).
pub fn test_server_config(certs: &TestCerts) -> ServerConfig {
    test_server_config_with_transport(certs, TransportConfig::new())
}

/// Create a server config with a custom transport config.
pub fn test_server_config_with_transport(
    certs: &TestCerts,
    mut transport: TransportConfig,
) -> ServerConfig {
    transport.max_idle_timeout(Some(Duration::from_secs(10)));

    let mut config = ServerConfig::new(
        certs.cert_path.clone(),
        certs.key_path.clone(),
        vec![TEST_ALPN.to_vec()],
    );
    config.transport_config(Arc::new(transport));
    config
}

/// Create a client config for tests (no cert verification).
pub fn test_client_config() -> ClientConfig {
    test_client_config_with_transport(TransportConfig::new())
}

/// Create a client config with a custom transport config.
pub fn test_client_config_with_transport(mut transport: TransportConfig) -> ClientConfig {
    transport.max_idle_timeout(Some(Duration::from_secs(10)));

    let mut config = ClientConfig::new(vec![TEST_ALPN.to_vec()]);
    config.verify(false);
    config.transport_config(Arc::new(transport));
    config
}

/// Create a connected server+client endpoint pair on localhost.
///
/// Returns `(server_endpoint, client_endpoint, server_addr)`.
pub fn create_endpoints(certs: &TestCerts) -> (Endpoint, Endpoint, SocketAddr) {
    let server_config = test_server_config(certs);

    let server = Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap())
        .expect("failed to create server endpoint");

    let server_addr = server.local_addr().expect("server local_addr");

    let client = Endpoint::client("127.0.0.1:0".parse().unwrap())
        .expect("failed to create client endpoint");
    client.set_default_client_config(test_client_config());

    (server, client, server_addr)
}

/// Create a server+client endpoint pair with custom transport configs.
///
/// Returns `(server_endpoint, client_endpoint, server_addr)`.
pub fn create_endpoints_with_transport(
    certs: &TestCerts,
    transport: TransportConfig,
) -> (Endpoint, Endpoint, SocketAddr) {
    let server_config = test_server_config_with_transport(certs, transport.clone());

    let server = Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap())
        .expect("failed to create server endpoint");

    let server_addr = server.local_addr().expect("server local_addr");

    let client = Endpoint::client("127.0.0.1:0".parse().unwrap())
        .expect("failed to create client endpoint");
    client.set_default_client_config(test_client_config_with_transport(transport));

    (server, client, server_addr)
}

/// Create a server+client endpoint pair with a custom endpoint config.
///
/// Returns `(server_endpoint, client_endpoint, server_addr)`.
pub fn create_endpoints_with_endpoint_config(
    certs: &TestCerts,
    endpoint_config: EndpointConfig,
) -> (Endpoint, Endpoint, SocketAddr) {
    let server_config = test_server_config(certs);

    let server = Endpoint::new(
        endpoint_config.clone(),
        Some(server_config),
        "127.0.0.1:0".parse().unwrap(),
    )
    .expect("failed to create server endpoint");

    let server_addr = server.local_addr().expect("server local_addr");

    let client = Endpoint::new(
        endpoint_config,
        None,
        "127.0.0.1:0".parse().unwrap(),
    )
    .expect("failed to create client endpoint");
    client.set_default_client_config(test_client_config());

    (server, client, server_addr)
}
