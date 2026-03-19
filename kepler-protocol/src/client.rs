use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use dashmap::DashMap;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tracing::debug;

use crate::{
    errors::ClientError,
    protocol::{
        Request, RequestEnvelope, Response,
        ServerEvent, ServerMessage, decode_server_message, encode_envelope,
    },
};

pub type Result<T> = std::result::Result<T, ClientError>;

/// Bounded channel capacity for the client writer task.
const WRITER_CHANNEL_CAPACITY: usize = 64;

struct PendingRequest {
    response_tx: oneshot::Sender<Response>,
    progress_tx: Option<mpsc::UnboundedSender<ServerEvent>>,
}

pub struct Client {
    writer_tx: mpsc::Sender<Vec<u8>>,
    pending: Arc<DashMap<u64, PendingRequest>>,
    next_id: Arc<AtomicU64>,
    /// Bearer token from `KEPLER_TOKEN` env var (hex-decoded).
    process_token: Option<[u8; 32]>,
    /// Set to `false` by the reader task when it exits (EOF or error).
    /// Checked by `send_request` to fail fast instead of hanging forever.
    connected: Arc<AtomicBool>,
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

        let pending: Arc<DashMap<u64, PendingRequest>> =
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

        // Reader task: reads length-prefixed frames from stream, dispatches to pending map
        let connected = Arc::new(AtomicBool::new(true));
        let reader_pending = pending.clone();
        let reader_connected = connected.clone();
        let reader_handle = tokio::spawn(async move {
            let mut reader = read_half;

            loop {
                // Read 4-byte length header
                let mut len_buf = [0u8; 4];
                if let Err(e) = reader.read_exact(&mut len_buf).await {
                    if e.kind() == std::io::ErrorKind::UnexpectedEof {
                        debug!("Server disconnected (EOF)");
                    } else {
                        debug!("Client reader error: {}", e);
                    }
                    // Mark disconnected before clearing so new send_request calls fail fast
                    reader_connected.store(false, Ordering::Release);
                    // Drop all pending senders so waiters get RecvError → Disconnected
                    reader_pending.clear();
                    return;
                }
                let msg_len = u32::from_be_bytes(len_buf) as usize;

                // Read payload
                let mut payload = vec![0u8; msg_len];
                if let Err(e) = reader.read_exact(&mut payload).await {
                    debug!("Client reader error: {}", e);
                    reader_connected.store(false, Ordering::Release);
                    reader_pending.clear();
                    return;
                }

                // Decode server message
                match decode_server_message(&payload) {
                    Ok(ServerMessage::Response { id, response }) => {
                        if let Some((_, pending_req)) = reader_pending.remove(&id) {
                            let _ = pending_req.response_tx.send(response);
                        } else {
                            debug!("Received response for unknown request id={}", id);
                        }
                    }
                    Ok(ServerMessage::Event { event }) => {
                        let request_id = event.request_id();
                        if let Some(pending_req) = reader_pending.get(&request_id)
                            && let Some(ref progress_tx) = pending_req.progress_tx
                        {
                            let _ = progress_tx.send(event);
                        }
                    }
                    Err(e) => {
                        debug!("Failed to decode server message: {}", e);
                    }
                }
            }
        });

        // Read KEPLER_TOKEN env var and hex-decode to [u8; 32]
        let process_token = std::env::var("KEPLER_TOKEN")
            .ok()
            .and_then(|s| {
                let bytes = hex::decode(&s).ok()?;
                if bytes.len() != 32 {
                    return None;
                }
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                Some(arr)
            });

        Ok(Self {
            writer_tx,
            pending,
            next_id: Arc::new(AtomicU64::new(1)),
            process_token,
            connected,
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
            Ok(client) => {
                let Ok((_rx, fut)) = client.send_request(Request::Ping) else {
                    return false;
                };
                matches!(fut.await, Ok(Response::Ok { .. }))
            }
            Err(_) => false,
        }
    }

    /// Send a request and receive both server events and a response.
    /// Takes `&self` - multiple requests can be in-flight concurrently.
    ///
    /// Returns an event receiver and a future that resolves to the final response.
    /// Events include progress updates, Ready, and Quiescent signals.
    pub fn send_request(
        &self,
        request: Request,
    ) -> Result<(mpsc::UnboundedReceiver<ServerEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        // Fail fast if the reader task has exited (daemon disconnected).
        // Without this check, new requests would hang forever waiting for
        // a response that will never arrive.
        if !self.connected.load(Ordering::Acquire) {
            return Err(ClientError::Disconnected);
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        let (response_tx, response_rx) = oneshot::channel();
        let (progress_tx, progress_rx) = mpsc::unbounded_channel();

        self.pending.insert(id, PendingRequest {
            response_tx,
            progress_tx: Some(progress_tx),
        });

        let envelope = RequestEnvelope { id, request, token: self.process_token };
        let bytes = encode_envelope(&envelope)?;

        let writer_tx = self.writer_tx.clone();
        let response_future = async move {
            writer_tx
                .send(bytes)
                .await
                .map_err(|_| ClientError::Disconnected)?;
            response_rx.await.map_err(|_| ClientError::Disconnected)
        };

        Ok((progress_rx, response_future))
    }

    /// Start services for a config
    pub fn start(
        &self,
        config_path: PathBuf,
        services: Vec<String>,
        sys_env: Option<HashMap<String, String>>,
        no_deps: bool,
        override_envs: Option<HashMap<String, String>>,
        hardening: Option<String>,
        follow: bool,
        define_flags: Option<HashMap<String, String>>,
    ) -> Result<(mpsc::UnboundedReceiver<ServerEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::Start {
            config_path,
            services,
            sys_env,
            no_deps,
            override_envs,
            hardening,
            follow,
            define_flags,
        })
    }

    /// Run services for a config (ephemeral mode: fresh reload, no snapshot)
    pub fn run(
        &self,
        config_path: PathBuf,
        services: Vec<String>,
        sys_env: Option<HashMap<String, String>>,
        no_deps: bool,
        override_envs: Option<HashMap<String, String>>,
        hardening: Option<String>,
        follow: bool,
        start_clean: bool,
        define_flags: Option<HashMap<String, String>>,
    ) -> Result<(mpsc::UnboundedReceiver<ServerEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::Run {
            config_path,
            services,
            sys_env,
            no_deps,
            override_envs,
            hardening,
            follow,
            start_clean,
            define_flags,
        })
    }

    /// Stop services for a config
    pub fn stop(
        &self,
        config_path: PathBuf,
        services: Vec<String>,
        clean: bool,
        signal: Option<String>,
    ) -> Result<(mpsc::UnboundedReceiver<ServerEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::Stop {
            config_path,
            services,
            clean,
            signal,
        })
    }

    /// Restart services for a config (preserves baked config, runs restart hooks)
    pub fn restart(
        &self,
        config_path: PathBuf,
        services: Vec<String>,
        sys_env: Option<HashMap<String, String>>,
        no_deps: bool,
        override_envs: Option<HashMap<String, String>>,
        define_flags: Option<HashMap<String, String>>,
    ) -> Result<(mpsc::UnboundedReceiver<ServerEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::Restart {
            config_path,
            services,
            sys_env,
            no_deps,
            override_envs,
            define_flags,
        })
    }

    /// Recreate config for a config (re-bake config snapshot, no start/stop)
    pub fn recreate(
        &self,
        config_path: PathBuf,
        sys_env: Option<HashMap<String, String>>,
        hardening: Option<String>,
        define_flags: Option<HashMap<String, String>>,
    ) -> Result<(mpsc::UnboundedReceiver<ServerEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::Recreate {
            config_path,
            sys_env,
            hardening,
            define_flags,
        })
    }

    /// Get status (for a specific config or all configs)
    pub fn status(
        &self,
        config_path: Option<PathBuf>,
    ) -> Result<(mpsc::UnboundedReceiver<ServerEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::Status { config_path })
    }

    /// List all loaded configs
    pub fn list_configs(
        &self,
    ) -> Result<(mpsc::UnboundedReceiver<ServerEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::ListConfigs)
    }

    /// Shutdown the daemon
    pub fn shutdown(
        &self,
    ) -> Result<(mpsc::UnboundedReceiver<ServerEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::Shutdown)
    }

    /// Prune all stopped/orphaned config state directories
    pub fn prune(
        &self,
        force: bool,
        dry_run: bool,
    ) -> Result<(mpsc::UnboundedReceiver<ServerEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::Prune { force, dry_run })
    }

    /// Log query (streaming with cursor, one-shot head, or one-shot tail).
    /// Client tracks position via `after_id` for streaming modes.
    pub fn logs_stream(
        &self,
        config_path: &Path,
        services: &[String],
        after_id: Option<i64>,
        from_end: bool,
        limit: usize,
        no_hooks: bool,
        filter: Option<&str>,
        sql: bool,
        raw: bool,
        tail: bool,
        after_ts: Option<i64>,
        before_ts: Option<i64>,
    ) -> Result<(mpsc::UnboundedReceiver<ServerEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::LogsStream {
            config_path: config_path.to_path_buf(),
            services: services.to_vec(),
            after_id,
            from_end,
            limit,
            no_hooks,
            filter: filter.map(String::from),
            sql,
            raw,
            tail,
            after_ts,
            before_ts,
        })
    }

    /// Subscribe to service state change events.
    ///
    /// Returns a progress receiver and a future that resolves when the subscription ends.
    /// Progress events are pushed as service statuses change.
    pub fn subscribe(
        &self,
        config_path: PathBuf,
        services: Option<Vec<String>>,
    ) -> Result<(mpsc::UnboundedReceiver<ServerEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::Subscribe {
            config_path,
            services,
        })
    }

    /// Inspect config and runtime state
    pub fn inspect(
        &self,
        config_path: PathBuf,
    ) -> Result<(mpsc::UnboundedReceiver<ServerEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::Inspect { config_path })
    }

    /// Check if all services are quiescent (settled — nothing more will change)
    pub fn check_quiescence(
        &self,
        config_path: PathBuf,
    ) -> Result<(mpsc::UnboundedReceiver<ServerEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::CheckQuiescence { config_path })
    }

    /// Query monitoring metrics for services
    pub fn monitor_metrics(
        &self,
        config_path: PathBuf,
        service: Option<String>,
        since: Option<i64>,
        limit: Option<usize>,
        filter: Option<String>,
        sql: bool,
        bucket_ms: Option<i64>,
        after_ts: Option<i64>,
        before_ts: Option<i64>,
    ) -> Result<(mpsc::UnboundedReceiver<ServerEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::MonitorMetrics { config_path, service, since, limit, filter, sql, bucket_ms, after_ts, before_ts })
    }

    /// Query effective rights for the calling user on a config.
    pub fn user_rights(
        &self,
        config_path: PathBuf,
    ) -> Result<(mpsc::UnboundedReceiver<ServerEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::UserRights { config_path })
    }

    /// Check if all services are ready (reached target state)
    pub fn check_readiness(
        &self,
        config_path: PathBuf,
    ) -> Result<(mpsc::UnboundedReceiver<ServerEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::CheckReadiness { config_path })
    }

    /// Subscribe to log-available notifications for a config.
    ///
    /// Returns a progress receiver that emits `LogsAvailable` events when new logs
    /// are flushed to SQLite. The caller should issue a `LogsStream` request to fetch.
    pub async fn subscribe_logs(
        &self,
        config_path: PathBuf,
    ) -> Result<mpsc::UnboundedReceiver<ServerEvent>> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        let (response_tx, _response_rx) = oneshot::channel();
        let (progress_tx, progress_rx) = mpsc::unbounded_channel();

        self.pending.insert(id, PendingRequest {
            response_tx,
            progress_tx: Some(progress_tx),
        });

        let envelope = RequestEnvelope {
            id,
            request: Request::SubscribeLogs { config_path },
            token: self.process_token,
        };
        let bytes = encode_envelope(&envelope)?;
        self.writer_tx
            .send(bytes)
            .await
            .map_err(|_| ClientError::Disconnected)?;

        Ok(progress_rx)
    }

    /// Fire-and-forget subscribe: sends the Subscribe request eagerly and returns
    /// only the progress receiver. The response future is discarded — when the server
    /// returns "Subscription ended", the pending entry is cleaned up and progress_tx
    /// is dropped, causing progress_rx.recv() to return None.
    pub async fn subscribe_events(
        &self,
        config_path: PathBuf,
        services: Option<Vec<String>>,
    ) -> Result<mpsc::UnboundedReceiver<ServerEvent>> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        let (response_tx, _response_rx) = oneshot::channel();
        let (progress_tx, progress_rx) = mpsc::unbounded_channel();

        self.pending.insert(id, PendingRequest {
            response_tx,
            progress_tx: Some(progress_tx),
        });

        let envelope = RequestEnvelope {
            id,
            request: Request::Subscribe { config_path, services },
            token: self.process_token,
        };
        let bytes = encode_envelope(&envelope)?;
        self.writer_tx
            .send(bytes)
            .await
            .map_err(|_| ClientError::Disconnected)?;

        Ok(progress_rx)
    }
}

#[cfg(test)]
mod tests;
