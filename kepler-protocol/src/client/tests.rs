use super::*;
use crate::protocol::{
    Response, ServerMessage, decode_envelope, encode_server_message,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;

/// Helper: spin up a mock server that reads one envelope and replies.
async fn mock_server_one_shot(
    listener: UnixListener,
    make_response: impl FnOnce(u64) -> ServerMessage + Send + 'static,
) {
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();

        // Read length prefix + payload
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await.unwrap();
        let msg_len = u32::from_be_bytes(len_buf) as usize;
        let mut payload = vec![0u8; msg_len];
        stream.read_exact(&mut payload).await.unwrap();

        let envelope = decode_envelope(&payload).unwrap();

        let resp = make_response(envelope.id);
        let bytes = encode_server_message(&resp).unwrap();
        stream.write_all(&bytes).await.unwrap();
        stream.shutdown().await.unwrap();
    });
}

#[tokio::test]
async fn request_id_monotonically_increases() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("test.sock");

    let listener = UnixListener::bind(&sock).unwrap();

    // Mock server: accept, read 3 envelopes, reply to each
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut ids = Vec::new();

        for _ in 0..3 {
            let mut len_buf = [0u8; 4];
            stream.read_exact(&mut len_buf).await.unwrap();
            let msg_len = u32::from_be_bytes(len_buf) as usize;
            let mut payload = vec![0u8; msg_len];
            stream.read_exact(&mut payload).await.unwrap();

            let envelope = decode_envelope(&payload).unwrap();
            ids.push(envelope.id);

            let resp = ServerMessage::Response {
                id: envelope.id,
                response: Response::ok_with_message("pong"),
            };
            let bytes = encode_server_message(&resp).unwrap();
            stream.write_all(&bytes).await.unwrap();
        }

        // Verify IDs are monotonically increasing
        assert_eq!(ids, vec![1, 2, 3]);
    });

    let client = Client::connect(&sock).await.unwrap();
    let (_, fut) = client.send_request(Request::Ping).unwrap();
    let _ = fut.await.unwrap();
    let (_, fut) = client.send_request(Request::Ping).unwrap();
    let _ = fut.await.unwrap();
    let (_, fut) = client.send_request(Request::Ping).unwrap();
    let _ = fut.await.unwrap();
}

#[tokio::test]
async fn send_request_returns_matching_response() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("test.sock");

    let listener = UnixListener::bind(&sock).unwrap();
    mock_server_one_shot(listener, |id| ServerMessage::Response {
        id,
        response: Response::ok_with_message("hello from server"),
    })
    .await;

    // Give server time to spawn
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let client = Client::connect(&sock).await.unwrap();
    let (_, fut) = client.send_request(Request::Ping).unwrap();
    let response = fut.await.unwrap();

    match response {
        Response::Ok { message, .. } => {
            assert_eq!(message, Some("hello from server".into()));
        }
        _ => panic!("Expected Ok response"),
    }
}

#[tokio::test]
async fn disconnection_detected_for_inflight_request() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("test.sock");

    let listener = UnixListener::bind(&sock).unwrap();

    // Server accepts, reads the request, then disconnects WITHOUT replying
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();

        // Read the request but don't reply
        let mut len_buf = [0u8; 4];
        let _ = stream.read_exact(&mut len_buf).await;
        let msg_len = u32::from_be_bytes(len_buf) as usize;
        let mut payload = vec![0u8; msg_len];
        let _ = stream.read_exact(&mut payload).await;

        // Close without replying — the client's pending request should get Disconnected
        drop(stream);
    });

    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    let client = Client::connect(&sock).await.unwrap();

    // The request is sent, the server reads it and disconnects
    // The client reader detects EOF, clears pending, dropping the oneshot sender
    // which causes the awaiting rx to return Err → Disconnected
    let (_, fut) = client.send_request(Request::Ping).unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        fut,
    ).await;

    match result {
        Ok(Err(ClientError::Disconnected)) => {} // Expected
        Ok(Err(e)) => panic!("Expected Disconnected, got: {}", e),
        Ok(Ok(_)) => panic!("Expected error, got Ok response"),
        Err(_) => panic!("Timed out waiting for disconnect detection"),
    }
}

#[tokio::test]
async fn connect_to_nonexistent_socket_fails() {
    let result = Client::connect(Path::new("/tmp/nonexistent_kepler_test_sock_12345")).await;
    assert!(result.is_err());
    match result {
        Err(ClientError::Connect(_)) => {} // expected
        Err(e) => panic!("Expected Connect error, got: {}", e),
        Ok(_) => panic!("Expected error, got Ok"),
    }
}

#[tokio::test]
async fn is_daemon_running_returns_false_for_missing_socket() {
    let running = Client::is_daemon_running(Path::new("/tmp/nonexistent_kepler_test_12345")).await;
    assert!(!running);
}
