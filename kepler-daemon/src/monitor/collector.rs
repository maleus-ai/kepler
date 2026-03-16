//! Async metric collection task.
//!
//! Periodically collects CPU and memory metrics for running services via
//! sysinfo / cgroups, and sends them to the writer thread through a channel.
//!
//! The collector first tries to enumerate PIDs from cgroups (via the
//! containment manager). If no cgroup PIDs are found, it falls back to
//! the service's main PID from the service state and walks the process
//! tree via sysinfo to find descendants.

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

            let now = chrono::Utc::now().timestamp_millis();
            let mut all_metrics = Vec::new();

            for service_name in &running {
                // Get PIDs from cgroup, or fall back to stored PID
                let mut pids =
                    containment.enumerate_service_pids(&config_hash, service_name);
                if pids.is_empty() {
                    // Fallback: get the main PID from service state
                    if let Some(state) = handle.get_service_state(service_name).await {
                        if let Some(pid) = state.pid {
                            pids.push(pid);
                            // Also collect child processes via sysinfo
                            collect_descendants(&sys, pid, &mut pids);
                        }
                    }
                }

                if pids.is_empty() {
                    continue;
                }

                // Refresh only the processes we care about
                let pids_to_update: Vec<Pid> =
                    pids.iter().map(|&p| Pid::from_u32(p)).collect();
                sys.refresh_processes(
                    ProcessesToUpdate::Some(&pids_to_update),
                    true,
                );

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

                all_metrics.push(ServiceMetrics {
                    service: service_name.clone(),
                    cpu_percent: total_cpu,
                    memory_rss: total_rss,
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
fn collect_descendants(sys: &System, parent_pid: u32, result: &mut Vec<u32>) {
    let parent = Pid::from_u32(parent_pid);
    for (pid, proc_info) in sys.processes() {
        if let Some(ppid) = proc_info.parent() {
            if ppid == parent && !result.contains(&pid.as_u32()) {
                let child_pid = pid.as_u32();
                result.push(child_pid);
                collect_descendants(sys, child_pid, result);
            }
        }
    }
}
