use super::*;

#[test]
fn test_exited_status_as_str() {
    assert_eq!(ServiceStatus::Exited.as_str(), "exited");
}

#[test]
fn test_exited_is_not_running() {
    assert!(!ServiceStatus::Exited.is_running());
}

#[test]
fn test_exited_display() {
    assert_eq!(format!("{}", ServiceStatus::Exited), "exited");
}

#[test]
fn test_killed_status_as_str() {
    assert_eq!(ServiceStatus::Killed.as_str(), "killed");
}

#[test]
fn test_killed_is_not_running() {
    assert!(!ServiceStatus::Killed.is_running());
}

#[test]
fn test_killed_display() {
    assert_eq!(format!("{}", ServiceStatus::Killed), "killed");
}

#[test]
fn test_persisted_status_roundtrip_killed() {
    let persisted: PersistedServiceStatus = ServiceStatus::Killed.into();
    assert_eq!(persisted, PersistedServiceStatus::Killed);
    let back: ServiceStatus = persisted.into();
    assert_eq!(back, ServiceStatus::Killed);
}

#[test]
fn test_persisted_status_roundtrip_exited() {
    let persisted: PersistedServiceStatus = ServiceStatus::Exited.into();
    assert_eq!(persisted, PersistedServiceStatus::Exited);
    let back: ServiceStatus = persisted.into();
    assert_eq!(back, ServiceStatus::Exited);
}

#[test]
fn test_persisted_status_all_variants_roundtrip() {
    let variants = [
        ServiceStatus::Stopped,
        ServiceStatus::Waiting,
        ServiceStatus::Starting,
        ServiceStatus::Running,
        ServiceStatus::Stopping,
        ServiceStatus::Failed,
        ServiceStatus::Healthy,
        ServiceStatus::Unhealthy,
        ServiceStatus::Exited,
        ServiceStatus::Killed,
        ServiceStatus::Skipped,
    ];
    for status in variants {
        let persisted: PersistedServiceStatus = status.into();
        let back: ServiceStatus = persisted.into();
        assert_eq!(back, status, "round-trip failed for {status:?}");
    }
}

#[test]
fn test_persisted_state_stopped_at_and_signal_roundtrip() {
    let now = Utc::now();
    let state = ServiceState {
        status: ServiceStatus::Exited,
        pid: None,
        started_at: None,
        stopped_at: Some(now),
        exit_code: Some(0),
        signal: None,
        health_check_failures: 0,
        restart_count: 1,
        initialized: true,
        computed_env: HashMap::new(),
        working_dir: PathBuf::from("/tmp"),
        was_healthy: false,
        skip_reason: None,
        fail_reason: None,
    };

    let persisted = PersistedServiceState::from(&state);
    assert_eq!(persisted.status, PersistedServiceStatus::Exited);
    assert_eq!(persisted.stopped_at, Some(now.timestamp()));
    assert_eq!(persisted.exit_code, Some(0));
    assert!(persisted.signal.is_none());

    let restored = persisted.to_service_state(HashMap::new(), PathBuf::from("/tmp"));
    assert_eq!(restored.status, ServiceStatus::Exited);
    assert!(restored.stopped_at.is_some());
    assert_eq!(restored.exit_code, Some(0));
    assert!(restored.signal.is_none());
}

#[test]
fn test_persisted_state_signal_roundtrip() {
    let now = Utc::now();
    let state = ServiceState {
        status: ServiceStatus::Killed,
        pid: None,
        started_at: None,
        stopped_at: Some(now),
        exit_code: None,
        signal: Some(9),
        health_check_failures: 0,
        restart_count: 0,
        initialized: true,
        computed_env: HashMap::new(),
        working_dir: PathBuf::new(),
        was_healthy: false,
        skip_reason: None,
        fail_reason: None,
    };

    let persisted = PersistedServiceState::from(&state);
    assert_eq!(persisted.signal, Some(9));
    assert!(persisted.exit_code.is_none());

    let restored = persisted.to_service_state(HashMap::new(), PathBuf::new());
    assert_eq!(restored.signal, Some(9));
    assert!(restored.exit_code.is_none());
    assert_eq!(restored.status, ServiceStatus::Killed);
}

#[test]
fn test_persisted_state_json_backward_compat() {
    // Old JSON without stopped_at or signal should deserialize with defaults
    let json = r#"{
            "status": "stopped",
            "pid": null,
            "started_at": null,
            "exit_code": null,
            "health_check_failures": 0,
            "restart_count": 0,
            "initialized": false,
            "was_healthy": false
        }"#;
    let persisted: PersistedServiceState = serde_json::from_str(json).unwrap();
    assert!(persisted.stopped_at.is_none());
    assert!(persisted.signal.is_none());
    assert_eq!(persisted.status, PersistedServiceStatus::Stopped);
}

#[test]
fn test_persisted_state_json_with_new_fields() {
    let json = r#"{
            "status": "exited",
            "pid": null,
            "started_at": null,
            "stopped_at": 1700000000,
            "exit_code": 0,
            "signal": null,
            "health_check_failures": 0,
            "restart_count": 1,
            "initialized": true,
            "was_healthy": false
        }"#;
    let persisted: PersistedServiceState = serde_json::from_str(json).unwrap();
    assert_eq!(persisted.status, PersistedServiceStatus::Exited);
    assert_eq!(persisted.stopped_at, Some(1700000000));
    assert_eq!(persisted.exit_code, Some(0));
    assert!(persisted.signal.is_none());
}

#[test]
fn test_persisted_state_json_killed_with_signal() {
    let json = r#"{
            "status": "killed",
            "pid": null,
            "started_at": null,
            "stopped_at": 1700000000,
            "exit_code": null,
            "signal": 9,
            "health_check_failures": 0,
            "restart_count": 0,
            "initialized": true,
            "was_healthy": true
        }"#;
    let persisted: PersistedServiceState = serde_json::from_str(json).unwrap();
    assert_eq!(persisted.status, PersistedServiceStatus::Killed);
    assert_eq!(persisted.signal, Some(9));
    assert!(persisted.exit_code.is_none());
    assert!(persisted.was_healthy);
}

#[test]
fn test_service_info_from_exited_state() {
    let now = Utc::now();
    let state = ServiceState {
        status: ServiceStatus::Exited,
        pid: None,
        started_at: None,
        stopped_at: Some(now),
        exit_code: Some(0),
        signal: None,
        health_check_failures: 0,
        restart_count: 0,
        initialized: true,
        computed_env: HashMap::new(),
        working_dir: PathBuf::new(),
        was_healthy: false,
        skip_reason: None,
        fail_reason: None,
    };

    let info = state.to_service_info();
    assert_eq!(info.status, "exited");
    assert!(info.pid.is_none());
    assert_eq!(info.stopped_at, Some(now.timestamp()));
    assert_eq!(info.exit_code, Some(0));
    assert!(info.signal.is_none());
}

#[test]
fn test_service_info_from_signal_killed_state() {
    let now = Utc::now();
    let state = ServiceState {
        status: ServiceStatus::Killed,
        pid: None,
        started_at: None,
        stopped_at: Some(now),
        exit_code: None,
        signal: Some(15),
        health_check_failures: 0,
        restart_count: 0,
        initialized: true,
        computed_env: HashMap::new(),
        working_dir: PathBuf::new(),
        was_healthy: false,
        skip_reason: None,
        fail_reason: None,
    };

    let info = state.to_service_info();
    assert_eq!(info.status, "killed");
    assert_eq!(info.signal, Some(15));
    assert_eq!(info.stopped_at, Some(now.timestamp()));
    assert!(info.exit_code.is_none());
}

#[test]
fn test_is_active_terminal_states() {
    assert!(!ServiceStatus::Stopped.is_active());
    assert!(!ServiceStatus::Failed.is_active());
    assert!(!ServiceStatus::Exited.is_active());
    assert!(!ServiceStatus::Killed.is_active());
    assert!(!ServiceStatus::Skipped.is_active());
}

#[test]
fn test_is_active_non_terminal_states() {
    assert!(ServiceStatus::Waiting.is_active());
    assert!(ServiceStatus::Starting.is_active());
    assert!(ServiceStatus::Running.is_active());
    assert!(ServiceStatus::Stopping.is_active());
    assert!(ServiceStatus::Healthy.is_active());
    assert!(ServiceStatus::Unhealthy.is_active());
}

#[test]
fn test_default_service_state() {
    let state = ServiceState::default();
    assert_eq!(state.status, ServiceStatus::Stopped);
    assert!(state.stopped_at.is_none());
    assert!(state.signal.is_none());
    assert!(state.exit_code.is_none());
    assert!(state.started_at.is_none());
    assert!(state.pid.is_none());
}
