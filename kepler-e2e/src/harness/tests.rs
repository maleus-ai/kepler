use super::*;

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
