use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tracing::debug;

use crate::{
    errors::ClientError,
    protocol::{
        FRAME_DELIMITER, LogMode, MAX_MESSAGE_SIZE, Request, RequestEnvelope, Response,
        ServerMessage, StartMode, decode_server_message, encode_envelope,
    },
};

pub type Result<T> = std::result::Result<T, ClientError>;

/// Bounded channel capacity for the client writer task.
const WRITER_CHANNEL_CAPACITY: usize = 64;

pub struct Client {
    writer_tx: mpsc::Sender<Vec<u8>>,
    pending: Arc<DashMap<u64, oneshot::Sender<Response>>>,
    next_id: Arc<AtomicU64>,
    _reader_handle: JoinHandle<()>,
    _writer_handle: JoinHandle<()>,
}

impl Client {
    /// Connect to the daemon at the given socket path
    pub async fn connect(socket_path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(socket_path)
            .await
            .map_err(ClientError::Connect)?;

        let (read_half, mut write_half) = stream.into_split();

        let pending: Arc<DashMap<u64, oneshot::Sender<Response>>> =
            Arc::new(DashMap::new());

        // Writer task: receives encoded bytes and writes to stream
        let (writer_tx, mut writer_rx) = mpsc::channel::<Vec<u8>>(WRITER_CHANNEL_CAPACITY);

        let writer_handle = tokio::spawn(async move {
            while let Some(bytes) = writer_rx.recv().await {
                if let Err(e) = write_half.write_all(&bytes).await {
                    debug!("Client writer error: {}", e);
                    break;
                }
            }
            let _ = write_half.shutdown().await;
        });

        // Reader task: reads frames from stream, dispatches to pending map
        let reader_pending = pending.clone();
        let reader_handle = tokio::spawn(async move {
            let mut reader = BufReader::new(read_half);
            let mut line = Vec::new();

            loop {
                line.clear();

                loop {
                    let bytes_read = match reader.read_until(FRAME_DELIMITER, &mut line).await {
                        Ok(n) => n,
                        Err(e) => {
                            debug!("Client reader error: {}", e);
                            return;
                        }
                    };

                    if bytes_read == 0 {
                        // EOF - server disconnected
                        debug!("Server disconnected (EOF)");
                        return;
                    }

                    if line.len() > MAX_MESSAGE_SIZE {
                        debug!("Server message exceeds maximum size");
                        return;
                    }

                    if line.last() == Some(&FRAME_DELIMITER) {
                        break;
                    }
                }

                // Remove delimiter
                if line.last() == Some(&FRAME_DELIMITER) {
                    line.pop();
                }

                // Decode server message
                match decode_server_message(&line) {
                    Ok(ServerMessage::Response { id, response }) => {
                        if let Some((_, tx)) = reader_pending.remove(&id) {
                            let _ = tx.send(response);
                        } else {
                            debug!("Received response for unknown request id={}", id);
                        }
                    }
                    Ok(ServerMessage::Event { event }) => {
                        debug!("Received server event: {:?}", event);
                        // Future: forward to event channel
                    }
                    Err(e) => {
                        debug!("Failed to decode server message: {}", e);
                    }
                }
            }
        });

        Ok(Self {
            writer_tx,
            pending,
            next_id: Arc::new(AtomicU64::new(1)),
            _reader_handle: reader_handle,
            _writer_handle: writer_handle,
        })
    }

    /// Check if daemon is running by attempting to connect and ping
    pub async fn is_daemon_running(socket_path: &Path) -> bool {
        if !socket_path.exists() {
            return false;
        }

        match Self::connect(socket_path).await {
            Ok(client) => matches!(
                client.send_request(Request::Ping).await,
                Ok(Response::Ok { .. })
            ),
            Err(_) => false,
        }
    }

    /// Send a request and receive a response.
    /// Takes `&self` - multiple requests can be in-flight concurrently.
    pub async fn send_request(&self, request: Request) -> Result<Response> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        // Create oneshot for this request
        let (tx, rx) = oneshot::channel();
        self.pending.insert(id, tx);

        // Encode and send
        let envelope = RequestEnvelope { id, request };
        let bytes = encode_envelope(&envelope)?;
        self.writer_tx
            .send(bytes)
            .await
            .map_err(|_| ClientError::Disconnected)?;

        // Wait for response
        rx.await.map_err(|_| ClientError::Disconnected)
    }

    /// Start services for a config
    pub async fn start(
        &self,
        config_path: PathBuf,
        service: Option<String>,
        sys_env: Option<HashMap<String, String>>,
        mode: StartMode,
    ) -> Result<Response> {
        self.send_request(Request::Start {
            config_path,
            service,
            sys_env,
            mode,
        })
        .await
    }

    /// Stop services for a config
    pub async fn stop(
        &self,
        config_path: PathBuf,
        service: Option<String>,
        clean: bool,
        signal: Option<String>,
    ) -> Result<Response> {
        self.send_request(Request::Stop {
            config_path,
            service,
            clean,
            signal,
        })
        .await
    }

    /// Restart services for a config (preserves baked config, runs restart hooks)
    pub async fn restart(
        &self,
        config_path: PathBuf,
        services: Vec<String>,
        sys_env: Option<HashMap<String, String>>,
        detach: bool,
    ) -> Result<Response> {
        self.send_request(Request::Restart {
            config_path,
            services,
            sys_env,
            detach,
        })
        .await
    }

    /// Recreate services for a config (re-bake config, clear state, start fresh)
    pub async fn recreate(
        &self,
        config_path: PathBuf,
        sys_env: Option<HashMap<String, String>>,
        detach: bool,
    ) -> Result<Response> {
        self.send_request(Request::Recreate {
            config_path,
            sys_env,
            detach,
        })
        .await
    }

    /// Get status (for a specific config or all configs)
    pub async fn status(&self, config_path: Option<PathBuf>) -> Result<Response> {
        self.send_request(Request::Status { config_path }).await
    }

    /// List all loaded configs
    pub async fn list_configs(&self) -> Result<Response> {
        self.send_request(Request::ListConfigs).await
    }

    /// Unload a config
    pub async fn unload_config(&self, config_path: PathBuf) -> Result<Response> {
        self.send_request(Request::UnloadConfig { config_path })
            .await
    }

    /// Get logs for a config
    pub async fn logs(
        &self,
        config_path: PathBuf,
        service: Option<String>,
        follow: bool,
        lines: usize,
        mode: LogMode,
        no_hooks: bool,
    ) -> Result<Response> {
        self.send_request(Request::Logs {
            config_path,
            service,
            follow,
            lines,
            max_bytes: None,
            mode,
            no_hooks,
        })
        .await
    }

    /// Get logs with pagination (for large log responses)
    pub async fn logs_chunk(
        &self,
        config_path: PathBuf,
        service: Option<String>,
        offset: usize,
        limit: usize,
        no_hooks: bool,
    ) -> Result<Response> {
        self.send_request(Request::LogsChunk {
            config_path,
            service,
            offset,
            limit,
            no_hooks,
        })
        .await
    }

    /// Shutdown the daemon
    pub async fn shutdown(&self) -> Result<Response> {
        self.send_request(Request::Shutdown).await
    }

    /// Prune all stopped/orphaned config state directories
    pub async fn prune(&self, force: bool, dry_run: bool) -> Result<Response> {
        self.send_request(Request::Prune { force, dry_run }).await
    }

    /// Cursor-based log streaming (for 'all' and 'follow' modes)
    pub async fn logs_cursor(
        &self,
        config_path: &Path,
        service: Option<&str>,
        cursor_id: Option<&str>,
        from_start: bool,
        no_hooks: bool,
    ) -> Result<Response> {
        self.send_request(Request::LogsCursor {
            config_path: config_path.to_path_buf(),
            service: service.map(String::from),
            cursor_id: cursor_id.map(String::from),
            from_start,
            no_hooks,
        })
        .await
    }
}
