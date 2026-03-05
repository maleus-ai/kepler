//! Process containment using cgroup v2 (Linux) with killpg fallback.
//!
//! `ContainmentManager` wraps the detection and lifecycle of cgroup-based
//! process containment. On Linux with cgroup v2 available and writable,
//! it uses cgroups for deterministic process tracking and cleanup.
//! Otherwise, it falls back to process-group-based signaling (killpg).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::process::{kill_process_by_pid, validate_running_process};

/// Max retries for kill_cgroup on pre-5.14 kernels (enumerate+kill fallback).
const KILL_CGROUP_RETRIES: usize = 3;

/// Sleep between kill_cgroup retries to let the kernel reap processes.
const KILL_CGROUP_RETRY_DELAY: Duration = Duration::from_millis(50);

/// Sleep after killing cgroup processes before retrying rmdir.
const CLEANUP_RETRY_DELAY: Duration = Duration::from_millis(50);

/// Process containment manager.
///
/// Detected once at startup, cached, and threaded through spawn/kill/restart.
#[derive(Clone)]
pub struct ContainmentManager {
    inner: Arc<ContainmentInner>,
}

enum ContainmentStrategy {
    /// Linux cgroup v2 — deterministic process tracking
    CgroupV2 { kepler_root: PathBuf },
    /// killpg fallback — process group signaling
    ProcessGroup,
}

struct ContainmentInner {
    strategy: ContainmentStrategy,
}

impl ContainmentManager {
    /// Detect available containment strategy.
    ///
    /// Tries cgroup v2 first (Linux only), falls back to process groups.
    pub fn detect() -> Self {
        let strategy = match kepler_unix::cgroup::detect_cgroupv2() {
            Some(kepler_root) => {
                info!("Process containment: cgroup v2 (root: {:?})", kepler_root);
                ContainmentStrategy::CgroupV2 { kepler_root }
            }
            None => {
                info!("Process containment: process groups (killpg fallback)");
                ContainmentStrategy::ProcessGroup
            }
        };

        ContainmentManager {
            inner: Arc::new(ContainmentInner { strategy }),
        }
    }

    /// Whether cgroup v2 containment is active.
    pub fn has_cgroup(&self) -> bool {
        matches!(self.inner.strategy, ContainmentStrategy::CgroupV2 { .. })
    }

    /// Prepare for spawning a service (creates cgroup directory if applicable).
    pub fn prepare_spawn(&self, config_hash: &str, service_name: &str) {
        if let ContainmentStrategy::CgroupV2 { ref kepler_root } = self.inner.strategy {
            match kepler_unix::cgroup::create_service_cgroup(kepler_root, config_hash, service_name)
            {
                Ok(path) => {
                    debug!("Created cgroup for {}/{}: {:?}", config_hash, service_name, path);
                }
                Err(e) => {
                    warn!(
                        "Failed to create cgroup for {}/{}: {}",
                        config_hash, service_name, e
                    );
                }
            }
        }
    }

    /// Register a PID in the service's cgroup after spawn.
    pub fn register_pid(&self, config_hash: &str, service_name: &str, pid: u32) {
        if let ContainmentStrategy::CgroupV2 { ref kepler_root } = self.inner.strategy {
            let cgroup_path =
                kepler_unix::cgroup::service_cgroup_path(kepler_root, config_hash, service_name);
            if let Err(e) = kepler_unix::cgroup::add_pid_to_cgroup(&cgroup_path, pid) {
                warn!(
                    "Failed to add PID {} to cgroup {:?}: {}",
                    pid, cgroup_path, e
                );
            } else {
                debug!("Added PID {} to cgroup {:?}", pid, cgroup_path);
            }
        }
    }

    /// Send a signal to a service's process tree (graceful shutdown).
    ///
    /// Only signals the process group — does NOT force-kill or touch cgroups.
    /// Use `force_kill_service` after a timeout if the process doesn't exit.
    pub fn signal_service(&self, pid: u32, signal_num: i32) {
        if let Err(e) = kepler_unix::process_tree::signal_process_tree(pid, signal_num) {
            warn!("Failed to signal process tree {}: {}", pid, e);
        }
    }

    /// Force-kill a service and all its descendants.
    ///
    /// With cgroup v2: kills the entire cgroup with retry loop (catches processes
    /// that escaped the process group or forked between enumerate and kill).
    /// With fallback: SIGKILL via killpg.
    pub async fn force_kill_service(&self, config_hash: &str, service_name: &str, pid: u32) {
        if let ContainmentStrategy::CgroupV2 { ref kepler_root } = self.inner.strategy {
            let cgroup_path =
                kepler_unix::cgroup::service_cgroup_path(kepler_root, config_hash, service_name);
            Self::kill_cgroup_with_retry(&cgroup_path).await;
        } else if let Err(e) = kepler_unix::process_tree::force_kill_process_tree(pid) {
            warn!("Failed to force kill process tree {}: {}", pid, e);
        }
    }

    /// Kill all processes in a cgroup, retrying to catch fork races on pre-5.14 kernels.
    async fn kill_cgroup_with_retry(cgroup_path: &Path) {
        for attempt in 0..KILL_CGROUP_RETRIES {
            if let Err(e) = kepler_unix::cgroup::kill_cgroup(cgroup_path) {
                warn!("Failed to kill cgroup {:?}: {}", cgroup_path, e);
                return;
            }
            // On kernel 5.14+ cgroup.kill is atomic — no retry needed.
            // For the enumerate+kill fallback, sleep to let the kernel reap, then
            // check if any processes survived (fork race).
            tokio::time::sleep(KILL_CGROUP_RETRY_DELAY).await;
            let remaining = kepler_unix::cgroup::enumerate_cgroup_pids(cgroup_path);
            if remaining.is_empty() {
                return;
            }
            debug!(
                "kill_cgroup retry {}: {} PIDs still in {:?}",
                attempt + 1,
                remaining.len(),
                cgroup_path,
            );
        }
    }

    /// Kill orphaned processes for a service from a previous daemon instance.
    ///
    /// With cgroup v2: enumerates the cgroup and kills all PIDs (no validation needed).
    ///
    /// Without cgroup: validates PID ownership/timing, then kills via killpg.
    pub async fn kill_orphans(
        &self,
        config_hash: &str,
        service_name: &str,
        pid: Option<u32>,
        started_at: Option<i64>,
    ) {
        if let ContainmentStrategy::CgroupV2 { ref kepler_root } = self.inner.strategy {
            let cgroup_path =
                kepler_unix::cgroup::service_cgroup_path(kepler_root, config_hash, service_name);
            let pids = kepler_unix::cgroup::enumerate_cgroup_pids(&cgroup_path);
            if !pids.is_empty() {
                info!(
                    "Killing {} orphaned processes in cgroup for {}/{}",
                    pids.len(),
                    config_hash,
                    service_name
                );
                Self::kill_cgroup_with_retry(&cgroup_path).await;
            }
        } else {
            // Fallback: validate PID and kill via process group
            if let Some(pid) = pid {
                let is_alive = validate_running_process(pid, started_at);
                if is_alive {
                    info!(
                        "Killing orphaned process {} for service {}",
                        pid, service_name
                    );
                    kill_process_by_pid(pid).await;
                }
            }
        }
    }

    /// Clean up a service's cgroup directory after it stops.
    ///
    /// If `rmdir` returns EBUSY (processes still lingering), kills them and retries.
    pub async fn cleanup_service(&self, config_hash: &str, service_name: &str) {
        if let ContainmentStrategy::CgroupV2 { ref kepler_root } = self.inner.strategy {
            let cgroup_path =
                kepler_unix::cgroup::service_cgroup_path(kepler_root, config_hash, service_name);
            match kepler_unix::cgroup::remove_service_cgroup(&cgroup_path) {
                Ok(()) => {}
                Err(e) if Self::is_ebusy(&e) => {
                    debug!(
                        "remove_service_cgroup EBUSY on {:?}, killing processes and retrying",
                        cgroup_path,
                    );
                    Self::kill_cgroup_with_retry(&cgroup_path).await;
                    tokio::time::sleep(CLEANUP_RETRY_DELAY).await;
                    if let Err(e) = kepler_unix::cgroup::remove_service_cgroup(&cgroup_path) {
                        if !Self::is_ebusy(&e) {
                            debug!("Failed to remove service cgroup {:?}: {}", cgroup_path, e);
                        } else {
                            debug!(
                                "remove_service_cgroup still EBUSY after retry on {:?}, ignoring",
                                cgroup_path,
                            );
                        }
                    }
                }
                Err(e) => {
                    debug!(
                        "Failed to remove service cgroup {:?}: {}",
                        cgroup_path, e
                    );
                }
            }
        }
    }

    /// Clean up a config-level cgroup directory (after all services stopped with --clean).
    pub fn cleanup_config(&self, config_hash: &str) {
        if let ContainmentStrategy::CgroupV2 { ref kepler_root } = self.inner.strategy
            && let Err(e) = kepler_unix::cgroup::remove_config_cgroup(kepler_root, config_hash)
        {
            debug!(
                "Failed to remove config cgroup for {}: {}",
                config_hash, e
            );
        }
    }

    /// Kill all orphaned cgroups for a config hash (used when no expanded config exists).
    pub async fn kill_orphans_from_cgroups(&self, config_hash: &str) {
        if let ContainmentStrategy::CgroupV2 { ref kepler_root } = self.inner.strategy {
            let services = kepler_unix::cgroup::list_service_cgroups(kepler_root, config_hash);
            for service_name in &services {
                let cgroup_path = kepler_unix::cgroup::service_cgroup_path(
                    kepler_root,
                    config_hash,
                    service_name,
                );
                let pids = kepler_unix::cgroup::enumerate_cgroup_pids(&cgroup_path);
                if !pids.is_empty() {
                    info!(
                        "Killing {} orphaned processes in cgroup for {}/{}",
                        pids.len(),
                        config_hash,
                        service_name
                    );
                    Self::kill_cgroup_with_retry(&cgroup_path).await;
                }
                if let Err(e) = kepler_unix::cgroup::remove_service_cgroup(&cgroup_path) {
                    debug!("Failed to remove cgroup {:?}: {}", cgroup_path, e);
                }
            }
            self.cleanup_config(config_hash);
        }
    }

    #[cfg(target_os = "linux")]
    fn is_ebusy(e: &std::io::Error) -> bool {
        e.raw_os_error() == Some(libc::EBUSY)
    }

    #[cfg(not(target_os = "linux"))]
    fn is_ebusy(_e: &std::io::Error) -> bool {
        false
    }
}
