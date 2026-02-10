use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

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
        LogMode, MAX_MESSAGE_SIZE, ProgressEvent, Request, RequestEnvelope, Response,
        ServerEvent, ServerMessage, decode_server_message, encode_envelope,
    },
};

pub type Result<T> = std::result::Result<T, ClientError>;

/// Bounded channel capacity for the client writer task.
const WRITER_CHANNEL_CAPACITY: usize = 64;

struct PendingRequest {
    response_tx: oneshot::Sender<Response>,
    progress_tx: Option<mpsc::Sender<ProgressEvent>>,
}

pub struct Client {
    writer_tx: mpsc::Sender<Vec<u8>>,
    pending: Arc<DashMap<u64, PendingRequest>>,
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
        let reader_pending = pending.clone();
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
                    // Drop all pending senders so waiters get RecvError → Disconnected
                    reader_pending.clear();
                    return;
                }
                let msg_len = u32::from_be_bytes(len_buf) as usize;

                if msg_len > MAX_MESSAGE_SIZE {
                    debug!("Server message exceeds maximum size");
                    reader_pending.clear();
                    return;
                }

                // Read payload
                let mut payload = vec![0u8; msg_len];
                if let Err(e) = reader.read_exact(&mut payload).await {
                    debug!("Client reader error: {}", e);
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
                    Ok(ServerMessage::Event { event: ServerEvent::Progress { request_id, event } }) => {
                        if let Some(pending_req) = reader_pending.get(&request_id)
                            && let Some(ref progress_tx) = pending_req.progress_tx
                        {
                            let _ = progress_tx.try_send(event);
                        }
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
            Ok(client) => {
                let Ok((_rx, fut)) = client.send_request(Request::Ping) else {
                    return false;
                };
                matches!(fut.await, Ok(Response::Ok { .. }))
            }
            Err(_) => false,
        }
    }

    /// Send a request and receive both progress events and a response.
    /// Takes `&self` - multiple requests can be in-flight concurrently.
    ///
    /// Returns a progress receiver and a future that resolves to the final response.
    /// Progress events are emitted as the server processes each service.
    pub fn send_request(
        &self,
        request: Request,
    ) -> Result<(mpsc::Receiver<ProgressEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        let (response_tx, response_rx) = oneshot::channel();
        let (progress_tx, progress_rx) = mpsc::channel(64);

        self.pending.insert(id, PendingRequest {
            response_tx,
            progress_tx: Some(progress_tx),
        });

        let envelope = RequestEnvelope { id, request };
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
        service: Option<String>,
        sys_env: Option<HashMap<String, String>>,
    ) -> Result<(mpsc::Receiver<ProgressEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::Start {
            config_path,
            service,
            sys_env,
        })
    }

    /// Stop services for a config
    pub fn stop(
        &self,
        config_path: PathBuf,
        service: Option<String>,
        clean: bool,
        signal: Option<String>,
    ) -> Result<(mpsc::Receiver<ProgressEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::Stop {
            config_path,
            service,
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
    ) -> Result<(mpsc::Receiver<ProgressEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::Restart {
            config_path,
            services,
            sys_env,
        })
    }

    /// Recreate config for a config (re-bake config snapshot, no start/stop)
    pub fn recreate(
        &self,
        config_path: PathBuf,
        sys_env: Option<HashMap<String, String>>,
    ) -> Result<(mpsc::Receiver<ProgressEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::Recreate {
            config_path,
            sys_env,
        })
    }

    /// Get status (for a specific config or all configs)
    pub fn status(
        &self,
        config_path: Option<PathBuf>,
    ) -> Result<(mpsc::Receiver<ProgressEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::Status { config_path })
    }

    /// List all loaded configs
    pub fn list_configs(
        &self,
    ) -> Result<(mpsc::Receiver<ProgressEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::ListConfigs)
    }

    /// Unload a config
    pub fn unload_config(
        &self,
        config_path: PathBuf,
    ) -> Result<(mpsc::Receiver<ProgressEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::UnloadConfig { config_path })
    }

    /// Get logs for a config
    pub fn logs(
        &self,
        config_path: PathBuf,
        service: Option<String>,
        follow: bool,
        lines: usize,
        mode: LogMode,
        no_hooks: bool,
    ) -> Result<(mpsc::Receiver<ProgressEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::Logs {
            config_path,
            service,
            follow,
            lines,
            max_bytes: None,
            mode,
            no_hooks,
        })
    }

    /// Get logs with pagination (for large log responses)
    pub fn logs_chunk(
        &self,
        config_path: PathBuf,
        service: Option<String>,
        offset: usize,
        limit: usize,
        no_hooks: bool,
    ) -> Result<(mpsc::Receiver<ProgressEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::LogsChunk {
            config_path,
            service,
            offset,
            limit,
            no_hooks,
        })
    }

    /// Shutdown the daemon
    pub fn shutdown(
        &self,
    ) -> Result<(mpsc::Receiver<ProgressEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::Shutdown)
    }

    /// Prune all stopped/orphaned config state directories
    pub fn prune(
        &self,
        force: bool,
        dry_run: bool,
    ) -> Result<(mpsc::Receiver<ProgressEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::Prune { force, dry_run })
    }

    /// Cursor-based log streaming (for 'all' and 'follow' modes)
    pub fn logs_cursor(
        &self,
        config_path: &Path,
        service: Option<&str>,
        cursor_id: Option<&str>,
        from_start: bool,
        no_hooks: bool,
    ) -> Result<(mpsc::Receiver<ProgressEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::LogsCursor {
            config_path: config_path.to_path_buf(),
            service: service.map(String::from),
            cursor_id: cursor_id.map(String::from),
            from_start,
            no_hooks,
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
    ) -> Result<(mpsc::Receiver<ProgressEvent>, impl Future<Output = Result<Response>> + use<'_>)> {
        self.send_request(Request::Subscribe {
            config_path,
            services,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        Response, ServerMessage, decode_envelope, encode_server_message,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;

    /// Helper: spin up a mock server that reads one envelope and replies.
    async fn mock_server_one_shot(
        listener: UnixListener,
        make_response: impl FnOnce(u64) -> ServerMessage + Send + 'static,
    ) {
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();

            // Read length prefix + payload
            let mut len_buf = [0u8; 4];
            stream.read_exact(&mut len_buf).await.unwrap();
            let msg_len = u32::from_be_bytes(len_buf) as usize;
            let mut payload = vec![0u8; msg_len];
            stream.read_exact(&mut payload).await.unwrap();

            let envelope = decode_envelope(&payload).unwrap();

            let resp = make_response(envelope.id);
            let bytes = encode_server_message(&resp).unwrap();
            stream.write_all(&bytes).await.unwrap();
            stream.shutdown().await.unwrap();
        });
    }

    #[tokio::test]
    async fn request_id_monotonically_increases() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("test.sock");

        let listener = UnixListener::bind(&sock).unwrap();

        // Mock server: accept, read 3 envelopes, reply to each
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut ids = Vec::new();

            for _ in 0..3 {
                let mut len_buf = [0u8; 4];
                stream.read_exact(&mut len_buf).await.unwrap();
                let msg_len = u32::from_be_bytes(len_buf) as usize;
                let mut payload = vec![0u8; msg_len];
                stream.read_exact(&mut payload).await.unwrap();

                let envelope = decode_envelope(&payload).unwrap();
                ids.push(envelope.id);

                let resp = ServerMessage::Response {
                    id: envelope.id,
                    response: Response::ok_with_message("pong"),
                };
                let bytes = encode_server_message(&resp).unwrap();
                stream.write_all(&bytes).await.unwrap();
            }

            // Verify IDs are monotonically increasing
            assert_eq!(ids, vec![1, 2, 3]);
        });

        let client = Client::connect(&sock).await.unwrap();
        let (_, fut) = client.send_request(Request::Ping).unwrap();
        let _ = fut.await.unwrap();
        let (_, fut) = client.send_request(Request::Ping).unwrap();
        let _ = fut.await.unwrap();
        let (_, fut) = client.send_request(Request::Ping).unwrap();
        let _ = fut.await.unwrap();
    }

    #[tokio::test]
    async fn send_request_returns_matching_response() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("test.sock");

        let listener = UnixListener::bind(&sock).unwrap();
        mock_server_one_shot(listener, |id| ServerMessage::Response {
            id,
            response: Response::ok_with_message("hello from server"),
        })
        .await;

        // Give server time to spawn
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let client = Client::connect(&sock).await.unwrap();
        let (_, fut) = client.send_request(Request::Ping).unwrap();
        let response = fut.await.unwrap();

        match response {
            Response::Ok { message, .. } => {
                assert_eq!(message, Some("hello from server".into()));
            }
            _ => panic!("Expected Ok response"),
        }
    }

    #[tokio::test]
    async fn disconnection_detected_for_inflight_request() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("test.sock");

        let listener = UnixListener::bind(&sock).unwrap();

        // Server accepts, reads the request, then disconnects WITHOUT replying
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();

            // Read the request but don't reply
            let mut len_buf = [0u8; 4];
            let _ = stream.read_exact(&mut len_buf).await;
            let msg_len = u32::from_be_bytes(len_buf) as usize;
            let mut payload = vec![0u8; msg_len];
            let _ = stream.read_exact(&mut payload).await;

            // Close without replying — the client's pending request should get Disconnected
            drop(stream);
        });

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let client = Client::connect(&sock).await.unwrap();

        // The request is sent, the server reads it and disconnects
        // The client reader detects EOF, clears pending, dropping the oneshot sender
        // which causes the awaiting rx to return Err → Disconnected
        let (_, fut) = client.send_request(Request::Ping).unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            fut,
        ).await;

        match result {
            Ok(Err(ClientError::Disconnected)) => {} // Expected
            Ok(Err(e)) => panic!("Expected Disconnected, got: {}", e),
            Ok(Ok(_)) => panic!("Expected error, got Ok response"),
            Err(_) => panic!("Timed out waiting for disconnect detection"),
        }
    }

    #[tokio::test]
    async fn connect_to_nonexistent_socket_fails() {
        let result = Client::connect(Path::new("/tmp/nonexistent_kepler_test_sock_12345")).await;
        assert!(result.is_err());
        match result {
            Err(ClientError::Connect(_)) => {} // expected
            Err(e) => panic!("Expected Connect error, got: {}", e),
            Ok(_) => panic!("Expected error, got Ok"),
        }
    }

    #[tokio::test]
    async fn is_daemon_running_returns_false_for_missing_socket() {
        let running = Client::is_daemon_running(Path::new("/tmp/nonexistent_kepler_test_12345")).await;
        assert!(!running);
    }
}
