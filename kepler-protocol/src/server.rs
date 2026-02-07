#[cfg(not(unix))]
compile_error!("kepler-protocol server requires a unix target for socket security (peer credentials, file permissions)");

use std::{future::Future, path::PathBuf, sync::Arc};

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::mpsc,
};
use tracing::{debug, error, info, warn};

use crate::{
    errors::ServerError,
    protocol::{
        FRAME_DELIMITER, MAX_MESSAGE_SIZE, Response, ServerMessage,
        decode_envelope, encode_server_message,
    },
};

pub type Result<T> = std::result::Result<T, ServerError>;
pub type ShutdownTx = mpsc::Sender<()>;

/// Bounded channel capacity for the per-connection writer task.
const WRITER_CHANNEL_CAPACITY: usize = 256;

pub struct Server<F, Fut>
where
    F: Fn(Request, ShutdownTx) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send,
{
    socket_path: PathBuf,
    handler: Arc<F>,
    shutdown_tx: mpsc::Sender<()>,
    shutdown_rx: mpsc::Receiver<()>,
}

// Re-export Request so the handler signature compiles
use crate::protocol::Request;

impl<F, Fut> Server<F, Fut>
where
    F: Fn(Request, ShutdownTx) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send,
{
    pub fn new(socket_path: PathBuf, handler: F) -> Result<Self> {
        let (shutdown_tx, shutdown_rx) = mpsc::channel(1);
        Ok(Self {
            socket_path,
            handler: Arc::new(handler),
            shutdown_tx,
            shutdown_rx,
        })
    }

    pub async fn run(mut self) -> Result<()> {
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

        // Set socket permissions to owner-only (0o600) for security
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                &self.socket_path,
                std::fs::Permissions::from_mode(0o600),
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
                            let shutdown_tx = self.shutdown_tx.clone();
                            let handler = Arc::clone(&self.handler);

                            tokio::spawn(async move {
                                if let Err(e) = handle_client(handler, stream, shutdown_tx).await {
                                    debug!("Client handler error: {}", e);
                                }
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
) -> Result<()>
where
    F: Fn(Request, ShutdownTx) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send,
{
    debug!("Client connected");

    // Verify peer credentials - only allow connections from the same user
    #[cfg(unix)]
    {
        let cred = stream.peer_cred().map_err(ServerError::PeerCredentials)?;
        let daemon_uid = nix::unistd::getuid().as_raw();

        if cred.uid() != daemon_uid {
            debug!(
                "Unauthorized connection attempt: client UID {} != daemon UID {}",
                cred.uid(),
                daemon_uid
            );
            return Err(ServerError::Unauthorized {
                client_uid: cred.uid(),
                daemon_uid,
            });
        }
        debug!("Peer credentials verified: UID {}", cred.uid());
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

    // Reader loop: read frames, dispatch to handler
    let mut reader = BufReader::new(read_half);
    let mut line = Vec::new();

    loop {
        line.clear();

        // Read one frame (up to delimiter)
        loop {
            let bytes_read = reader
                .read_until(FRAME_DELIMITER, &mut line)
                .await
                .map_err(ServerError::Receive)?;

            if bytes_read == 0 {
                // EOF - client disconnected
                debug!("Client disconnected (EOF)");
                drop(write_tx);
                let _ = writer_task.await;
                return Ok(());
            }

            // Check message size limit
            if line.len() > MAX_MESSAGE_SIZE {
                debug!("Request exceeds maximum message size: {} bytes", line.len());
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

            // Check if we found the delimiter
            if line.last() == Some(&FRAME_DELIMITER) {
                break;
            }
        }

        // Remove delimiter
        if line.last() == Some(&FRAME_DELIMITER) {
            line.pop();
        }

        // Parse envelope
        let envelope = match decode_envelope(&line) {
            Ok(env) => env,
            Err(e) => {
                debug!("Failed to parse request envelope: {}", e);
                // We don't know the ID, use 0
                let msg = ServerMessage::Response {
                    id: 0,
                    response: Response::error(format!("Invalid request: {}", e)),
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

        // Spawn handler task
        let handler = Arc::clone(&handler);
        let shutdown_tx = shutdown_tx.clone();
        let write_tx = write_tx.clone();
        tokio::spawn(async move {
            let response = handler(request, shutdown_tx).await;
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
