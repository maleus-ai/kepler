#[cfg(not(unix))]
compile_error!("kepler-protocol server requires a unix target for socket security (peer credentials, file permissions)");

use std::{future::Future, path::PathBuf, sync::Arc};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::mpsc,
};
use tracing::{debug, error, info, warn};

use crate::{
    errors::ServerError,
    protocol::{
        MAX_MESSAGE_SIZE, Response, ServerMessage,
        decode_envelope, encode_server_message,
    },
};

/// Environment variable that overrides the default state directory.
/// When set, the server uses legacy UID-based auth (for tests without kepler group).
const KEPLER_DAEMON_PATH_ENV: &str = "KEPLER_DAEMON_PATH";

/// Resolve the GID of the "kepler" group.
fn resolve_kepler_group_gid() -> std::result::Result<u32, ServerError> {
    nix::unistd::Group::from_name("kepler")
        .map_err(|e| ServerError::GroupResolution(format!("failed to look up kepler group: {}", e)))?
        .map(|g| g.gid.as_raw())
        .ok_or_else(|| ServerError::GroupResolution("kepler group does not exist".into()))
}

/// Check if a process (by PID) has a given GID in its supplementary groups.
/// Reads /proc/<pid>/status to get the full group list.
fn pid_has_supplementary_gid(pid: u32, target_gid: u32) -> bool {
    let status_path = format!("/proc/{}/status", pid);
    let content = match std::fs::read_to_string(&status_path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    for line in content.lines() {
        if let Some(groups_str) = line.strip_prefix("Groups:") {
            return groups_str
                .split_whitespace()
                .filter_map(|s| s.parse::<u32>().ok())
                .any(|gid| gid == target_gid);
        }
    }
    false
}

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

        // Set socket permissions to owner+group (0o660) for kepler group access
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                &self.socket_path,
                std::fs::Permissions::from_mode(0o660),
            )
            .map_err(|e| ServerError::SocketPermissions {
                socket_path: self.socket_path.clone(),
                source: e,
            })?;

            // chown socket to root:kepler (skip in test mode when KEPLER_DAEMON_PATH is set)
            if std::env::var(KEPLER_DAEMON_PATH_ENV).is_err() {
                let kepler_gid = resolve_kepler_group_gid()?;
                nix::unistd::chown(
                    &self.socket_path,
                    Some(nix::unistd::Uid::from_raw(0)),
                    Some(nix::unistd::Gid::from_raw(kepler_gid)),
                )
                .map_err(|e| ServerError::SocketOwnership {
                    socket_path: self.socket_path.clone(),
                    source: e.into(),
                })?;
            }
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

    // Verify peer credentials
    #[cfg(unix)]
    {
        let cred = stream.peer_cred().map_err(ServerError::PeerCredentials)?;

        if std::env::var(KEPLER_DAEMON_PATH_ENV).is_ok() {
            // Test mode: use legacy same-UID auth (no kepler group required)
            let daemon_uid = nix::unistd::getuid().as_raw();
            if cred.uid() != daemon_uid {
                debug!(
                    "Unauthorized connection attempt (test mode): client UID {} != daemon UID {}",
                    cred.uid(),
                    daemon_uid
                );
                return Err(ServerError::Unauthorized {
                    client_uid: cred.uid(),
                    kepler_gid: 0,
                });
            }
            debug!("Peer credentials verified (test mode): UID {}", cred.uid());
        } else {
            // Production mode: root always allowed, otherwise check kepler group membership
            if cred.uid() == 0 {
                debug!("Peer credentials verified: root (UID 0)");
            } else {
                let kepler_gid = resolve_kepler_group_gid()?;
                let in_group = cred.gid() == kepler_gid
                    || cred.pid().is_some_and(|pid| pid_has_supplementary_gid(pid as u32, kepler_gid));

                if !in_group {
                    debug!(
                        "Unauthorized connection attempt: UID {} not in kepler group (GID {})",
                        cred.uid(),
                        kepler_gid
                    );
                    return Err(ServerError::Unauthorized {
                        client_uid: cred.uid(),
                        kepler_gid,
                    });
                }
                debug!("Peer credentials verified: UID {} in kepler group", cred.uid());
            }
        }
    }

    // Split stream into read/write halves
    let (read_half, mut write_half) = stream.into_split();

    // Create bounded channel for outgoing messages
    let (write_tx, mut write_rx) = mpsc::channel::<Vec<u8>>(WRITER_CHANNEL_CAPACITY);

    // Spawn writer task: receives encoded bytes and writes to stream
    let writer_task = tokio::spawn(async move {
        while let Some(bytes) = write_rx.recv().await {
            let len = bytes.len();
            let t_write = std::time::Instant::now();
            if let Err(e) = write_half.write_all(&bytes).await {
                warn!("Failed to write to client: {}", e);
                break;
            }
            let write_ms = t_write.elapsed().as_secs_f64() * 1000.0;
            if len > 100_000 {
                eprintln!(
                    "[writer-perf] socket_write {:.1}ms ({:.2} MB)",
                    write_ms,
                    len as f64 / (1024.0 * 1024.0),
                );
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
                debug!("Client disconnected (EOF)");
                drop(write_tx);
                let _ = writer_task.await;
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
            let t_encode = std::time::Instant::now();
            match encode_server_message(&msg) {
                Ok(bytes) => {
                    let encode_ms = t_encode.elapsed().as_secs_f64() * 1000.0;
                    let byte_len = bytes.len();
                    let t_send = std::time::Instant::now();
                    if let Err(e) = write_tx.send(bytes).await {
                        debug!("Failed to send response for request {}: {}", request_id, e);
                    }
                    let send_ms = t_send.elapsed().as_secs_f64() * 1000.0;
                    if byte_len > 100_000 {
                        eprintln!(
                            "[server-perf] id={} | encode {:.1}ms ({:.2} MB) | chan_send {:.1}ms",
                            request_id,
                            encode_ms,
                            byte_len as f64 / (1024.0 * 1024.0),
                            send_ms,
                        );
                    }
                }
                Err(e) => {
                    error!("Failed to encode response for request {}: {}", request_id, e);
                }
            }
        });
    }
}
