use super::*;
use crate::config::{ConfigValue, DependsOn, RestartConfig, RestartPolicy};

fn make_service(deps: Vec<&str>) -> RawServiceConfig {
    let depends_on = if deps.is_empty() {
        DependsOn::default()
    } else {
        // Parse from YAML to get correct DependsOn
        let yaml = serde_yaml::to_string(&deps).unwrap();
        serde_yaml::from_str(&yaml).unwrap()
    };
    RawServiceConfig {
        command: ConfigValue::wrap_vec(vec!["test".to_string()]).into(),
        depends_on,
        ..Default::default()
    }
}

#[test]
fn test_no_deps() {
    let mut services = HashMap::new();
    services.insert("a".to_string(), make_service(vec![]));
    services.insert("b".to_string(), make_service(vec![]));

    let order = get_start_order(&services).unwrap();
    assert_eq!(order.len(), 2);
}

#[test]
fn test_simple_deps() {
    let mut services = HashMap::new();
    services.insert("a".to_string(), make_service(vec![]));
    services.insert("b".to_string(), make_service(vec!["a"]));

    let order = get_start_order(&services).unwrap();
    assert_eq!(order, vec!["a", "b"]);
}

#[test]
fn test_chain_deps() {
    let mut services = HashMap::new();
    services.insert("a".to_string(), make_service(vec![]));
    services.insert("b".to_string(), make_service(vec!["a"]));
    services.insert("c".to_string(), make_service(vec!["b"]));

    let order = get_start_order(&services).unwrap();
    assert_eq!(order, vec!["a", "b", "c"]);
}

#[test]
fn test_cycle_detection() {
    let mut services = HashMap::new();
    services.insert("a".to_string(), make_service(vec!["b"]));
    services.insert("b".to_string(), make_service(vec!["a"]));

    let result = get_start_order(&services);
    assert!(matches!(result, Err(DaemonError::DependencyCycle(_))));
}

#[test]
fn test_missing_dep() {
    let mut services = HashMap::new();
    services.insert("a".to_string(), make_service(vec!["nonexistent"]));

    let result = get_start_order(&services);
    assert!(matches!(result, Err(DaemonError::MissingDependency { .. })));
}

// --- Topological sort tests ---

#[test]
fn test_topological_sort_deterministic_ordering() {
    // Services at the same level should have a deterministic order
    // Kahn's algorithm with sorted queue (pop from end) gives reverse-alphabetical
    let mut services = HashMap::new();
    services.insert("zebra".to_string(), make_service(vec![]));
    services.insert("alpha".to_string(), make_service(vec![]));
    services.insert("mango".to_string(), make_service(vec![]));

    let order1 = get_start_order(&services).unwrap();
    let order2 = get_start_order(&services).unwrap();
    assert_eq!(order1, order2, "Order should be deterministic across runs");
    assert_eq!(order1.len(), 3);
}

#[test]
fn test_topological_sort_diamond() {
    // Diamond: A → B, A → C, B → D, C → D
    let mut services = HashMap::new();
    services.insert("a".to_string(), make_service(vec![]));
    services.insert("b".to_string(), make_service(vec!["a"]));
    services.insert("c".to_string(), make_service(vec!["a"]));
    services.insert("d".to_string(), make_service(vec!["b", "c"]));

    let order = get_start_order(&services).unwrap();
    assert_eq!(order[0], "a", "A must be first");
    assert_eq!(order[3], "d", "D must be last");
    // B and C can be in either order but both before D
    assert!(order[1..3].contains(&"b".to_string()));
    assert!(order[1..3].contains(&"c".to_string()));
}

#[test]
fn test_stop_order_reverse_of_start() {
    let mut services = HashMap::new();
    services.insert("a".to_string(), make_service(vec![]));
    services.insert("b".to_string(), make_service(vec!["a"]));
    services.insert("c".to_string(), make_service(vec!["b"]));

    let start = get_start_order(&services).unwrap();
    let stop = get_stop_order(&services).unwrap();
    let mut reversed_start = start.clone();
    reversed_start.reverse();
    assert_eq!(stop, reversed_start, "Stop order should be reverse of start order");
}

// --- get_start_levels tests ---

#[test]
fn test_start_levels_no_deps() {
    // All independent services should be at level 0
    let mut services = HashMap::new();
    services.insert("a".to_string(), make_service(vec![]));
    services.insert("b".to_string(), make_service(vec![]));
    services.insert("c".to_string(), make_service(vec![]));

    let levels = get_start_levels(&services).unwrap();
    assert_eq!(levels.len(), 1, "All should be at single level");
    assert_eq!(levels[0].len(), 3);
}

#[test]
fn test_start_levels_chain() {
    // Chain: a → b → c → d (each at a different level)
    let mut services = HashMap::new();
    services.insert("a".to_string(), make_service(vec![]));
    services.insert("b".to_string(), make_service(vec!["a"]));
    services.insert("c".to_string(), make_service(vec!["b"]));
    services.insert("d".to_string(), make_service(vec!["c"]));

    let levels = get_start_levels(&services).unwrap();
    assert_eq!(levels.len(), 4);
    assert_eq!(levels[0], vec!["a"]);
    assert_eq!(levels[1], vec!["b"]);
    assert_eq!(levels[2], vec!["c"]);
    assert_eq!(levels[3], vec!["d"]);
}

#[test]
fn test_start_levels_diamond() {
    // Diamond: a(L0), b+c(L1), d(L2)
    let mut services = HashMap::new();
    services.insert("a".to_string(), make_service(vec![]));
    services.insert("b".to_string(), make_service(vec!["a"]));
    services.insert("c".to_string(), make_service(vec!["a"]));
    services.insert("d".to_string(), make_service(vec!["b", "c"]));

    let levels = get_start_levels(&services).unwrap();
    assert_eq!(levels.len(), 3);
    assert_eq!(levels[0], vec!["a"]);
    assert_eq!(levels[1], vec!["b", "c"]);
    assert_eq!(levels[2], vec!["d"]);
}

#[test]
fn test_start_levels_wide_tree() {
    // Wide tree: root → {a, b, c, d, e} (all at level 1)
    let mut services = HashMap::new();
    services.insert("root".to_string(), make_service(vec![]));
    for name in &["a", "b", "c", "d", "e"] {
        services.insert(name.to_string(), make_service(vec!["root"]));
    }

    let levels = get_start_levels(&services).unwrap();
    assert_eq!(levels.len(), 2);
    assert_eq!(levels[0], vec!["root"]);
    assert_eq!(levels[1].len(), 5, "All children at level 1");
}

#[test]
fn test_start_levels_complex_graph() {
    // a(L0), b(L0), c depends on a(L1), d depends on a+b(L1), e depends on c+d(L2)
    let mut services = HashMap::new();
    services.insert("a".to_string(), make_service(vec![]));
    services.insert("b".to_string(), make_service(vec![]));
    services.insert("c".to_string(), make_service(vec!["a"]));
    services.insert("d".to_string(), make_service(vec!["a", "b"]));
    services.insert("e".to_string(), make_service(vec!["c", "d"]));

    let levels = get_start_levels(&services).unwrap();
    assert_eq!(levels.len(), 3);
    assert_eq!(levels[0], vec!["a", "b"]);
    assert_eq!(levels[1], vec!["c", "d"]);
    assert_eq!(levels[2], vec!["e"]);
}

// --- get_service_with_deps tests ---

#[test]
fn test_service_with_deps_no_deps() {
    let mut services = HashMap::new();
    services.insert("a".to_string(), make_service(vec![]));
    services.insert("b".to_string(), make_service(vec![]));

    let result = get_service_with_deps("a", &services).unwrap();
    assert_eq!(result, vec!["a"], "Only the service itself, no deps");
}

#[test]
fn test_service_with_deps_transitive() {
    // a → b → c, d independent
    let mut services = HashMap::new();
    services.insert("a".to_string(), make_service(vec![]));
    services.insert("b".to_string(), make_service(vec!["a"]));
    services.insert("c".to_string(), make_service(vec!["b"]));
    services.insert("d".to_string(), make_service(vec![]));

    let result = get_service_with_deps("c", &services).unwrap();
    assert_eq!(result, vec!["a", "b", "c"], "Should include all transitive deps");
    // d should NOT be included
    assert!(!result.contains(&"d".to_string()));
}

#[test]
fn test_service_with_deps_diamond() {
    let mut services = HashMap::new();
    services.insert("a".to_string(), make_service(vec![]));
    services.insert("b".to_string(), make_service(vec!["a"]));
    services.insert("c".to_string(), make_service(vec!["a"]));
    services.insert("d".to_string(), make_service(vec!["b", "c"]));

    let result = get_service_with_deps("d", &services).unwrap();
    assert_eq!(result.len(), 4);
    assert_eq!(result[0], "a");
    assert_eq!(result[3], "d");
}

#[test]
fn test_service_with_deps_not_found() {
    let services = HashMap::new();
    let result = get_service_with_deps("nonexistent", &services);
    assert!(matches!(result, Err(DaemonError::ServiceNotFound(_))));
}

// --- Multi-node topology: start_levels ---

#[test]
fn test_start_levels_wide_tree_many_children() {
    // Parent with 10 children — all children at the same level
    let mut services = HashMap::new();
    services.insert("parent".to_string(), make_service(vec![]));
    for i in 0..10 {
        services.insert(format!("child-{}", i), make_service(vec!["parent"]));
    }

    let levels = get_start_levels(&services).unwrap();
    assert_eq!(levels.len(), 2, "Should have 2 levels");
    assert_eq!(levels[0], vec!["parent"]);
    assert_eq!(levels[1].len(), 10, "All 10 children at level 1");
}

#[test]
fn test_start_levels_double_diamond() {
    // Two diamonds sharing a root:
    //        a
    //       / \
    //      b   c
    //       \ / \
    //        d   e
    //         \ /
    //          f
    let mut services = HashMap::new();
    services.insert("a".to_string(), make_service(vec![]));
    services.insert("b".to_string(), make_service(vec!["a"]));
    services.insert("c".to_string(), make_service(vec!["a"]));
    services.insert("d".to_string(), make_service(vec!["b", "c"]));
    services.insert("e".to_string(), make_service(vec!["c"]));
    services.insert("f".to_string(), make_service(vec!["d", "e"]));

    let levels = get_start_levels(&services).unwrap();
    assert_eq!(levels.len(), 4);
    assert_eq!(levels[0], vec!["a"]);
    assert_eq!(levels[1], vec!["b", "c"]);
    assert_eq!(levels[2], vec!["d", "e"]);
    assert_eq!(levels[3], vec!["f"]);
}

#[test]
fn test_start_levels_independent_clusters() {
    // Two independent clusters should both start at level 0
    // Cluster 1: a → b → c
    // Cluster 2: x → y
    let mut services = HashMap::new();
    services.insert("a".to_string(), make_service(vec![]));
    services.insert("b".to_string(), make_service(vec!["a"]));
    services.insert("c".to_string(), make_service(vec!["b"]));
    services.insert("x".to_string(), make_service(vec![]));
    services.insert("y".to_string(), make_service(vec!["x"]));

    let levels = get_start_levels(&services).unwrap();
    assert_eq!(levels.len(), 3);
    // Level 0: a and x (both independent roots)
    assert_eq!(levels[0], vec!["a", "x"]);
    // Level 1: b and y
    assert_eq!(levels[1], vec!["b", "y"]);
    // Level 2: c
    assert_eq!(levels[2], vec!["c"]);
}

#[test]
fn test_start_levels_multiple_roots_converging() {
    // Multiple roots converge to a single node
    // a, b, c all independent → d depends on all three
    let mut services = HashMap::new();
    services.insert("a".to_string(), make_service(vec![]));
    services.insert("b".to_string(), make_service(vec![]));
    services.insert("c".to_string(), make_service(vec![]));
    services.insert("d".to_string(), make_service(vec!["a", "b", "c"]));

    let levels = get_start_levels(&services).unwrap();
    assert_eq!(levels.len(), 2);
    assert_eq!(levels[0].len(), 3, "All 3 roots at level 0");
    assert_eq!(levels[1], vec!["d"]);
}

#[test]
fn test_start_levels_asymmetric_paths() {
    // a → b → c → d
    //          ↑
    // x -------'
    // x has a short path (level 0), c depends on both b (level 2) and x (level 0)
    // c should be at level max(1, 2) + 1 = level 2 (from b path)
    let mut services = HashMap::new();
    services.insert("a".to_string(), make_service(vec![]));
    services.insert("b".to_string(), make_service(vec!["a"]));
    services.insert("x".to_string(), make_service(vec![]));
    services.insert("c".to_string(), make_service(vec!["b", "x"]));
    services.insert("d".to_string(), make_service(vec!["c"]));

    let levels = get_start_levels(&services).unwrap();
    assert_eq!(levels.len(), 4);
    // Level 0: a and x (both roots)
    assert_eq!(levels[0], vec!["a", "x"]);
    // Level 1: b
    assert_eq!(levels[1], vec!["b"]);
    // Level 2: c (max depth of deps is b at level 1)
    assert_eq!(levels[2], vec!["c"]);
    // Level 3: d
    assert_eq!(levels[3], vec!["d"]);
}

// --- Restart policy interaction with permanently unsatisfied deps ---

#[test]
fn test_restart_no_wont_restart() {
    let restart = RestartConfig::Simple(RestartPolicy::No);
    assert!(!restart.should_restart_on_exit(Some(0)));
    assert!(!restart.should_restart_on_exit(Some(1)));
    assert!(!restart.should_restart_on_exit(None));
}

#[test]
fn test_restart_always_will_restart() {
    let restart = RestartConfig::Simple(RestartPolicy::Always);
    assert!(restart.should_restart_on_exit(Some(0)));
    assert!(restart.should_restart_on_exit(Some(1)));
    assert!(restart.should_restart_on_exit(None));
}

#[test]
fn test_restart_on_failure_only_on_nonzero() {
    let restart = RestartConfig::Simple(RestartPolicy::OnFailure);
    assert!(!restart.should_restart_on_exit(Some(0)), "Exit 0 should not restart");
    assert!(restart.should_restart_on_exit(Some(1)), "Exit 1 should restart");
    assert!(restart.should_restart_on_exit(None), "Signal kill (None) should restart");
}

// --- Transient exit satisfaction tests ---

fn make_state_with_status(status: ServiceStatus, exit_code: Option<i32>) -> ServiceState {
    ServiceState {
        status,
        exit_code,
        ..Default::default()
    }
}

#[test]
fn test_transient_exited_restart_always() {
    let restart = RestartConfig::Simple(RestartPolicy::Always);
    let state = make_state_with_status(ServiceStatus::Exited, Some(0));
    assert!(is_transient_satisfaction(&state, &restart), "Exited(0) + restart:always should be transient");

    let state = make_state_with_status(ServiceStatus::Exited, Some(1));
    assert!(is_transient_satisfaction(&state, &restart), "Exited(1) + restart:always should be transient");
}

#[test]
fn test_transient_exited_restart_no() {
    let restart = RestartConfig::Simple(RestartPolicy::No);
    let state = make_state_with_status(ServiceStatus::Exited, Some(0));
    assert!(!is_transient_satisfaction(&state, &restart), "Exited(0) + restart:no should NOT be transient");

    let state = make_state_with_status(ServiceStatus::Exited, Some(1));
    assert!(!is_transient_satisfaction(&state, &restart), "Exited(1) + restart:no should NOT be transient");
}

#[test]
fn test_transient_exited_restart_on_failure() {
    let restart = RestartConfig::Simple(RestartPolicy::OnFailure);

    // Exit 1 → will restart → transient
    let state = make_state_with_status(ServiceStatus::Exited, Some(1));
    assert!(is_transient_satisfaction(&state, &restart), "Exited(1) + restart:on-failure should be transient");

    // Exit 0 → won't restart → permanent
    let state = make_state_with_status(ServiceStatus::Exited, Some(0));
    assert!(!is_transient_satisfaction(&state, &restart), "Exited(0) + restart:on-failure should NOT be transient");
}

#[test]
fn test_transient_killed_restart_always() {
    let restart = RestartConfig::Simple(RestartPolicy::Always);
    let state = make_state_with_status(ServiceStatus::Killed, None);
    assert!(is_transient_satisfaction(&state, &restart), "Killed + restart:always should be transient");
}

#[test]
fn test_transient_failed_restart_always() {
    let restart = RestartConfig::Simple(RestartPolicy::Always);
    let state = make_state_with_status(ServiceStatus::Failed, None);
    assert!(is_transient_satisfaction(&state, &restart), "Failed + restart:always should be transient");
}

#[test]
fn test_transient_stopped_never_transient() {
    // Stopped means explicitly stopped by user — never transient regardless of restart policy
    let restart = RestartConfig::Simple(RestartPolicy::Always);
    let state = make_state_with_status(ServiceStatus::Stopped, Some(0));
    assert!(!is_transient_satisfaction(&state, &restart), "Stopped + restart:always should NOT be transient");
}

#[test]
fn test_transient_running_not_transient() {
    // Running is not a terminal state — transient filter doesn't apply
    let restart = RestartConfig::Simple(RestartPolicy::Always);
    let state = make_state_with_status(ServiceStatus::Running, None);
    assert!(!is_transient_satisfaction(&state, &restart), "Running should NOT be transient");
}

#[test]
fn test_transient_waiting_not_transient() {
    let restart = RestartConfig::Simple(RestartPolicy::Always);
    let state = make_state_with_status(ServiceStatus::Waiting, None);
    assert!(!is_transient_satisfaction(&state, &restart), "Waiting should NOT be transient");
}

#[test]
fn test_transient_skipped_not_transient() {
    let restart = RestartConfig::Simple(RestartPolicy::Always);
    let state = make_state_with_status(ServiceStatus::Skipped, None);
    assert!(!is_transient_satisfaction(&state, &restart), "Skipped should NOT be transient");
}
