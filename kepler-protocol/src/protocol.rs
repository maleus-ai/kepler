use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::errors::ProtocolError;

/// Maximum message size (10MB) — local Unix socket, no network concerns
pub const MAX_MESSAGE_SIZE: usize = 10 * 1024 * 1024;

/// Maximum lines for one-shot queries (head/tail)
pub const MAX_LINES_ONE_SHOT: usize = 10_000;

/// Maximum entries per cursor batch (secondary cap; byte budget is the primary limit)
pub const MAX_CURSOR_BATCH_SIZE: usize = 50_000;

/// Stream type for log entries (stdout/stderr)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamType {
    Stdout,
    Stderr,
}

impl StreamType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
    pub fn is_stderr(&self) -> bool {
        matches!(self, Self::Stderr)
    }
}

impl std::fmt::Display for StreamType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Log reading mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LogMode {
    /// Read all logs chronologically using forward iterator (default)
    #[default]
    All,
    /// Return the last N lines (newest entries, in chronological order)
    Tail,
    /// Return the first N lines (oldest first)
    Head,
}

/// Request sent from CLI to daemon
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    /// Start service(s) - runs full pipeline synchronously, responds when complete
    Start {
        /// Path to the config file
        config_path: PathBuf,
        /// Service name (None = all services)
        service: Option<String>,
        /// System environment variables captured from CLI
        #[serde(default)]
        sys_env: Option<HashMap<String, String>>,
    },
    /// Stop service(s)
    Stop {
        /// Path to the config file
        config_path: PathBuf,
        /// Service name (None = all services)
        service: Option<String>,
        /// Whether to cleanup everything after stopping processes
        clean: bool,
        /// Signal to send (e.g., "SIGKILL", "TERM", "9"). Default: SIGTERM
        #[serde(default)]
        signal: Option<String>,
    },
    /// Restart service(s) - runs full pipeline synchronously, responds when complete
    Restart {
        /// Path to the config file
        config_path: PathBuf,
        /// Services to restart (empty = all running services)
        #[serde(default)]
        services: Vec<String>,
        /// System environment variables (unused, kept for API compatibility)
        #[serde(default)]
        sys_env: Option<HashMap<String, String>>,
    },
    /// Recreate config - re-bake config snapshot (no start/stop)
    Recreate {
        /// Path to the config file
        config_path: PathBuf,
        /// System environment variables captured from CLI (for re-baking config snapshot)
        #[serde(default)]
        sys_env: Option<HashMap<String, String>>,
    },
    /// Get status of services
    Status {
        /// Path to the config file (None = all configs)
        config_path: Option<PathBuf>,
    },
    /// Get logs
    Logs {
        /// Path to the config file
        config_path: PathBuf,
        /// Service name (None = all services)
        service: Option<String>,
        /// Whether to follow (stream) logs
        follow: bool,
        /// Number of lines to retrieve
        lines: usize,
        /// Maximum bytes to read (prevents OOM with large logs)
        #[serde(default)]
        max_bytes: Option<usize>,
        /// Log reading mode (head or tail)
        #[serde(default)]
        mode: LogMode,
        /// Whether to exclude hook log entries
        #[serde(default)]
        no_hooks: bool,
    },
    /// Get logs with pagination (for large log responses)
    LogsChunk {
        /// Path to the config file
        config_path: PathBuf,
        /// Service name (None = all services)
        service: Option<String>,
        /// Offset for pagination
        offset: usize,
        /// Maximum number of entries to return
        limit: usize,
        /// Whether to exclude hook log entries
        #[serde(default)]
        no_hooks: bool,
    },
    /// Shutdown the daemon
    Shutdown,
    /// Ping to check if daemon is alive
    Ping,
    /// List all loaded configs
    ListConfigs,
    /// Unload a config (stops all its services)
    UnloadConfig {
        /// Path to the config file
        config_path: PathBuf,
    },
    /// Prune all stopped/orphaned config state directories
    Prune {
        /// Force prune even if services appear running
        force: bool,
        /// Show what would be pruned without deleting
        dry_run: bool,
    },
    /// Cursor-based log streaming (for 'all' and 'follow' modes)
    LogsCursor {
        /// Path to the config file
        config_path: PathBuf,
        /// Service name (None = all services)
        service: Option<String>,
        /// Cursor ID from previous response (None = create new cursor)
        cursor_id: Option<String>,
        /// If true, start cursor at beginning of files (for 'all' mode)
        /// If false, start cursor at end of files (for 'follow' mode)
        from_start: bool,
        /// Whether to exclude hook log entries
        #[serde(default)]
        no_hooks: bool,
    },
    /// Subscribe to service state change events
    Subscribe {
        /// Path to the config file
        config_path: PathBuf,
        /// Services to watch (None = all)
        services: Option<Vec<String>>,
    },
}

impl Request {
    /// Return the variant name as a static string (for lightweight error reporting)
    pub fn variant_name(&self) -> &'static str {
        match self {
            Request::Start { .. } => "Start",
            Request::Stop { .. } => "Stop",
            Request::Restart { .. } => "Restart",
            Request::Recreate { .. } => "Recreate",
            Request::Status { .. } => "Status",
            Request::Logs { .. } => "Logs",
            Request::LogsChunk { .. } => "LogsChunk",
            Request::Shutdown => "Shutdown",
            Request::Ping => "Ping",
            Request::ListConfigs => "ListConfigs",
            Request::UnloadConfig { .. } => "UnloadConfig",
            Request::Prune { .. } => "Prune",
            Request::LogsCursor { .. } => "LogsCursor",
            Request::Subscribe { .. } => "Subscribe",
        }
    }
}

/// Response sent from daemon to CLI
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    /// Successful response
    Ok {
        /// Optional message
        message: Option<String>,
        /// Optional data payload
        data: Option<ResponseData>,
    },
    /// Error response
    Error {
        /// Error message
        message: String,
    },
}

impl Response {
    /// Create a success response with a message
    pub fn ok_with_message(msg: impl Into<String>) -> Self {
        Response::Ok {
            message: Some(msg.into()),
            data: None,
        }
    }

    /// Create a success response with data
    pub fn ok_with_data(data: ResponseData) -> Self {
        Response::Ok {
            message: None,
            data: Some(data),
        }
    }

    /// Create an error response
    pub fn error(msg: impl Into<String>) -> Self {
        Response::Error {
            message: msg.into(),
        }
    }
}

/// Data payload in response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResponseData {
    /// Service status information for a single config
    ServiceStatus(HashMap<String, ServiceInfo>),
    /// Service status information for all configs
    MultiConfigStatus(Vec<ConfigStatus>),
    /// List of loaded configs
    ConfigList(Vec<LoadedConfigInfo>),
    /// Log entries
    Logs(Vec<LogEntry>),
    /// Log entries chunk (for large log responses)
    LogChunk(LogChunkData),
    /// Log entries for cursor-based streaming (all/follow modes)
    LogCursor(LogCursorData),
    /// Daemon info
    DaemonInfo(DaemonInfo),
    /// Pruned configs info
    PrunedConfigs(Vec<PrunedConfigInfo>),
}

/// Information about a pruned config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrunedConfigInfo {
    /// Original config path (or "unknown" if orphaned)
    pub config_path: String,
    /// Config hash (state directory name)
    pub config_hash: String,
    /// Bytes freed by pruning
    pub bytes_freed: u64,
    /// Status: "pruned", "skipped", "would_prune", "orphaned"
    pub status: String,
}

/// Chunked log entries for large log responses
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogChunkData {
    /// Log entries in this chunk
    pub entries: Vec<LogEntry>,
    /// Whether there are more entries after this chunk
    pub has_more: bool,
    /// Offset for next chunk request
    pub next_offset: usize,
    /// Total number of entries (if known)
    pub total: Option<usize>,
}

/// Compact log entry for cursor-based streaming.
/// Uses a u16 service index into the service_table instead of a full service name per entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorLogEntry {
    /// Index into LogCursorData::service_table
    pub service_id: u16,
    /// Log line content
    pub line: String,
    /// Timestamp in milliseconds since Unix epoch
    pub timestamp: i64,
    /// Stream (stdout/stderr)
    pub stream: StreamType,
}

/// Log entries for cursor-based streaming (all/follow modes)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogCursorData {
    /// Service name table: CursorLogEntry::service_id indexes into this vec
    pub service_table: Vec<Arc<str>>,
    /// Compact log entries (service stored as u16 index)
    pub entries: Vec<CursorLogEntry>,
    /// Cursor ID to use for next request
    pub cursor_id: String,
    /// Whether there are more entries to read (false when EOF reached for 'all' mode)
    pub has_more: bool,
}

/// Status information for a single config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigStatus {
    /// Path to the config file
    pub config_path: String,
    /// Config hash
    pub config_hash: String,
    /// Services in this config
    pub services: HashMap<String, ServiceInfo>,
}

/// Information about a loaded config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedConfigInfo {
    /// Path to the config file
    pub config_path: String,
    /// Config hash
    pub config_hash: String,
    /// Number of services
    pub service_count: usize,
    /// Number of running services
    pub running_count: usize,
}

/// Information about a service
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceInfo {
    /// Service status
    pub status: String,
    /// Process ID if running
    pub pid: Option<u32>,
    /// Started timestamp
    pub started_at: Option<i64>,
    /// Stopped/exited/failed timestamp
    #[serde(default)]
    pub stopped_at: Option<i64>,
    /// Health check failures
    pub health_check_failures: u32,
    /// Exit code (for stopped/failed services)
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// Signal that killed the process (e.g., 9 for SIGKILL)
    #[serde(default)]
    pub signal: Option<i32>,
}

/// A log entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    /// Service name
    pub service: Arc<str>,
    /// Log line content
    pub line: String,
    /// Timestamp
    pub timestamp: Option<i64>,
    /// Stream (stdout/stderr)
    pub stream: StreamType,
}

/// Information about the daemon
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonInfo {
    /// Daemon PID
    pub pid: u32,
    /// Number of loaded configs
    pub loaded_configs: usize,
    /// Uptime in seconds
    pub uptime_secs: u64,
}

/// Client-to-server message with request ID for multiplexing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub id: u64,
    pub request: Request,
}

/// Server-to-client message: either a response to a request, or a pushed event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    Response {
        id: u64,
        response: Response,
    },
    Event {
        event: ServerEvent,
    },
}

/// Server-pushed events
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerEvent {
    /// Progress update for a specific request
    Progress {
        request_id: u64,
        event: ProgressEvent,
    },
}

/// A progress event for a single service during a lifecycle operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressEvent {
    pub service: String,
    pub phase: ServicePhase,
}

/// Expected final state for a service during a start operation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ServiceTarget {
    Started,
    Healthy,
}

/// Phase of a service lifecycle operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServicePhase {
    Pending { target: ServiceTarget },
    Waiting,
    Starting,
    Started,
    Healthy,
    Stopping,
    Stopped,
    Cleaning,
    Cleaned,
    Failed { message: String },
    /// A lifecycle hook started execution
    HookStarted { hook: String },
    /// A lifecycle hook completed successfully (exit code 0)
    HookCompleted { hook: String },
    /// A lifecycle hook failed (non-zero exit code)
    HookFailed { hook: String, message: String },
}

pub type Result<T> = std::result::Result<T, ProtocolError>;

/// Encode a request envelope to length-prefixed bincode bytes
pub fn encode_envelope(envelope: &RequestEnvelope) -> Result<Vec<u8>> {
    let size = bincode::serialized_size(envelope).map_err(ProtocolError::Encode)?;
    if size > MAX_MESSAGE_SIZE as u64 {
        return Err(ProtocolError::MessageTooLarge);
    }
    let len = size as u32;
    let mut frame = Vec::with_capacity(4 + size as usize);
    frame.extend_from_slice(&len.to_be_bytes());
    bincode::serialize_into(&mut frame, envelope).map_err(ProtocolError::Encode)?;
    Ok(frame)
}

/// Decode a request envelope from raw bincode payload (framing already stripped)
pub fn decode_envelope(bytes: &[u8]) -> Result<RequestEnvelope> {
    bincode::deserialize(bytes).map_err(ProtocolError::Decode)
}

/// Encode a server message to length-prefixed bincode bytes
pub fn encode_server_message(msg: &ServerMessage) -> Result<Vec<u8>> {
    let size = bincode::serialized_size(msg).map_err(ProtocolError::Encode)?;
    if size > MAX_MESSAGE_SIZE as u64 {
        return Err(ProtocolError::MessageTooLarge);
    }
    let len = size as u32;
    let mut frame = Vec::with_capacity(4 + size as usize);
    frame.extend_from_slice(&len.to_be_bytes());
    bincode::serialize_into(&mut frame, msg).map_err(ProtocolError::Encode)?;
    Ok(frame)
}

/// Decode a server message from raw bincode payload (framing already stripped)
pub fn decode_server_message(bytes: &[u8]) -> Result<ServerMessage> {
    bincode::deserialize(bytes).map_err(ProtocolError::Decode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // ========================================================================
    // RequestEnvelope roundtrip tests
    // ========================================================================

    #[test]
    fn roundtrip_envelope_ping() {
        let envelope = RequestEnvelope { id: 1, request: Request::Ping };
        let bytes = encode_envelope(&envelope).unwrap();
        // Strip 4-byte length prefix
        let decoded = decode_envelope(&bytes[4..]).unwrap();
        assert_eq!(decoded.id, 1);
        assert!(matches!(decoded.request, Request::Ping));
    }

    #[test]
    fn roundtrip_envelope_shutdown() {
        let envelope = RequestEnvelope { id: 42, request: Request::Shutdown };
        let bytes = encode_envelope(&envelope).unwrap();
        let decoded = decode_envelope(&bytes[4..]).unwrap();
        assert_eq!(decoded.id, 42);
        assert!(matches!(decoded.request, Request::Shutdown));
    }

    #[test]
    fn roundtrip_envelope_start() {
        let envelope = RequestEnvelope {
            id: 7,
            request: Request::Start {
                config_path: PathBuf::from("/tmp/test.yaml"),
                service: Some("web".into()),
                sys_env: Some(HashMap::from([("PATH".into(), "/usr/bin".into())])),
            },
        };
        let bytes = encode_envelope(&envelope).unwrap();
        let decoded = decode_envelope(&bytes[4..]).unwrap();
        assert_eq!(decoded.id, 7);
        match decoded.request {
            Request::Start { config_path, service, sys_env } => {
                assert_eq!(config_path, PathBuf::from("/tmp/test.yaml"));
                assert_eq!(service, Some("web".into()));
                assert!(sys_env.is_some());
                assert_eq!(sys_env.unwrap().get("PATH").unwrap(), "/usr/bin");
            }
            _ => panic!("Expected Start request"),
        }
    }

    #[test]
    fn roundtrip_envelope_stop() {
        let envelope = RequestEnvelope {
            id: 3,
            request: Request::Stop {
                config_path: PathBuf::from("/etc/kepler.yaml"),
                service: None,
                clean: true,
                signal: Some("SIGKILL".into()),
            },
        };
        let bytes = encode_envelope(&envelope).unwrap();
        let decoded = decode_envelope(&bytes[4..]).unwrap();
        assert_eq!(decoded.id, 3);
        match decoded.request {
            Request::Stop { config_path, service, clean, signal } => {
                assert_eq!(config_path, PathBuf::from("/etc/kepler.yaml"));
                assert!(service.is_none());
                assert!(clean);
                assert_eq!(signal, Some("SIGKILL".into()));
            }
            _ => panic!("Expected Stop request"),
        }
    }

    #[test]
    fn roundtrip_envelope_restart() {
        let envelope = RequestEnvelope {
            id: 5,
            request: Request::Restart {
                config_path: PathBuf::from("/app/kepler.yaml"),
                services: vec!["api".into(), "web".into()],
                sys_env: None,
            },
        };
        let bytes = encode_envelope(&envelope).unwrap();
        let decoded = decode_envelope(&bytes[4..]).unwrap();
        match decoded.request {
            Request::Restart { services, .. } => {
                assert_eq!(services, vec!["api".to_string(), "web".to_string()]);
            }
            _ => panic!("Expected Restart request"),
        }
    }

    #[test]
    fn roundtrip_envelope_logs() {
        let envelope = RequestEnvelope {
            id: 10,
            request: Request::Logs {
                config_path: PathBuf::from("/app/k.yaml"),
                service: Some("worker".into()),
                follow: true,
                lines: 500,
                max_bytes: Some(1024 * 1024),
                mode: LogMode::Tail,
                no_hooks: true,
            },
        };
        let bytes = encode_envelope(&envelope).unwrap();
        let decoded = decode_envelope(&bytes[4..]).unwrap();
        match decoded.request {
            Request::Logs { service, follow, lines, max_bytes, mode, no_hooks, .. } => {
                assert_eq!(service, Some("worker".into()));
                assert!(follow);
                assert_eq!(lines, 500);
                assert_eq!(max_bytes, Some(1024 * 1024));
                assert_eq!(mode, LogMode::Tail);
                assert!(no_hooks);
            }
            _ => panic!("Expected Logs request"),
        }
    }

    #[test]
    fn roundtrip_envelope_status() {
        let envelope = RequestEnvelope {
            id: 99,
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
        let envelope = RequestEnvelope { id: 100, request: Request::ListConfigs };
        let bytes = encode_envelope(&envelope).unwrap();
        let decoded = decode_envelope(&bytes[4..]).unwrap();
        assert!(matches!(decoded.request, Request::ListConfigs));
    }

    #[test]
    fn roundtrip_envelope_prune() {
        let envelope = RequestEnvelope {
            id: 200,
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
    fn roundtrip_envelope_logs_cursor() {
        let envelope = RequestEnvelope {
            id: 50,
            request: Request::LogsCursor {
                config_path: PathBuf::from("/app/k.yaml"),
                service: None,
                cursor_id: Some("cursor-abc123".into()),
                from_start: true,
                no_hooks: false,
            },
        };
        let bytes = encode_envelope(&envelope).unwrap();
        let decoded = decode_envelope(&bytes[4..]).unwrap();
        match decoded.request {
            Request::LogsCursor { cursor_id, from_start, no_hooks, .. } => {
                assert_eq!(cursor_id, Some("cursor-abc123".into()));
                assert!(from_start);
                assert!(!no_hooks);
            }
            _ => panic!("Expected LogsCursor request"),
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
        });
        services.insert("api".into(), ServiceInfo {
            status: "exited".into(),
            pid: None,
            started_at: Some(1700000000),
            stopped_at: Some(1700001000),
            health_check_failures: 0,
            exit_code: Some(0),
            signal: None,
        });
        services.insert("worker".into(), ServiceInfo {
            status: "killed".into(),
            pid: None,
            started_at: Some(1700000000),
            stopped_at: Some(1700001000),
            health_check_failures: 0,
            exit_code: None,
            signal: Some(9),
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
    fn roundtrip_log_entry() {
        let entries = vec![
            LogEntry {
                service: Arc::from("web"),
                line: "Server started on port 8080".into(),
                timestamp: Some(1700000000000),
                stream: StreamType::Stdout,
            },
            LogEntry {
                service: Arc::from("web"),
                line: "Warning: deprecated API".into(),
                timestamp: Some(1700000001000),
                stream: StreamType::Stderr,
            },
        ];
        let msg = ServerMessage::Response {
            id: 20,
            response: Response::ok_with_data(ResponseData::Logs(entries)),
        };
        let bytes = encode_server_message(&msg).unwrap();
        let decoded = decode_server_message(&bytes[4..]).unwrap();
        match decoded {
            ServerMessage::Response { response: Response::Ok { data: Some(ResponseData::Logs(logs)), .. }, .. } => {
                assert_eq!(logs.len(), 2);
                assert_eq!(&*logs[0].service, "web");
                assert_eq!(logs[0].stream, StreamType::Stdout);
                assert_eq!(logs[1].stream, StreamType::Stderr);
            }
            _ => panic!("Expected Logs response"),
        }
    }

    #[test]
    fn roundtrip_cursor_log_entry() {
        let data = LogCursorData {
            service_table: vec![Arc::from("web"), Arc::from("api")],
            entries: vec![
                CursorLogEntry {
                    service_id: 0,
                    line: "hello from web".into(),
                    timestamp: 1700000000000,
                    stream: StreamType::Stdout,
                },
                CursorLogEntry {
                    service_id: 1,
                    line: "hello from api".into(),
                    timestamp: 1700000001000,
                    stream: StreamType::Stderr,
                },
            ],
            cursor_id: "cursor-xyz".into(),
            has_more: true,
        };
        let msg = ServerMessage::Response {
            id: 30,
            response: Response::ok_with_data(ResponseData::LogCursor(data)),
        };
        let bytes = encode_server_message(&msg).unwrap();
        let decoded = decode_server_message(&bytes[4..]).unwrap();
        match decoded {
            ServerMessage::Response { response: Response::Ok { data: Some(ResponseData::LogCursor(d)), .. }, .. } => {
                assert_eq!(d.service_table.len(), 2);
                assert_eq!(&*d.service_table[0], "web");
                assert_eq!(d.entries.len(), 2);
                assert_eq!(d.entries[0].service_id, 0);
                assert!(d.has_more);
                assert_eq!(d.cursor_id, "cursor-xyz");
            }
            _ => panic!("Expected LogCursor response"),
        }
    }

    // ========================================================================
    // Length prefix framing tests
    // ========================================================================

    #[test]
    fn encode_envelope_includes_length_prefix() {
        let envelope = RequestEnvelope { id: 1, request: Request::Ping };
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
        let envelope = RequestEnvelope { id: 1, request: Request::Ping };
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
                service: None,
                sys_env: None,
            }.variant_name(),
            "Start"
        );
    }

    // ========================================================================
    // Request ID uniqueness test
    // ========================================================================

    #[test]
    fn different_request_ids_produce_distinct_envelopes() {
        let env1 = RequestEnvelope { id: 1, request: Request::Ping };
        let env2 = RequestEnvelope { id: 2, request: Request::Ping };
        let bytes1 = encode_envelope(&env1).unwrap();
        let bytes2 = encode_envelope(&env2).unwrap();
        // Payloads should differ (different IDs)
        assert_ne!(bytes1, bytes2);
        let dec1 = decode_envelope(&bytes1[4..]).unwrap();
        let dec2 = decode_envelope(&bytes2[4..]).unwrap();
        assert_ne!(dec1.id, dec2.id);
    }

    // ========================================================================
    // StreamType tests
    // ========================================================================

    #[test]
    fn stream_type_as_str() {
        assert_eq!(StreamType::Stdout.as_str(), "stdout");
        assert_eq!(StreamType::Stderr.as_str(), "stderr");
    }

    #[test]
    fn stream_type_is_stderr() {
        assert!(!StreamType::Stdout.is_stderr());
        assert!(StreamType::Stderr.is_stderr());
    }

    #[test]
    fn stream_type_display() {
        assert_eq!(format!("{}", StreamType::Stdout), "stdout");
        assert_eq!(format!("{}", StreamType::Stderr), "stderr");
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
            ServicePhase::HookFailed { hook: "on_init".into(), message: "Exit code: Some(127)".into() },
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
}
