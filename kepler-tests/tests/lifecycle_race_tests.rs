//! Reloading a config while its previous startup is still in flight.
//!
//! `run` and `recreate` unload the config and start it afresh. Issued right after
//! a `start` (which returns once startups are dispatched), they used to find
//! services still spawning, leave them behind, and have their teardown force-kill
//! the per-service cgroup — which the freshly started instances already share.

use kepler_daemon::config_registry::ConfigRegistry;
use kepler_daemon::orchestrator::ServiceOrchestrator;
use kepler_daemon::process::ProcessExitEvent;
use kepler_daemon::state::ServiceStatus;
use kepler_daemon::watcher::FileChangeEvent;
use kepler_tests::helpers::config_builder::{TestConfigBuilder, TestServiceBuilder};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::mpsc;

const SERVICES: [&str; 3] = ["svc-a", "svc-b", "svc-c"];
/// Delays between the start returning and the reload, swept in `STEP`s to land
/// the reload in every phase of the background startup, spawn included.
const ITERATIONS: u32 = 40;
const STEP: Duration = Duration::from_micros(500);
/// Longer than the containment layer's cgroup kill retries, so a stray kill from
/// the previous lifecycle has landed by the time liveness is checked.
const SETTLE: Duration = Duration::from_millis(300);

fn setup_orchestrator(temp_dir: &TempDir) -> Arc<ServiceOrchestrator> {
    // SAFETY: this binary holds a single test, so nothing reads the environment concurrently.
    unsafe {
        std::env::set_var("KEPLER_DAEMON_PATH", temp_dir.path().join(".kepler"));
    }
    let (exit_tx, _exit_rx) = mpsc::channel::<ProcessExitEvent>(32);
    let (restart_tx, _restart_rx) = mpsc::channel::<FileChangeEvent>(32);
    Arc::new(ServiceOrchestrator::new(
        Arc::new(ConfigRegistry::new()),
        exit_tx,
        restart_tx,
        Arc::new(dashmap::DashMap::new()),
        kepler_daemon::hardening::HardeningLevel::None,
        None,
        Arc::new(kepler_daemon::token_store::TokenStore::new()),
        kepler_daemon::containment::ContainmentManager::detect(),
    ))
}

fn write_config(temp_dir: &TempDir, name: &str) -> std::path::PathBuf {
    let mut builder = TestConfigBuilder::new();
    for service in SERVICES {
        builder = builder.add_service(service, TestServiceBuilder::long_running().build());
    }
    let path = temp_dir.path().join(name);
    std::fs::write(&path, serde_yaml::to_string(&builder.build()).unwrap()).unwrap();
    // Canonical, as the daemon keys configs by canonical path.
    path.canonicalize().unwrap()
}

/// Whether `pid` is a live (non-zombie) process.
fn is_alive(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", pid)) else {
        return false;
    };
    // The state follows the parenthesised command name, which may contain spaces.
    stat.rsplit_once(')').and_then(|(_, rest)| rest.split_whitespace().next()) != Some("Z")
}

/// Every service must be running, its recorded process still alive after `SETTLE`.
async fn assert_all_alive(orchestrator: &ServiceOrchestrator, config_path: &Path, context: &str) {
    tokio::time::sleep(SETTLE).await;
    let handle = orchestrator.registry().get(&config_path.to_path_buf()).expect("config loaded");
    for service in SERVICES {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let state = loop {
            let state = handle.get_service_state(service).await.expect("service state");
            if state.status == ServiceStatus::Running || tokio::time::Instant::now() >= deadline {
                break state;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert_eq!(state.status, ServiceStatus::Running, "{}: {} not running", context, service);
        let pid = state.pid.expect("running service has a pid");
        assert!(is_alive(pid), "{}: {} (pid {}) was killed after the reload", context, service, pid);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reload_right_after_start_keeps_new_instances_alive() {
    let temp_dir = TempDir::new().unwrap();
    let orchestrator = setup_orchestrator(&temp_dir);
    let sys_env: HashMap<String, String> = std::env::vars().collect();

    for iteration in 0..ITERATIONS {
        // A fresh config per iteration, so each start loads it from source.
        let config_path = write_config(&temp_dir, &format!("run_{}.kepler.yaml", iteration));
        orchestrator
            .start_services(&config_path, &[], Some(sys_env.clone()), None, None, false, None, None, None, None)
            .await
            .unwrap();
        tokio::time::sleep(STEP * iteration).await;
        orchestrator
            .run_services(&config_path, &[], Some(sys_env.clone()), None, None, false, None, None, None, false, None)
            .await
            .unwrap();
        assert_all_alive(&orchestrator, &config_path, &format!("run #{}", iteration)).await;
        orchestrator.stop_services(&config_path, &[], false, None).await.unwrap();

        let config_path = write_config(&temp_dir, &format!("recreate_{}.kepler.yaml", iteration));
        orchestrator
            .start_services(&config_path, &[], Some(sys_env.clone()), None, None, false, None, None, None, None)
            .await
            .unwrap();
        tokio::time::sleep(STEP * iteration).await;
        orchestrator
            .recreate_services(&config_path, Some(sys_env.clone()), None, None, None, None, None)
            .await
            .unwrap();
        assert_all_alive(&orchestrator, &config_path, &format!("recreate #{}", iteration)).await;
        orchestrator.stop_services(&config_path, &[], false, None).await.unwrap();
    }
}
