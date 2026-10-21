use super::*;

#[tokio::test]
async fn test_extract_pid_from_ps_uses_pid_header() -> E2eResult<()> {
    let harness = E2eHarness::new().await?;
    for (output, expected) in [
        (
            "NAME STATUS PID RESTARTS\nworker Up 5s 1234 0\n",
            Some(1234),
        ),
        (
            "NAME STATUS PID RESTARTS\nworker Up 2m (healthy) 4321 3\n",
            Some(4321),
        ),
        ("NAME STATUS PID\nworker Up 5s 1234\n", Some(1234)),
        (
            "NAME STATUS PID RESTARTS\nworker Exited (1) 5s ago - 3\n",
            None,
        ),
        (
            "NAME STATUS PID RESTARTS\nworker-extra Up 5s 9999 0\nworker Up 5s 1234 1\n",
            Some(1234),
        ),
        ("NAME STATUS PID RESTARTS\nother Up 5s 9999 0\n", None),
        ("", None),
    ] {
        assert_eq!(
            harness.extract_pid_from_ps(output, "worker"),
            expected,
            "{output}"
        );
    }
    Ok(())
}

#[test]
fn test_command_output() {
    let output = CommandOutput {
        stdout: "Hello World".to_string(),
        stderr: "".to_string(),
        exit_code: 0,
    };

    assert!(output.success());
    assert!(output.stdout_contains("Hello"));
    assert!(!output.stdout_contains("Goodbye"));
}
