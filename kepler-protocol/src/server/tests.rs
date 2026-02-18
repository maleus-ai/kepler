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
