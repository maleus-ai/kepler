use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::errors::ProtocolError;

/// Maximum message size (1MB)
pub const MAX_MESSAGE_SIZE: usize = 1024 * 1024;

/// Maximum lines for one-shot queries (head/tail)
pub const MAX_LINES_ONE_SHOT: usize = 10_000;

/// Maximum entries per cursor batch (for all/follow modes)
pub const MAX_CURSOR_BATCH_SIZE: usize = 1_000;

/// Log reading mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
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
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Request {
    /// Start service(s)
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
    },
    /// Restart service(s) - preserves baked config, runs restart hooks
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
    /// Recreate services - re-bake config, clear state, start fresh
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
    },
}

/// Response sent from daemon to CLI
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    /// Successful response
    Ok {
        /// Optional message
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        /// Optional data payload
        #[serde(skip_serializing_if = "Option::is_none")]
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
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
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

/// Log entries for cursor-based streaming (all/follow modes)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogCursorData {
    /// Log entries in this batch
    pub entries: Vec<LogEntry>,
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
    /// Health check failures
    pub health_check_failures: u32,
}

/// A log entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    /// Service name
    pub service: String,
    /// Log line content
    pub line: String,
    /// Timestamp
    pub timestamp: Option<i64>,
    /// Stream (stdout/stderr)
    pub stream: String,
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

/// Frame delimiter for messages over socket
pub const FRAME_DELIMITER: u8 = b'\n';

pub type Result<T> = std::result::Result<T, ProtocolError>;

/// Encode a request to bytes
pub fn encode_request(request: &Request) -> Result<Vec<u8>> {
    let json = serde_json::to_string(request).map_err(ProtocolError::Encode)?;
    let mut bytes = json.into_bytes();
    bytes.push(FRAME_DELIMITER);
    Ok(bytes)
}

/// Encode a response to bytes
pub fn encode_response(response: &Response) -> Result<Vec<u8>> {
    let json = serde_json::to_string(response).map_err(ProtocolError::Encode)?;
    let mut bytes = json.into_bytes();
    bytes.push(FRAME_DELIMITER);
    Ok(bytes)
}

/// Decode a request from bytes
pub fn decode_request(bytes: &[u8]) -> Result<Request> {
    let json = std::str::from_utf8(bytes)?;
    let request: Request = serde_json::from_str(json).map_err(ProtocolError::Decode)?;
    Ok(request)
}

/// Decode a response from bytes
pub fn decode_response(bytes: &[u8]) -> Result<Response> {
    let json = std::str::from_utf8(bytes)?;
    let response: Response = serde_json::from_str(json).map_err(ProtocolError::Decode)?;
    Ok(response)
}
