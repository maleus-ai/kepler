//! Async metric collection task.
//!
//! Periodically collects CPU and memory metrics for running services via
//! sysinfo / cgroups, and sends them to the writer thread through a channel.
//!
//! The collector first tries to enumerate PIDs from cgroups (via the
//! containment manager). If no cgroup PIDs are found, it falls back to the
//! service's main PID from the service state and reconstructs the process
//! tree from a full system scan — see `collect_service_tree`.

use std::collections::HashMap;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::task::JoinHandle;

use crate::config::MonitorConfig;
use crate::config_actor::ConfigActorHandle;
use crate::containment::ContainmentManager;

use super::ServiceMetrics;
use super::writer::MonitorCommand;

/// How a service's PID set is obtained for one sample.
enum PidSource {
    /// The service's cgroup listed these PIDs — authoritative, no walk needed.
    Cgroup(Vec<u32>),
    /// No cgroup: the tree has to be reconstructed from this leader PID.
    Tree { leader: u32 },
}

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

            // Resolve where each service's PIDs come from before refreshing, so
            // the refresh can be scoped to what is actually needed.
            let mut sources: Vec<(String, PidSource)> = Vec::new();
            let mut needs_full_scan = false;
            for service_name in &running {
                let cgroup_pids = containment.enumerate_service_pids(&config_hash, service_name);
                if !cgroup_pids.is_empty() {
                    sources.push((service_name.clone(), PidSource::Cgroup(cgroup_pids)));
                } else if let Some(state) = handle.get_service_state(service_name).await
                    && let Some(leader) = state.pid
                {
                    needs_full_scan = true;
                    sources.push((service_name.clone(), PidSource::Tree { leader }));
                }
            }

            if sources.is_empty() {
                continue;
            }

            // A tree walk needs every process in the table, so scan the whole
            // system; with cgroups the PID set is already known and a targeted
            // refresh is enough. One refresh per tick, so sysinfo's CPU deltas
            // span one collection interval.
            let refresh_kind = ProcessRefreshKind::nothing().with_memory().with_cpu();
            if needs_full_scan {
                sys.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind);
            } else {
                let known: Vec<Pid> = sources
                    .iter()
                    .flat_map(|(_, source)| match source {
                        PidSource::Cgroup(pids) => pids.as_slice(),
                        PidSource::Tree { .. } => &[],
                    })
                    .map(|&pid| Pid::from_u32(pid))
                    .collect();
                sys.refresh_processes_specifics(
                    ProcessesToUpdate::Some(&known),
                    true,
                    refresh_kind,
                );
            }

            let index = needs_full_scan.then(|| ProcessIndex::build(&sys));

            let now = chrono::Utc::now().timestamp_millis();
            let mut all_metrics = Vec::new();

            for (service_name, source) in sources {
                let pids = match source {
                    PidSource::Cgroup(pids) => pids,
                    PidSource::Tree { leader } => match &index {
                        Some(index) => collect_service_tree(index, leader),
                        None => vec![leader],
                    },
                };

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
                    service: service_name,
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

/// Parent and process-group relations of every live process, built once per
/// sample so `getpgid` costs one syscall per process rather than one per
/// process and per service.
struct ProcessIndex {
    /// Children of each PID.
    children: HashMap<u32, Vec<u32>>,
    /// Members of each process group.
    group_members: HashMap<u32, Vec<u32>>,
}

impl ProcessIndex {
    /// Build the index from a `System` refreshed with `ProcessesToUpdate::All`.
    fn build(sys: &System) -> Self {
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut group_members: HashMap<u32, Vec<u32>> = HashMap::new();

        for (pid, proc_info) in sys.processes() {
            let pid = pid.as_u32();
            if let Some(ppid) = proc_info.parent() {
                children.entry(ppid.as_u32()).or_default().push(pid);
            }
            if let Some(pgid) = kepler_unix::process_tree::process_group_id(pid) {
                group_members.entry(pgid).or_default().push(pid);
            }
        }

        ProcessIndex {
            children,
            group_members,
        }
    }
}

/// Enumerate a service's processes from its leader PID, for hosts where cgroup
/// containment is unavailable.
///
/// Two relations are unioned because neither is complete on its own. Services
/// are spawned as their own process-group leader, so group membership catches
/// descendants that a double fork re-parented away from the leader; the parent
/// chain catches descendants that left the group through `setpgid`/`setsid`.
/// A descendant that did both is invisible to either — only cgroups track those.
fn collect_service_tree(index: &ProcessIndex, leader: u32) -> Vec<u32> {
    let mut pids = vec![leader];
    if let Some(members) = index.group_members.get(&leader) {
        for &pid in members {
            if pid != leader {
                pids.push(pid);
            }
        }
    }

    // Breadth-first over a growing worklist: descending from the group members
    // too, since a child that left the group may still have descendants.
    let mut next = 0;
    while next < pids.len() {
        let parent = pids[next];
        next += 1;
        let Some(children) = index.children.get(&parent) else {
            continue;
        };
        for &child in children {
            if child != parent && !pids.contains(&child) {
                pids.push(child);
            }
        }
    }

    pids
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two relations must cover for each other: a descendant re-parented to
    /// init is only reachable through the process group, and one that left the
    /// group is only reachable through the parent chain.
    #[test]
    fn service_tree_unions_group_members_and_parent_chain() {
        let init = 1;
        let (leader, child, grandchild) = (100, 101, 102);
        let reparented_into_init = 103;
        let unrelated = 200;

        let index = ProcessIndex {
            children: HashMap::from([
                (leader, vec![child]),
                (child, vec![grandchild]),
                (init, vec![reparented_into_init, unrelated]),
            ]),
            group_members: HashMap::from([
                (leader, vec![leader, child, reparented_into_init]),
                // Its own group: it called setsid().
                (grandchild, vec![grandchild]),
                (unrelated, vec![unrelated]),
            ]),
        };

        let mut pids = collect_service_tree(&index, leader);
        pids.sort_unstable();

        assert_eq!(pids, vec![leader, child, grandchild, reparented_into_init]);
    }

    /// A leader the scan did not see yields itself alone, never another
    /// service's PIDs.
    #[test]
    fn service_tree_of_unknown_leader_is_the_leader_alone() {
        let index = ProcessIndex {
            children: HashMap::new(),
            group_members: HashMap::new(),
        };

        assert_eq!(collect_service_tree(&index, 100), vec![100]);
    }

    /// End-to-end over a real process tree, against the failure mode the walk
    /// exists to prevent: returning the leader alone, which under-reports every
    /// forking service on hosts without cgroup containment.
    #[cfg(unix)]
    #[test]
    fn service_tree_finds_real_children_and_their_memory() {
        use std::os::unix::process::CommandExt;
        use std::process::Command;

        // Two levels deep. `wait` keeps each shell alive, which also stops it
        // from exec'ing into its last command and collapsing the tree.
        let mut leader = Command::new("sh")
            .arg("-c")
            .arg("sh -c 'sleep 30 & wait' & wait")
            .process_group(0)
            .spawn()
            .expect("spawn test process tree");
        let leader_pid = leader.id();

        let mut sys = System::new();
        let refresh_kind = ProcessRefreshKind::nothing().with_memory().with_cpu();
        let mut pids = Vec::new();
        // The shells need a moment to fork.
        for _ in 0..60 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            sys.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind);
            pids = collect_service_tree(&ProcessIndex::build(&sys), leader_pid);
            if pids.len() >= 3 {
                break;
            }
        }

        let _ = kepler_unix::process_tree::force_kill_process_tree(leader_pid);
        let _ = leader.wait();

        assert!(
            pids.len() >= 3,
            "expected the leader, its child shell and the grandchild sleep, got {:?}",
            pids
        );
        assert!(pids.contains(&leader_pid), "leader missing from {:?}", pids);

        let leader_rss = sys
            .process(Pid::from_u32(leader_pid))
            .expect("leader still in the process table")
            .memory();
        let tree_rss: u64 = pids
            .iter()
            .filter_map(|&pid| sys.process(Pid::from_u32(pid)))
            .map(|proc_info| proc_info.memory())
            .sum();

        assert!(
            tree_rss > leader_rss,
            "tree RSS {} should exceed the leader's own {}",
            tree_rss,
            leader_rss
        );
    }
}
