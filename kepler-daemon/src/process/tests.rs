use super::*;

#[test]
fn test_parse_signal_name_with_sig_prefix() {
    assert_eq!(parse_signal_name("SIGTERM"), Some(15));
    assert_eq!(parse_signal_name("SIGKILL"), Some(9));
    assert_eq!(parse_signal_name("SIGINT"), Some(2));
    assert_eq!(parse_signal_name("SIGHUP"), Some(1));
    assert_eq!(parse_signal_name("SIGQUIT"), Some(3));
    assert_eq!(parse_signal_name("SIGUSR1"), Some(10));
    assert_eq!(parse_signal_name("SIGUSR2"), Some(12));
}

#[test]
fn test_parse_signal_name_without_prefix() {
    assert_eq!(parse_signal_name("TERM"), Some(15));
    assert_eq!(parse_signal_name("KILL"), Some(9));
    assert_eq!(parse_signal_name("INT"), Some(2));
    assert_eq!(parse_signal_name("HUP"), Some(1));
    assert_eq!(parse_signal_name("QUIT"), Some(3));
    assert_eq!(parse_signal_name("USR1"), Some(10));
    assert_eq!(parse_signal_name("USR2"), Some(12));
}

#[test]
fn test_parse_signal_name_lowercase() {
    assert_eq!(parse_signal_name("sigterm"), Some(15));
    assert_eq!(parse_signal_name("kill"), Some(9));
    assert_eq!(parse_signal_name("sigkill"), Some(9));
    assert_eq!(parse_signal_name("term"), Some(15));
}

#[test]
fn test_parse_signal_name_numeric() {
    assert_eq!(parse_signal_name("9"), Some(9));
    assert_eq!(parse_signal_name("15"), Some(15));
    assert_eq!(parse_signal_name("2"), Some(2));
    assert_eq!(parse_signal_name("1"), Some(1));
}

#[test]
fn test_parse_signal_name_invalid() {
    assert_eq!(parse_signal_name("INVALID"), None);
    assert_eq!(parse_signal_name(""), None);
    assert_eq!(parse_signal_name("abc"), None);
}
