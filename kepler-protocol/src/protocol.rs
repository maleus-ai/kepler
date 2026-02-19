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
    /// Reason for being skipped (when status is "skipped")
    #[serde(default)]
    pub skip_reason: Option<String>,
    /// Reason for failure (when status is "failed")
    #[serde(default)]
    pub fail_reason: Option<String>,
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
    /// All services reached their target state (for --wait)
    Ready { request_id: u64 },
    /// All services settled — nothing more will change (for foreground mode exit)
    Quiescent { request_id: u64 },
}

impl ServerEvent {
    /// Get the request_id for this event
    pub fn request_id(&self) -> u64 {
        match self {
            ServerEvent::Progress { request_id, .. } => *request_id,
            ServerEvent::Ready { request_id } => *request_id,
            ServerEvent::Quiescent { request_id } => *request_id,
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
