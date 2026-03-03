//! HTTP/0.9 client example.
//!
//! Connects to a QUIC server and requests files using a simple protocol.
//! Each file request opens a new bidirectional stream, sends the path,
//! and reads the response.
//!
//! Usage:
//!   cargo run --example client -- <server_addr> <path> [<path>...]
//!
//! Example:
//!   cargo run --example client -- 127.0.0.1:4433 /index.html /style.css

use std::net::SocketAddr;

use anyhow::Result;
use tokio_tquic::{ClientConfig, Endpoint, VarInt};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: {} <server_addr> <path> [<path>...]", args[0]);
        std::process::exit(1);
    }

    let server_addr: SocketAddr = args[1].parse()?;
    let paths: Vec<&str> = args[2..].iter().map(String::as_str).collect();

    let addr: SocketAddr = "0.0.0.0:0".parse()?;
    let endpoint = Endpoint::client(addr)?;

    let mut client_config = ClientConfig::new(vec![b"hq-interop".to_vec(), b"hq-29".to_vec()]);
    client_config.verify(false);
    endpoint.set_default_client_config(client_config);

    println!("Connecting to {server_addr}...");
    let conn = endpoint.connect(server_addr, "localhost")?.await?;
    println!("Connected (stable_id={})", conn.stable_id());

    // Request all paths concurrently.
    let mut handles = Vec::new();
    for path in paths {
        let conn = conn.clone();
        let path = path.to_string();
        handles.push(tokio::spawn(async move {
            let (mut send, mut recv) = conn.open_bi().await?;

            // Send the request.
            send.write_all(path.as_bytes()).await?;
            send.finish()?;

            // Read the response.
            let response = recv.read_to_end(10 * 1024 * 1024).await?;

            println!("--- {} ({} bytes) ---", path, response.len());
            if response.len() <= 1024 {
                println!("{}", String::from_utf8_lossy(&response));
            } else {
                println!(
                    "{}... (truncated)",
                    String::from_utf8_lossy(&response[..512])
                );
            }

            Ok::<_, anyhow::Error>(())
        }));
    }

    for handle in handles {
        handle.await??;
    }

    conn.close(VarInt::from(0u32), b"done");
    endpoint.wait_idle().await;

    Ok(())
}
