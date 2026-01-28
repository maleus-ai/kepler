use std::path::{Path, PathBuf};

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

use crate::{
    errors::ClientError,
    protocol::{FRAME_DELIMITER, MAX_MESSAGE_SIZE, Request, Response, decode_response, encode_request},
};

pub type Result<T> = std::result::Result<T, ClientError>;

pub struct Client {
    stream: UnixStream,
}

impl Client {
    /// Connect to the daemon at the given socket path
    pub async fn connect(socket_path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(socket_path)
            .await
            .map_err(ClientError::Connect)?;
        Ok(Self { stream })
    }

    /// Check if daemon is running by attempting to connect
    pub async fn is_daemon_running(socket_path: &Path) -> bool {
        if !socket_path.exists() {
            return false;
        }

        match UnixStream::connect(socket_path).await {
            Ok(stream) => {
                // Try to send a ping
                let mut client = Self { stream };
                matches!(
                    client.send_request(&Request::Ping).await,
                    Ok(Response::Ok { .. })
                )
            }
            Err(_) => false,
        }
    }

    /// Send a request and receive a response
    async fn send_request(&mut self, request: &Request) -> Result<Response> {
        let bytes = encode_request(request)?;
        self.stream
            .write_all(&bytes)
            .await
            .map_err(|e| ClientError::Send {
                request: request.clone(),
                source: e,
            })?;

        let line = {
            let mut reader = BufReader::new(&mut self.stream);
            let mut line: Vec<u8> = Vec::new();

            // Read response with size limit
            loop {
                let bytes_read = reader
                    .read_until(FRAME_DELIMITER, &mut line)
                    .await
                    .map_err(|e| ClientError::Receive {
                        request: request.clone(),
                        source: e,
                    })?;

                if bytes_read == 0 {
                    break; // EOF
                }

                // Check message size limit
                if line.len() > MAX_MESSAGE_SIZE {
                    return Err(ClientError::MessageTooLarge);
                }

                // Check if we found the delimiter
                if line.last() == Some(&FRAME_DELIMITER) {
                    break;
                }
            }

            if line.last() == Some(&FRAME_DELIMITER) {
                line.pop();
            }

            line
        };

        Ok(decode_response(&line)?)
    }

    /// Start services for a config
    pub async fn start(
        &mut self,
        config_path: PathBuf,
        service: Option<String>,
    ) -> Result<Response> {
        self.send_request(&Request::Start {
            config_path,
            service,
        })
        .await
    }

    /// Stop services for a config
    pub async fn stop(
        &mut self,
        config_path: PathBuf,
        service: Option<String>,
        clean: bool,
    ) -> Result<Response> {
        self.send_request(&Request::Stop {
            config_path,
            service,
            clean,
        })
        .await
    }

    /// Restart services for a config
    pub async fn restart(
        &mut self,
        config_path: PathBuf,
        service: Option<String>,
    ) -> Result<Response> {
        self.send_request(&Request::Restart {
            config_path,
            service,
        })
        .await
    }

    /// Get status (for a specific config or all configs)
    pub async fn status(&mut self, config_path: Option<PathBuf>) -> Result<Response> {
        self.send_request(&Request::Status { config_path }).await
    }

    /// List all loaded configs
    pub async fn list_configs(&mut self) -> Result<Response> {
        self.send_request(&Request::ListConfigs).await
    }

    /// Unload a config
    pub async fn unload_config(&mut self, config_path: PathBuf) -> Result<Response> {
        self.send_request(&Request::UnloadConfig { config_path })
            .await
    }

    /// Get logs for a config
    pub async fn logs(
        &mut self,
        config_path: PathBuf,
        service: Option<String>,
        follow: bool,
        lines: usize,
    ) -> Result<Response> {
        self.send_request(&Request::Logs {
            config_path,
            service,
            follow,
            lines,
        })
        .await
    }

    /// Get logs with pagination (for large log responses)
    pub async fn logs_chunk(
        &mut self,
        config_path: PathBuf,
        service: Option<String>,
        offset: usize,
        limit: usize,
    ) -> Result<Response> {
        self.send_request(&Request::LogsChunk {
            config_path,
            service,
            offset,
            limit,
        })
        .await
    }

    /// Shutdown the daemon
    pub async fn shutdown(&mut self) -> Result<Response> {
        self.send_request(&Request::Shutdown).await
    }

    /// Ping the daemon
    #[allow(dead_code)]
    pub async fn ping(&mut self) -> Result<Response> {
        self.send_request(&Request::Ping).await
    }

    /// Prune all stopped/orphaned config state directories
    pub async fn prune(&mut self, force: bool, dry_run: bool) -> Result<Response> {
        self.send_request(&Request::Prune { force, dry_run }).await
    }
}
