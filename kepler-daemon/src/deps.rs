use std::collections::{HashMap, HashSet};

use crate::config::{DependencyCondition, DependencyConfig, RawServiceConfig, RestartConfig, RestartPolicy};
use crate::config_actor::ConfigActorHandle;
use crate::errors::{DaemonError, Result};
use crate::state::{ServiceState, ServiceStatus};

/// Get the order in which services should be started (respecting dependencies)
/// Returns services in topological order (dependencies first)
pub fn get_start_order(services: &HashMap<String, RawServiceConfig>) -> Result<Vec<String>> {
    topological_sort(services)
}

/// Get the order in which services should be stopped (reverse of start order)
pub fn get_stop_order(services: &HashMap<String, RawServiceConfig>) -> Result<Vec<String>> {
    let mut order = topological_sort(services)?;
    order.reverse();
    Ok(order)
}

/// Get services grouped by dependency level for parallel execution.
///
/// Services at the same level have no dependencies on each other and can be started
/// in parallel. Services at level N only depend on services at levels < N.
///
/// Example: For services A (no deps), B (no deps), C (depends on A), D (depends on A, B)
/// Returns: [[A, B], [C, D]]
/// - Level 0: A, B (no deps, can start in parallel)
/// - Level 1: C, D (depend only on level 0 services, can start in parallel after level 0)
pub fn get_start_levels(services: &HashMap<String, RawServiceConfig>) -> Result<Vec<Vec<String>>> {
    let service_names: HashSet<_> = services.keys().cloned().collect();

    // Validate all dependencies exist
    for (name, raw) in services {
        for dep in raw.depends_on.names() {
            if !service_names.contains(&dep) {
                return Err(DaemonError::MissingDependency {
                    service: name.clone(),
                    dependency: dep,
                });
            }
        }
    }

    // Calculate the level of each service (max depth in dependency tree)
    let mut levels: HashMap<String, usize> = HashMap::new();
    let mut computed: HashSet<String> = HashSet::new();

    fn compute_level(
        name: &str,
        services: &HashMap<String, RawServiceConfig>,
        levels: &mut HashMap<String, usize>,
        computed: &mut HashSet<String>,
        stack: &mut HashSet<String>,
    ) -> Result<usize> {
        if let Some(&level) = levels.get(name) {
            return Ok(level);
        }

        if stack.contains(name) {
            return Err(DaemonError::DependencyCycle(format!(
                "Cycle detected involving: {}",
                name
            )));
        }

        stack.insert(name.to_string());

        let raw = services.get(name).ok_or_else(|| {
            DaemonError::ServiceNotFound(name.to_string())
        })?;

        let dep_names = raw.depends_on.names();
        let level = if dep_names.is_empty() {
            0
        } else {
            let mut max_dep_level = 0;
            for dep in dep_names {
                let dep_level = compute_level(&dep, services, levels, computed, stack)?;
                max_dep_level = max_dep_level.max(dep_level);
            }
            max_dep_level + 1
        };

        stack.remove(name);
        levels.insert(name.to_string(), level);
        computed.insert(name.to_string());

        Ok(level)
    }

    // Compute level for each service
    for name in services.keys() {
        let mut stack = HashSet::new();
        compute_level(name, services, &mut levels, &mut computed, &mut stack)?;
    }

    // Group services by level
    let max_level = levels.values().copied().max().unwrap_or(0);
    let mut result: Vec<Vec<String>> = vec![Vec::new(); max_level + 1];

    for (name, level) in levels {
        result[level].push(name);
    }

    // Sort each level for deterministic ordering
    for level in &mut result {
        level.sort();
    }

    Ok(result)
}

/// Perform topological sort using Kahn's algorithm
fn topological_sort(services: &HashMap<String, RawServiceConfig>) -> Result<Vec<String>> {
    let service_names: HashSet<_> = services.keys().cloned().collect();

    // Validate all dependencies exist
    for (name, raw) in services {
        for dep in raw.depends_on.names() {
            if !service_names.contains(&dep) {
                return Err(DaemonError::MissingDependency {
                    service: name.clone(),
                    dependency: dep,
                });
            }
        }
    }

    // Build adjacency list and in-degree count
    // Edge from A -> B means B depends on A (A must start before B)
    let mut in_degree: HashMap<String, usize> = HashMap::new();
    let mut dependents: HashMap<String, Vec<String>> = HashMap::new();

    for name in services.keys() {
        in_degree.insert(name.clone(), 0);
        dependents.insert(name.clone(), Vec::new());
    }

    for (name, raw) in services {
        for dep in raw.depends_on.names() {
            // name depends on dep, so increment in_degree of name
            *in_degree.get_mut(name).ok_or_else(|| {
                DaemonError::Internal(format!("unknown service '{}' in dependency graph", name))
            })? += 1;
            // dep has name as a dependent
            dependents.get_mut(&dep).ok_or_else(|| {
                DaemonError::MissingDependency {
                    service: name.clone(),
                    dependency: dep.clone(),
                }
            })?.push(name.clone());
        }
    }

    // Start with nodes that have no dependencies
    let mut queue: Vec<String> = in_degree
        .iter()
        .filter(|&(_, deg)| *deg == 0)
        .map(|(name, _)| name.clone())
        .collect();

    // Sort for deterministic ordering
    queue.sort();

    let mut result = Vec::new();

    while let Some(node) = queue.pop() {
        result.push(node.clone());

        // For each dependent of this node, decrease in_degree
        let deps = dependents.get(&node).cloned().unwrap_or_default();
        for dep in deps {
            let deg = in_degree.get_mut(&dep).ok_or_else(|| {
                DaemonError::Internal(format!("unknown dependency '{}' in service '{}'", dep, node))
            })?;
            *deg -= 1;
            if *deg == 0 {
                // Insert in sorted position for deterministic order
                let pos = queue.partition_point(|x| x > &dep);
                queue.insert(pos, dep);
            }
        }
    }

    // Check for cycles
    if result.len() != services.len() {
        let remaining: Vec<_> = services
            .keys()
            .filter(|k| !result.contains(k))
            .cloned()
            .collect();
        return Err(DaemonError::DependencyCycle(format!(
            "Cycle detected involving: {}",
            remaining.join(", ")
        )));
    }

    Ok(result)
}

/// Get only the services that need to be started to run a specific service
/// (the service itself and all its transitive dependencies)
pub fn get_service_with_deps(
    service: &str,
    services: &HashMap<String, RawServiceConfig>,
) -> Result<Vec<String>> {
    if !services.contains_key(service) {
        return Err(DaemonError::ServiceNotFound(service.to_string()));
    }

    let mut needed: HashSet<String> = HashSet::new();
    collect_deps(service, services, &mut needed)?;

    // Filter to only needed services and sort
    let filtered: HashMap<_, _> = services
        .iter()
        .filter(|(k, _)| needed.contains(*k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    topological_sort(&filtered)
}

fn collect_deps(
    service: &str,
    services: &HashMap<String, RawServiceConfig>,
    needed: &mut HashSet<String>,
) -> Result<()> {
    if needed.contains(service) {
        return Ok(());
    }

    needed.insert(service.to_string());

    if let Some(raw) = services.get(service) {
        for dep in raw.depends_on.names() {
            if !services.contains_key(&dep) {
                return Err(DaemonError::MissingDependency {
                    service: service.to_string(),
                    dependency: dep,
                });
            }
            collect_deps(&dep, services, needed)?;
        }
    }

    Ok(())
}

/// Check if a dependency condition is met based on service state.
///
/// This is the shared condition-matching logic used by both async `check_dependency_satisfied`
/// and the synchronous `is_condition_satisfied_sync` inside the actor.
///
/// When `dep_restart_config` is `Some`, applies transient exit filtering: if the dep exited
/// but its restart policy will restart it, terminal-state conditions are NOT considered met.
pub fn is_condition_met(
    state: &ServiceState,
    dep_config: &DependencyConfig,
    dep_restart_config: Option<&RestartConfig>,
) -> bool {
    // If the dependency is Skipped and allow_skipped is true, treat as satisfied
    if state.status == ServiceStatus::Skipped && dep_config.allow_skipped {
        return true;
    }
    // If the dependency is Skipped and allow_skipped is false, never satisfied
    if state.status == ServiceStatus::Skipped {
        return false;
    }

    let satisfied = match &dep_config.condition {
        DependencyCondition::ServiceStarted => {
            matches!(
                state.status,
                ServiceStatus::Running | ServiceStatus::Healthy | ServiceStatus::Unhealthy
            )
        }
        DependencyCondition::ServiceHealthy => {
            state.status == ServiceStatus::Healthy
        }
        DependencyCondition::ServiceCompletedSuccessfully => {
            matches!(state.status, ServiceStatus::Stopped | ServiceStatus::Exited) && state.exit_code == Some(0)
        }
        DependencyCondition::ServiceUnhealthy => {
            state.status == ServiceStatus::Unhealthy && state.was_healthy
        }
        DependencyCondition::ServiceFailed => {
            let is_failure = match state.status {
                ServiceStatus::Failed | ServiceStatus::Killed => true,
                ServiceStatus::Exited => state.exit_code != Some(0),
                _ => false,
            };
            is_failure
                && dep_config
                    .exit_code
                    .matches(state.exit_code.unwrap_or(-1))
        }
        DependencyCondition::ServiceStopped => {
            matches!(
                state.status,
                ServiceStatus::Stopped | ServiceStatus::Exited | ServiceStatus::Failed | ServiceStatus::Killed
            ) && dep_config
                .exit_code
                .matches(state.exit_code.unwrap_or(-1))
        }
    };

    if !satisfied {
        return false;
    }

    // Filter transient exits: if dep exited but will restart, the condition is only
    // transiently satisfied. Stopped is excluded (explicitly stopped = won't restart).
    if let Some(restart_config) = dep_restart_config
        && is_transient_satisfaction(state, restart_config) {
            return false;
        }

    true
}

/// Check if a dependency condition is satisfied based on current service status.
///
/// This is the async version for use outside the actor (no transient filter —
/// callers handle transient exits separately).
pub async fn check_dependency_satisfied(
    dep_name: &str,
    dep_config: &DependencyConfig,
    handle: &ConfigActorHandle,
) -> bool {
    let state = match handle.get_service_state(dep_name).await {
        Some(s) => s,
        None => return false,
    };

    is_condition_met(&state, dep_config, None)
}

/// Check if a dependency is permanently unsatisfied.
///
/// A dependency is permanently unsatisfied when:
/// 1. The dependency service is in a terminal state (Stopped/Failed)
/// 2. Its restart policy won't restart it
/// 3. The condition is not currently satisfied
pub async fn is_dependency_permanently_unsatisfied(
    dep_name: &str,
    dep_config: &DependencyConfig,
    handle: &ConfigActorHandle,
    dep_restart_config: &RestartConfig,
) -> bool {
    let state = match handle.get_service_state(dep_name).await {
        Some(s) => s,
        None => return false,
    };

    // Skipped is always permanently unsatisfied (unless allow_skipped, handled by check_dependency_satisfied)
    if state.status == ServiceStatus::Skipped {
        return !dep_config.allow_skipped;
    }

    // Only check terminal states
    if !matches!(state.status, ServiceStatus::Stopped | ServiceStatus::Exited | ServiceStatus::Failed | ServiceStatus::Killed) {
        return false;
    }

    // If the condition is already satisfied, it's not unsatisfied
    if check_dependency_satisfied(dep_name, dep_config, handle).await {
        return false;
    }

    // Check if the restart policy would restart the service
    !dep_restart_config.should_restart_on_exit(state.exit_code)
}

/// Check if a dependency condition is structurally unreachable given the dep's restart policy.
///
/// For example, `service_failed` can never be permanently met when the dep has `restart: always`,
/// because the dep will always restart after any exit — it never stays in a terminal failure state.
///
/// Returns `Some(reason)` if unreachable, `None` if the condition could potentially be met.
pub fn is_condition_unreachable_by_policy(
    dep_name: &str,
    condition: &DependencyCondition,
    dep_restart_policy: &RestartPolicy,
) -> Option<String> {
    match (condition, dep_restart_policy) {
        // restart: always → service always restarts on any exit, so it can never
        // permanently reach a failed/stopped/exited state
        (DependencyCondition::ServiceFailed, RestartPolicy::Always) => {
            Some(format!(
                "`{}` has restart policy `always` — it will always restart, \
                 so `service_failed` can never be met",
                dep_name
            ))
        }
        (DependencyCondition::ServiceStopped, RestartPolicy::Always) => {
            Some(format!(
                "`{}` has restart policy `always` — it will always restart, \
                 so `service_stopped` can never be met",
                dep_name
            ))
        }
        (DependencyCondition::ServiceCompletedSuccessfully, RestartPolicy::Always) => {
            Some(format!(
                "`{}` has restart policy `always` — it will always restart, \
                 so `service_completed_successfully` can never be met",
                dep_name
            ))
        }
        // restart: on-failure → service restarts on non-zero exit, so service_failed
        // (which requires non-zero exit) can never be permanently met
        (DependencyCondition::ServiceFailed, RestartPolicy::OnFailure) => {
            Some(format!(
                "`{}` has restart policy `on-failure` — it will always restart on failure, \
                 so `service_failed` can never be met",
                dep_name
            ))
        }
        _ => None,
    }
}

/// Check if a dependency's satisfied condition is only transient because the dep will restart.
///
/// When a dependency exits but its restart policy will restart it, terminal-state conditions
/// (service_stopped, service_failed, service_completed_successfully) are only transiently
/// satisfied — the dep will restart and the condition will no longer hold.
///
/// `Stopped` status is excluded because it means the service was explicitly stopped by the
/// user and will NOT be restarted by the restart policy.
///
/// Returns true if the condition satisfaction is transient and should be ignored.
pub fn is_transient_satisfaction(dep_state: &ServiceState, restart_config: &RestartConfig) -> bool {
    matches!(dep_state.status, ServiceStatus::Exited | ServiceStatus::Killed | ServiceStatus::Failed)
        && restart_config.should_restart_on_exit(dep_state.exit_code)
}

/// Detect dependency cycles in the service configuration
pub fn detect_cycles(services: &HashMap<String, RawServiceConfig>) -> Result<()> {
    // Validate all dependencies exist first
    let service_names: HashSet<_> = services.keys().cloned().collect();
    for (name, raw) in services {
        for dep in raw.depends_on.names() {
            if !service_names.contains(&dep) {
                return Err(DaemonError::MissingDependency {
                    service: name.clone(),
                    dependency: dep,
                });
            }
        }
    }

    // Try to compute start order - will fail if there's a cycle
    topological_sort(services)?;
    Ok(())
}

#[cfg(test)]
mod tests {
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
            command: ConfigValue::Static(vec!["test".to_string()]),
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
}
