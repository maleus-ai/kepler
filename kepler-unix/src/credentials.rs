/// Peer credentials extracted from a Unix domain socket connection.
/// Provides the UID and GID of the connecting CLI client.
#[derive(Debug, Clone, Copy)]
pub struct PeerCredentials {
    pub uid: u32,
    pub gid: u32,
    /// Unique ID for this connection (assigned by the server).
    pub connection_id: u64,
}
