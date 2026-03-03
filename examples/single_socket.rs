//! Example demonstrating multiple connections over a single socket.
//!
//! Creates a server endpoint and multiple client connections to it,
//! all sharing the same client endpoint (single UDP socket).
//!
//! This example uses self-signed certificates generated at runtime.
//!
//! Usage:
//!   cargo run --example single_socket

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use tokio_tquic::{ClientConfig, Endpoint, ServerConfig, TransportConfig, VarInt};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Generate self-signed certificate.
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])?;
    let dir = tempfile::TempDir::new()?;
    let cert_path = dir.path().join("cert.pem");
    let key_path = dir.path().join("key.pem");
    std::fs::write(&cert_path, cert.cert.pem())?;
    std::fs::write(&key_path, cert.key_pair.serialize_pem())?;

    let mut transport = TransportConfig::new();
    transport.max_idle_timeout(Some(std::time::Duration::from_secs(5)));

    let server_config = {
        let mut sc = ServerConfig::new(
            cert_path.to_string_lossy().to_string(),
            key_path.to_string_lossy().to_string(),
            vec![b"demo".to_vec()],
        );
        sc.transport_config(Arc::new(transport.clone()));
        sc
    };

    // Start server.
    let server_addr: SocketAddr = "127.0.0.1:0".parse()?;
    let server = Endpoint::server(server_config, server_addr)?;
    let server_addr = server.local_addr()?;
    println!("Server listening on {server_addr}");

    // Server accept loop.
    let server_handle = tokio::spawn(async move {
        let mut conn_count = 0u32;
        while let Some(incoming) = server.accept().await {
            conn_count += 1;
            let id = conn_count;
            println!("[server] Accepted connection #{id} from {}", incoming.remote_address());

            tokio::spawn(async move {
                let conn = match incoming.accept() {
                    Ok(c) => match c.await {
                        Ok(c) => c,
                        Err(e) => {
                            eprintln!("[server #{id}] Handshake failed: {e}");
                            return;
                        }
                    },
                    Err(e) => {
                        eprintln!("[server #{id}] Accept failed: {e}");
                        return;
                    }
                };

                // Echo server: read and send back data on each stream.
                loop {
                    match conn.accept_bi().await {
                        Ok((mut send, mut recv)) => {
                            let data = match recv.read_to_end(64 * 1024).await {
                                Ok(d) => d,
                                Err(_) => break,
                            };
                            let _ = send.write_all(&data).await;
                            let _ = send.finish();
                        }
                        Err(_) => break,
                    }
                }

                println!("[server #{id}] Connection closed");
            });
        }
    });

    // Create a single client endpoint.
    let client_addr: SocketAddr = "127.0.0.1:0".parse()?;
    let client = Endpoint::client(client_addr)?;

    let mut client_config = ClientConfig::new(vec![b"demo".to_vec()]);
    client_config.verify(false);
    client_config.transport_config(Arc::new(transport));
    client.set_default_client_config(client_config);

    println!("Client bound to {}", client.local_addr()?);

    // Open multiple connections from the same client endpoint.
    let num_connections = 3;
    let mut handles = Vec::new();

    for i in 0..num_connections {
        let client = client.clone();
        handles.push(tokio::spawn(async move {
            let conn = client.connect(server_addr, "localhost")?.await?;
            println!("[client #{i}] Connected (stable_id={})", conn.stable_id());

            // Send a message and read the echo.
            let (mut send, mut recv) = conn.open_bi().await?;
            let msg = format!("Hello from connection #{i}!");
            send.write_all(msg.as_bytes()).await?;
            send.finish()?;

            let response = recv.read_to_end(64 * 1024).await?;
            println!("[client #{i}] Echo: {}", String::from_utf8_lossy(&response));

            conn.close(VarInt::from(0u32), b"done");
            Ok::<_, anyhow::Error>(())
        }));
    }

    for handle in handles {
        handle.await??;
    }

    println!("\nAll connections completed.");
    println!("Open connections: {}", client.open_connections());

    // Give the server time to process the closes.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    server_handle.abort();

    Ok(())
}
