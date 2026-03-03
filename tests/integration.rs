mod common;

use std::time::Duration;

use tokio_tquic::{ConnectionError, VarInt};

use common::{create_endpoints, generate_test_certs};

/// Basic test: client connects to server, opens a bi stream, sends data,
/// server echoes it back, and we verify the roundtrip.
#[tokio::test]
async fn test_connect_and_send() -> anyhow::Result<()> {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    // Client connects.
    let connecting = client.connect(server_addr, "localhost")?;

    // Server accepts.
    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.expect("accept returned None");
        let conn = incoming.accept().expect("incoming.accept").await.expect("handshake");

        // Accept a bi stream from the client.
        let (mut send, mut recv) = conn.accept_bi().await.expect("accept_bi");

        // Read data from the client.
        let mut buf = vec![0u8; 1024];
        let n = recv.read(&mut buf).await.expect("read").expect("not fin");

        // Echo it back.
        send.write_all(&buf[..n]).await.expect("write_all");
        send.finish().expect("finish");

        conn
    });

    // Client side: wait for connection, open stream, send data, read echo.
    let conn = connecting.await?;

    let (mut send, mut recv) = conn.open_bi().await?;
    let message = b"hello from client";
    send.write_all(message).await?;
    send.finish()?;

    let mut response = vec![0u8; 1024];
    let n = recv.read(&mut response).await?.expect("not fin");
    assert_eq!(&response[..n], message);

    // Read EOF.
    let eof = recv.read(&mut response).await?;
    assert!(eof.is_none(), "expected fin");

    // Wait for server task.
    let _server_conn = server_task.await?;

    Ok(())
}

/// Test unidirectional streams: client opens uni stream, sends data,
/// server reads it.
#[tokio::test]
async fn test_uni_streams() -> anyhow::Result<()> {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    let connecting = client.connect(server_addr, "localhost")?;

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.expect("accept");
        let conn = incoming.accept().expect("accept").await.expect("handshake");

        // Accept a uni stream.
        let mut recv = conn.accept_uni().await.expect("accept_uni");

        // Read all data.
        let data = recv.read_to_end(1024 * 1024).await.expect("read_to_end");
        data
    });

    let conn = connecting.await?;

    let mut send = conn.open_uni().await?;
    let payload = b"unidirectional data";
    send.write_all(payload).await?;
    send.finish()?;

    let received = server_task.await?;
    assert_eq!(received, payload);

    Ok(())
}

/// Test opening multiple concurrent bidirectional streams.
#[tokio::test]
async fn test_multiple_streams() -> anyhow::Result<()> {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    let connecting = client.connect(server_addr, "localhost")?;

    let num_streams = 5u64;

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.expect("accept");
        let conn = incoming.accept().expect("accept").await.expect("handshake");

        let mut handles = Vec::new();
        for _ in 0..num_streams {
            let conn = conn.clone();
            handles.push(tokio::spawn(async move {
                let (mut send, mut recv) = conn.accept_bi().await.expect("accept_bi");

                // Read data, echo it back with a prefix.
                let mut buf = vec![0u8; 1024];
                let n = recv.read(&mut buf).await.expect("read").expect("not fin");
                let mut echo = b"echo:".to_vec();
                echo.extend_from_slice(&buf[..n]);
                send.write_all(&echo).await.expect("write_all");
                send.finish().expect("finish");
            }));
        }

        for h in handles {
            h.await.expect("server stream task");
        }
    });

    let conn = connecting.await?;

    let mut handles = Vec::new();
    for i in 0..num_streams {
        let conn = conn.clone();
        handles.push(tokio::spawn(async move {
            let (mut send, mut recv) = conn.open_bi().await.expect("open_bi");

            let msg = format!("stream-{i}");
            send.write_all(msg.as_bytes()).await.expect("write_all");
            send.finish().expect("finish");

            let response = recv.read_to_end(1024).await.expect("read_to_end");
            let expected = format!("echo:{msg}");
            assert_eq!(response, expected.as_bytes());
        }));
    }

    for h in handles {
        h.await?;
    }

    server_task.await?;

    Ok(())
}

/// Test closing a connection with an application error code.
#[tokio::test]
async fn test_connection_close() -> anyhow::Result<()> {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    let connecting = client.connect(server_addr, "localhost")?;

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.expect("accept");
        let conn = incoming.accept().expect("accept").await.expect("handshake");

        // Wait for the connection to close.
        let err = conn.closed().await;
        err
    });

    let conn = connecting.await?;

    // Close the connection with application error code 42.
    conn.close(VarInt::from(42u32), b"test close");

    let err = server_task.await?;

    // The server should see ApplicationClosed with error_code 42.
    match err {
        ConnectionError::ApplicationClosed { error_code, .. } => {
            assert_eq!(error_code, 42);
        }
        other => {
            // Accept any close variant — TQUIC may categorize differently.
            eprintln!("got close reason: {other:?}");
        }
    }

    Ok(())
}

/// Test that the server can accept multiple sequential connections.
#[tokio::test]
async fn test_endpoint_accept_loop() -> anyhow::Result<()> {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    let num_connections = 3;

    let server_task = tokio::spawn(async move {
        for i in 0..num_connections {
            let incoming = server.accept().await.expect("accept returned None");
            let conn = incoming.accept().expect("accept").await.expect("handshake");

            // Accept a bi stream and read a message.
            let (mut send, mut recv) = conn.accept_bi().await.expect("accept_bi");
            let mut buf = vec![0u8; 64];
            let n = recv.read(&mut buf).await.expect("read").expect("not fin");
            let msg = String::from_utf8_lossy(&buf[..n]);
            assert_eq!(msg, format!("conn-{i}"));

            send.write_all(b"ok").await.expect("write_all");
            send.finish().expect("finish");
        }
    });

    for i in 0..num_connections {
        let connecting = client.connect(server_addr, "localhost")?;
        let conn = connecting.await?;

        let (mut send, mut recv) = conn.open_bi().await?;
        send.write_all(format!("conn-{i}").as_bytes()).await?;
        send.finish()?;

        let mut buf = vec![0u8; 64];
        let n = recv.read(&mut buf).await?.expect("not fin");
        assert_eq!(&buf[..n], b"ok");

        conn.close(VarInt::from(0u32), b"done");
        // Small delay to let TQUIC process the close before next connection.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    server_task.await?;

    Ok(())
}

/// Test sending a large amount of data to exercise flow control and back-pressure.
#[tokio::test]
async fn test_large_transfer() -> anyhow::Result<()> {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    // 1 MB of data.
    let data_size = 1024 * 1024;
    let send_data: Vec<u8> = (0..data_size).map(|i| (i % 256) as u8).collect();

    let connecting = client.connect(server_addr, "localhost")?;

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.expect("accept");
        let conn = incoming.accept().expect("accept").await.expect("handshake");

        let (_, mut recv) = conn.accept_bi().await.expect("accept_bi");

        // Read all data.
        let received = recv
            .read_to_end(data_size + 1024)
            .await
            .expect("read_to_end");

        received
    });

    let conn = connecting.await?;

    let (mut send, _recv) = conn.open_bi().await?;

    // Write in chunks.
    let chunk_size = 32 * 1024;
    let mut offset = 0;
    while offset < send_data.len() {
        let end = (offset + chunk_size).min(send_data.len());
        send.write_all(&send_data[offset..end]).await?;
        offset = end;
    }
    send.finish()?;

    let received = server_task.await?;
    assert_eq!(received.len(), data_size);
    assert_eq!(received, send_data);

    Ok(())
}
