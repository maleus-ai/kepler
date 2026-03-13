use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::errors::ProtocolError;

/// Maximum message size (10MB) — local Unix socket, no network concerns
pub const MAX_MESSAGE_SIZE: usize = 10 * 1024 * 1024;

/// Maximum lines for one-shot queries (head/tail)
pub const MAX_LINES_ONE_SHOT: usize = 10_000;

/// Maximum entries per stream batch (secondary cap; byte budget is the primary limit)
pub const MAX_STREAM_BATCH_SIZE: usize = 50_000;

fn default_stream_limit() -> usize { MAX_STREAM_BATCH_SIZE }

/// Request sent from CLI to daemon
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    /// Start service(s) - runs full pipeline synchronously, responds when complete
    Start {
        /// Path to the config file
        config_path: PathBuf,
        /// Services to start (empty = all services)
        #[serde(default)]
        services: Vec<String>,
        /// System environment variables captured from CLI
        #[serde(default)]
        sys_env: Option<HashMap<String, String>>,
        /// Skip dependency waiting and `if:` condition (requires specific services)
        #[serde(default)]
        no_deps: bool,
        /// Override specific system environment variables (merged into stored sys_env)
        #[serde(default)]
        override_envs: Option<HashMap<String, String>>,
        /// Per-config hardening level (e.g. "none", "no-root", "strict")
        #[serde(default)]
        hardening: Option<String>,
        /// When true, the handler streams inline progress events (state changes,
        /// Ready, Quiescent, UnhandledFailure) until all services settle — no
        /// separate Subscribe request needed.
        #[serde(default)]
        follow: bool,
    },
    /// Stop service(s)
    Stop {
        /// Path to the config file
        config_path: PathBuf,
        /// Services to stop (empty = all services)
        #[serde(default)]
        services: Vec<String>,
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
        /// Skip dependency ordering (use user-specified order instead)
        #[serde(default)]
        no_deps: bool,
        /// Override specific system environment variables (merged into stored sys_env)
        #[serde(default)]
        override_envs: Option<HashMap<String, String>>,
    },
    /// Recreate config - stop, re-bake config snapshot, start
    Recreate {
        /// Path to the config file
        config_path: PathBuf,
        /// System environment variables captured from CLI (for re-baking config snapshot)
        #[serde(default)]
        sys_env: Option<HashMap<String, String>>,
        /// Per-config hardening level (e.g. "none", "no-root", "strict")
        #[serde(default)]
        hardening: Option<String>,
    },
    /// Get status of services
    Status {
        /// Path to the config file (None = all configs)
        config_path: Option<PathBuf>,
    },
    /// Shutdown the daemon
    Shutdown,
    /// Ping to check if daemon is alive
    Ping,
    /// List all loaded configs
    ListConfigs,
    /// Prune all stopped/orphaned config state directories
    Prune {
        /// Force prune even if services appear running
        force: bool,
        /// Show what would be pruned without deleting
        dry_run: bool,
    },
    /// Streaming log query (for 'all' and 'follow' modes).
    /// The client tracks its own position via `last_id`.
    LogsStream {
        /// Path to the config file
        config_path: PathBuf,
        /// Services to show logs for (empty = all services)
        #[serde(default)]
        services: Vec<String>,
        /// Read entries after this row ID (None = from start).
        /// The client passes back `last_id` from the previous response.
        #[serde(default)]
        after_id: Option<i64>,
        /// If true and after_id is None, start from the end (for 'follow' mode).
        /// The server returns `last_id = max_id` with empty entries.
        #[serde(default)]
        from_end: bool,
        /// Maximum number of entries to return per batch
        #[serde(default = "default_stream_limit")]
        limit: usize,
        /// Whether to exclude hook log entries
        #[serde(default)]
        no_hooks: bool,
        /// Optional filter expression (DSL). Requires `logs:search` sub-right.
        /// Example: `level='err' AND (service='web' OR service='api')`
        #[serde(default)]
        filter: Option<String>,
        /// Raw mode: only return log line content, skip metadata
        #[serde(default)]
        raw: bool,
        /// Tail mode: return the last `limit` entries in chronological order.
        /// Ignores `after_id` and `from_end`.
        #[serde(default)]
        tail: bool,
    },
    /// Subscribe to log-available notifications for a config.
    /// The server pushes LogsAvailable events when new logs are flushed to SQLite.
    /// The client should fetch logs via LogsStream after receiving the event.
    SubscribeLogs {
        /// Path to the config file
        config_path: PathBuf,
    },
    /// Subscribe to service state change events
    Subscribe {
        /// Path to the config file
        config_path: PathBuf,
        /// Services to watch (None = all)
        services: Option<Vec<String>>,
    },
    /// Inspect config and runtime state (JSON output)
    Inspect {
        /// Path to the config file
        config_path: PathBuf,
    },
    /// Check if all services are quiescent (settled — nothing more will change)
    CheckQuiescence {
        /// Path to the config file
        config_path: PathBuf,
    },
    /// Check if all services are ready (reached target state)
    CheckReadiness {
        /// Path to the config file
        config_path: PathBuf,
    },
    /// Query effective rights for the calling user on a config.
    /// Returns the union of matching ACL rules (no authorizer evaluation).
    UserRights {
        /// Path to the config file
        config_path: PathBuf,
    },
    /// Query monitoring metrics (CPU, memory) for services
    MonitorMetrics {
        /// Path to the config file
        config_path: PathBuf,
        /// Service name (None = all services)
        service: Option<String>,
        /// None = latest per service, Some = history since timestamp (ms)
        since: Option<i64>,
        /// Maximum number of entries to return
        #[serde(default)]
        limit: Option<usize>,
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
            Request::Shutdown => "Shutdown",
            Request::Ping => "Ping",
            Request::ListConfigs => "ListConfigs",
            Request::Prune { .. } => "Prune",
            Request::LogsStream { .. } => "LogsStream",
            Request::SubscribeLogs { .. } => "SubscribeLogs",
            Request::Subscribe { .. } => "Subscribe",
            Request::Inspect { .. } => "Inspect",
            Request::CheckQuiescence { .. } => "CheckQuiescence",
            Request::CheckReadiness { .. } => "CheckReadiness",
            Request::UserRights { .. } => "UserRights",
            Request::MonitorMetrics { .. } => "MonitorMetrics",
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
    /// Permission denied (ACL, filesystem, or authorization failure)
    PermissionDenied {
        /// Human-readable reason
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
    /// Log stream batch
    LogStream(LogStreamData),
    /// Daemon info
    DaemonInfo(DaemonInfo),
    /// Pruned configs info
    PrunedConfigs(Vec<PrunedConfigInfo>),
    /// Inspect output (pre-built JSON string)
    Inspect(String),
    /// Boolean result for check commands (quiescence, readiness)
    CheckResult(bool),
    /// Effective rights for the calling user on a config
    UserRights(Vec<String>),
    /// Monitoring metrics entries
    MonitorMetrics(Vec<MonitorMetricEntry>),
}

/// A single monitoring metrics sample for one service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorMetricEntry {
    pub timestamp: i64,
    pub service: String,
    pub cpu_percent: f32,
    pub memory_rss: u64,
    pub memory_vss: u64,
    pub pids: Vec<u32>,
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

/// Compact log entry for streaming.
/// Uses a u16 service index into the service_table instead of a full service name per entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamLogEntry {
    /// Index into LogStreamData::service_table
    pub service_id: u16,
    /// Log line content
    pub line: String,
    /// Timestamp in milliseconds since Unix epoch
    pub timestamp: i64,
    /// Log level ("out", "err", "trace", "debug", "info", "warn", "error", "fatal")
    pub level: Arc<str>,
    /// Hook name (None for service process logs)
    pub hook: Option<Arc<str>>,
    /// Remaining JSON attributes (None for non-JSON lines)
    pub attributes: Option<Arc<str>>,
}

/// Log stream batch response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogStreamData {
    /// Service name table: StreamLogEntry::service_id indexes into this vec
    pub service_table: Vec<Arc<str>>,
    /// Compact log entries (service stored as u16 index)
    pub entries: Vec<StreamLogEntry>,
    /// Row ID of the last entry returned. Pass as `after_id` in the next request.
    pub last_id: i64,
    /// Whether there are more entries to read
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
    /// Whether the service has completed its first start
    #[serde(default)]
    pub initialized: bool,
    /// Reason for being skipped (when status is "skipped")
    #[serde(default)]
    pub skip_reason: Option<String>,
    /// Reason for failure (when status is "failed")
    #[serde(default)]
    pub fail_reason: Option<String>,
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
#[derive(Clone, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub id: u64,
    pub request: Request,
    /// Bearer token for process authentication (from `KEPLER_TOKEN` env var).
    #[serde(default)]
    pub token: Option<[u8; 32]>,
}

impl std::fmt::Debug for RequestEnvelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestEnvelope")
            .field("id", &self.id)
            .field("request", &self.request)
            .finish()
    }
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
    /// All services reached their target state (for --wait)
    Ready { request_id: u64 },
    /// All services settled — nothing more will change (for foreground mode exit)
    Quiescent { request_id: u64 },
    /// New log data has been flushed to SQLite for a config.
    /// The client should issue a LogsStream request to fetch it.
    LogsAvailable { request_id: u64 },
    /// A service failed with no handler (no `service_failed`/`service_stopped` dependency, won't restart)
    UnhandledFailure {
        request_id: u64,
        service: String,
        exit_code: Option<i32>,
    },
}

impl ServerEvent {
    /// Get the request_id for this event
    pub fn request_id(&self) -> u64 {
        match self {
            ServerEvent::Progress { request_id, .. } => *request_id,
            ServerEvent::Ready { request_id } => *request_id,
            ServerEvent::Quiescent { request_id } => *request_id,
            ServerEvent::LogsAvailable { request_id } => *request_id,
            ServerEvent::UnhandledFailure { request_id, .. } => *request_id,
        }
    }
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
    Restarting,
    Stopping,
    Stopped,
    Cleaning,
    Cleaned,
    Skipped { reason: String },
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
mod tests;
