use super::*;

#[tokio::test]
async fn test_event_channel() {
    let (tx, mut rx) = service_event_channel();

    // Send an event
    let msg = ServiceEventMessage::new(ServiceEvent::Start);
    tx.send(msg.clone()).await.unwrap();

    // Receive the event
    let received = rx.recv().await.unwrap();
    assert_eq!(received.event, ServiceEvent::Start);
}

#[test]
fn test_event_as_str() {
    assert_eq!(ServiceEvent::Init.as_str(), "init");
    assert_eq!(ServiceEvent::Start.as_str(), "start");
    assert_eq!(
        ServiceEvent::Restart {
            reason: RestartReason::Manual
        }
        .as_str(),
        "restart"
    );
    assert_eq!(ServiceEvent::Exit { code: Some(0) }.as_str(), "exit");
    assert_eq!(ServiceEvent::Stop.as_str(), "stop");
    assert_eq!(ServiceEvent::Cleanup.as_str(), "cleanup");
    assert_eq!(
        ServiceEvent::Healthcheck {
            status: HealthStatus::Success
        }
        .as_str(),
        "healthcheck"
    );
    assert_eq!(ServiceEvent::Healthy.as_str(), "healthy");
    assert_eq!(ServiceEvent::Unhealthy.as_str(), "unhealthy");
}

#[test]
fn test_restart_reason() {
    let watch = RestartReason::Watch;
    let failure = RestartReason::Failure { exit_code: Some(1) };
    let manual = RestartReason::Manual;
    let dep = RestartReason::DependencyRestart {
        dependency: "service-a".to_string(),
    };

    // Just verify they can be created and compared
    assert_eq!(watch, RestartReason::Watch);
    assert_ne!(failure, manual);
    assert_eq!(
        dep,
        RestartReason::DependencyRestart {
            dependency: "service-a".to_string()
        }
    );
}
