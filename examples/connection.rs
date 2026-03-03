//! Minimal example: client connects to server, sends a message, server echoes it back.
//!
//! Usage:
//!   cargo run --example connection -- server <cert_file> <key_file>
//!   cargo run --example connection -- client <server_addr>

use std::net::SocketAddr;

use anyhow::Result;
use tokio_tquic::{ClientConfig, Endpoint, ServerConfig, VarInt};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage:");
        eprintln!("  {} server <cert_file> <key_file>", args[0]);
        eprintln!("  {} client <server_addr>", args[0]);
        std::process::exit(1);
    }

    match args[1].as_str() {
        "server" => {
            if args.len() < 4 {
                eprintln!("Usage: {} server <cert_file> <key_file>", args[0]);
                std::process::exit(1);
            }
            run_server(&args[2], &args[3]).await
        }
        "client" => {
            if args.len() < 3 {
                eprintln!("Usage: {} client <server_addr>", args[0]);
                std::process::exit(1);
            }
            run_client(&args[2]).await
        }
        other => {
            eprintln!("Unknown mode: {other}");
            std::process::exit(1);
        }
    }
}

async fn run_server(cert_file: &str, key_file: &str) -> Result<()> {
    let server_config = ServerConfig::new(
        cert_file.to_string(),
        key_file.to_string(),
        vec![b"hq-interop".to_vec()],
    );

    let addr: SocketAddr = "0.0.0.0:4433".parse()?;
    let endpoint = Endpoint::server(server_config, addr)?;
    println!("Server listening on {}", endpoint.local_addr()?);

    while let Some(incoming) = endpoint.accept().await {
        println!("New connection from {}", incoming.remote_address());
        let conn = incoming.accept()?.await?;
        println!("Connection established (stable_id={})", conn.stable_id());

        tokio::spawn(async move {
            loop {
                match conn.accept_bi().await {
                    Ok((mut send, mut recv)) => {
                        tokio::spawn(async move {
                            let mut buf = vec![0u8; 4096];
                            match recv.read(&mut buf).await {
                                Ok(Some(n)) => {
                                    println!("Received {} bytes", n);
                                    if let Err(e) = send.write_all(&buf[..n]).await {
                                        eprintln!("Write error: {e}");
                                    }
                                    let _ = send.finish();
                                }
                                Ok(None) => println!("Stream finished (no data)"),
                                Err(e) => eprintln!("Read error: {e}"),
                            }
                        });
                    }
                    Err(e) => {
                        println!("Connection closed: {e}");
                        break;
                    }
                }
            }
        });
    }

    Ok(())
}

async fn run_client(server_addr: &str) -> Result<()> {
    let addr: SocketAddr = "0.0.0.0:0".parse()?;
    let endpoint = Endpoint::client(addr)?;

    let mut client_config = ClientConfig::new(vec![b"hq-interop".to_vec()]);
    client_config.verify(false);
    endpoint.set_default_client_config(client_config);

    let server_addr: SocketAddr = server_addr.parse()?;
    println!("Connecting to {server_addr}...");

    let conn = endpoint.connect(server_addr, "localhost")?.await?;
    println!("Connected (stable_id={})", conn.stable_id());

    let (mut send, mut recv) = conn.open_bi().await?;

    let message = b"Hello from tokio-tquic!";
    send.write_all(message).await?;
    send.finish()?;
    println!("Sent: {}", String::from_utf8_lossy(message));

    let response = recv.read_to_end(4096).await?;
    println!("Received: {}", String::from_utf8_lossy(&response));

    conn.close(VarInt::from(0u32), b"done");
    endpoint.wait_idle().await;

    Ok(())
}
