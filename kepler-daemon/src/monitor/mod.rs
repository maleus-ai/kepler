//! Resource monitoring for running services.
//!
//! Periodically collects CPU and memory metrics for each running service's
//! process tree and stores them in a SQLite database.
//!
//! # Architecture
//!
//! The monitor is split into three components:
//!
//! - **Collector** (`collector.rs`): An async tokio task that periodically
//!   samples CPU/memory from sysinfo/cgroups and sends `InsertMetrics`
//!   commands through an `mpsc` channel.
//!
//! - **Writer** (`writer.rs`): A dedicated `std::thread` actor that owns the
//!   SQLite write connection. Receives metrics from the collector via the
//!   channel, flushes them to disk, and runs time-budgeted retention cleanup.
//!   This mirrors the `LogStoreActor` pattern.
//!
//! - **Query** (`query.rs`): Read-only functions that open a separate
//!   `SQLITE_OPEN_READ_ONLY` connection per call for concurrent reads.
//!
//! # Shutdown
//!
//! Two shutdown modes are supported:
//!
//! - **Graceful** (`MonitorHandle::shutdown`): Flushes pending metrics, runs
//!   `PRAGMA optimize`, then exits. The collector exits when its channel
//!   disconnects.
//!
//! - **Discard** (`MonitorHandle::shutdown_discard`): Sets an atomic discard
//!   flag, interrupts any running SQLite query, aborts the collector, and
//!   sends a `Shutdown` command to unblock the writer. Pending metrics are
//!   dropped without flushing.

mod collector;
pub mod query;
pub mod writer;

pub use query::{
    monitor_query_dsl, open_monitor_db_readonly, query_filtered, query_history, query_latest,
};
pub use writer::{MonitorHandle, MonitorShutdownToken};

/// A single metrics sample for one service, collected by the collector task
/// and sent to the writer thread for storage.
pub struct ServiceMetrics {
    pub service: String,
    pub cpu_percent: f32,
    pub memory_rss: u64,
    pub memory_vss: u64,
    pub pids: Vec<u32>,
}

/// Spawn the monitor (writer thread + collector task). Returns a handle for
/// shutdown control.
pub fn spawn_monitor(
    config: crate::config::MonitorConfig,
    shutdown_token: std::sync::Arc<MonitorShutdownToken>,
    handle: crate::config_actor::ConfigActorHandle,
    state_dir: std::path::PathBuf,
    containment: crate::containment::ContainmentManager,
    storage_mode: crate::config::StorageMode,
) -> MonitorHandle {
    MonitorHandle::spawn(config, shutdown_token, handle, state_dir, containment, storage_mode)
}
