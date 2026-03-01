// MutexGuard held across await is intentional for env safety in tests
#![allow(clippy::await_holding_lock)]
//! Token lifecycle and KEPLER_TOKEN hygiene tests
//!
//! Tests that:
//! - Services with `permissions` receive KEPLER_TOKEN
//! - Services without `permissions` do NOT receive KEPLER_TOKEN
//! - Hooks of services with `permissions` receive the token
//! - Token is cleaned up after service stop
//! - Stale caller KEPLER_TOKEN does not leak into service environment

use kepler_daemon::config::{HookList, ServiceHooks};
use kepler_tests::helpers::config_builder::{TestConfigBuilder, TestHookBuilder, TestServiceBuilder};
use kepler_tests::helpers::daemon_harness::{TestDaemonHarness, ENV_LOCK};
use kepler_tests::helpers::marker_files::MarkerFileHelper;
use std::time::Duration;
use tempfile::TempDir;

/// Service with `permissions` receives KEPLER_TOKEN in its environment
#[tokio::test]
async fn test_service_with_permissions_gets_kepler_token() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("token_present");

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"HAS_TOKEN=${{KEPLER_TOKEN:+yes}}\" >> {} && sleep 3600",
                    marker_path.display()
                ),
            ])
            .with_permissions(vec!["service:start"])
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let got = marker
        .wait_for_marker_lines("token_present", 1, Duration::from_secs(2))
        .await;
    assert!(got, "Service should have written marker");

    let content = marker.read_marker("token_present").unwrap();
    assert!(
        content.contains("HAS_TOKEN=yes"),
        "Service with permissions should have KEPLER_TOKEN set. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// Service without `permissions` does NOT receive KEPLER_TOKEN
#[tokio::test]
async fn test_service_without_permissions_has_no_kepler_token() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("no_token");

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"HAS_TOKEN=${{KEPLER_TOKEN:+yes}}\" >> {} && sleep 3600",
                    marker_path.display()
                ),
            ])
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let got = marker
        .wait_for_marker_lines("no_token", 1, Duration::from_secs(2))
        .await;
    assert!(got, "Service should have written marker");

    let content = marker.read_marker("no_token").unwrap();
    assert!(
        content.contains("HAS_TOKEN="),
        "Service without permissions should have empty KEPLER_TOKEN. Got: {}",
        content
    );
    assert!(
        !content.contains("HAS_TOKEN=yes"),
        "KEPLER_TOKEN should NOT be set for permissionless service. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// KEPLER_TOKEN in the service is a valid 64-char hex string
#[tokio::test]
async fn test_kepler_token_is_valid_hex() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("token_hex");

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"$KEPLER_TOKEN\" >> {} && sleep 3600",
                    marker_path.display()
                ),
            ])
            .with_permissions(vec!["service:start"])
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let got = marker
        .wait_for_marker_lines("token_hex", 1, Duration::from_secs(2))
        .await;
    assert!(got, "Service should have written token");

    let content = marker.read_marker("token_hex").unwrap();
    let token_str = content.trim();
    assert_eq!(
        token_str.len(),
        64,
        "KEPLER_TOKEN should be 64 hex chars. Got: '{}'",
        token_str
    );
    assert!(
        token_str.chars().all(|c| c.is_ascii_hexdigit()),
        "KEPLER_TOKEN should only contain hex characters. Got: '{}'",
        token_str
    );

    harness.stop_service("test").await.unwrap();
}

/// pre_start hook of a service with `permissions` receives KEPLER_TOKEN
#[tokio::test]
async fn test_pre_start_hook_gets_kepler_token() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("hook_token");

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![TestHookBuilder::script(&format!(
            "echo \"HOOK_TOKEN=${{KEPLER_TOKEN:+yes}}\" >> {}",
            marker_path.display()
        ))])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sleep".to_string(),
                "3600".to_string(),
            ])
            .with_hooks(hooks)
            .with_permissions(vec!["service:start"])
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let got = marker
        .wait_for_marker_lines("hook_token", 1, Duration::from_secs(2))
        .await;
    assert!(got, "Pre-start hook should have run");

    let content = marker.read_marker("hook_token").unwrap();
    assert!(
        content.contains("HOOK_TOKEN=yes"),
        "pre_start hook should receive KEPLER_TOKEN. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// post_start hook of a service with `permissions` receives KEPLER_TOKEN
#[tokio::test]
async fn test_post_start_hook_gets_kepler_token() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("post_start_token");

    let hooks = ServiceHooks {
        post_start: Some(HookList(vec![TestHookBuilder::script(&format!(
            "echo \"HOOK_TOKEN=${{KEPLER_TOKEN:+yes}}\" >> {}",
            marker_path.display()
        ))])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sleep".to_string(),
                "3600".to_string(),
            ])
            .with_hooks(hooks)
            .with_permissions(vec!["service:start"])
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let got = marker
        .wait_for_marker_lines("post_start_token", 1, Duration::from_secs(2))
        .await;
    assert!(got, "Post-start hook should have run");

    let content = marker.read_marker("post_start_token").unwrap();
    assert!(
        content.contains("HOOK_TOKEN=yes"),
        "post_start hook should receive KEPLER_TOKEN. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// Hook of a service WITHOUT `permissions` does NOT receive KEPLER_TOKEN
#[tokio::test]
async fn test_hook_without_permissions_has_no_kepler_token() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("hook_no_token");

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![TestHookBuilder::script(&format!(
            "echo \"HOOK_TOKEN=${{KEPLER_TOKEN:+yes}}\" >> {}",
            marker_path.display()
        ))])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sleep".to_string(),
                "3600".to_string(),
            ])
            .with_hooks(hooks)
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let got = marker
        .wait_for_marker_lines("hook_no_token", 1, Duration::from_secs(2))
        .await;
    assert!(got, "Pre-start hook should have run");

    let content = marker.read_marker("hook_no_token").unwrap();
    assert!(
        !content.contains("HOOK_TOKEN=yes"),
        "Hook without permissions should NOT have KEPLER_TOKEN. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// Hook and service receive the SAME token
#[tokio::test]
async fn test_hook_and_service_share_same_token() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let hook_marker_path = marker.marker_path("hook_token_val");
    let svc_marker_path = marker.marker_path("svc_token_val");

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![TestHookBuilder::script(&format!(
            "echo \"$KEPLER_TOKEN\" >> {}",
            hook_marker_path.display()
        ))])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"$KEPLER_TOKEN\" >> {} && sleep 3600",
                    svc_marker_path.display()
                ),
            ])
            .with_hooks(hooks)
            .with_permissions(vec!["service:start"])
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let got_hook = marker
        .wait_for_marker_lines("hook_token_val", 1, Duration::from_secs(2))
        .await;
    let got_svc = marker
        .wait_for_marker_lines("svc_token_val", 1, Duration::from_secs(2))
        .await;

    assert!(got_hook, "Hook should have written token");
    assert!(got_svc, "Service should have written token");

    let hook_token = marker.read_marker("hook_token_val").unwrap();
    let svc_token = marker.read_marker("svc_token_val").unwrap();

    assert_eq!(
        hook_token.trim(),
        svc_token.trim(),
        "Hook and service should receive the same token"
    );

    harness.stop_service("test").await.unwrap();
}

/// Stale KEPLER_TOKEN from the caller's environment does not leak
/// into a service without permissions (env hygiene)
#[tokio::test]
async fn test_stale_kepler_token_stripped_from_env() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("stale_token");

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"TOKEN=${{KEPLER_TOKEN:-none}}\" >> {} && sleep 3600",
                    marker_path.display()
                ),
            ])
            .with_inherit_env(true)
            .build(),
        )
        .build();

    // Hold ENV_LOCK around set_var + harness creation so the env snapshot
    // is taken atomically (prevents race with parallel tests' env mutations).
    let harness = {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: We hold ENV_LOCK, so no parallel test can mutate env concurrently.
        unsafe {
            std::env::set_var("KEPLER_TOKEN", "stale_token_from_caller");
        }
        let h = TestDaemonHarness::new_with_env_lock_held(config, temp_dir.path())
            .await
            .unwrap();
        unsafe {
            std::env::remove_var("KEPLER_TOKEN");
        }
        h
    };

    harness.start_service("test").await.unwrap();

    let got = marker
        .wait_for_marker_lines("stale_token", 1, Duration::from_secs(2))
        .await;
    assert!(got, "Service should have written marker");

    let content = marker.read_marker("stale_token").unwrap();
    assert!(
        !content.contains("stale_token_from_caller"),
        "Stale KEPLER_TOKEN should be stripped from env. Got: {}",
        content
    );
    assert!(
        content.contains("TOKEN=none"),
        "Service without permissions should not have any KEPLER_TOKEN. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// post_stop hook of a service with `permissions` receives KEPLER_TOKEN
/// (token is revoked AFTER post_stop, not before)
#[tokio::test]
async fn test_post_stop_hook_gets_kepler_token() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("post_stop_token");

    let hooks = ServiceHooks {
        post_stop: Some(HookList(vec![TestHookBuilder::script(&format!(
            "echo \"HOOK_TOKEN=${{KEPLER_TOKEN:+yes}}\" >> {}",
            marker_path.display()
        ))])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sleep".to_string(),
                "3600".to_string(),
            ])
            .with_hooks(hooks)
            .with_permissions(vec!["service:start"])
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();
    // Give service a moment to fully start
    tokio::time::sleep(Duration::from_millis(100)).await;

    harness.stop_service("test").await.unwrap();

    let got = marker
        .wait_for_marker_lines("post_stop_token", 1, Duration::from_secs(2))
        .await;
    assert!(got, "Post-stop hook should have run");

    let content = marker.read_marker("post_stop_token").unwrap();
    assert!(
        content.contains("HOOK_TOKEN=yes"),
        "post_stop hook should still have KEPLER_TOKEN (revoked after hooks). Got: {}",
        content
    );
}
