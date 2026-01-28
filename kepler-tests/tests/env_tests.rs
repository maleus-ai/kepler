//! Environment variable handling and merging tests

use kepler_tests::helpers::config_builder::{TestConfigBuilder, TestServiceBuilder};
use kepler_tests::helpers::daemon_harness::TestDaemonHarness;
use kepler_tests::helpers::marker_files::MarkerFileHelper;
use std::time::Duration;
use tempfile::TempDir;

/// Service environment variables from the `environment` array are available to the process
#[tokio::test]
async fn test_service_environment_array() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("env_array");

    // Create a service that echoes environment variables
    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"MY_VAR=$MY_VAR\" >> {} && echo \"OTHER_VAR=$OTHER_VAR\" >> {} && sleep 3600",
                    marker_path.display(),
                    marker_path.display()
                ),
            ])
            .with_environment(vec![
                "MY_VAR=hello".to_string(),
                "OTHER_VAR=world".to_string(),
            ])
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("env_array", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Service should have written env vars");
    let content = content.unwrap();
    assert!(
        content.contains("MY_VAR=hello"),
        "MY_VAR should be 'hello'. Got: {}",
        content
    );
    assert!(
        content.contains("OTHER_VAR=world"),
        "OTHER_VAR should be 'world'. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// Service environment variables from `env_file` are loaded
#[tokio::test]
async fn test_service_env_file() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("env_file");

    // Create an env file
    let env_file_path = temp_dir.path().join(".env");
    std::fs::write(&env_file_path, "FILE_VAR=from_file\nANOTHER_FILE_VAR=also_from_file\n").unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"FILE_VAR=$FILE_VAR ANOTHER_FILE_VAR=$ANOTHER_FILE_VAR\" >> {} && sleep 3600",
                    marker_path.display()
                ),
            ])
            .with_env_file(env_file_path)
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("env_file", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Service should have written env vars from file");
    let content = content.unwrap();
    assert!(
        content.contains("FILE_VAR=from_file"),
        "FILE_VAR should be 'from_file'. Got: {}",
        content
    );
    assert!(
        content.contains("ANOTHER_FILE_VAR=also_from_file"),
        "ANOTHER_FILE_VAR should be 'also_from_file'. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// Environment array overrides env_file variables
///
/// NOTE: We use `printenv SHARED_VAR` instead of `echo $SHARED_VAR` because
/// $SHARED_VAR in the command would be expanded at config load time (using
/// env_file values). Using printenv reads the actual runtime environment.
#[tokio::test]
async fn test_env_array_overrides_env_file() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("override");

    // Create an env file with a variable
    let env_file_path = temp_dir.path().join(".env");
    std::fs::write(&env_file_path, "SHARED_VAR=from_file\n").unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo SHARED_VAR=$(printenv SHARED_VAR) >> {} && sleep 3600",
                    marker_path.display()
                ),
            ])
            .with_env_file(env_file_path)
            .with_environment(vec!["SHARED_VAR=from_array".to_string()])
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("override", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Service should have written env var");
    let content = content.unwrap();
    assert!(
        content.contains("SHARED_VAR=from_array"),
        "Environment array should override env_file. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// Variable expansion using ${VAR} syntax works
#[tokio::test]
async fn test_env_variable_expansion() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("expansion");

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"EXPANDED=$EXPANDED\" >> {} && sleep 3600",
                    marker_path.display()
                ),
            ])
            .with_environment(vec![
                "BASE_VAR=base".to_string(),
                "EXPANDED=${BASE_VAR}_expanded".to_string(),
            ])
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("expansion", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Service should have written expanded var");
    let content = content.unwrap();
    assert!(
        content.contains("EXPANDED=base_expanded"),
        "Variable expansion should work. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// Variable expansion can reference env_file variables
#[tokio::test]
async fn test_env_expansion_references_env_file() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("expansion_file");

    // Create an env file with a variable to reference
    let env_file_path = temp_dir.path().join(".env");
    std::fs::write(&env_file_path, "FILE_BASE=file_value\n").unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"EXPANDED=$EXPANDED\" >> {} && sleep 3600",
                    marker_path.display()
                ),
            ])
            .with_env_file(env_file_path)
            .with_environment(vec!["EXPANDED=${FILE_BASE}_plus_more".to_string()])
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("expansion_file", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Service should have written expanded var");
    let content = content.unwrap();
    assert!(
        content.contains("EXPANDED=file_value_plus_more"),
        "Expansion should reference env_file vars. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// Minimal system environment variables (PATH, HOME, USER, SHELL) are inherited by the service
#[tokio::test]
async fn test_system_env_inherited() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("system_env");

    // HOME should be available from system environment
    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"HOME=$HOME\" >> {} && sleep 3600",
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

    let content = marker
        .wait_for_marker_content("system_env", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Service should have written system env var");
    let content = content.unwrap();
    assert!(
        content.contains("HOME=") && !content.contains("HOME=$HOME"),
        "System HOME var should be available. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// Service env can override inherited system environment variables
///
/// NOTE: We use `printenv HOME` instead of `echo $HOME` because $HOME in the
/// command would be expanded at config load time by shellexpand. Using printenv
/// reads the actual runtime environment injected into the process.
#[tokio::test]
async fn test_service_env_overrides_system_env() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("override_system");

    // Override HOME which is one of the inherited vars (PATH, HOME, USER, SHELL)
    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo HOME=$(printenv HOME) >> {} && sleep 3600",
                    marker_path.display()
                ),
            ])
            .with_environment(vec!["HOME=/custom/home".to_string()])
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("override_system", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Service should have written HOME");
    let content = content.unwrap();
    assert!(
        content.contains("HOME=/custom/home"),
        "Service env should override inherited HOME. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// All system variables ARE passed to service and CAN also be accessed via ${VAR} expansion
///
/// NOTE: We use `printenv` to check if a variable exists in the runtime environment.
/// The ${VAR} expansion happens at config load time, so if we define EXPANDED_VAR=${KEPLER_TEST_VAR}
/// in the environment array, it becomes EXPANDED_VAR=test_value_123 at config time.
/// ALL system vars are now inherited (changed from only PATH,HOME,USER,SHELL).
#[tokio::test]
async fn test_system_var_passed_and_expandable() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("sys_var");

    // Set a system env var in the test process (simulating daemon having it)
    // SAFETY: This is a test environment, and we clean up the var at the end
    unsafe {
        std::env::set_var("KEPLER_TEST_VAR", "test_value_123");
    }

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    concat!(
                        // Check if KEPLER_TEST_VAR exists in runtime env (should exist - all vars inherited)
                        "echo DIRECT=$(printenv KEPLER_TEST_VAR 2>/dev/null || echo NOTSET) >> {} && ",
                        // Check EXPANDED_VAR which should have the expanded value
                        "echo EXPANDED=$(printenv EXPANDED_VAR) >> {} && ",
                        "sleep 3600"
                    ),
                    marker_path.display(),
                    marker_path.display()
                ),
            ])
            // Use ${VAR} syntax to explicitly reference the var at config time
            .with_environment(vec!["EXPANDED_VAR=${KEPLER_TEST_VAR}".to_string()])
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("sys_var", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Service should have written env vars");
    let content = content.unwrap();

    // KEPLER_TEST_VAR SHOULD exist in process runtime environment
    // (all system vars are now inherited)
    assert!(
        content.contains("DIRECT=test_value_123"),
        "System var should be directly available in runtime env. Got: {}",
        content
    );

    // EXPANDED_VAR should have the value that was expanded at config time
    assert!(
        content.contains("EXPANDED=test_value_123"),
        "Var expanded via ${{VAR}} at config time should be in runtime env. Got: {}",
        content
    );

    // Cleanup
    // SAFETY: This is a test environment cleanup
    unsafe {
        std::env::remove_var("KEPLER_TEST_VAR");
    }

    harness.stop_service("test").await.unwrap();
}

/// Full priority chain: system < env_file < environment array
///
/// NOTE: We use `printenv` to check actual runtime environment values.
/// Using $VAR in the command would expand at config load time (using env_file
/// + system vars as context), which is different from runtime injection.
#[tokio::test]
async fn test_env_merge_priority_chain() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("priority");

    // Create env file
    let env_file_path = temp_dir.path().join(".env");
    std::fs::write(
        &env_file_path,
        "VAR_ONLY_FILE=from_file\nVAR_FILE_AND_ARRAY=from_file\n",
    )
    .unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    concat!(
                        "echo VAR_ONLY_FILE=$(printenv VAR_ONLY_FILE) >> {} && ",
                        "echo VAR_FILE_AND_ARRAY=$(printenv VAR_FILE_AND_ARRAY) >> {} && ",
                        "echo VAR_ONLY_ARRAY=$(printenv VAR_ONLY_ARRAY) >> {} && ",
                        "echo HOME_SET=$(if printenv HOME > /dev/null 2>&1; then echo yes; else echo no; fi) >> {} && ",
                        "sleep 3600"
                    ),
                    marker_path.display(),
                    marker_path.display(),
                    marker_path.display(),
                    marker_path.display()
                ),
            ])
            .with_env_file(env_file_path)
            .with_environment(vec![
                "VAR_FILE_AND_ARRAY=from_array".to_string(),
                "VAR_ONLY_ARRAY=only_in_array".to_string(),
            ])
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("priority", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Service should have written priority test");
    let content = content.unwrap();

    // VAR_ONLY_FILE: only in env_file
    assert!(
        content.contains("VAR_ONLY_FILE=from_file"),
        "VAR_ONLY_FILE should come from env_file. Got: {}",
        content
    );

    // VAR_FILE_AND_ARRAY: in both, array wins
    assert!(
        content.contains("VAR_FILE_AND_ARRAY=from_array"),
        "VAR_FILE_AND_ARRAY should come from array (higher priority). Got: {}",
        content
    );

    // VAR_ONLY_ARRAY: only in array
    assert!(
        content.contains("VAR_ONLY_ARRAY=only_in_array"),
        "VAR_ONLY_ARRAY should come from array. Got: {}",
        content
    );

    // System env (HOME) should still exist
    assert!(
        content.contains("HOME_SET=yes"),
        "System env (HOME) should be inherited. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// Missing env_file is silently ignored (common for optional .env files)
#[tokio::test]
async fn test_missing_env_file_ignored() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("missing_file");

    // Reference a non-existent env file
    let non_existent_path = temp_dir.path().join(".env.does.not.exist");

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"started\" >> {} && sleep 3600",
                    marker_path.display()
                ),
            ])
            .with_env_file(non_existent_path)
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // Should start successfully even with missing env_file
    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("missing_file", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Service should start even with missing env_file");
    assert!(
        content.unwrap().contains("started"),
        "Service should have written 'started'"
    );

    harness.stop_service("test").await.unwrap();
}

/// Variables with equals signs in values are handled correctly
#[tokio::test]
async fn test_env_with_equals_in_value() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("equals");

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"CONNECTION=$CONNECTION\" >> {} && sleep 3600",
                    marker_path.display()
                ),
            ])
            .with_environment(vec!["CONNECTION=host=localhost;port=5432".to_string()])
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("equals", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Service should have written connection string");
    let content = content.unwrap();
    assert!(
        content.contains("CONNECTION=host=localhost;port=5432"),
        "Values with equals signs should be preserved. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// Empty environment array works (uses minimal system env only)
#[tokio::test]
async fn test_empty_environment() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("empty");

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"USER=$USER\" >> {} && sleep 3600",
                    marker_path.display()
                ),
            ])
            // No environment or env_file specified
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("empty", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Service should start with empty env config");
    let content = content.unwrap();
    // USER should come from system environment
    assert!(
        content.contains("USER=") && !content.contains("USER=$USER"),
        "System USER var should be available. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// Multiple services can have independent environment configurations
#[tokio::test]
async fn test_independent_service_environments() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path_a = marker.marker_path("service_a");
    let marker_path_b = marker.marker_path("service_b");

    let config = TestConfigBuilder::new()
        .add_service(
            "service_a",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"SERVICE_NAME=$SERVICE_NAME\" >> {} && sleep 3600",
                    marker_path_a.display()
                ),
            ])
            .with_environment(vec!["SERVICE_NAME=alpha".to_string()])
            .build(),
        )
        .add_service(
            "service_b",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"SERVICE_NAME=$SERVICE_NAME\" >> {} && sleep 3600",
                    marker_path_b.display()
                ),
            ])
            .with_environment(vec!["SERVICE_NAME=beta".to_string()])
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("service_a").await.unwrap();
    harness.start_service("service_b").await.unwrap();

    let content_a = marker
        .wait_for_marker_content("service_a", Duration::from_secs(2))
        .await;
    let content_b = marker
        .wait_for_marker_content("service_b", Duration::from_secs(2))
        .await;

    assert!(content_a.is_some(), "Service A should have written its env");
    assert!(content_b.is_some(), "Service B should have written its env");

    assert!(
        content_a.unwrap().contains("SERVICE_NAME=alpha"),
        "Service A should have SERVICE_NAME=alpha"
    );
    assert!(
        content_b.unwrap().contains("SERVICE_NAME=beta"),
        "Service B should have SERVICE_NAME=beta"
    );

    harness.stop_all().await.unwrap();
}

/// Env file with comments and empty lines is parsed correctly
#[tokio::test]
async fn test_env_file_with_comments() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("comments");

    // Create env file with comments and empty lines
    let env_file_path = temp_dir.path().join(".env");
    std::fs::write(
        &env_file_path,
        r#"# This is a comment
FIRST_VAR=first_value

# Another comment
SECOND_VAR=second_value
# COMMENTED_VAR=should_not_appear
"#,
    )
    .unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"FIRST_VAR=$FIRST_VAR SECOND_VAR=$SECOND_VAR COMMENTED_VAR=$COMMENTED_VAR\" >> {} && sleep 3600",
                    marker_path.display()
                ),
            ])
            .with_env_file(env_file_path)
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("comments", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Service should have parsed env file");
    let content = content.unwrap();

    assert!(
        content.contains("FIRST_VAR=first_value"),
        "FIRST_VAR should be parsed. Got: {}",
        content
    );
    assert!(
        content.contains("SECOND_VAR=second_value"),
        "SECOND_VAR should be parsed. Got: {}",
        content
    );
    // Commented var should be empty (not set) - it will appear as COMMENTED_VAR= (empty)
    assert!(
        content.contains("COMMENTED_VAR=") && !content.contains("COMMENTED_VAR=should_not_appear"),
        "COMMENTED_VAR should not have the commented value. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// Env file with quoted values is parsed correctly
#[tokio::test]
async fn test_env_file_quoted_values() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("quoted");

    // Create env file with quoted values (dotenvy requires quotes for values with spaces)
    let env_file_path = temp_dir.path().join(".env");
    std::fs::write(
        &env_file_path,
        r#"DOUBLE_QUOTED="hello world"
SINGLE_QUOTED='hello world'
"#,
    )
    .unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"DOUBLE_QUOTED=$DOUBLE_QUOTED SINGLE_QUOTED=$SINGLE_QUOTED\" >> {} && sleep 3600",
                    marker_path.display()
                ),
            ])
            .with_env_file(env_file_path)
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("quoted", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Service should have parsed quoted env file");
    let content = content.unwrap();

    assert!(
        content.contains("DOUBLE_QUOTED=hello world"),
        "Double quoted value should be unquoted. Got: {}",
        content
    );
    assert!(
        content.contains("SINGLE_QUOTED=hello world"),
        "Single quoted value should be unquoted. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}
