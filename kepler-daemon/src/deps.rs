use std::collections::{HashMap, HashSet};

use tracing::warn;

use crate::config::{DependencyCondition, DependencyConfig, ServiceConfig};
use crate::config_actor::ConfigActorHandle;
use crate::errors::{DaemonError, Result};
use crate::state::ServiceStatus;

/// Get the order in which services should be started (respecting dependencies)
/// Returns services in topological order (dependencies first)
pub fn get_start_order(services: &HashMap<String, ServiceConfig>) -> Result<Vec<String>> {
    topological_sort(services)
}

/// Get the order in which services should be stopped (reverse of start order)
pub fn get_stop_order(services: &HashMap<String, ServiceConfig>) -> Result<Vec<String>> {
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
pub fn get_start_levels(services: &HashMap<String, ServiceConfig>) -> Result<Vec<Vec<String>>> {
    let service_names: HashSet<_> = services.keys().cloned().collect();

    // Validate all dependencies exist
    for (name, config) in services {
        for dep in config.depends_on.names() {
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
        services: &HashMap<String, ServiceConfig>,
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

        let config = services.get(name).ok_or_else(|| {
            DaemonError::ServiceNotFound(name.to_string())
        })?;

        let dep_names = config.depends_on.names();
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
fn topological_sort(services: &HashMap<String, ServiceConfig>) -> Result<Vec<String>> {
    let service_names: HashSet<_> = services.keys().cloned().collect();

    // Validate all dependencies exist
    for (name, config) in services {
        for dep in config.depends_on.names() {
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

    for (name, config) in services {
        for dep in config.depends_on.names() {
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
    services: &HashMap<String, ServiceConfig>,
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
    services: &HashMap<String, ServiceConfig>,
    needed: &mut HashSet<String>,
) -> Result<()> {
    if needed.contains(service) {
        return Ok(());
    }

    needed.insert(service.to_string());

    if let Some(config) = services.get(service) {
        for dep in config.depends_on.names() {
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

/// Resolve effective_wait for each service, modifying the configs in place.
///
/// Processes services in topological order. For each service S:
/// 1. `S.wait = Some(true)` → `effective_wait = true`
/// 2. `S.wait = Some(false)` → `effective_wait = false`
/// 3. `S.wait = None` + no dependencies → `effective_wait = true`
/// 4. `S.wait = None` + has dependencies → `effective_wait = AND` over all deps of:
///    `[dep.wait.unwrap() AND edge_wait]`
///    where `edge_wait = dep_config.wait.unwrap_or(condition.is_startup_condition())`
///
/// When a service gets `effective_wait=false` without explicitly setting `wait: false`
/// (inherited from deps), emits a warning with the specific reasons.
pub fn resolve_effective_wait(
    services: &mut HashMap<String, ServiceConfig>,
) -> Result<()> {
    let topo_order = topological_sort(services)?;

    // Store computed effective_wait values as we go
    let mut effective_map: HashMap<String, bool> = HashMap::new();

    for name in &topo_order {
        let config = services.get(name).ok_or_else(|| {
            DaemonError::ServiceNotFound(name.clone())
        })?;

        let effective_wait = match config.wait {
            Some(true) => true,
            Some(false) => false,
            None => {
                if config.depends_on.is_empty() {
                    true
                } else {
                    // AND over all deps: dep.wait.unwrap() AND edge_wait
                    config.depends_on.iter().all(|(dep_name, dep_config)| {
                        let dep_effective = effective_map.get(&dep_name).copied().unwrap_or(true);
                        let edge_wait = dep_config.wait.unwrap_or_else(|| dep_config.condition.is_startup_condition());
                        dep_effective && edge_wait
                    })
                }
            }
        };

        // Emit warning if service gets effective_wait=false without explicit wait: false
        if !effective_wait && config.wait.is_none() {
            let mut reasons = Vec::new();
            for (dep_name, dep_config) in config.depends_on.iter() {
                let dep_effective = effective_map.get(&dep_name).copied().unwrap_or(true);
                let edge_wait = dep_config.wait.unwrap_or_else(|| dep_config.condition.is_startup_condition());
                if !dep_effective {
                    reasons.push(format!(
                        "dependency '{}' has effective_wait=false",
                        dep_name
                    ));
                }
                if !edge_wait {
                    reasons.push(format!(
                        "edge to '{}' (condition: {:?}) has wait=false",
                        dep_name, dep_config.condition
                    ));
                }
            }
            warn!(
                "Service '{}' will be deferred (effective_wait=false) because: {}. Add `wait: true` to override.",
                name,
                reasons.join("; ")
            );
        }

        effective_map.insert(name.clone(), effective_wait);
    }

    // Apply computed values back into wait field
    for (name, effective_wait) in &effective_map {
        if let Some(config) = services.get_mut(name) {
            config.wait = Some(*effective_wait);
        }
    }

    Ok(())
}

/// Check if a dependency condition is satisfied based on current service status
pub async fn check_dependency_satisfied(
    dep_name: &str,
    dep_config: &DependencyConfig,
    handle: &ConfigActorHandle,
) -> bool {
    let state = match handle.get_service_state(dep_name).await {
        Some(s) => s,
        None => return false,
    };

    match &dep_config.condition {
        DependencyCondition::ServiceStarted => {
            // Service is considered "started" if it's running (in any running state)
            matches!(
                state.status,
                ServiceStatus::Running | ServiceStatus::Healthy | ServiceStatus::Unhealthy
            )
        }
        DependencyCondition::ServiceHealthy => {
            // Service must be in the Healthy state
            state.status == ServiceStatus::Healthy
        }
        DependencyCondition::ServiceCompletedSuccessfully => {
            // Service must have stopped with exit code 0
            state.status == ServiceStatus::Stopped && state.exit_code == Some(0)
        }
        DependencyCondition::ServiceUnhealthy => {
            // Service must be unhealthy and must have been healthy before
            state.status == ServiceStatus::Unhealthy && state.was_healthy
        }
        DependencyCondition::ServiceFailed => {
            // Service must have failed, optionally matching exit code filter.
            // exit_code is None when the process was killed by a signal (no exit status),
            // mapped to -1 so signal-killed processes can be matched with exit_code: [-1].
            state.status == ServiceStatus::Failed
                && dep_config
                    .exit_code
                    .matches(state.exit_code.unwrap_or(-1))
        }
        DependencyCondition::ServiceStopped => {
            // Service must have stopped or failed, optionally matching exit code filter.
            // exit_code is None when the process was killed by a signal (no exit status),
            // mapped to -1 so signal-killed processes can be matched with exit_code: [-1].
            matches!(
                state.status,
                ServiceStatus::Stopped | ServiceStatus::Failed
            ) && dep_config
                .exit_code
                .matches(state.exit_code.unwrap_or(-1))
        }
    }
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
    dep_service_config: &ServiceConfig,
) -> bool {
    let state = match handle.get_service_state(dep_name).await {
        Some(s) => s,
        None => return false,
    };

    // Only check terminal states
    if !matches!(state.status, ServiceStatus::Stopped | ServiceStatus::Failed) {
        return false;
    }

    // If the condition is already satisfied, it's not unsatisfied
    if check_dependency_satisfied(dep_name, dep_config, handle).await {
        return false;
    }

    // Check if the restart policy would restart the service
    !dep_service_config.restart.should_restart_on_exit(state.exit_code)
}

/// Detect dependency cycles in the service configuration
pub fn detect_cycles(services: &HashMap<String, ServiceConfig>) -> Result<()> {
    // Validate all dependencies exist first
    let service_names: HashSet<_> = services.keys().cloned().collect();
    for (name, config) in services {
        for dep in config.depends_on.names() {
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
    use crate::config::{DependencyCondition, DependencyConfig, DependencyEntry, DependsOn, RestartConfig, RestartPolicy};

    fn make_service(deps: Vec<&str>) -> ServiceConfig {
        ServiceConfig {
            command: vec!["test".to_string()],
            working_dir: None,
            environment: vec![],
            env_file: None,
            sys_env: Default::default(),
            restart: RestartConfig::default(),
            depends_on: DependsOn::from(deps.into_iter().map(String::from).collect::<Vec<_>>()),
            healthcheck: None,
            hooks: None,
            logs: None,
            user: None,
            group: None,
            limits: None,
            wait: None,
        }
    }

    /// Make a service with explicit wait field
    fn make_service_with_wait(deps: Vec<&str>, wait: Option<bool>) -> ServiceConfig {
        let mut svc = make_service(deps);
        svc.wait = wait;
        svc
    }

    /// Make a service with extended dependency configs
    fn make_service_with_deps(deps: Vec<(&str, DependencyCondition, Option<bool>)>) -> ServiceConfig {
        let mut map = std::collections::HashMap::new();
        for (name, condition, wait) in deps {
            map.insert(name.to_string(), DependencyConfig {
                condition,
                wait,
                ..Default::default()
            });
        }
        ServiceConfig {
            command: vec!["test".to_string()],
            working_dir: None,
            environment: vec![],
            env_file: None,
            sys_env: Default::default(),
            restart: RestartConfig::default(),
            depends_on: DependsOn(if map.is_empty() {
                vec![]
            } else {
                vec![DependencyEntry::Extended(map)]
            }),
            healthcheck: None,
            hooks: None,
            logs: None,
            user: None,
            group: None,
            limits: None,
            wait: None,
        }
    }

    /// Make a service with extended deps and explicit service-level wait
    fn make_service_with_deps_and_wait(deps: Vec<(&str, DependencyCondition, Option<bool>)>, wait: Option<bool>) -> ServiceConfig {
        let mut svc = make_service_with_deps(deps);
        svc.wait = wait;
        svc
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

    // --- resolve_effective_wait tests ---

    #[test]
    fn test_all_startup() {
        // All services with startup conditions → all effective_wait = true
        let mut services = HashMap::new();
        services.insert("a".to_string(), make_service(vec![]));
        services.insert("b".to_string(), make_service_with_deps(vec![
            ("a", DependencyCondition::ServiceStarted, None),
        ]));
        services.insert("c".to_string(), make_service_with_deps(vec![
            ("b", DependencyCondition::ServiceHealthy, None),
        ]));

        resolve_effective_wait(&mut services).unwrap();
        assert!(services["a"].wait.unwrap());
        assert!(services["b"].wait.unwrap());
        assert!(services["c"].wait.unwrap());
    }

    #[test]
    fn test_deferred_propagation() {
        // E depends on D with service_failed (deferred), F depends on E with service_started
        // E should be deferred, F should also be deferred (target is deferred)
        let mut services = HashMap::new();
        services.insert("d".to_string(), make_service(vec![]));
        services.insert("e".to_string(), make_service_with_deps(vec![
            ("d", DependencyCondition::ServiceFailed, None),
        ]));
        services.insert("f".to_string(), make_service_with_deps(vec![
            ("e", DependencyCondition::ServiceStarted, None),
        ]));

        resolve_effective_wait(&mut services).unwrap();
        assert!(services["d"].wait.unwrap(), "D should be startup");
        assert!(!services["e"].wait.unwrap(), "E should be deferred (service_failed condition)");
        assert!(!services["f"].wait.unwrap(), "F should be deferred (depends on deferred E)");
    }

    #[test]
    fn test_mixed_startup_deferred() {
        // A, B, C, D are startup; E depends on D with service_failed; F, G depend on E
        let mut services = HashMap::new();
        services.insert("a".to_string(), make_service(vec![]));
        services.insert("b".to_string(), make_service_with_deps(vec![
            ("a", DependencyCondition::ServiceStarted, None),
        ]));
        services.insert("c".to_string(), make_service_with_deps(vec![
            ("a", DependencyCondition::ServiceHealthy, None),
        ]));
        services.insert("d".to_string(), make_service_with_deps(vec![
            ("b", DependencyCondition::ServiceStarted, None),
            ("c", DependencyCondition::ServiceStarted, None),
        ]));
        // E has deferred condition
        services.insert("e".to_string(), make_service_with_deps(vec![
            ("d", DependencyCondition::ServiceFailed, None),
        ]));
        services.insert("f".to_string(), make_service_with_deps(vec![
            ("e", DependencyCondition::ServiceStarted, None),
        ]));
        services.insert("g".to_string(), make_service_with_deps(vec![
            ("e", DependencyCondition::ServiceStarted, None),
        ]));

        resolve_effective_wait(&mut services).unwrap();
        assert!(services["a"].wait.unwrap());
        assert!(services["b"].wait.unwrap());
        assert!(services["c"].wait.unwrap());
        assert!(services["d"].wait.unwrap());
        assert!(!services["e"].wait.unwrap());
        assert!(!services["f"].wait.unwrap());
        assert!(!services["g"].wait.unwrap());
    }

    #[test]
    fn test_all_deferred() {
        // Single service with no deps is startup; service depending on it with deferred condition
        let mut services = HashMap::new();
        services.insert("a".to_string(), make_service_with_deps(vec![
            ("b", DependencyCondition::ServiceFailed, None),
        ]));
        services.insert("b".to_string(), make_service(vec![]));

        resolve_effective_wait(&mut services).unwrap();
        assert!(services["b"].wait.unwrap(), "B (no deps) should be startup");
        assert!(!services["a"].wait.unwrap(), "A (deferred dep) should be deferred");
    }

    #[test]
    fn test_service_wait_true_overrides_deferred_deps() {
        // Service-level wait: true overrides deferred propagation from deps
        let mut services = HashMap::new();
        services.insert("a".to_string(), make_service(vec![]));
        services.insert("b".to_string(), make_service_with_deps(vec![
            ("a", DependencyCondition::ServiceFailed, None),
        ]));
        // C depends on deferred B, but has wait: true
        services.insert("c".to_string(), make_service_with_deps_and_wait(vec![
            ("b", DependencyCondition::ServiceStarted, None),
        ], Some(true)));

        resolve_effective_wait(&mut services).unwrap();
        assert!(services["a"].wait.unwrap());
        assert!(!services["b"].wait.unwrap(), "B should be deferred");
        assert!(services["c"].wait.unwrap(), "C should be startup due to wait: true override");
    }

    #[test]
    fn test_service_wait_false_overrides_startup_deps() {
        // Service-level wait: false overrides startup propagation
        let mut services = HashMap::new();
        services.insert("a".to_string(), make_service(vec![]));
        services.insert("b".to_string(), make_service_with_wait(vec!["a"], Some(false)));

        resolve_effective_wait(&mut services).unwrap();
        assert!(services["a"].wait.unwrap());
        assert!(!services["b"].wait.unwrap(), "B should be deferred due to wait: false");
    }

    #[test]
    fn test_edge_wait_true_on_service_failed() {
        // Edge wait: true on service_failed makes it startup
        let mut services = HashMap::new();
        services.insert("a".to_string(), make_service(vec![]));
        services.insert("b".to_string(), make_service_with_deps(vec![
            ("a", DependencyCondition::ServiceFailed, Some(true)),
        ]));

        resolve_effective_wait(&mut services).unwrap();
        assert!(services["a"].wait.unwrap());
        assert!(services["b"].wait.unwrap(), "B should be startup due to edge wait: true");
    }

    #[test]
    fn test_edge_wait_false_on_startup_condition() {
        // Edge wait: false on service_started makes it deferred
        let mut services = HashMap::new();
        services.insert("a".to_string(), make_service(vec![]));
        services.insert("b".to_string(), make_service_with_deps(vec![
            ("a", DependencyCondition::ServiceStarted, Some(false)),
        ]));

        resolve_effective_wait(&mut services).unwrap();
        assert!(services["a"].wait.unwrap());
        assert!(!services["b"].wait.unwrap(), "B should be deferred due to edge wait: false");
    }

    #[test]
    fn test_no_deps_all_startup() {
        // Services with no dependencies always have effective_wait = true
        let mut services = HashMap::new();
        services.insert("a".to_string(), make_service(vec![]));
        services.insert("b".to_string(), make_service(vec![]));

        resolve_effective_wait(&mut services).unwrap();
        assert!(services["a"].wait.unwrap());
        assert!(services["b"].wait.unwrap());
    }

    #[test]
    fn test_service_stopped_defaults_to_deferred() {
        // service_stopped is a deferred condition
        let mut services = HashMap::new();
        services.insert("a".to_string(), make_service(vec![]));
        services.insert("b".to_string(), make_service_with_deps(vec![
            ("a", DependencyCondition::ServiceStopped, None),
        ]));

        resolve_effective_wait(&mut services).unwrap();
        assert!(services["a"].wait.unwrap());
        assert!(!services["b"].wait.unwrap(), "B should be deferred (service_stopped is a deferred condition)");
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

    // --- resolve_effective_wait: additional edge cases ---

    #[test]
    fn test_resolve_mixed_edges_one_startup_one_deferred() {
        // Service with two deps: one startup edge, one deferred edge → deferred (AND is false)
        let mut services = HashMap::new();
        services.insert("a".to_string(), make_service(vec![]));
        services.insert("b".to_string(), make_service(vec![]));
        services.insert("c".to_string(), make_service_with_deps(vec![
            ("a", DependencyCondition::ServiceStarted, None),  // startup
            ("b", DependencyCondition::ServiceFailed, None),    // deferred
        ]));

        resolve_effective_wait(&mut services).unwrap();
        assert!(services["a"].wait.unwrap());
        assert!(services["b"].wait.unwrap());
        assert!(!services["c"].wait.unwrap(), "C should be deferred (one deferred edge makes AND false)");
    }

    #[test]
    fn test_resolve_all_six_conditions_default_wait() {
        // Test each condition type as the sole dependency
        let conditions = vec![
            ("started", DependencyCondition::ServiceStarted, true),
            ("healthy", DependencyCondition::ServiceHealthy, true),
            ("completed", DependencyCondition::ServiceCompletedSuccessfully, true),
            ("stopped", DependencyCondition::ServiceStopped, false),
            ("unhealthy", DependencyCondition::ServiceUnhealthy, false),
            ("failed", DependencyCondition::ServiceFailed, false),
        ];

        for (label, condition, expected) in conditions {
            let mut services = HashMap::new();
            services.insert("dep".to_string(), make_service(vec![]));
            services.insert("svc".to_string(), make_service_with_deps(vec![
                ("dep", condition, None),
            ]));

            resolve_effective_wait(&mut services).unwrap();
            assert_eq!(
                services["svc"].wait.unwrap(), expected,
                "Condition {:?} should produce effective_wait={}", label, expected
            );
        }
    }

    #[test]
    fn test_resolve_diamond_with_one_deferred_path() {
        // Diamond: a → b (startup), a → c (deferred), b+c → d
        // d depends on both b (startup) and c (deferred) → d is deferred
        let mut services = HashMap::new();
        services.insert("a".to_string(), make_service(vec![]));
        services.insert("b".to_string(), make_service_with_deps(vec![
            ("a", DependencyCondition::ServiceHealthy, None),
        ]));
        services.insert("c".to_string(), make_service_with_deps(vec![
            ("a", DependencyCondition::ServiceFailed, None),
        ]));
        services.insert("d".to_string(), make_service_with_deps(vec![
            ("b", DependencyCondition::ServiceStarted, None),
            ("c", DependencyCondition::ServiceStarted, None),
        ]));

        resolve_effective_wait(&mut services).unwrap();
        assert!(services["a"].wait.unwrap());
        assert!(services["b"].wait.unwrap(), "B startup path");
        assert!(!services["c"].wait.unwrap(), "C deferred path");
        assert!(!services["d"].wait.unwrap(), "D deferred because C (one dep) is deferred");
    }

    #[test]
    fn test_resolve_deep_chain_propagation() {
        // a → b(failed) → c → d → e: deferred propagates through the entire chain
        let mut services = HashMap::new();
        services.insert("a".to_string(), make_service(vec![]));
        services.insert("b".to_string(), make_service_with_deps(vec![
            ("a", DependencyCondition::ServiceFailed, None),
        ]));
        services.insert("c".to_string(), make_service_with_deps(vec![
            ("b", DependencyCondition::ServiceStarted, None),
        ]));
        services.insert("d".to_string(), make_service_with_deps(vec![
            ("c", DependencyCondition::ServiceStarted, None),
        ]));
        services.insert("e".to_string(), make_service_with_deps(vec![
            ("d", DependencyCondition::ServiceStarted, None),
        ]));

        resolve_effective_wait(&mut services).unwrap();
        assert!(services["a"].wait.unwrap());
        assert!(!services["b"].wait.unwrap());
        assert!(!services["c"].wait.unwrap());
        assert!(!services["d"].wait.unwrap());
        assert!(!services["e"].wait.unwrap(), "Deferred should propagate through 4-deep chain");
    }

    #[test]
    fn test_resolve_wait_true_stops_propagation() {
        // a → b(failed) → c(wait:true) → d: propagation stops at c
        let mut services = HashMap::new();
        services.insert("a".to_string(), make_service(vec![]));
        services.insert("b".to_string(), make_service_with_deps(vec![
            ("a", DependencyCondition::ServiceFailed, None),
        ]));
        services.insert("c".to_string(), make_service_with_deps_and_wait(vec![
            ("b", DependencyCondition::ServiceStarted, None),
        ], Some(true)));
        services.insert("d".to_string(), make_service_with_deps(vec![
            ("c", DependencyCondition::ServiceStarted, None),
        ]));

        resolve_effective_wait(&mut services).unwrap();
        assert!(services["a"].wait.unwrap());
        assert!(!services["b"].wait.unwrap(), "B deferred from service_failed");
        assert!(services["c"].wait.unwrap(), "C forced startup by wait: true");
        assert!(services["d"].wait.unwrap(), "D startup because C is startup");
    }

    #[test]
    fn test_resolve_edge_wait_overrides_per_edge() {
        // Two edges from same dep: one default, one with wait override
        let mut services = HashMap::new();
        services.insert("a".to_string(), make_service(vec![]));
        services.insert("b".to_string(), make_service(vec![]));
        // c depends on a(service_failed, edge wait: true) and b(service_started, no override)
        services.insert("c".to_string(), make_service_with_deps(vec![
            ("a", DependencyCondition::ServiceFailed, Some(true)),
            ("b", DependencyCondition::ServiceStarted, None),
        ]));

        resolve_effective_wait(&mut services).unwrap();
        // Both edges are startup (edge override makes service_failed edge startup, service_started is default startup)
        assert!(services["c"].wait.unwrap(), "C startup because both edges are wait=true");
    }

    #[test]
    fn test_resolve_single_service_no_deps() {
        // Single service with no deps should always be startup
        let mut services = HashMap::new();
        services.insert("lonely".to_string(), make_service(vec![]));

        resolve_effective_wait(&mut services).unwrap();
        assert!(services["lonely"].wait.unwrap());
    }

    #[test]
    fn test_resolve_effective_wait_preserves_explicit_wait_false_no_deps() {
        // Service with no deps but wait: false → deferred
        let mut services = HashMap::new();
        services.insert("a".to_string(), make_service_with_wait(vec![], Some(false)));

        resolve_effective_wait(&mut services).unwrap();
        assert!(!services["a"].wait.unwrap(), "Explicit wait: false should override even with no deps");
    }

    #[test]
    fn test_resolve_cycle_detection() {
        // Cycles should be detected by topological_sort inside resolve_effective_wait
        let mut services = HashMap::new();
        services.insert("a".to_string(), make_service(vec!["b"]));
        services.insert("b".to_string(), make_service(vec!["a"]));

        let result = resolve_effective_wait(&mut services);
        assert!(matches!(result, Err(DaemonError::DependencyCycle(_))));
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

    fn make_service_with_restart(deps: Vec<&str>, policy: RestartPolicy) -> ServiceConfig {
        let mut svc = make_service(deps);
        svc.restart = RestartConfig::Simple(policy);
        svc
    }

    #[test]
    fn test_restart_no_wont_restart() {
        let svc = make_service_with_restart(vec![], RestartPolicy::No);
        assert!(!svc.restart.should_restart_on_exit(Some(0)));
        assert!(!svc.restart.should_restart_on_exit(Some(1)));
        assert!(!svc.restart.should_restart_on_exit(None));
    }

    #[test]
    fn test_restart_always_will_restart() {
        let svc = make_service_with_restart(vec![], RestartPolicy::Always);
        assert!(svc.restart.should_restart_on_exit(Some(0)));
        assert!(svc.restart.should_restart_on_exit(Some(1)));
        assert!(svc.restart.should_restart_on_exit(None));
    }

    #[test]
    fn test_restart_on_failure_only_on_nonzero() {
        let svc = make_service_with_restart(vec![], RestartPolicy::OnFailure);
        assert!(!svc.restart.should_restart_on_exit(Some(0)), "Exit 0 should not restart");
        assert!(svc.restart.should_restart_on_exit(Some(1)), "Exit 1 should restart");
        assert!(svc.restart.should_restart_on_exit(None), "Signal kill (None) should restart");
    }
}
