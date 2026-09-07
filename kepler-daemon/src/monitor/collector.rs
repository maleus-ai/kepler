//! Async metric collection task.
//!
//! Periodically collects CPU and memory metrics for running services via
//! sysinfo / cgroups, and sends them to the writer thread through a channel.
//!
//! PIDs come from two complementary sources, unioned rather than tried in
//! order — neither is complete on its own:
//!
//! - **The service's cgroup** (via the containment manager) catches processes
//!   that detached and were re-parented to init, which a process-tree walk can
//!   no longer reach from the service's main PID.
//! - **A process-tree walk** from the main PID catches children that were
//!   forked before the daemon wrote the leader's PID into `cgroup.procs` and
//!   therefore stayed behind in the daemon's own cgroup.
//!
//! On platforms without cgroups only the second source yields anything.

use std::collections::HashSet;

use sysinfo::{Pid, ProcessesToUpdate, System};
use tokio::task::JoinHandle;

use crate::config::MonitorConfig;
use crate::config_actor::ConfigActorHandle;
use crate::containment::ContainmentManager;

use super::ServiceMetrics;
use super::writer::MonitorCommand;

/// Spawn the metric collector loop. Exits when the writer channel disconnects.
pub(crate) fn spawn_collector(
    config: MonitorConfig,
    handle: ConfigActorHandle,
    tx: std::sync::mpsc::Sender<MonitorCommand>,
    containment: ContainmentManager,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut sys = System::new();
        let config_hash = handle.config_hash().to_string();

        loop {
            tokio::time::sleep(config.interval).await;

            let running = handle.get_running_services().await;
            if running.is_empty() {
                continue;
            }

            // One full refresh per cycle, before any tree walking.
            // `ProcessesToUpdate::Some` only updates processes already in the
            // table — it never inserts new ones — so a walk against a table
            // refreshed that way can never discover a child. Doing this once
            // per cycle rather than once per service also means CPU deltas are
            // measured over the whole interval for every service.
            sys.refresh_processes(ProcessesToUpdate::All, true);

            let now = chrono::Utc::now().timestamp_millis();
            let mut all_metrics = Vec::new();

            for service_name in &running {
                // Source 1: the cgroup (catches detached / re-parented processes).
                let cgroup_pids =
                    containment.enumerate_service_pids(&config_hash, service_name);
                let mut pids = cgroup_pids.clone();

                // Source 2: the process tree below the main PID (catches children
                // that escaped the cgroup by forking before registration).
                if let Some(state) = handle.get_service_state(service_name).await
                    && let Some(pid) = state.pid
                {
                    let mut visited = HashSet::from([pid]);
                    pids.push(pid);
                    collect_descendants(&sys, pid, &mut pids, &mut visited);
                }

                // The two sources overlap; a PID counted twice would double its
                // RSS and CPU contribution.
                pids.sort_unstable();
                pids.dedup();

                if pids.is_empty() {
                    continue;
                }

                let mut total_cpu: f32 = 0.0;
                let mut total_rss: u64 = 0;
                let mut total_vss: u64 = 0;

                for &pid in &pids {
                    if let Some(proc_info) = sys.process(Pid::from_u32(pid)) {
                        total_cpu += proc_info.cpu_usage();
                        total_rss += proc_info.memory();
                        total_vss += proc_info.virtual_memory();
                    }
                }

                // Prefer the kernel's own cgroup accounting for memory: summing
                // per-process RSS double-counts pages shared through fork()
                // copy-on-write, so a pre-fork server with N workers reports
                // roughly N+1 times its real footprint.
                //
                // Only when the cgroup actually holds every PID we found, though.
                // `memory.current` accounts for cgroup members and nothing else,
                // so if some process escaped it (see the module docs), that
                // process's memory would silently vanish from the total. Between
                // over-reporting shared pages and under-reporting a whole
                // process, the RSS sum is the safer answer.
                let cgroup_covers_all_pids = !cgroup_pids.is_empty() && {
                    let in_cgroup: HashSet<u32> = cgroup_pids.iter().copied().collect();
                    pids.iter().all(|p| in_cgroup.contains(p))
                };
                let memory_rss = if cgroup_covers_all_pids {
                    containment
                        .service_memory_current(&config_hash, service_name)
                        .unwrap_or(total_rss)
                } else {
                    total_rss
                };

                all_metrics.push(ServiceMetrics {
                    service: service_name.clone(),
                    cpu_percent: total_cpu,
                    memory_rss,
                    memory_vss: total_vss,
                    pids,
                });
            }

            if all_metrics.is_empty() {
                continue;
            }

            // Send to writer. If the channel is disconnected, the writer
            // has shut down — exit the collector.
            if tx
                .send(MonitorCommand::InsertMetrics {
                    timestamp: now,
                    metrics: all_metrics,
                })
                .is_err()
            {
                break;
            }
        }
    })
}

/// Collect descendant PIDs by walking sysinfo's process tree.
///
/// `visited` tracks which subtrees have already been walked, and is deliberately
/// separate from `result`: a PID can already be in `result` because the cgroup
/// listed it, and we must still descend into its children. Using `result` as the
/// recursion guard would silently drop every grandchild below such a PID.
fn collect_descendants(
    sys: &System,
    parent_pid: u32,
    result: &mut Vec<u32>,
    visited: &mut HashSet<u32>,
) {
    let parent = Pid::from_u32(parent_pid);
    for (pid, proc_info) in sys.processes() {
        if proc_info.parent() != Some(parent) {
            continue;
        }
        let child_pid = pid.as_u32();
        if !visited.insert(child_pid) {
            continue; // subtree already walked
        }
        result.push(child_pid);
        collect_descendants(sys, child_pid, result, visited);
    }
}
