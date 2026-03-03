//! HTTP/0.9 file server example.
//!
//! Serves files from a directory over QUIC using a simple request/response protocol.
//! The client sends a path (e.g., "/index.html") on a bidirectional stream,
//! and the server responds with the file contents.
//!
//! Usage:
//!   cargo run --example server -- --cert <cert_file> --key <key_file> [--root <dir>] [--listen <addr>]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tokio_tquic::{Endpoint, ServerConfig};

struct Options {
    cert_file: String,
    key_file: String,
    root: PathBuf,
    listen: SocketAddr,
}

fn parse_args() -> Result<Options> {
    let args: Vec<String> = std::env::args().collect();
    let mut cert_file = None;
    let mut key_file = None;
    let mut root = PathBuf::from(".");
    let mut listen: SocketAddr = "0.0.0.0:4433".parse()?;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--cert" => {
                i += 1;
                cert_file = Some(args[i].clone());
            }
            "--key" => {
                i += 1;
                key_file = Some(args[i].clone());
            }
            "--root" => {
                i += 1;
                root = PathBuf::from(&args[i]);
            }
            "--listen" => {
                i += 1;
                listen = args[i].parse()?;
            }
            _ => anyhow::bail!("Unknown argument: {}", args[i]),
        }
        i += 1;
    }

    let cert_file = cert_file.ok_or_else(|| anyhow::anyhow!("--cert is required"))?;
    let key_file = key_file.ok_or_else(|| anyhow::anyhow!("--key is required"))?;

    Ok(Options {
        cert_file,
        key_file,
        root,
        listen,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let opts = parse_args()?;
    let root = Arc::new(opts.root);

    let server_config = ServerConfig::new(
        opts.cert_file,
        opts.key_file,
        vec![b"hq-interop".to_vec(), b"hq-29".to_vec()],
    );

    let endpoint = Endpoint::server(server_config, opts.listen)?;
    println!("Serving files from {:?}", root);
    println!("Listening on {}", endpoint.local_addr()?);

    while let Some(incoming) = endpoint.accept().await {
        let remote = incoming.remote_address();
        println!("[{remote}] New connection");

        let conn = match incoming.accept() {
            Ok(connecting) => match connecting.await {
                Ok(conn) => conn,
                Err(e) => {
                    eprintln!("[{remote}] Handshake failed: {e}");
                    continue;
                }
            },
            Err(e) => {
                eprintln!("[{remote}] Accept failed: {e}");
                continue;
            }
        };

        let root = root.clone();
        tokio::spawn(async move {
            loop {
                match conn.accept_bi().await {
                    Ok((mut send, mut recv)) => {
                        let root = root.clone();
                        tokio::spawn(async move {
                            // Read the request path.
                            let data = match recv.read_to_end(4096).await {
                                Ok(data) => data,
                                Err(e) => {
                                    eprintln!("[{remote}] Read error: {e}");
                                    return;
                                }
                            };

                            let request = String::from_utf8_lossy(&data);
                            let path = request.trim();
                            println!("[{remote}] GET {path}");

                            // Resolve the file path (strip leading /).
                            let file_path = root.join(path.trim_start_matches('/'));

                            match tokio::fs::read(&file_path).await {
                                Ok(contents) => {
                                    if let Err(e) = send.write_all(&contents).await {
                                        eprintln!("[{remote}] Write error: {e}");
                                    }
                                }
                                Err(e) => {
                                    let msg = format!("404 Not Found: {e}");
                                    let _ = send.write_all(msg.as_bytes()).await;
                                }
                            }

                            let _ = send.finish();
                        });
                    }
                    Err(_) => {
                        println!("[{remote}] Connection closed");
                        break;
                    }
                }
            }
        });
    }

    Ok(())
}
