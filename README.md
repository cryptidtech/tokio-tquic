# tokio-tquic

A Tokio-friendly async wrapper around [TQUIC](https://tquic.net) with a
[Quinn](https://docs.rs/quinn)-compatible API.

## Why tokio-tquic?

[TQUIC](https://github.com/tencent/tquic) is a high-performance QUIC
implementation from Tencent, featuring advanced congestion control algorithms
(BBRv3, COPA), multipath QUIC support, and a BoringSSL-based TLS stack.
However, TQUIC exposes a callback-driven, synchronous API that is difficult to
use directly from async Rust code, and its core types are `!Send` due to
internal use of `Rc<RefCell<...>>`.

[Quinn](https://github.com/quinn-rs/quinn) is the de facto standard async QUIC
library for Rust, with an ergonomic `async`/`await` API built on Tokio. Many
Rust projects already depend on Quinn's API surface.

**tokio-tquic** bridges these two worlds: it wraps TQUIC's engine behind a
Quinn-compatible async API so that you can:

- Use TQUIC's performance and features (BBRv3, COPA, multipath) with familiar
  async Rust ergonomics.
- Swap between Quinn and TQUIC as the QUIC backend with minimal code changes.
- Incrementally adopt TQUIC-specific features (multipath, path management)
  through opt-in extension traits.

## Design

### The `!Send` problem

TQUIC's `Endpoint` and `Connection` types use `Rc<RefCell<...>>` internally,
making them `!Send`. They cannot be shared across Tokio tasks or held across
`.await` points on a multi-threaded runtime. This is the core architectural
challenge that tokio-tquic solves.

### Dedicated bridge thread

tokio-tquic spawns a **dedicated OS thread** that owns all TQUIC state. This
thread runs a single-threaded Tokio runtime with a `LocalSet`, so TQUIC's
`!Send` types never leave their thread. The user-facing async types
(`Endpoint`, `Connection`, `SendStream`, `RecvStream`) are thin `Send`-safe
handles that communicate with the bridge thread through channels:

```
Tokio tasks (Send, multi-threaded)       Bridge thread (!Send, single-threaded)
+-----------------------------------+    +-------------------------------------+
|                                   |    |                                     |
|  Endpoint::connect()              |    |  tquic::Endpoint                    |
|    |                              |    |    |                                |
|    +-- BridgeCommand::Connect --> |    | -> endpoint.connect()               |
|                                   |    |    |                                |
|  Connection::open_bi()            |    |  on_conn_established callback       |
|    |                              |    |    |                                |
|    +-- BridgeCommand::OpenBi ---> |    | -> conn.stream_bidi_new()           |
|                                   |    |    |                                |
|  SendStream::write()              |    |  on_stream_writable callback        |
|    |                              |    |    |                                |
|    +-- BridgeCommand::Write ----> |    | -> conn.stream_write()              |
|                                   |    |                                     |
+-----------------------------------+    +-------------------------------------+
         mpsc channel (Send)                   TQUIC event loop + UDP I/O
```

Commands flow from async tasks to the bridge via `tokio::sync::mpsc`. Responses
come back via `oneshot` channels. Stream readability/writability notifications
use `tokio::sync::Notify` instances, driven by TQUIC's `TransportHandler`
callbacks.

### Quinn API compatibility

The public API mirrors Quinn's type names, method signatures, and error types:

| Quinn | tokio-tquic | Notes |
|-------|-------------|-------|
| `Endpoint` | `Endpoint` | `client()`, `server()`, `connect()`, `accept()`, `close()`, `wait_idle()` |
| `Connecting` | `Connecting` | Future that resolves to `Connection` after handshake |
| `Incoming` | `Incoming` | `accept()`, `refuse()`, `ignore()`, `remote_address()` |
| `Connection` | `Connection` | `open_bi()`, `open_uni()`, `accept_bi()`, `accept_uni()`, `close()`, `closed()` |
| `SendStream` | `SendStream` | `write()`, `write_all()`, `finish()`, `reset()`, `AsyncWrite` |
| `RecvStream` | `RecvStream` | `read()`, `read_exact()`, `read_to_end()`, `stop()`, `AsyncRead` |
| `ClientConfig` | `ClientConfig` | ALPN, TLS verification, transport config |
| `ServerConfig` | `ServerConfig` | Cert/key paths, ALPN, transport config |
| `TransportConfig` | `TransportConfig` | Idle timeout, flow control, stream limits |
| `ConnectionError` | `ConnectionError` | `TimedOut`, `ConnectionClosed`, `ApplicationClosed`, etc. |

### TQUIC extensions

TQUIC-specific features that go beyond Quinn's API are available behind the
`tquic-ext` feature flag through extension traits:

- **`TransportConfigExt`**: Configure congestion control algorithm (Cubic, BBR,
  BBRv3, COPA), enable multipath, tune BBR/COPA parameters, enable pacing.
- **`ConnectionExt`**: Add/abandon/migrate network paths, query per-path
  statistics.

This keeps the default API surface identical to Quinn while letting you opt in
to TQUIC's advanced capabilities.

## Quick start

### Dependencies

```toml
[dependencies]
tokio-tquic = { path = "../tokio-tquic" }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }

# For generating self-signed certificates in examples:
rcgen = "0.13"
tempfile = "3"
```

### Generating certificates

Both the server and client need TLS certificates. For testing, you can generate
self-signed certs at runtime with `rcgen`:

```rust
use tempfile::TempDir;

struct CertFiles {
    _dir: TempDir,
    cert_path: String,
    key_path: String,
}

fn generate_self_signed_cert(subject: &str) -> CertFiles {
    let dir = TempDir::new().unwrap();
    let certified_key =
        rcgen::generate_simple_self_signed(vec![subject.to_string()]).unwrap();

    let cert_path = dir.path().join("cert.pem");
    let key_path = dir.path().join("key.pem");

    std::fs::write(&cert_path, certified_key.cert.pem()).unwrap();
    std::fs::write(&key_path, certified_key.key_pair.serialize_pem()).unwrap();

    CertFiles {
        _dir: dir,
        cert_path: cert_path.to_string_lossy().to_string(),
        key_path: key_path.to_string_lossy().to_string(),
    }
}
```

### Server

```rust
use std::sync::Arc;
use tokio_tquic::{Endpoint, ServerConfig, TransportConfig, VarInt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Generate a server certificate.
    let server_cert = generate_self_signed_cert("localhost");

    // Configure the server.
    let mut transport = TransportConfig::new();
    transport.max_idle_timeout(Some(std::time::Duration::from_secs(30)));

    let mut server_config = ServerConfig::new(
        server_cert.cert_path.clone(),
        server_cert.key_path.clone(),
        vec![b"my-app-protocol".to_vec()],
    );
    server_config.transport_config(Arc::new(transport));

    // Bind and listen.
    let server = Endpoint::server(server_config, "0.0.0.0:4433".parse()?)?;
    println!("Listening on {}", server.local_addr()?);

    // Accept loop.
    while let Some(incoming) = server.accept().await {
        println!("Connection from {}", incoming.remote_address());

        tokio::spawn(async move {
            let conn = incoming.accept().unwrap().await.unwrap();

            // Echo server: accept bidi streams, read data, send it back.
            while let Ok((mut send, mut recv)) = conn.accept_bi().await {
                tokio::spawn(async move {
                    let data = recv.read_to_end(64 * 1024).await.unwrap();
                    send.write_all(&data).await.unwrap();
                    send.finish().unwrap();
                });
            }
        });
    }

    Ok(())
}
```

### Client

```rust
use std::sync::Arc;
use tokio_tquic::{ClientConfig, Endpoint, TransportConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Configure the client.
    let mut transport = TransportConfig::new();
    transport.max_idle_timeout(Some(std::time::Duration::from_secs(30)));

    let mut client_config = ClientConfig::new(vec![b"my-app-protocol".to_vec()]);
    client_config.verify(false); // Skip cert verification for testing.
    client_config.transport_config(Arc::new(transport));

    // Create a client endpoint.
    let client = Endpoint::client("0.0.0.0:0".parse()?)?;
    client.set_default_client_config(client_config);

    // Connect to the server.
    let conn = client
        .connect("127.0.0.1:4433".parse()?, "localhost")?
        .await?;
    println!("Connected to {}", conn.remote_address());

    // Open a bidirectional stream, send data, and read the echo.
    let (mut send, mut recv) = conn.open_bi().await?;
    send.write_all(b"Hello, QUIC!").await?;
    send.finish()?;

    let response = recv.read_to_end(64 * 1024).await?;
    println!("Received: {}", String::from_utf8_lossy(&response));

    conn.close(tokio_tquic::VarInt::from(0u32), b"done");

    Ok(())
}
```

### Mutual TLS (mTLS)

For production use, generate separate key pairs for the server and client and
configure certificate verification:

```rust
// Generate two separate certificates.
let server_cert = generate_self_signed_cert("my-server.example.com");
let client_cert = generate_self_signed_cert("my-client.example.com");

// Server configuration with its own cert/key.
let server_config = ServerConfig::new(
    server_cert.cert_path.clone(),
    server_cert.key_path.clone(),
    vec![b"my-protocol".to_vec()],
);

// Client configuration with verification against a known CA.
let mut client_config = ClientConfig::new(vec![b"my-protocol".to_vec()]);
client_config.verify(true);
client_config.ca_certs(server_cert.cert_path.clone()); // Trust the server's cert.
```

### Unidirectional streams

```rust
// Sender side:
let mut send = conn.open_uni().await?;
send.write_all(b"one-way data").await?;
send.finish()?;

// Receiver side:
let mut recv = conn.accept_uni().await?;
let data = recv.read_to_end(1024 * 1024).await?;
```

### Using TQUIC extensions

Enable the `tquic-ext` feature to access multipath and advanced congestion
control:

```toml
[dependencies]
tokio-tquic = { path = "../tokio-tquic", features = ["tquic-ext"] }
```

```rust
use tokio_tquic::ext::{TransportConfigExt, ConnectionExt};

// Configure BBRv3 congestion control.
let mut transport = TransportConfig::new();
transport.set_congestion_control_algorithm(tquic::CongestionControlAlgorithm::Bbr3);
transport.enable_multipath(true);
transport.set_multipath_algorithm(tquic::MultipathAlgorithm::MinRtt);

// At runtime, manage network paths.
conn.add_path("10.0.1.1:0".parse()?, "10.0.1.2:4433".parse()?).await?;
let stats = conn.path_stats().await?;
```

## Feature flags

| Flag | Default | Description |
|------|---------|-------------|
| `tquic-ext` | off | Expose TQUIC-specific extensions: multipath, COPA, BBRv3, path management |

## Differences from Quinn

tokio-tquic aims for API compatibility with Quinn, but some differences exist
due to the different underlying QUIC implementations:

- **TLS backend**: TQUIC uses BoringSSL; Quinn uses rustls. Certificate
  configuration uses file paths (`cert_path`, `key_path`) rather than in-memory
  `CertificateDer`/`PrivateKeyDer` types.
- **0-RTT**: `Connecting::into_0rtt()` always returns `Err` (TQUIC handles
  0-RTT internally via `TlsConfig::set_early_data_enabled`).
- **Retry**: `Incoming::retry()` always returns `Err`. TQUIC handles retry
  globally via `ServerConfig::enable_retry()`, not per-connection.
- **Datagrams**: Not currently supported (TQUIC does not expose a public
  unreliable datagram API).
- **Rebind**: `Endpoint::rebind()` is not available. TQUIC manages socket I/O
  internally.
- **Runtime abstraction**: tokio-tquic is Tokio-only. There is no `Runtime`
  trait or pluggable runtime support.
- **Connection ID generator**: Configured via `EndpointConfig::cid_len()`
  rather than a custom `ConnectionIdGenerator` trait.

## Examples

The `examples/` directory contains runnable demos:

- **`connection.rs`** - Minimal client/server echo.
- **`server.rs`** - HTTP/0.9 file server.
- **`client.rs`** - HTTP/0.9 client.
- **`single_socket.rs`** - Multiple connections sharing a single endpoint.

Run them with:

```bash
cargo run --example single_socket
cargo run --example server -- ./www
cargo run --example client -- https://localhost:4433/index.html
```

## License

Apache-2.0
