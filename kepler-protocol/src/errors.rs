use std::path::PathBuf;

use thiserror::Error;

use crate::protocol::MAX_MESSAGE_SIZE;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("failed to encode message: {0}")]
    Encode(#[source] bincode::Error),

    #[error("failed to decode message: {0}")]
    Decode(#[source] bincode::Error),

    #[error("message exceeds maximum size of {MAX_MESSAGE_SIZE} bytes")]
    MessageTooLarge,
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("failed to create socket: {0}")]
    Connect(#[source] std::io::Error),

    #[error("failed to send request ({request_type}): {source}")]
    Send {
        request_type: &'static str,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to receive response ({request_type}): {source}")]
    Receive {
        request_type: &'static str,
        #[source]
        source: std::io::Error,
    },

    #[error("response message exceeds maximum size of {MAX_MESSAGE_SIZE} bytes")]
    MessageTooLarge,

    #[error("connection to daemon was lost")]
    Disconnected,

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

    #[error("unauthorized connection: UID {client_uid} is not in kepler group (GID {kepler_gid})")]
    Unauthorized { client_uid: u32, kepler_gid: u32 },

    #[error("failed to resolve kepler group: {0}")]
    GroupResolution(String),

    #[error("failed to set socket ownership at {socket_path}: {source}")]
    SocketOwnership {
        socket_path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to send response: {0}")]
    Send(#[source] std::io::Error),

    #[error("failed to receive request: {0}")]
    Receive(#[source] std::io::Error),

    #[error("request message exceeds maximum size of {MAX_MESSAGE_SIZE} bytes")]
    MessageTooLarge,

    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),
}
