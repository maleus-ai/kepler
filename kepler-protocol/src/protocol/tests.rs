use super::*;
use std::sync::Arc;

// ========================================================================
// RequestEnvelope roundtrip tests
// ========================================================================

#[test]
fn roundtrip_envelope_ping() {
    let envelope = RequestEnvelope { id: 1, request: Request::Ping, token: None };
    let bytes = encode_envelope(&envelope).unwrap();
    // Strip 4-byte length prefix
    let decoded = decode_envelope(&bytes[4..]).unwrap();
    assert_eq!(decoded.id, 1);
    assert!(matches!(decoded.request, Request::Ping));
}

#[test]
fn roundtrip_envelope_shutdown() {
    let envelope = RequestEnvelope { id: 42, request: Request::Shutdown, token: None };
    let bytes = encode_envelope(&envelope).unwrap();
    let decoded = decode_envelope(&bytes[4..]).unwrap();
    assert_eq!(decoded.id, 42);
    assert!(matches!(decoded.request, Request::Shutdown));
}

#[test]
fn roundtrip_envelope_start() {
    let envelope = RequestEnvelope {
        id: 7,
        token: None,
        request: Request::Start {
            config_path: PathBuf::from("/tmp/test.yaml"),
            services: vec!["web".into()],
            sys_env: Some(HashMap::from([("PATH".into(), "/usr/bin".into())])),
            no_deps: true,
            override_envs: None,
            hardening: None,

            follow: false,
        },
    };
    let bytes = encode_envelope(&envelope).unwrap();
    let decoded = decode_envelope(&bytes[4..]).unwrap();
    assert_eq!(decoded.id, 7);
    match decoded.request {
        Request::Start { config_path, services, sys_env, no_deps, .. } => {
            assert_eq!(config_path, PathBuf::from("/tmp/test.yaml"));
            assert_eq!(services, vec!["web".to_string()]);
            assert!(sys_env.is_some());
            assert_eq!(sys_env.unwrap().get("PATH").unwrap(), "/usr/bin");
            assert!(no_deps);
        }
        _ => panic!("Expected Start request"),
    }
}

#[test]
fn roundtrip_envelope_stop() {
    let envelope = RequestEnvelope {
        id: 3,
        token: None,
        request: Request::Stop {
            config_path: PathBuf::from("/etc/kepler.yaml"),
            services: vec![],
            clean: true,
            signal: Some("SIGKILL".into()),
        },
    };
    let bytes = encode_envelope(&envelope).unwrap();
    let decoded = decode_envelope(&bytes[4..]).unwrap();
    assert_eq!(decoded.id, 3);
    match decoded.request {
        Request::Stop { config_path, services, clean, signal } => {
            assert_eq!(config_path, PathBuf::from("/etc/kepler.yaml"));
            assert!(services.is_empty());
            assert!(clean);
            assert_eq!(signal, Some("SIGKILL".into()));
        }
        _ => panic!("Expected Stop request"),
    }
}

#[test]
fn roundtrip_envelope_stop_multiple_services() {
    let envelope = RequestEnvelope {
        id: 4,
        token: None,
        request: Request::Stop {
            config_path: PathBuf::from("/etc/kepler.yaml"),
            services: vec!["web".into(), "worker".into()],
            clean: false,
            signal: None,
        },
    };
    let bytes = encode_envelope(&envelope).unwrap();
    let decoded = decode_envelope(&bytes[4..]).unwrap();
    assert_eq!(decoded.id, 4);
    match decoded.request {
        Request::Stop { config_path, services, clean, signal } => {
            assert_eq!(config_path, PathBuf::from("/etc/kepler.yaml"));
            assert_eq!(services, vec!["web".to_string(), "worker".to_string()]);
            assert!(!clean);
            assert_eq!(signal, None);
        }
        _ => panic!("Expected Stop request"),
    }
}

#[test]
fn roundtrip_envelope_restart() {
    let envelope = RequestEnvelope {
        id: 5,
        token: None,
        request: Request::Restart {
            config_path: PathBuf::from("/app/kepler.yaml"),
            services: vec!["api".into(), "web".into()],
            sys_env: None,
            no_deps: true,
            override_envs: None,
        },
    };
    let bytes = encode_envelope(&envelope).unwrap();
    let decoded = decode_envelope(&bytes[4..]).unwrap();
    match decoded.request {
        Request::Restart { services, no_deps, .. } => {
            assert_eq!(services, vec!["api".to_string(), "web".to_string()]);
            assert!(no_deps);
        }
        _ => panic!("Expected Restart request"),
    }
}

#[test]
fn roundtrip_envelope_status() {
    let envelope = RequestEnvelope {
        id: 99,
        token: None,
        request: Request::Status { config_path: None },
    };
    let bytes = encode_envelope(&envelope).unwrap();
    let decoded = decode_envelope(&bytes[4..]).unwrap();
    assert_eq!(decoded.id, 99);
    match decoded.request {
        Request::Status { config_path } => assert!(config_path.is_none()),
        _ => panic!("Expected Status request"),
    }
}

#[test]
fn roundtrip_envelope_list_configs() {
    let envelope = RequestEnvelope { id: 100, request: Request::ListConfigs, token: None };
    let bytes = encode_envelope(&envelope).unwrap();
    let decoded = decode_envelope(&bytes[4..]).unwrap();
    assert!(matches!(decoded.request, Request::ListConfigs));
}

#[test]
fn roundtrip_envelope_prune() {
    let envelope = RequestEnvelope {
        id: 200,
        token: None,
        request: Request::Prune { force: true, dry_run: false },
    };
    let bytes = encode_envelope(&envelope).unwrap();
    let decoded = decode_envelope(&bytes[4..]).unwrap();
    match decoded.request {
        Request::Prune { force, dry_run } => {
            assert!(force);
            assert!(!dry_run);
        }
        _ => panic!("Expected Prune request"),
    }
}

#[test]
fn roundtrip_envelope_logs_stream() {
    let envelope = RequestEnvelope {
        id: 50,
        token: None,
        request: Request::LogsStream {
            config_path: PathBuf::from("/app/k.yaml"),
            services: vec![],
            after_id: Some(42),
            from_end: false,
            limit: 10000,
            no_hooks: false,
            filter: Some("level='err'".to_string()),
            sql: false,
            raw: false,
            tail: true,
            after_ts: None,
            before_ts: None,
        },
    };
    let bytes = encode_envelope(&envelope).unwrap();
    let decoded = decode_envelope(&bytes[4..]).unwrap();
    match decoded.request {
        Request::LogsStream { after_id, from_end, limit, no_hooks, tail, .. } => {
            assert_eq!(after_id, Some(42));
            assert!(!from_end);
            assert_eq!(limit, 10000);
            assert!(!no_hooks);
            assert!(tail);
        }
        _ => panic!("Expected LogsStream request"),
    }
}

// ========================================================================
// ServerMessage roundtrip tests
// ========================================================================

#[test]
fn roundtrip_server_message_ok_with_message() {
    let msg = ServerMessage::Response {
        id: 1,
        response: Response::ok_with_message("All services started"),
    };
    let bytes = encode_server_message(&msg).unwrap();
    let decoded = decode_server_message(&bytes[4..]).unwrap();
    match decoded {
        ServerMessage::Response { id, response: Response::Ok { message, data } } => {
            assert_eq!(id, 1);
            assert_eq!(message, Some("All services started".into()));
            assert!(data.is_none());
        }
        _ => panic!("Expected Ok response"),
    }
}

#[test]
fn roundtrip_server_message_error() {
    let msg = ServerMessage::Response {
        id: 5,
        response: Response::error("Service not found"),
    };
    let bytes = encode_server_message(&msg).unwrap();
    let decoded = decode_server_message(&bytes[4..]).unwrap();
    match decoded {
        ServerMessage::Response { id, response: Response::Error { message } } => {
            assert_eq!(id, 5);
            assert_eq!(message, "Service not found");
        }
        _ => panic!("Expected Error response"),
    }
}

#[test]
fn roundtrip_server_message_service_status() {
    let mut services = HashMap::new();
    services.insert("web".into(), ServiceInfo {
        status: "running".into(),
        pid: Some(1234),
        started_at: Some(1700000000),
        stopped_at: None,
        health_check_failures: 0,
        exit_code: None,
        signal: None,
        initialized: false,
        skip_reason: None,
        fail_reason: None,

    });
    services.insert("api".into(), ServiceInfo {
        status: "exited".into(),
        pid: None,
        started_at: Some(1700000000),
        stopped_at: Some(1700001000),
        health_check_failures: 0,
        exit_code: Some(0),
        signal: None,
        initialized: false,
        skip_reason: None,
        fail_reason: None,

    });
    services.insert("worker".into(), ServiceInfo {
        status: "killed".into(),
        pid: None,
        started_at: Some(1700000000),
        stopped_at: Some(1700001000),
        health_check_failures: 0,
        exit_code: None,
        signal: Some(9),
        initialized: false,
        skip_reason: None,
        fail_reason: None,

    });

    let msg = ServerMessage::Response {
        id: 10,
        response: Response::ok_with_data(ResponseData::ServiceStatus(services)),
    };
    let bytes = encode_server_message(&msg).unwrap();
    let decoded = decode_server_message(&bytes[4..]).unwrap();
    match decoded {
        ServerMessage::Response { id, response: Response::Ok { data: Some(ResponseData::ServiceStatus(s)), .. } } => {
            assert_eq!(id, 10);
            assert_eq!(s.len(), 3);
            assert_eq!(s["web"].pid, Some(1234));
            assert_eq!(s["api"].exit_code, Some(0));
            assert_eq!(s["worker"].signal, Some(9));
        }
        _ => panic!("Expected ServiceStatus response"),
    }
}

#[test]
fn roundtrip_server_message_progress_event() {
    let msg = ServerMessage::Event {
        event: ServerEvent::Progress {
            request_id: 42,
            event: ProgressEvent {
                service: "web".into(),
                phase: ServicePhase::Starting,
            },
        },
    };
    let bytes = encode_server_message(&msg).unwrap();
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

#[test]
fn roundtrip_stream_log_entry() {
    let data = LogStreamData {
        service_table: vec![Arc::from("web"), Arc::from("api")],
        entries: vec![
            StreamLogEntry {
                id: 0,
                service_id: 0,
                line: "hello from web".into(),
                timestamp: 1700000000000,
                level: Arc::from("info"),
                hook: None,
                attributes: None,
            },
            StreamLogEntry {
                id: 0,
                service_id: 1,
                line: "hello from api".into(),
                timestamp: 1700000001000,
                level: Arc::from("err"),
                hook: None,
                attributes: None,
            },
        ],
        last_id: 99,
        has_more: true,
    };
    let msg = ServerMessage::Response {
        id: 30,
        response: Response::ok_with_data(ResponseData::LogStream(data)),
    };
    let bytes = encode_server_message(&msg).unwrap();
    let decoded = decode_server_message(&bytes[4..]).unwrap();
    match decoded {
        ServerMessage::Response { response: Response::Ok { data: Some(ResponseData::LogStream(d)), .. }, .. } => {
            assert_eq!(d.service_table.len(), 2);
            assert_eq!(&*d.service_table[0], "web");
            assert_eq!(d.entries.len(), 2);
            assert_eq!(d.entries[0].service_id, 0);
            assert!(d.has_more);
            assert_eq!(d.last_id, 99);
        }
        _ => panic!("Expected LogStream response"),
    }
}

// ========================================================================
// Length prefix framing tests
// ========================================================================

#[test]
fn encode_envelope_includes_length_prefix() {
    let envelope = RequestEnvelope { id: 1, request: Request::Ping, token: None };
    let bytes = encode_envelope(&envelope).unwrap();
    assert!(bytes.len() > 4);
    let len = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    assert_eq!(len, bytes.len() - 4);
}

#[test]
fn encode_server_message_includes_length_prefix() {
    let msg = ServerMessage::Response {
        id: 1,
        response: Response::ok_with_message("ok"),
    };
    let bytes = encode_server_message(&msg).unwrap();
    assert!(bytes.len() > 4);
    let len = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    assert_eq!(len, bytes.len() - 4);
}

// ========================================================================
// Malformed input tests
// ========================================================================

#[test]
fn decode_envelope_random_bytes_fails() {
    let garbage = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03];
    let result = decode_envelope(&garbage);
    assert!(result.is_err());
}

#[test]
fn decode_server_message_random_bytes_fails() {
    let garbage = vec![0xFF, 0xFE, 0xFD, 0xFC, 0xFB, 0xFA];
    let result = decode_server_message(&garbage);
    assert!(result.is_err());
}

#[test]
fn decode_envelope_empty_payload_fails() {
    let result = decode_envelope(&[]);
    assert!(result.is_err());
}

#[test]
fn decode_server_message_empty_payload_fails() {
    let result = decode_server_message(&[]);
    assert!(result.is_err());
}

#[test]
fn decode_envelope_truncated_payload_fails() {
    let envelope = RequestEnvelope { id: 1, request: Request::Ping, token: None };
    let bytes = encode_envelope(&envelope).unwrap();
    // Take only half of the payload (skip length prefix)
    let payload = &bytes[4..];
    let half = &payload[..payload.len() / 2];
    let result = decode_envelope(half);
    assert!(result.is_err());
}

// ========================================================================
// Request variant name tests
// ========================================================================

#[test]
fn request_variant_names() {
    assert_eq!(Request::Ping.variant_name(), "Ping");
    assert_eq!(Request::Shutdown.variant_name(), "Shutdown");
    assert_eq!(Request::ListConfigs.variant_name(), "ListConfigs");
    assert_eq!(
        Request::Start {
            config_path: PathBuf::new(),
            services: vec![],
            sys_env: None,
            no_deps: false,
            override_envs: None,
            hardening: None,

            follow: false,
        }.variant_name(),
        "Start"
    );
}

// ========================================================================
// Request ID uniqueness test
// ========================================================================

#[test]
fn different_request_ids_produce_distinct_envelopes() {
    let env1 = RequestEnvelope { id: 1, request: Request::Ping, token: None };
    let env2 = RequestEnvelope { id: 2, request: Request::Ping, token: None };
    let bytes1 = encode_envelope(&env1).unwrap();
    let bytes2 = encode_envelope(&env2).unwrap();
    // Payloads should differ (different IDs)
    assert_ne!(bytes1, bytes2);
    let dec1 = decode_envelope(&bytes1[4..]).unwrap();
    let dec2 = decode_envelope(&bytes2[4..]).unwrap();
    assert_ne!(dec1.id, dec2.id);
}

// ========================================================================
// Response helper tests
// ========================================================================

#[test]
fn response_ok_with_message_helper() {
    match Response::ok_with_message("done") {
        Response::Ok { message, data } => {
            assert_eq!(message, Some("done".into()));
            assert!(data.is_none());
        }
        _ => panic!("Expected Ok"),
    }
}

#[test]
fn response_ok_with_data_helper() {
    let data = ResponseData::DaemonInfo(DaemonInfo {
        pid: 42,
        loaded_configs: 3,
        uptime_secs: 1000,
    });
    match Response::ok_with_data(data) {
        Response::Ok { message, data } => {
            assert!(message.is_none());
            assert!(data.is_some());
        }
        _ => panic!("Expected Ok"),
    }
}

#[test]
fn response_error_helper() {
    match Response::error("fail") {
        Response::Error { message } => assert_eq!(message, "fail"),
        _ => panic!("Expected Error"),
    }
}

// ========================================================================
// ServicePhase roundtrip (all variants)
// ========================================================================

#[test]
fn roundtrip_all_service_phases() {
    let phases = vec![
        ServicePhase::Pending { target: ServiceTarget::Started },
        ServicePhase::Pending { target: ServiceTarget::Healthy },
        ServicePhase::Waiting,
        ServicePhase::Starting,
        ServicePhase::Started,
        ServicePhase::Healthy,
        ServicePhase::Stopping,
        ServicePhase::Stopped,
        ServicePhase::Cleaning,
        ServicePhase::Cleaned,
        ServicePhase::Failed { message: "boom".into() },
        ServicePhase::HookStarted { hook: "pre_start".into() },
        ServicePhase::HookCompleted { hook: "pre_start".into() },
        ServicePhase::HookFailed { hook: "pre_start".into(), message: "Exit code: Some(127)".into() },
    ];

    for (i, phase) in phases.into_iter().enumerate() {
        let msg = ServerMessage::Event {
            event: ServerEvent::Progress {
                request_id: i as u64,
                event: ProgressEvent {
                    service: "svc".into(),
                    phase,
                },
            },
        };
        let bytes = encode_server_message(&msg).unwrap();
        let decoded = decode_server_message(&bytes[4..]).unwrap();
        assert!(matches!(decoded, ServerMessage::Event { .. }));
    }
}

// ========================================================================
// CheckQuiescence / CheckReadiness roundtrip tests
// ========================================================================

#[test]
fn roundtrip_envelope_check_quiescence() {
    let envelope = RequestEnvelope {
        id: 200,
        request: Request::CheckQuiescence { config_path: PathBuf::from("/test.yaml") },
        token: None,
    };
    let bytes = encode_envelope(&envelope).unwrap();
    let decoded = decode_envelope(&bytes[4..]).unwrap();
    assert_eq!(decoded.id, 200);
    assert!(matches!(decoded.request, Request::CheckQuiescence { .. }));
}

#[test]
fn roundtrip_envelope_check_readiness() {
    let envelope = RequestEnvelope {
        id: 201,
        request: Request::CheckReadiness { config_path: PathBuf::from("/test.yaml") },
        token: None,
    };
    let bytes = encode_envelope(&envelope).unwrap();
    let decoded = decode_envelope(&bytes[4..]).unwrap();
    assert_eq!(decoded.id, 201);
    assert!(matches!(decoded.request, Request::CheckReadiness { .. }));
}

#[test]
fn roundtrip_server_message_check_result() {
    for val in [true, false] {
        let msg = ServerMessage::Response {
            id: 50,
            response: Response::ok_with_data(ResponseData::CheckResult(val)),
        };
        let bytes = encode_server_message(&msg).unwrap();
        let decoded = decode_server_message(&bytes[4..]).unwrap();
        match decoded {
            ServerMessage::Response { id, response: Response::Ok { data: Some(ResponseData::CheckResult(v)), .. } } => {
                assert_eq!(id, 50);
                assert_eq!(v, val);
            }
            other => panic!("Unexpected: {:?}", other),
        }
    }
}

// ========================================================================
// Request::Run roundtrip tests
// ========================================================================

#[test]
fn roundtrip_envelope_run_basic() {
    let envelope = RequestEnvelope {
        id: 300,
        token: None,
        request: Request::Run {
            config_path: PathBuf::from("/tmp/run.yaml"),
            services: vec!["web".into()],
            sys_env: None,
            no_deps: false,
            override_envs: None,
            hardening: None,
            follow: true,
            start_clean: false,
        },
    };
    let bytes = encode_envelope(&envelope).unwrap();
    let decoded = decode_envelope(&bytes[4..]).unwrap();
    assert_eq!(decoded.id, 300);
    match decoded.request {
        Request::Run { config_path, services, sys_env, no_deps, override_envs, hardening, follow, start_clean } => {
            assert_eq!(config_path, PathBuf::from("/tmp/run.yaml"));
            assert_eq!(services, vec!["web".to_string()]);
            assert!(sys_env.is_none());
            assert!(!no_deps);
            assert!(override_envs.is_none());
            assert!(hardening.is_none());
            assert!(follow);
            assert!(!start_clean);
        }
        _ => panic!("Expected Run request"),
    }
}

#[test]
fn roundtrip_envelope_run_with_all_fields() {
    let mut envs = HashMap::new();
    envs.insert("K".to_string(), "V".to_string());
    let mut sys = HashMap::new();
    sys.insert("PATH".to_string(), "/usr/bin".to_string());
    let envelope = RequestEnvelope {
        id: 301,
        token: Some([0u8; 32]),
        request: Request::Run {
            config_path: PathBuf::from("/app/kepler.yaml"),
            services: vec!["api".into(), "worker".into()],
            sys_env: Some(sys),
            no_deps: true,
            override_envs: Some(envs),
            hardening: Some("strict".into()),
            follow: false,
            start_clean: true,
        },
    };
    let bytes = encode_envelope(&envelope).unwrap();
    let decoded = decode_envelope(&bytes[4..]).unwrap();
    assert_eq!(decoded.id, 301);
    match decoded.request {
        Request::Run { config_path, services, sys_env, no_deps, override_envs, hardening, follow, start_clean } => {
            assert_eq!(config_path, PathBuf::from("/app/kepler.yaml"));
            assert_eq!(services, vec!["api".to_string(), "worker".to_string()]);
            assert!(sys_env.is_some());
            assert_eq!(sys_env.unwrap().get("PATH").unwrap(), "/usr/bin");
            assert!(no_deps);
            assert!(override_envs.is_some());
            assert_eq!(override_envs.unwrap().get("K").unwrap(), "V");
            assert_eq!(hardening, Some("strict".into()));
            assert!(!follow);
            assert!(start_clean);
        }
        _ => panic!("Expected Run request"),
    }
}

#[test]
fn run_variant_name() {
    assert_eq!(
        Request::Run {
            config_path: PathBuf::new(),
            services: vec![],
            sys_env: None,
            no_deps: false,
            override_envs: None,
            hardening: None,
            follow: false,
            start_clean: false,
        }.variant_name(),
        "Run"
    );
}

#[test]
fn roundtrip_envelope_run_empty_services() {
    let envelope = RequestEnvelope {
        id: 302,
        token: None,
        request: Request::Run {
            config_path: PathBuf::from("/app/kepler.yaml"),
            services: vec![],
            sys_env: None,
            no_deps: false,
            override_envs: None,
            hardening: None,
            follow: false,
            start_clean: false,
        },
    };
    let bytes = encode_envelope(&envelope).unwrap();
    let decoded = decode_envelope(&bytes[4..]).unwrap();
    assert_eq!(decoded.id, 302);
    match decoded.request {
        Request::Run { services, start_clean, .. } => {
            assert!(services.is_empty(), "Empty services should roundtrip");
            assert!(!start_clean);
        }
        _ => panic!("Expected Run request"),
    }
}
