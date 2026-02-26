/// Peer credentials extracted from a Unix domain socket connection.
/// Provides the UID, GID, and PID of the connecting process.
#[derive(Debug, Clone, Copy)]
pub struct PeerCredentials {
    pub uid: u32,
    pub gid: u32,
    /// PID of the connecting process (from `SO_PEERCRED`).
    pub pid: Option<u32>,
    /// Unique ID for this connection (assigned by the server).
    pub connection_id: u64,
    /// Bearer token sent by the client (from `KEPLER_TOKEN` env var).
    pub token: Option<[u8; 32]>,
}
