//! E2E tests for child-process accounting in `kepler top`.
//!
//! A service's memory and CPU must cover its whole process tree, not just the
//! PID the daemon spawned. Two separate defects used to break this — see
//! `docs/plans/monitor-child-processes-ram.md`:
//!
//! - **Without cgroups** (macOS, or Linux where the daemon cannot write to
//!   `/sys/fs/cgroup`): the process-tree walk ran against a `sysinfo` table
//!   that was only ever refreshed for already-known PIDs, so children were
//!   never discovered.
//! - **With cgroups**: children forked between `spawn()` and the write to
//!   `cgroup.procs` stay in the daemon's cgroup and never appear in the
//!   service's `cgroup.procs`.
//!
//! These tests assert the user-visible symptom (`memory_rss` and `pids` from
//! `kepler top --json`) rather than either internal mechanism, so they hold in
//! both environments: `docker compose run --rm test` exercises the fallback
//! path, `docker compose run --rm test-cgroup` the cgroup path.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "monitor_children_test";

/// memhog holds 200 MB; a parent shell is ~1-3 MB. Anything above this floor
/// can only mean the child was accounted for.
const CHILD_RSS_FLOOR: u64 = 150 * 1024 * 1024;

/// Fetch `kepler top --json` and return the entry for one service.
async fn top_entry(
    harness: &E2eHarness,
    config_path: &std::path::Path,
    service: &str,
) -> E2eResult<serde_json::Value> {
    let output = harness
        .run_cli(&["-f", config_path.to_str().unwrap(), "top", "--json"])
        .await?;
    assert!(
        output.success(),
        "kepler top --json should succeed. stderr: {}",
        output.stderr
    );
    let json: serde_json::Value = serde_json::from_str(&output.stdout)
        .unwrap_or_else(|e| panic!("failed to parse JSON: {}. stdout: {}", e, output.stdout));
    Ok(json[service].clone())
}

/// Poll `kepler top` until the service reports at least `min_pids` PIDs and
/// `min_rss` bytes, or the deadline passes. Returns the last entry seen either
/// way, so the caller's assertions produce the real numbers on failure.
///
/// Polling rather than sleeping a fixed time: the workload needs a moment to
/// become fully resident, and the monitor samples on its own 1s cadence, so any
/// single fixed wait is either flaky or needlessly slow.
async fn wait_for_top(
    harness: &E2eHarness,
    config_path: &std::path::Path,
    service: &str,
    min_pids: usize,
    min_rss: u64,
) -> E2eResult<serde_json::Value> {
    let deadline = std::time::Instant::now() + Duration::from_secs(45);
    let mut last = serde_json::Value::Null;

    while std::time::Instant::now() < deadline {
        last = top_entry(harness, config_path, service).await?;
        let pids = last["pids"].as_array().map(|a| a.len()).unwrap_or(0);
        let rss = last["memory_rss"].as_u64().unwrap_or(0);
        if pids >= min_pids && rss >= min_rss {
            return Ok(last);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    Ok(last)
}

/// True when running where cgroup v2 is active (the `test-cgroup` compose
/// service). Mirrors the gate used by `kepler-tests/tests/cgroup_tests.rs`.
fn require_cgroupv2() -> bool {
    if std::env::var("REQUIRE_CGROUPV2").as_deref() == Ok("1") {
        return true;
    }
    eprintln!("Skipping: REQUIRE_CGROUPV2 not set");
    false
}

/// Read `cgroup.procs` for a service, locating it by service name under
/// `/sys/fs/cgroup/kepler/<config_hash>/<service>`. The config hash is derived
/// from the harness's temp path and is not exposed to tests, so scan for it.
fn cgroup_pids_for_service(service: &str) -> Option<Vec<u32>> {
    let kepler_root = std::path::Path::new("/sys/fs/cgroup/kepler");
    for config_dir in std::fs::read_dir(kepler_root).ok()?.flatten() {
        let procs = config_dir.path().join(service).join("cgroup.procs");
        if let Ok(contents) = std::fs::read_to_string(&procs) {
            let pids: Vec<u32> = contents
                .lines()
                .filter_map(|l| l.trim().parse().ok())
                .collect();
            if !pids.is_empty() {
                return Some(pids);
            }
        }
    }
    None
}

/// Memory held by a forked child must be counted in the service total.
#[tokio::test]
async fn test_child_process_memory_is_counted() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_child_memory_counted")?;

    harness.start_daemon().await?;
    harness.start_services_wait(&config_path).await?;

    let entry = wait_for_top(&harness, &config_path, "forker", 2, CHILD_RSS_FLOOR).await?;
    let rss = entry["memory_rss"].as_u64().expect("memory_rss should be u64");
    let pids = entry["pids"].as_array().expect("pids should be an array");

    assert!(
        pids.len() >= 2,
        "the service forks a child, so at least 2 PIDs are expected, got {} ({:?}). \
         Daemon log:\n{}",
        pids.len(),
        pids,
        harness.daemon_logs(),
    );
    assert!(
        rss > CHILD_RSS_FLOOR,
        "the child holds 200 MB, so memory_rss should exceed 150 MB, got {} MB. \
         PIDs seen: {:?}",
        rss / 1024 / 1024,
        pids,
    );

    let _ = harness.stop_services(&config_path).await;
    harness.stop_daemon().await?;
    Ok(())
}

/// Same, one level deeper: the tree walk must recurse past direct children.
#[tokio::test]
async fn test_grandchild_process_memory_is_counted() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_grandchild_memory_counted")?;

    harness.start_daemon().await?;
    harness.start_services_wait(&config_path).await?;

    let entry = wait_for_top(&harness, &config_path, "nested", 3, CHILD_RSS_FLOOR).await?;
    let rss = entry["memory_rss"].as_u64().expect("memory_rss should be u64");
    let pids = entry["pids"].as_array().expect("pids should be an array");

    assert!(
        pids.len() >= 3,
        "expected the shell, its child shell and memhog (3 PIDs), got {} ({:?})",
        pids.len(),
        pids,
    );
    assert!(
        rss > CHILD_RSS_FLOOR,
        "the grandchild holds 200 MB, so memory_rss should exceed 150 MB, got {} MB",
        rss / 1024 / 1024,
    );

    let _ = harness.stop_services(&config_path).await;
    harness.stop_daemon().await?;
    Ok(())
}

/// The forked child must be *inside* the service's cgroup, not merely found by
/// the collector's tree walk.
///
/// This is the test that separates "the monitor compensates for the escape"
/// from "there is no escape": `kepler-exec` joins the cgroup before exec'ing,
/// so everything the service forks afterwards inherits membership. Without that,
/// `cgroup.procs` holds the leader alone and containment (`cgroup.kill`, limits)
/// has a hole in it regardless of what monitoring reports.
#[tokio::test]
async fn test_forked_child_is_inside_the_cgroup() -> E2eResult<()> {
    if !require_cgroupv2() {
        return Ok(());
    }

    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_child_memory_counted")?;

    harness.start_daemon().await?;
    harness.start_services_wait(&config_path).await?;

    // Wait until the monitor has seen both processes, so the workload is up.
    wait_for_top(&harness, &config_path, "forker", 2, CHILD_RSS_FLOOR).await?;

    let cgroup_pids = cgroup_pids_for_service("forker")
        .expect("the service's cgroup should exist and be non-empty");

    assert!(
        cgroup_pids.len() >= 2,
        "the shell and its forked child should both be in cgroup.procs, got {:?}. \
         Only the leader means the child was forked before the cgroup was joined.",
        cgroup_pids,
    );

    let _ = harness.stop_services(&config_path).await;
    harness.stop_daemon().await?;
    Ok(())
}

/// A process orphaned and re-parented to init is unreachable by a tree walk,
/// but still belongs to the service. The cgroup is the only source that knows.
#[tokio::test]
async fn test_detached_child_memory_is_counted() -> E2eResult<()> {
    if !require_cgroupv2() {
        return Ok(());
    }

    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_detached_child_counted")?;

    harness.start_daemon().await?;
    harness.start_services_wait(&config_path).await?;

    let entry = wait_for_top(&harness, &config_path, "detached", 2, CHILD_RSS_FLOOR).await?;
    let rss = entry["memory_rss"].as_u64().expect("memory_rss should be u64");

    assert!(
        rss > CHILD_RSS_FLOOR,
        "the re-parented process holds 200 MB and is still a member of the \
         service's cgroup, so it must be counted; got {} MB. If this fails, the \
         collector is relying on the process tree alone.",
        rss / 1024 / 1024,
    );

    let _ = harness.stop_services(&config_path).await;
    harness.stop_daemon().await?;
    Ok(())
}

/// The collector merges PIDs from the cgroup and from the process tree. A PID
/// present in both sources must be counted once, not twice.
#[tokio::test]
async fn test_single_process_is_not_double_counted() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path =
        harness.load_config(TEST_MODULE, "test_single_process_not_double_counted")?;

    harness.start_daemon().await?;
    harness.start_services_wait(&config_path).await?;

    let entry = wait_for_top(&harness, &config_path, "solo", 1, CHILD_RSS_FLOOR).await?;
    let rss = entry["memory_rss"].as_u64().expect("memory_rss should be u64");
    let pids = entry["pids"].as_array().expect("pids should be an array");

    // `exec` means the shell is replaced — memhog is the only process.
    assert_eq!(
        pids.len(),
        1,
        "exec'd service should have exactly one PID, got {:?}",
        pids,
    );
    assert!(
        rss > CHILD_RSS_FLOOR,
        "memhog holds 200 MB, got {} MB",
        rss / 1024 / 1024,
    );
    // Double-counting a single 200 MB process would land near 400 MB.
    assert!(
        rss < 320 * 1024 * 1024,
        "a single 200 MB process must not be counted twice, got {} MB",
        rss / 1024 / 1024,
    );

    let _ = harness.stop_services(&config_path).await;
    harness.stop_daemon().await?;
    Ok(())
}
