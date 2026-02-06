#[cfg(not(unix))]
compile_error!("kepler-protocol server requires a unix target for socket security (peer credentials, file permissions)");

use std::{future::Future, path::PathBuf, sync::Arc};

use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::mpsc,
};
use tracing::{debug, error, info};

use crate::{
    errors::ServerError,
    protocol::{FRAME_DELIMITER, MAX_MESSAGE_SIZE, Request, Response, decode_request, encode_response},
};

pub type Result<T> = std::result::Result<T, ServerError>;
pub type ShutdownTx = mpsc::Sender<()>;

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
    mut stream: UnixStream,
    shutdown_tx: mpsc::Sender<()>,
) -> Result<()>
where
    F: Fn(Request, ShutdownTx) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send,
{
    debug!("Client connected, reading request...");

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
            let response = Response::error(format!(
                "Unauthorized: UID {} is not allowed to connect",
                cred.uid()
            ));
            let bytes = encode_response(&response)?;
            stream.write_all(&bytes).await.map_err(ServerError::Send)?;
            return Err(ServerError::Unauthorized {
                client_uid: cred.uid(),
                daemon_uid,
            });
        }
        debug!("Peer credentials verified: UID {}", cred.uid());
    }

    let line = {
        let mut reader = BufReader::new((&mut stream).take(MAX_MESSAGE_SIZE as u64 + 1));
        let mut line = Vec::new();

        // Read request with size limit (reader is capped at MAX_MESSAGE_SIZE+1)
        loop {
            let byte_read = reader
                .read_until(FRAME_DELIMITER, &mut line)
                .await
                .map_err(ServerError::Receive)?;

            if byte_read == 0 {
                break; // EOF
            }

            // Check message size limit
            if line.len() > MAX_MESSAGE_SIZE {
                debug!("Request exceeds maximum message size: {} bytes", line.len());
                let response = Response::error(format!(
                    "Request exceeds maximum message size of {} bytes",
                    MAX_MESSAGE_SIZE
                ));
                let bytes = encode_response(&response)?;
                stream.write_all(&bytes).await.map_err(ServerError::Send)?;
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

        line
    };

    debug!("Read {} bytes from client", line.len());

    // Parse request
    let request = match decode_request(&line) {
        Ok(req) => req,
        Err(e) => {
            debug!("Failed to parse request: {}", e);
            let response = Response::error(format!("Invalid request: {}", e));
            let bytes = encode_response(&response)?;
            stream.write_all(&bytes).await.map_err(ServerError::Send)?;
            return Ok(());
        }
    };

    debug!("Received request: {:?}", request);

    let response = handler(request, shutdown_tx).await;

    let bytes = encode_response(&response)?;
    stream.write_all(&bytes).await.map_err(ServerError::Send)?;

    Ok(())
}
