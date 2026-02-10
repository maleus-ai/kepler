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
        MAX_MESSAGE_SIZE, ProgressEvent, Response, ServerEvent, ServerMessage,
        decode_envelope, encode_server_message,
    },
};

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
    parse_groups_from_status(&content, target_gid)
}

/// Parse the Groups line from /proc/<pid>/status content.
/// Returns true if `target_gid` appears in the supplementary groups.
fn parse_groups_from_status(content: &str, target_gid: u32) -> bool {
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
}

pub struct Server<F, Fut>
where
    F: Fn(Request, ShutdownTx, ProgressSender) -> Fut + Send + Sync + 'static,
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
    F: Fn(Request, ShutdownTx, ProgressSender) -> Fut + Send + Sync + 'static,
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

            // chown socket to root:kepler
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
    F: Fn(Request, ShutdownTx, ProgressSender) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send,
{
    debug!("Client connected");

    // Verify peer credentials
    #[cfg(unix)]
    {
        let cred = stream.peer_cred().map_err(ServerError::PeerCredentials)?;

        // Root always allowed, otherwise check kepler group membership
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

        // Spawn handler task
        let handler = Arc::clone(&handler);
        let shutdown_tx = shutdown_tx.clone();
        let write_tx = write_tx.clone();
        tokio::spawn(async move {
            let progress_sender = ProgressSender::new(write_tx.clone(), request_id);
            let response = handler(request, shutdown_tx, progress_sender).await;
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
mod tests {
    use super::*;
    use crate::protocol::{ServicePhase, decode_server_message};

    // ========================================================================
    // parse_groups_from_status tests
    // ========================================================================

    #[test]
    fn parse_groups_single_matching_gid() {
        let content = "\
Name:\tcat
Pid:\t1234
Groups:\t1000
";
        assert!(parse_groups_from_status(content, 1000));
    }

    #[test]
    fn parse_groups_multiple_gids_match() {
        let content = "\
Name:\tbash
Pid:\t5678
Groups:\t1000 27 44 100
";
        assert!(parse_groups_from_status(content, 44));
        assert!(parse_groups_from_status(content, 1000));
        assert!(parse_groups_from_status(content, 100));
    }

    #[test]
    fn parse_groups_no_match() {
        let content = "\
Name:\tbash
Pid:\t5678
Groups:\t1000 27 44
";
        assert!(!parse_groups_from_status(content, 9999));
    }

    #[test]
    fn parse_groups_missing_groups_line() {
        let content = "\
Name:\tbash
Pid:\t5678
Uid:\t1000\t1000\t1000\t1000
Gid:\t1000\t1000\t1000\t1000
";
        assert!(!parse_groups_from_status(content, 1000));
    }

    #[test]
    fn parse_groups_empty_groups_line() {
        let content = "\
Name:\tbash
Groups:\t
Pid:\t5678
";
        assert!(!parse_groups_from_status(content, 1000));
    }

    #[test]
    fn parse_groups_empty_content() {
        assert!(!parse_groups_from_status("", 1000));
    }

    #[test]
    fn parse_groups_with_tabs_and_spaces() {
        // Real /proc/status uses tabs after the field name
        let content = "Groups:\t1000  27\t44\t\t100\n";
        assert!(parse_groups_from_status(content, 27));
        assert!(parse_groups_from_status(content, 100));
    }

    // ========================================================================
    // ProgressSender tests
    // ========================================================================

    #[tokio::test]
    async fn progress_sender_encodes_event() {
        let (tx, mut rx) = mpsc::channel(16);
        let sender = ProgressSender::new(tx, 42);

        sender.send(ProgressEvent {
            service: "web".into(),
            phase: ServicePhase::Starting,
        }).await;

        let bytes = rx.recv().await.unwrap();
        // Should be a valid length-prefixed server message
        assert!(bytes.len() > 4);
        let len = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        assert_eq!(len, bytes.len() - 4);

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

    #[tokio::test]
    async fn progress_sender_ignores_closed_channel() {
        let (tx, rx) = mpsc::channel(1);
        let sender = ProgressSender::new(tx, 1);
        drop(rx); // Close receiver

        // Should not panic
        sender.send(ProgressEvent {
            service: "web".into(),
            phase: ServicePhase::Starting,
        }).await;
    }
}
