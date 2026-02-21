use super::*;

#[test]
fn test_config_builder() {
    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_healthcheck(TestHealthCheckBuilder::always_healthy().build())
                .build(),
        )
        .build();

    assert!(config.services.contains_key("test"));
    assert!(config.services["test"].has_healthcheck());
}

#[test]
fn test_health_check_builder() {
    let hc = TestHealthCheckBuilder::always_healthy()
        .with_interval(Duration::from_secs(5))
        .with_retries(5)
        .build();

    let cmd: Vec<&str> = hc.command.as_static().unwrap().iter()
        .filter_map(|cv| cv.as_static().map(|s| s.as_str()))
        .collect();
    assert_eq!(cmd, vec!["true"]);
    assert_eq!(hc.interval, Duration::from_secs(5));
    assert_eq!(hc.retries, 5);
}
