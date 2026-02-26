#[cfg(not(unix))]
compile_error!("kepler-protocol server requires a unix target for socket security (peer credentials, file permissions)");

use std::{future::Future, path::PathBuf, sync::Arc};
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::mpsc,
};
use tracing::{debug, error, info, warn};

use crate::{
    errors::ServerError,
    protocol::{
        MAX_MESSAGE_SIZE, ProgressEvent, Response, ServerEvent, ServerMessage,
        decode_envelope, encode_server_message,
    },
};

pub type Result<T> = std::result::Result<T, ServerError>;
pub type ShutdownTx = mpsc::Sender<()>;

pub use kepler_unix::credentials::PeerCredentials;

/// Bounded channel capacity for the per-connection writer task.
const WRITER_CHANNEL_CAPACITY: usize = 256;

/// Default maximum number of concurrent connections.
const DEFAULT_MAX_CONNECTIONS: usize = 1024;

/// Sender for progress events from a request handler back to the client.
///
/// Wraps the shared write channel and the request ID so handlers can
/// emit per-service progress updates without knowing about framing.
#[derive(Clone)]
pub struct ProgressSender {
    write_tx: mpsc::Sender<Vec<u8>>,
    request_id: u64,
}

impl ProgressSender {
    /// Create a new progress sender.
    pub fn new(write_tx: mpsc::Sender<Vec<u8>>, request_id: u64) -> Self {
        Self { write_tx, request_id }
    }

    /// Send a progress event to the client. Fire-and-forget: errors are silently ignored.
    pub async fn send(&self, event: ProgressEvent) {
        let msg = ServerMessage::Event {
            event: ServerEvent::Progress {
                request_id: self.request_id,
                event,
            },
        };
        if let Ok(bytes) = encode_server_message(&msg) {
            let _ = self.write_tx.send(bytes).await;
        }
    }

    /// Send a Ready event to the client. Fire-and-forget: errors are silently ignored.
    pub async fn send_ready(&self) {
        let msg = ServerMessage::Event {
            event: ServerEvent::Ready { request_id: self.request_id },
        };
        if let Ok(bytes) = encode_server_message(&msg) {
            let _ = self.write_tx.send(bytes).await;
        }
    }

    /// Send a Quiescent event to the client. Fire-and-forget: errors are silently ignored.
    pub async fn send_quiescent(&self) {
        let msg = ServerMessage::Event {
            event: ServerEvent::Quiescent { request_id: self.request_id },
        };
        if let Ok(bytes) = encode_server_message(&msg) {
            let _ = self.write_tx.send(bytes).await;
        }
    }

    /// Send an UnhandledFailure event to the client. Fire-and-forget: errors are silently ignored.
    pub async fn send_unhandled_failure(&self, service: String, exit_code: Option<i32>) {
        let msg = ServerMessage::Event {
            event: ServerEvent::UnhandledFailure {
                request_id: self.request_id,
                service,
                exit_code,
            },
        };
        if let Ok(bytes) = encode_server_message(&msg) {
            let _ = self.write_tx.send(bytes).await;
        }
    }

    /// Check if the client has disconnected (the write channel is closed).
    pub fn is_closed(&self) -> bool {
        self.write_tx.is_closed()
    }

    /// Wait until the client disconnects (the write channel is closed).
    pub async fn closed(&self) {
        self.write_tx.closed().await
    }
}

/// Monotonic counter for assigning unique connection IDs.
static CONNECTION_COUNTER: AtomicU64 = AtomicU64::new(1);

pub struct Server<F, Fut>
where
    F: Fn(Request, ShutdownTx, ProgressSender, PeerCredentials) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send,
{
    socket_path: PathBuf,
    handler: Arc<F>,
    shutdown_tx: mpsc::Sender<()>,
    shutdown_rx: mpsc::Receiver<()>,
    on_disconnect: Option<Arc<dyn Fn(u64) + Send + Sync>>,
    connection_semaphore: Arc<tokio::sync::Semaphore>,
}

// Re-export Request so the handler signature compiles
use crate::protocol::Request;

impl<F, Fut> Server<F, Fut>
where
    F: Fn(Request, ShutdownTx, ProgressSender, PeerCredentials) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send,
{
    pub fn new(socket_path: PathBuf, handler: F) -> Result<Self> {
        let (shutdown_tx, shutdown_rx) = mpsc::channel(1);
        Ok(Self {
            socket_path,
            handler: Arc::new(handler),
            shutdown_tx,
            shutdown_rx,
            on_disconnect: None,
            connection_semaphore: Arc::new(tokio::sync::Semaphore::new(DEFAULT_MAX_CONNECTIONS)),
        })
    }

    /// Set a callback that fires when a client disconnects, receiving the connection_id.
    pub fn with_on_disconnect(mut self, cb: impl Fn(u64) + Send + Sync + 'static) -> Self {
        self.on_disconnect = Some(Arc::new(cb));
        self
    }

    pub async fn run(mut self) -> Result<()> {
        // Reject symlinked socket path before any operations
        if self.socket_path.exists() {
            let meta = std::fs::symlink_metadata(&self.socket_path).map_err(|e| {
                ServerError::StaleSocket {
                    socket_path: self.socket_path.clone(),
                    source: e,
                }
            })?;
            if meta.file_type().is_symlink() {
                return Err(ServerError::SocketSymlink {
                    socket_path: self.socket_path.clone(),
                });
            }
        }

        // Remove stale socket file (atomic - avoid TOCTOU race)
        match std::fs::remove_file(&self.socket_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(ServerError::StaleSocket {
                    socket_path: self.socket_path.clone(),
                    source: e,
                })
            }
        }

        let listener = UnixListener::bind(&self.socket_path).map_err(|e| ServerError::Bind {
            socket_path: self.socket_path.clone(),
            source: e,
        })?;

        // Set socket permissions to world-accessible (0o666).
        // Auth is handled at the protocol level: kepler group members get access scoped
        // by ownership and ACL, spawned processes are identified by PID, others are rejected.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                &self.socket_path,
                std::fs::Permissions::from_mode(0o666),
            )
            .map_err(|e| ServerError::SocketPermissions {
                socket_path: self.socket_path.clone(),
                source: e,
            })?;
        }

        loop {
            tokio::select! {
                result = listener.accept() => {
                    match result {
                        Ok((stream,_)) => {
                            let permit = match self.connection_semaphore.clone().acquire_owned().await {
                                Ok(permit) => permit,
                                Err(_) => {
                                    error!("Connection semaphore closed");
                                    break;
                                }
                            };
                            let shutdown_tx = self.shutdown_tx.clone();
                            let handler = Arc::clone(&self.handler);
                            let on_disconnect = self.on_disconnect.clone();
                            let connection_id = CONNECTION_COUNTER.fetch_add(1, Ordering::Relaxed);

                            tokio::spawn(async move {
                                if let Err(e) = handle_client(handler, stream, shutdown_tx, connection_id, on_disconnect).await {
                                    debug!("Client handler error: {}", e);
                                }
                                drop(permit);
                            });
                        },
                        Err(e) => {
                            error!("Failed to accept connection: {}", e);
                        },
                    }
                }
                _ = self.shutdown_rx.recv() => {
                    info!("Server shutdown!");
                    break;
                }
            }
        }

        Ok(())
    }
}

async fn handle_client<F, Fut>(
    handler: Arc<F>,
    stream: UnixStream,
    shutdown_tx: mpsc::Sender<()>,
    connection_id: u64,
    on_disconnect: Option<Arc<dyn Fn(u64) + Send + Sync>>,
) -> Result<()>
where
    F: Fn(Request, ShutdownTx, ProgressSender, PeerCredentials) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send,
{
    debug!("Client connected (connection_id={})", connection_id);

    // Extract peer credentials (uid/gid/pid) for the handler.
    // Auth decisions (kepler group check, PID-based token lookup) are made by the
    // daemon's auth gate, not here.
    let peer_credentials;
    #[cfg(unix)]
    {
        let cred = stream.peer_cred().map_err(ServerError::PeerCredentials)?;
        let pid = cred.pid().map(|p| p as u32);
        debug!("Peer credentials: UID {}, GID {}, PID {:?}", cred.uid(), cred.gid(), pid);

        peer_credentials = PeerCredentials {
            uid: cred.uid(),
            gid: cred.gid(),
            pid,
            connection_id,
            token: None,
        };
    }

    // Split stream into read/write halves
    let (read_half, mut write_half) = stream.into_split();

    // Create bounded channel for outgoing messages
    let (write_tx, mut write_rx) = mpsc::channel::<Vec<u8>>(WRITER_CHANNEL_CAPACITY);

    // Spawn writer task: receives encoded bytes and writes to stream
    let writer_task = tokio::spawn(async move {
        while let Some(bytes) = write_rx.recv().await {
            if let Err(e) = write_half.write_all(&bytes).await {
                warn!("Failed to write to client: {}", e);
                break;
            }
        }
        // Flush before exiting
        let _ = write_half.shutdown().await;
    });

    // Reader loop: read length-prefixed frames
    let mut reader = read_half;

    loop {
        // Read 4-byte length header
        let mut len_buf = [0u8; 4];
        if let Err(e) = reader.read_exact(&mut len_buf).await {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                debug!("Client disconnected (EOF, connection_id={})", connection_id);
                drop(write_tx);
                // Abort the writer task so write_rx is dropped immediately.
                // This unblocks any handler task waiting on progress.closed().
                writer_task.abort();
                // Notify the daemon so it can clean up cursors for this connection
                if let Some(ref cb) = on_disconnect {
                    cb(connection_id);
                }
                return Ok(());
            }
            return Err(ServerError::Receive(e));
        }
        let msg_len = u32::from_be_bytes(len_buf) as usize;

        // Check message size limit
        if msg_len > MAX_MESSAGE_SIZE {
            debug!("Request exceeds maximum message size: {} bytes", msg_len);
            let msg = ServerMessage::Response {
                id: 0,
                response: Response::error(format!(
                    "Request exceeds maximum message size of {} bytes",
                    MAX_MESSAGE_SIZE
                )),
            };
            if let Ok(bytes) = encode_server_message(&msg) {
                let _ = write_tx.send(bytes).await;
            }
            drop(write_tx);
            let _ = writer_task.await;
            return Err(ServerError::MessageTooLarge);
        }

        // Read payload
        let mut payload = vec![0u8; msg_len];
        reader.read_exact(&mut payload).await.map_err(ServerError::Receive)?;

        // Parse envelope
        let envelope = match decode_envelope(&payload) {
            Ok(env) => env,
            Err(e) => {
                warn!("Failed to parse request envelope: {}", e);
                // We don't know the ID, use 0
                let msg = ServerMessage::Response {
                    id: 0,
                    response: Response::error("Invalid request format"),
                };
                if let Ok(bytes) = encode_server_message(&msg) {
                    let _ = write_tx.send(bytes).await;
                }
                continue;
            }
        };

        let request_id = envelope.id;
        let request = envelope.request;
        debug!("Received request id={}: {:?}", request_id, request);

        // Spawn handler task — inject per-request bearer token into peer credentials
        let handler = Arc::clone(&handler);
        let shutdown_tx = shutdown_tx.clone();
        let write_tx = write_tx.clone();
        let request_token = envelope.token;
        tokio::spawn(async move {
            let mut peer = peer_credentials;
            peer.token = request_token;
            let progress_sender = ProgressSender::new(write_tx.clone(), request_id);
            let response = handler(request, shutdown_tx, progress_sender, peer).await;
            let msg = ServerMessage::Response {
                id: request_id,
                response,
            };
            match encode_server_message(&msg) {
                Ok(bytes) => {
                    if let Err(e) = write_tx.send(bytes).await {
                        debug!("Failed to send response for request {}: {}", request_id, e);
                    }
                }
                Err(e) => {
                    error!("Failed to encode response for request {}: {}", request_id, e);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests;
