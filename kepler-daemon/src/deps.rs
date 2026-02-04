use std::collections::{HashMap, HashSet};

use crate::config::{DependencyCondition, ServiceConfig};
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
            *in_degree.get_mut(name).unwrap() += 1;
            // dep has name as a dependent
            dependents.get_mut(&dep).unwrap().push(name.clone());
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
            let deg = in_degree.get_mut(&dep).unwrap();
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

/// Check if a dependency condition is satisfied based on current service status
pub async fn check_dependency_satisfied(
    dep_name: &str,
    condition: &DependencyCondition,
    handle: &ConfigActorHandle,
) -> bool {
    let state = match handle.get_service_state(dep_name).await {
        Some(s) => s,
        None => return false,
    };

    match condition {
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
    }
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
    use crate::config::{DependsOn, RestartConfig};

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
}
