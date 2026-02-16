//! Memory leak detection tests
//!
//! These tests verify that the daemon doesn't leak memory across repeated
//! operations. They are marked #[ignore] because they take longer to run
//! and require /proc/pid/status (Linux only).
//!
//! Test configs exercise every Kepler feature: Lua scripting, hooks (pre/post
//! start/stop/restart, healthcheck hooks, if-conditions), healthchecks,
//! log buffers, resource limits, dependency chains (service_healthy,
//! service_completed_successfully, service_started, service_failed), Lua
//! depends_on/command/environment/healthcheck, sys_env overrides, etc.
//!
//! Run manually with:
//!   cargo test -p kepler-e2e --test memory_leak_test -- --ignored --nocapture

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "memory_leak_test";

/// Longer CLI timeout for complex configs (20 services with deep dependency chains).
const CLI_TIMEOUT: Duration = Duration::from_secs(60);

/// Repeated `start -d --wait` on already-running services should not leak memory.
///
/// This catches leaks in the Subscribe path: each `start --wait` creates a
/// subscription, receives Ready/Quiescent, then disconnects. If the handler
/// task or subscriber channel isn't cleaned up, memory grows per invocation.
///
/// Config: 20 services across 5 tiers with Lua, hooks, healthchecks, limits,
/// deferred deps, fan-in deps, various restart policies.
#[tokio::test]
#[ignore]
async fn test_no_leak_on_repeated_start_wait() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_no_leak_on_repeated_start_wait")?;
    let config_str = config_path.to_str().unwrap().to_string();

    harness.start_daemon().await?;

    // Initial start to get all 20 services running (with deps, healthchecks, Lua, hooks)
    harness.run_cli_with_timeout(
        &["-f", &config_str, "start", "-d", "--wait"],
        CLI_TIMEOUT,
    ).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "dashboard", "running", Duration::from_secs(30))
        .await?;

    // Let things stabilize
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Measure baseline RSS (after first start, things are warmed up)
    let baseline_rss = harness.daemon_rss_kb()
        .expect("Failed to read daemon RSS — is /proc available?");

    // Run many start --wait cycles on already-running services
    let iterations = 20;
    for i in 0..iterations {
        let output = harness.run_cli_with_timeout(
            &["-f", &config_str, "start", "-d", "--wait"],
            CLI_TIMEOUT,
        ).await?;
        assert!(
            output.success(),
            "start --wait failed on iteration {}: stderr: {}",
            i, output.stderr
        );

        if (i + 1) % 10 == 0 {
            let current_rss = harness.daemon_rss_kb().unwrap_or(0);
            eprintln!(
                "  iter {}/{}: RSS={}KB (baseline={}KB, growth={}KB)",
                i + 1, iterations, current_rss, baseline_rss,
                current_rss.saturating_sub(baseline_rss)
            );
        }
    }

    // Let things stabilize
    tokio::time::sleep(Duration::from_millis(500)).await;

    let final_rss = harness.daemon_rss_kb()
        .expect("Failed to read daemon RSS after iterations");

    let growth_kb = final_rss.saturating_sub(baseline_rss);
    let max_allowed_growth_kb = 2048; // 2 MB max allowed growth

    // Print debug stats from daemon BEFORE assert
    let debug_log = harness.state_dir().join("leak_debug.log");
    if let Ok(contents) = std::fs::read_to_string(&debug_log) {
        let lines: Vec<&str> = contents.lines().collect();
        eprintln!("--- LEAK DEBUG ({} entries, first 3 + last 3) ---", lines.len());
        for line in lines.iter().take(3) {
            eprintln!("  {}", line);
        }
        if lines.len() > 6 { eprintln!("  ..."); }
        for line in lines.iter().rev().take(3).collect::<Vec<_>>().into_iter().rev() {
            eprintln!("  {}", line);
        }
    } else {
        eprintln!("--- LEAK DEBUG: no debug log found ---");
    }

    eprintln!(
        "Memory: baseline={}KB, final={}KB, growth={}KB ({} iterations)",
        baseline_rss, final_rss, growth_kb, iterations
    );

    assert!(
        growth_kb < max_allowed_growth_kb,
        "Memory grew by {}KB over {} iterations (limit: {}KB). \
         baseline={}KB, final={}KB. Likely a leak in the Subscribe path.",
        growth_kb, iterations, max_allowed_growth_kb, baseline_rss, final_rss
    );

    // Cleanup
    harness.stop_daemon().await?;
    Ok(())
}

/// Repeated start/stop cycles should not leak memory.
///
/// This catches leaks in service lifecycle management: spawned tasks,
/// channels, health check handles, Lua evaluator, hooks, file watchers, etc.
///
/// Config: 15 services across 4 tiers with Lua, hooks, healthchecks, limits,
/// fan-in deps, various restart policies.
#[tokio::test]
#[ignore]
async fn test_no_leak_on_start_stop_cycles() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_no_leak_on_start_stop_cycles")?;
    let config_str = config_path.to_str().unwrap().to_string();

    harness.start_daemon().await?;

    // Warm-up cycle (15 services with deps, healthchecks, Lua, hooks)
    harness.run_cli_with_timeout(
        &["-f", &config_str, "start", "-d", "--wait"],
        CLI_TIMEOUT,
    ).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "dashboard", "running", Duration::from_secs(30))
        .await?;
    harness.stop_services(&config_path).await?.assert_success();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Measure baseline after warm-up
    let baseline_rss = harness.daemon_rss_kb()
        .expect("Failed to read daemon RSS");

    // Run start/stop cycles
    let iterations = 15;
    for i in 0..iterations {
        harness.run_cli_with_timeout(
            &["-f", &config_str, "start", "-d", "--wait"],
            CLI_TIMEOUT,
        ).await?.assert_success();
        harness
            .wait_for_service_status(&config_path, "dashboard", "running", Duration::from_secs(30))
            .await?;
        harness.stop_services(&config_path).await?.assert_success();
        // Brief pause between cycles
        tokio::time::sleep(Duration::from_millis(200)).await;

        if (i + 1) % 5 == 0 {
            let current_rss = harness.daemon_rss_kb().unwrap_or(0);
            eprintln!(
                "  cycle {}/{}: RSS={}KB (baseline={}KB, growth={}KB)",
                i + 1, iterations, current_rss, baseline_rss,
                current_rss.saturating_sub(baseline_rss)
            );
        }
    }

    tokio::time::sleep(Duration::from_millis(500)).await;
    let final_rss = harness.daemon_rss_kb()
        .expect("Failed to read daemon RSS after cycles");

    let growth_kb = final_rss.saturating_sub(baseline_rss);
    let max_allowed_growth_kb = 8192; // 8 MB max for start/stop cycles (each cycle spawns ~60 processes)

    eprintln!(
        "Memory: baseline={}KB, final={}KB, growth={}KB ({} cycles)",
        baseline_rss, final_rss, growth_kb, iterations
    );

    assert!(
        growth_kb < max_allowed_growth_kb,
        "Memory grew by {}KB over {} start/stop cycles (limit: {}KB). \
         baseline={}KB, final={}KB. Likely a leak in service lifecycle.",
        growth_kb, iterations, max_allowed_growth_kb, baseline_rss, final_rss
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Bisection test: 20 minimal services (no Lua, hooks, healthchecks, deps).
///
/// If this test leaks, the issue is in the protocol/server layer (Subscribe path).
/// If it doesn't, the issue is in Lua/hooks/healthchecks features.
#[tokio::test]
#[ignore]
async fn test_no_leak_minimal_baseline() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_no_leak_minimal_baseline")?;
    let config_str = config_path.to_str().unwrap().to_string();

    harness.start_daemon().await?;

    // Initial start to get all 20 services running
    harness.run_cli_with_timeout(
        &["-f", &config_str, "start", "-d", "--wait"],
        CLI_TIMEOUT,
    ).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "svc-20", "running", Duration::from_secs(30))
        .await?;

    // Let things stabilize
    tokio::time::sleep(Duration::from_millis(500)).await;

    let baseline_rss = harness.daemon_rss_kb()
        .expect("Failed to read daemon RSS");

    let iterations = 20;
    for i in 0..iterations {
        let output = harness.run_cli_with_timeout(
            &["-f", &config_str, "start", "-d", "--wait"],
            CLI_TIMEOUT,
        ).await?;
        assert!(
            output.success(),
            "start --wait failed on iteration {}: stderr: {}",
            i, output.stderr
        );

        if (i + 1) % 10 == 0 {
            let current_rss = harness.daemon_rss_kb().unwrap_or(0);
            eprintln!(
                "  [minimal] iter {}/{}: RSS={}KB (baseline={}KB, growth={}KB)",
                i + 1, iterations, current_rss, baseline_rss,
                current_rss.saturating_sub(baseline_rss)
            );
        }
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let final_rss = harness.daemon_rss_kb()
        .expect("Failed to read daemon RSS after iterations");

    let growth_kb = final_rss.saturating_sub(baseline_rss);
    let max_allowed_growth_kb = 2048; // 2 MB max

    // Read debug log
    let debug_log = harness.state_dir().join("leak_debug.log");
    if let Ok(contents) = std::fs::read_to_string(&debug_log) {
        let lines: Vec<&str> = contents.lines().collect();
        eprintln!("--- LEAK DEBUG ({} entries, first 3 + last 3) ---", lines.len());
        for line in lines.iter().take(3) {
            eprintln!("  {}", line);
        }
        if lines.len() > 6 { eprintln!("  ..."); }
        for line in lines.iter().rev().take(3).collect::<Vec<_>>().into_iter().rev() {
            eprintln!("  {}", line);
        }
    } else {
        eprintln!("--- LEAK DEBUG: no debug log found ---");
    }

    eprintln!(
        "[MINIMAL] Memory: baseline={}KB, final={}KB, growth={}KB ({} iterations)",
        baseline_rss, final_rss, growth_kb, iterations
    );

    assert!(
        growth_kb < max_allowed_growth_kb,
        "[MINIMAL] Memory grew by {}KB over {} iterations (limit: {}KB). \
         baseline={}KB, final={}KB. Leak is in the protocol/server layer.",
        growth_kb, iterations, max_allowed_growth_kb, baseline_rss, final_rss
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Repeated ps/status queries should not leak memory.
///
/// This catches leaks in read-only request paths (no subscriptions).
///
/// Config: 15 services across 4 tiers with Lua, hooks, healthchecks, limits.
#[tokio::test]
#[ignore]
async fn test_no_leak_on_repeated_status_queries() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_no_leak_on_repeated_status_queries")?;
    let config_str = config_path.to_str().unwrap().to_string();

    harness.start_daemon().await?;
    harness.run_cli_with_timeout(
        &["-f", &config_str, "start", "-d", "--wait"],
        CLI_TIMEOUT,
    ).await?.assert_success();
    harness
        .wait_for_service_status(&config_path, "dashboard", "running", Duration::from_secs(30))
        .await?;

    tokio::time::sleep(Duration::from_millis(500)).await;
    let baseline_rss = harness.daemon_rss_kb()
        .expect("Failed to read daemon RSS");

    // Hammer the daemon with ps/status queries
    let iterations = 100;
    for i in 0..iterations {
        harness.ps(&config_path).await?.assert_success();
        harness.daemon_status().await?;

        if (i + 1) % 25 == 0 {
            let current_rss = harness.daemon_rss_kb().unwrap_or(0);
            eprintln!(
                "  query {}/{}: RSS={}KB (baseline={}KB, growth={}KB)",
                i + 1, iterations, current_rss, baseline_rss,
                current_rss.saturating_sub(baseline_rss)
            );
        }
    }

    tokio::time::sleep(Duration::from_millis(500)).await;
    let final_rss = harness.daemon_rss_kb()
        .expect("Failed to read daemon RSS after queries");

    let growth_kb = final_rss.saturating_sub(baseline_rss);
    let max_allowed_growth_kb = 1024; // 1 MB max for read-only queries

    eprintln!(
        "Memory: baseline={}KB, final={}KB, growth={}KB ({} queries)",
        baseline_rss, final_rss, growth_kb, iterations
    );

    assert!(
        growth_kb < max_allowed_growth_kb,
        "Memory grew by {}KB over {} status queries (limit: {}KB). \
         baseline={}KB, final={}KB. Likely a leak in request handling.",
        growth_kb, iterations, max_allowed_growth_kb, baseline_rss, final_rss
    );

    harness.stop_daemon().await?;
    Ok(())
}
