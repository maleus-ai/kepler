use std::path::PathBuf;

use thiserror::Error;

use crate::protocol::Request;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("failed to encode message: {0}")]
    Encode(#[source] serde_json::Error),

    #[error("failed to decode message: {0}")]
    Decode(#[source] serde_json::Error),

    #[error("invalid UTF-8 in message: {0}")]
    InvalidUtf8(#[from] std::str::Utf8Error),
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("failed to create socket: {0}")]
    Connect(#[source] std::io::Error),

    #[error("failed to send request: {request:?} {source}")]
    Send {
        request: Request,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to receive response: {request:?} {source}")]
    Receive {
        request: Request,
        #[source]
        source: std::io::Error,
    },

    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),
}

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("stale socket found at {socket_path} cannot be deleted: {source}")]
    StaleSocket {
        socket_path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("cannot bind unix socket at {socket_path}: {source}")]
    Bind {
        socket_path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to set socket permissions at {socket_path}: {source}")]
    SocketPermissions {
        socket_path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to verify peer credentials: {0}")]
    PeerCredentials(#[source] std::io::Error),

    #[error("unauthorized connection: UID {client_uid} does not match daemon UID {daemon_uid}")]
    Unauthorized { client_uid: u32, daemon_uid: u32 },

    #[error("failed to send response: {0}")]
    Send(#[source] std::io::Error),

    #[error("failed to receive request: {0}")]
    Receive(#[source] std::io::Error),

    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),
}
