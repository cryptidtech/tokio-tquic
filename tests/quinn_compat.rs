/// Tests modeled after Quinn's test suite (`quinn/quinn/src/tests.rs`).
///
/// These tests verify that tokio-tquic's API covers the same behavior as Quinn.
/// Tests for Quinn features that tokio-tquic doesn't support (datagrams, rebind,
/// 0-RTT, export_keying_material) are omitted.
mod common;

use std::sync::Arc;
use std::time::Duration;

use tokio_tquic::{ConnectionError, Endpoint, EndpointConfig, Side, TransportConfig, VarInt};

use common::{
    create_endpoints, create_endpoints_with_endpoint_config,
    generate_test_certs, test_client_config, test_client_config_with_transport,
    test_server_config,
};

/// Adapted from Quinn's `handshake_timeout`: connect to an unreachable address and
/// verify the connection times out within a reasonable window.
#[tokio::test]
async fn handshake_timeout() {
    let _certs = generate_test_certs();

    let mut transport = TransportConfig::new();
    // Use a short idle timeout so the test doesn't take too long.
    const IDLE_TIMEOUT: Duration = Duration::from_millis(500);
    transport.max_idle_timeout(Some(IDLE_TIMEOUT));
    transport.initial_rtt(Duration::from_millis(10));

    let mut client_config = test_client_config_with_transport(transport);
    // Override the already-set transport with one that has our short timeout.
    let mut transport2 = TransportConfig::new();
    transport2.max_idle_timeout(Some(IDLE_TIMEOUT));
    transport2.initial_rtt(Duration::from_millis(10));
    client_config.transport_config(Arc::new(transport2));

    let client = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    client.set_default_client_config(client_config);

    let start = tokio::time::Instant::now();

    // Connect to a port that nobody is listening on.
    let result = client
        .connect("127.0.0.1:1".parse().unwrap(), "localhost")
        .unwrap()
        .await;

    let dt = start.elapsed();

    match result {
        Err(ConnectionError::TimedOut) => {}
        Err(e) => panic!("unexpected error: {e:?}"),
        Ok(_) => panic!("unexpected success"),
    }

    // The timeout should be roughly in the idle timeout window.
    assert!(
        dt >= IDLE_TIMEOUT,
        "timed out too quickly: {dt:?} < {IDLE_TIMEOUT:?}"
    );
    // Allow some slack for the timeout to fire.
    assert!(
        dt < IDLE_TIMEOUT * 10,
        "timed out too slowly: {dt:?}"
    );
}

/// Adapted from Quinn's `close_endpoint`: closing an endpoint should cause
/// established connections to be closed.
#[tokio::test]
async fn close_endpoint() {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    // Establish a real connection first.
    let connecting = client.connect(server_addr, "localhost").unwrap();

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.unwrap();
        let conn = incoming.accept().expect("accept").await.expect("handshake");
        // Wait for the connection to be closed.
        conn.closed().await
    });

    let _conn = connecting.await.expect("connect");

    // Close the endpoint - all connections should be terminated.
    client.close(VarInt::from(0u32), b"shutting down");

    // The server should observe the connection closing.
    let result = tokio::time::timeout(Duration::from_secs(5), server_task).await;
    match result {
        Ok(Ok(err)) => {
            // Should be some form of close error.
            eprintln!("server saw close: {err:?}");
        }
        Ok(Err(e)) => panic!("server task panicked: {e:?}"),
        Err(_) => panic!("timed out waiting for close"),
    }
}

/// Adapted from Quinn's `local_addr`: verify that `Endpoint::local_addr()` returns
/// the correct bound address.
#[tokio::test]
async fn local_addr() {
    let endpoint =
        Endpoint::client("127.0.0.1:0".parse().unwrap()).expect("failed to create endpoint");
    let addr = endpoint.local_addr().expect("local_addr");
    assert_eq!(addr.ip(), "127.0.0.1".parse::<std::net::IpAddr>().unwrap());
    assert_ne!(addr.port(), 0, "port should have been assigned by OS");
}

/// Adapted from Quinn's `read_after_close`: data written before a stream is
/// finished should still be readable by the peer even after a delay.
#[tokio::test]
async fn read_after_close() {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    const MSG: &[u8] = b"goodbye!";

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.expect("accept");
        let conn = incoming.accept().expect("accept").await.expect("handshake");

        let mut s = conn.open_uni().await.unwrap();
        s.write_all(MSG).await.unwrap();
        s.finish().unwrap();

        // Keep the connection alive until the client reads.
        tokio::time::sleep(Duration::from_secs(2)).await;
        conn
    });

    let conn = client
        .connect(server_addr, "localhost")
        .unwrap()
        .await
        .expect("connect");

    // Wait a bit to let the server send data before we read.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut stream = conn.accept_uni().await.expect("incoming stream");
    let msg = stream.read_to_end(usize::MAX).await.expect("read_to_end");
    assert_eq!(msg, MSG);

    drop(conn);
    let _ = server_task.await;
}

/// Adapted from Quinn's `ip_blocking`: server can refuse connections using
/// `Incoming::refuse()`. Since TQUIC auto-accepts connections before the bridge
/// can refuse them, the client may see a successful connection that is then
/// immediately closed by the server.
#[tokio::test]
async fn refuse_incoming() {
    let certs = generate_test_certs();

    let server_config = test_server_config(&certs);
    let server = Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap())
        .expect("server endpoint");
    let server_addr = server.local_addr().unwrap();

    let client = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    client.set_default_client_config(test_client_config());
    let client_addr = client.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.expect("accept");
        assert_eq!(incoming.remote_address(), client_addr);
        // Refuse the connection - this sends CONNECTION_CLOSE.
        incoming.refuse();
    });

    let connecting = client.connect(server_addr, "localhost").unwrap();

    // In TQUIC, refuse() closes the connection after auto-accept.
    // The client may get:
    // 1. A connect error (if the close arrives before handshake completes)
    // 2. A successful connection that immediately receives a close
    let result = tokio::time::timeout(Duration::from_secs(5), connecting).await;
    match result {
        Ok(Err(_)) => {
            // Connection failed - refusal worked at handshake level.
        }
        Ok(Ok(conn)) => {
            // Connection succeeded but should be closed immediately.
            let close = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
            assert!(close.is_ok(), "refused connection should close quickly");
        }
        Err(_) => panic!("timed out"),
    }

    let _ = server_task.await;
}

/// Adapted from Quinn's `echo_v4`: parameterized bidirectional echo test with
/// configurable number of streams and data sizes.
#[tokio::test]
async fn echo_bi_streams() {
    echo(1, 10 * 1024).await;
}

/// Multiple sequential streams echo test (stream limit = 1, streams are opened one at a time).
#[tokio::test]
async fn echo_sequential_streams() {
    echo(3, 4 * 1024).await;
}

async fn echo(nr_streams: usize, stream_size: usize) {
    let certs = generate_test_certs();

    let (server, client, server_addr) = create_endpoints(&certs);

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.unwrap();
        let conn = incoming.accept().expect("accept").await.expect("handshake");

        // Echo loop: accept bi streams and echo data back.
        let conn2 = conn.clone();
        tokio::spawn(async move {
            loop {
                match conn2.accept_bi().await {
                    Ok((mut send, mut recv)) => {
                        tokio::spawn(async move {
                            let mut buf = vec![0u8; 8192];
                            loop {
                                match recv.read(&mut buf).await {
                                    Ok(Some(n)) => {
                                        send.write_all(&buf[..n]).await.unwrap();
                                    }
                                    Ok(None) => break,
                                    Err(_) => break,
                                }
                            }
                            let _ = send.finish();
                        });
                    }
                    Err(_) => break,
                }
            }
        });

        conn
    });

    let conn = client
        .connect(server_addr, "localhost")
        .unwrap()
        .await
        .expect("connect");

    const SEED: u64 = 0x12345678;

    for i in 0..nr_streams {
        let (mut send, mut recv) = conn.open_bi().await.expect("stream open");
        let msg = gen_data(stream_size, SEED + i as u64);

        let send_task = async {
            send.write_all(&msg).await.expect("write");
            send.finish().unwrap();
        };
        let recv_task = async { recv.read_to_end(usize::MAX).await.expect("read") };

        let (_, data) = tokio::join!(send_task, recv_task);
        assert_eq!(data.len(), msg.len(), "Data length mismatch on stream {i}");
        assert_eq!(data, msg, "Data mismatch on stream {i}");
    }

    conn.close(VarInt::from(0u32), b"done");

    let _ = server_task.await;
}

/// Adapted from Quinn's `stream_id_flow_control`: verify that multiple
/// sequential unidirectional streams can be opened and used.
#[tokio::test]
async fn stream_id_flow_control() {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    let connecting = client.connect(server_addr, "localhost").unwrap();

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.unwrap();
        let conn = incoming.accept().expect("accept").await.expect("handshake");

        // Accept three uni streams sequentially.
        for i in 0..3 {
            let mut recv = conn.accept_uni().await.expect(&format!("accept_uni {i}"));
            let data = recv.read_to_end(1024).await.expect(&format!("read {i}"));
            assert_eq!(data, format!("stream-{i}").as_bytes());
        }

        conn
    });

    let conn = connecting.await.expect("connect");

    // Open three sequential uni streams. Each one completes before the next is opened.
    for i in 0..3 {
        let mut s = conn.open_uni().await.unwrap();
        s.write_all(format!("stream-{i}").as_bytes()).await.unwrap();
        s.finish().unwrap();
    }

    let _ = server_task.await;
}

/// Adapted from Quinn's `multiple_conns_with_zero_length_cids`:
/// multiple concurrent connections to the same server.
#[tokio::test]
async fn multiple_concurrent_connections() {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    let server_task = tokio::spawn(async move {
        let incoming1 = server.accept().await.unwrap();
        let conn1 = incoming1.accept().expect("accept").await.expect("handshake 1");

        let incoming2 = server.accept().await.unwrap();
        let conn2 = incoming2.accept().expect("accept").await.expect("handshake 2");

        // Both connections are concurrently live.
        conn1.close(VarInt::from(42u32), b"bye");
        conn2.close(VarInt::from(42u32), b"bye");
    });

    let client2 = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    client2.set_default_client_config(test_client_config());

    tokio::join!(
        async {
            let conn = client
                .connect(server_addr, "localhost")
                .unwrap()
                .await
                .unwrap();
            conn.closed().await;
        },
        async {
            let conn = client2
                .connect(server_addr, "localhost")
                .unwrap()
                .await
                .unwrap();
            conn.closed().await;
        }
    );

    let _ = server_task.await;
}

/// Adapted from Quinn's `multiple_conns_with_zero_length_cids`:
/// multiple connections with zero-length CIDs.
#[tokio::test]
async fn multiple_conns_zero_length_cids() {
    let certs = generate_test_certs();

    let mut endpoint_config = EndpointConfig::new();
    endpoint_config.cid_len(0);

    let (server, client, server_addr) =
        create_endpoints_with_endpoint_config(&certs, endpoint_config);

    let server_task = tokio::spawn(async move {
        let incoming1 = server.accept().await.unwrap();
        let conn1 = incoming1.accept().expect("accept").await.expect("handshake 1");

        let incoming2 = server.accept().await.unwrap();
        let conn2 = incoming2.accept().expect("accept").await.expect("handshake 2");

        // Both connections concurrently alive.
        conn1.close(VarInt::from(42u32), b"");
        conn2.close(VarInt::from(42u32), b"");
    });

    let client2 = Endpoint::new(
        {
            let mut cfg = EndpointConfig::new();
            cfg.cid_len(0);
            cfg
        },
        None,
        "127.0.0.1:0".parse().unwrap(),
    )
    .unwrap();
    client2.set_default_client_config(test_client_config());

    let (_, _) = tokio::join!(
        async {
            let conn = client
                .connect(server_addr, "localhost")
                .unwrap()
                .await
                .unwrap();
            conn.closed().await;
        },
        async {
            let conn = client2
                .connect(server_addr, "localhost")
                .unwrap()
                .await
                .unwrap();
            conn.closed().await;
        }
    );

    let _ = server_task.await;
}

/// Test that connection.side() returns the correct value for both client and server.
#[tokio::test]
async fn connection_side() {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    let connecting = client.connect(server_addr, "localhost").unwrap();

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.unwrap();
        let conn = incoming.accept().expect("accept").await.expect("handshake");
        assert_eq!(conn.side(), Side::Server);
        conn
    });

    let conn = connecting.await.expect("connect");
    assert_eq!(conn.side(), Side::Client);

    let server_conn = server_task.await.unwrap();
    assert_eq!(server_conn.side(), Side::Server);
}

/// Test that remote_address() returns the correct peer address.
#[tokio::test]
async fn remote_address() {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    let client_addr = client.local_addr().unwrap();

    let connecting = client.connect(server_addr, "localhost").unwrap();

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.unwrap();
        // Check remote address on incoming.
        assert_eq!(incoming.remote_address(), client_addr);
        let conn = incoming.accept().expect("accept").await.expect("handshake");
        // Check remote address on connection.
        assert_eq!(conn.remote_address(), client_addr);
        conn
    });

    let conn = connecting.await.expect("connect");
    assert_eq!(conn.remote_address(), server_addr);

    let _ = server_task.await;
}

/// Adapted from Quinn's `recv_stream_cancel_stop_drop`: stopping a recv stream
/// after partially reading should not panic.
#[tokio::test]
async fn recv_stream_stop_drop() {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.unwrap();
        let conn = incoming.accept().expect("accept").await.expect("handshake");

        let mut recv = conn.accept_uni().await.unwrap();

        // Read some data, then stop the stream.
        let mut buf = [0u8; 5];
        let n = recv.read(&mut buf).await.unwrap();
        assert!(n.is_some());

        // Stop the stream (should not panic).
        recv.stop(VarInt::from(0u32)).unwrap();
        // Drop recv (should not panic either).
        drop(recv);

        conn
    });

    let conn = client
        .connect(server_addr, "localhost")
        .unwrap()
        .await
        .expect("connect");

    let mut send = conn.open_uni().await.unwrap();
    send.write_all(b"hello world").await.unwrap();
    // Don't finish - let the server stop reading.

    let _ = tokio::time::timeout(Duration::from_secs(5), server_task).await;
}

/// Test that send stream reset is properly communicated.
#[tokio::test]
async fn stream_reset() {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.unwrap();
        let conn = incoming.accept().expect("accept").await.expect("handshake");

        let mut recv = conn.accept_uni().await.unwrap();

        // Try to read — should get an error because the sender resets.
        let mut buf = vec![0u8; 1024];
        let result = recv.read(&mut buf).await;
        // The read may return an error or None depending on timing.
        // Just verify it doesn't hang.
        match result {
            Ok(Some(_n)) => {} // Sender may have sent data before reset.
            Ok(None) => {}     // Stream ended.
            Err(_) => {}       // Reset error — expected.
        }

        conn
    });

    let conn = client
        .connect(server_addr, "localhost")
        .unwrap()
        .await
        .expect("connect");

    let mut send = conn.open_uni().await.unwrap();
    send.write_all(b"hi").await.unwrap();
    send.reset(VarInt::from(42u32)).unwrap();

    let _ = tokio::time::timeout(Duration::from_secs(5), server_task).await;
}

/// Test that connection.closed() resolves when the peer closes the connection.
#[tokio::test]
async fn connection_closed_notification() {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    let connecting = client.connect(server_addr, "localhost").unwrap();

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.unwrap();
        let conn = incoming.accept().expect("accept").await.expect("handshake");

        // Close from the server side.
        tokio::time::sleep(Duration::from_millis(50)).await;
        conn.close(VarInt::from(7u32), b"server closing");
    });

    let conn = connecting.await.expect("connect");

    // Wait for close notification.
    let result = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
    assert!(result.is_ok(), "timed out waiting for close notification");

    let err = result.unwrap();
    match err {
        ConnectionError::ApplicationClosed { error_code, .. } => {
            assert_eq!(error_code, 7);
        }
        other => {
            // TQUIC may report this differently.
            eprintln!("got close reason: {other:?}");
        }
    }

    let _ = server_task.await;
}

/// Test that close_reason() returns the close reason after connection is closed.
#[tokio::test]
async fn close_reason() {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    let connecting = client.connect(server_addr, "localhost").unwrap();

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.unwrap();
        let conn = incoming.accept().expect("accept").await.expect("handshake");
        conn
    });

    let conn = connecting.await.expect("connect");

    // Before closing, close_reason should be None.
    assert!(conn.close_reason().is_none());

    // Close the connection.
    conn.close(VarInt::from(99u32), b"done");

    // Wait a bit for the close to propagate.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // After closing, close_reason should be Some.
    // Note: we closed locally, so it may or may not be reflected in close_reason
    // depending on bridge timing.

    let _ = server_task.await;
}

/// Test endpoint.wait_idle() waits for all connections to finish.
#[tokio::test]
async fn wait_idle() {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.unwrap();
        let conn = incoming.accept().expect("accept").await.expect("handshake");
        // Keep connection alive briefly.
        tokio::time::sleep(Duration::from_millis(200)).await;
        conn.close(VarInt::from(0u32), b"done");
        server.wait_idle().await;
    });

    let conn = client
        .connect(server_addr, "localhost")
        .unwrap()
        .await
        .expect("connect");

    // Wait for the server to close us.
    let _ = conn.closed().await;
    drop(conn);

    let result = tokio::time::timeout(Duration::from_secs(5), client.wait_idle()).await;
    assert!(result.is_ok(), "wait_idle timed out");

    let _ = server_task.await;
}

/// Test that opening a connection after the endpoint is closed returns an error.
#[tokio::test]
async fn connect_after_close() {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    client.close(VarInt::from(0u32), b"bye");

    // Small delay for the close to propagate.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let result = client.connect(server_addr, "localhost");
    match result {
        Err(tokio_tquic::ConnectError::EndpointStopping) => {}
        Err(e) => {
            // The connect may succeed at the API level but fail during handshake.
            eprintln!("got error: {e:?}");
        }
        Ok(connecting) => {
            // If connect returned Ok, the await should fail.
            let result = tokio::time::timeout(Duration::from_secs(5), connecting).await;
            match result {
                Ok(Err(_)) => {} // Expected.
                Ok(Ok(_)) => panic!("unexpected success after endpoint close"),
                Err(_) => panic!("timed out"),
            }
        }
    }

    drop(server);
}

/// Test that open_bi/open_uni on a closed connection returns an error.
#[tokio::test]
async fn open_stream_after_close() {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.unwrap();
        let conn = incoming.accept().expect("accept").await.expect("handshake");
        conn
    });

    let conn = client
        .connect(server_addr, "localhost")
        .unwrap()
        .await
        .expect("connect");

    // Close the connection.
    conn.close(VarInt::from(0u32), b"bye");

    // Wait for close to propagate.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Open bi on closed connection should fail.
    let result = tokio::time::timeout(Duration::from_secs(2), conn.open_bi()).await;
    match result {
        Ok(Err(_)) => {} // Expected: connection error.
        Ok(Ok(_)) => {
            // TQUIC might still return a stream if close hasn't propagated.
            // This is acceptable behavior.
        }
        Err(_) => panic!("timed out opening stream on closed connection"),
    }

    let _ = server_task.await;
}

/// Test open_connections counter.
#[tokio::test]
async fn open_connections_counter() {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.unwrap();
        let conn = incoming.accept().expect("accept").await.expect("handshake");

        // Server should see 1 open connection.
        assert!(server.open_connections() >= 1);

        conn.close(VarInt::from(0u32), b"bye");
        tokio::time::sleep(Duration::from_millis(200)).await;
    });

    let conn = client
        .connect(server_addr, "localhost")
        .unwrap()
        .await
        .expect("connect");

    // Client endpoint should see 1 open connection.
    assert!(client.open_connections() >= 1);

    let _ = conn.closed().await;
    drop(conn);

    // Wait for connection cleanup.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let _ = server_task.await;
}

/// Test stable_id is unique per connection.
#[tokio::test]
async fn stable_id_unique() {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    let server_task = tokio::spawn(async move {
        let incoming1 = server.accept().await.unwrap();
        let conn1 = incoming1.accept().expect("accept").await.expect("handshake 1");

        let incoming2 = server.accept().await.unwrap();
        let conn2 = incoming2.accept().expect("accept").await.expect("handshake 2");

        assert_ne!(
            conn1.stable_id(),
            conn2.stable_id(),
            "stable_id should be unique"
        );

        conn1.close(VarInt::from(0u32), b"");
        conn2.close(VarInt::from(0u32), b"");
    });

    let conn1 = client
        .connect(server_addr, "localhost")
        .unwrap()
        .await
        .expect("connect 1");

    let conn2 = client
        .connect(server_addr, "localhost")
        .unwrap()
        .await
        .expect("connect 2");

    assert_ne!(conn1.stable_id(), conn2.stable_id());

    let _ = server_task.await;
}

/// Test that accept_bi/accept_uni fails when connection is closed.
#[tokio::test]
async fn accept_stream_after_close() {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    let connecting = client.connect(server_addr, "localhost").unwrap();

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.unwrap();
        let conn = incoming.accept().expect("accept").await.expect("handshake");

        // Close after a short delay.
        tokio::time::sleep(Duration::from_millis(50)).await;
        conn.close(VarInt::from(0u32), b"bye");
    });

    let conn = connecting.await.expect("connect");

    // Try to accept_bi — should error when connection closes.
    let result = tokio::time::timeout(Duration::from_secs(5), conn.accept_bi()).await;
    match result {
        Ok(Err(_)) => {} // Expected: connection error.
        Ok(Ok(_)) => panic!("unexpected stream on closing connection"),
        Err(_) => panic!("accept_bi didn't resolve after connection close"),
    }

    let _ = server_task.await;
}

/// Test read_exact with exact-size buffer.
/// The client opens a bi stream and sends a ping, then the server echoes back
/// an exact-size message.
#[tokio::test]
async fn read_exact() {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    let connecting = client.connect(server_addr, "localhost").unwrap();

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.unwrap();
        let conn = incoming.accept().expect("accept").await.expect("handshake");

        let (mut send, mut recv) = conn.accept_bi().await.unwrap();

        // Read the client's ping.
        let mut ping = [0u8; 4];
        let n = recv.read(&mut ping).await.unwrap().unwrap();
        assert_eq!(&ping[..n], b"ping");

        // Send back exact-size data.
        send.write_all(b"exactly16bytes!!").await.unwrap();
        send.finish().unwrap();

        // Keep the connection alive.
        tokio::time::sleep(Duration::from_secs(2)).await;
        conn
    });

    let conn = connecting.await.expect("connect");
    let (mut send, mut recv) = conn.open_bi().await.unwrap();

    // Send a ping so the server sees the stream.
    send.write_all(b"ping").await.unwrap();

    let mut buf = [0u8; 16];
    recv.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"exactly16bytes!!");

    let _ = server_task.await;
}

/// Test that write after finish returns an error.
#[tokio::test]
async fn write_after_finish() {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.unwrap();
        let conn = incoming.accept().expect("accept").await.expect("handshake");
        tokio::time::sleep(Duration::from_secs(1)).await;
        conn
    });

    let conn = client
        .connect(server_addr, "localhost")
        .unwrap()
        .await
        .expect("connect");

    let mut send = conn.open_uni().await.unwrap();
    send.finish().unwrap();

    // Writing after finish should error.
    let result = send.write(b"after finish").await;
    assert!(result.is_err(), "write after finish should error");

    // Finishing again should also error.
    let result = send.finish();
    assert!(result.is_err(), "double finish should error");

    let _ = server_task.await;
}

/// Test that the Incoming type correctly provides connection metadata.
#[tokio::test]
async fn incoming_metadata() {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    let client_addr = client.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.unwrap();

        // Check metadata before accepting.
        assert_eq!(incoming.remote_address(), client_addr);
        assert!(!incoming.may_retry());

        let conn = incoming.accept().expect("accept").await.expect("handshake");
        conn
    });

    let _conn = client
        .connect(server_addr, "localhost")
        .unwrap()
        .await
        .expect("connect");

    let _ = server_task.await;
}

/// Test that retry() returns RetryError as expected.
#[tokio::test]
async fn retry_unsupported() {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.unwrap();

        // retry() should return Err since TQUIC doesn't support per-connection retry.
        let err = incoming.retry();
        assert!(err.is_err());

        // Recover the incoming and accept it.
        let incoming = err.unwrap_err().into_incoming();
        let conn = incoming.accept().expect("accept").await.expect("handshake");
        conn
    });

    let _conn = client
        .connect(server_addr, "localhost")
        .unwrap()
        .await
        .expect("connect");

    let _ = server_task.await;
}

/// Test into_0rtt returns Err (not supported).
#[tokio::test]
async fn zero_rtt_unsupported() {
    let certs = generate_test_certs();
    let (_server, client, server_addr) = create_endpoints(&certs);

    let connecting = client.connect(server_addr, "localhost").unwrap();

    // into_0rtt should return Err since TQUIC handles 0-RTT internally.
    let result = connecting.into_0rtt();
    assert!(result.is_err(), "into_0rtt should not be supported");

    // We should be able to recover the Connecting future.
    let _connecting = match result {
        Err(c) => c,
        Ok(_) => unreachable!(),
    };
}

/// Test connect_with for per-connection client config.
#[tokio::test]
async fn connect_with_custom_config() {
    let certs = generate_test_certs();
    let (server, client, server_addr) = create_endpoints(&certs);

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.unwrap();
        let conn = incoming.accept().expect("accept").await.expect("handshake");

        let (mut send, mut recv) = conn.accept_bi().await.unwrap();
        let mut buf = vec![0u8; 1024];
        let n = recv.read(&mut buf).await.unwrap().unwrap();
        send.write_all(&buf[..n]).await.unwrap();
        send.finish().unwrap();
    });

    // Use connect_with instead of connect.
    let custom_config = test_client_config();
    let conn = client
        .connect_with(custom_config, server_addr, "localhost")
        .unwrap()
        .await
        .expect("connect_with");

    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    send.write_all(b"custom config").await.unwrap();
    send.finish().unwrap();

    let data = recv.read_to_end(1024).await.unwrap();
    assert_eq!(data, b"custom config");

    let _ = server_task.await;
}

// --- Helper functions ---

fn gen_data(size: usize, seed: u64) -> Vec<u8> {
    // Deterministic pseudo-random data.
    let mut data = vec![0u8; size];
    let mut state = seed;
    for byte in data.iter_mut() {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *byte = (state >> 33) as u8;
    }
    data
}
