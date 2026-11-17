use super::*;
use crate::protocol::{ServicePhase, decode_server_message};

#[tokio::test]
async fn progress_sender_encodes_event() {
    let (tx, mut rx) = mpsc::channel(16);
    let sender = ProgressSender::new(tx, 42);

    sender.send(ProgressEvent {
        service: "web".into(),
        phase: ServicePhase::Starting,
    }).await;

    let bytes = rx.recv().await.unwrap();
    // Should be a valid length-prefixed server message
    assert!(bytes.len() > 4);
    let len = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    assert_eq!(len, bytes.len() - 4);

    let decoded = decode_server_message(&bytes[4..]).unwrap();
    match decoded {
        ServerMessage::Event { event: ServerEvent::Progress { request_id, event } } => {
            assert_eq!(request_id, 42);
            assert_eq!(event.service, "web");
            assert!(matches!(event.phase, ServicePhase::Starting));
        }
        _ => panic!("Expected Progress event"),
    }
}

#[tokio::test]
async fn progress_sender_ignores_closed_channel() {
    let (tx, rx) = mpsc::channel(1);
    let sender = ProgressSender::new(tx, 1);
    drop(rx); // Close receiver

    // Should not panic
    sender.send(ProgressEvent {
        service: "web".into(),
        phase: ServicePhase::Starting,
    }).await;
}

/// A handler that requests shutdown must still deliver its response, even though
/// the daemon drops its runtime — aborting every connection task — as soon as
/// `run` returns.
#[test]
fn shutdown_response_reaches_client_before_run_returns() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("kepler.sock");

    let server_socket = socket.clone();
    let server_thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let server = Server::new(server_socket, |_request, shutdown_tx: ShutdownTx, _progress, _peer| async move {
                let _ = shutdown_tx.send(()).await;
                // Work done between the shutdown request and the response widens
                // the window in which an early shutdown loses the response.
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                Response::ok_with_message("Daemon shutting down")
            })
            .unwrap();
            server.run().await.unwrap();
        });
    });

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let response = runtime.block_on(async {
        let client = loop {
            if let Ok(client) = crate::client::Client::connect(&socket).await {
                break client;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        };
        let (_events, response) = client.shutdown().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), response).await.unwrap()
    });
    server_thread.join().unwrap();

    assert!(matches!(response, Ok(Response::Ok { .. })), "shutdown response lost: {:?}", response);
}
