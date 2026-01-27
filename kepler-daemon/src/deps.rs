use std::collections::{HashMap, HashSet};

use crate::config::ServiceConfig;
use crate::errors::{DaemonError, Result};

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

/// Perform topological sort using Kahn's algorithm
fn topological_sort(services: &HashMap<String, ServiceConfig>) -> Result<Vec<String>> {
    let service_names: HashSet<_> = services.keys().cloned().collect();

    // Validate all dependencies exist
    for (name, config) in services {
        for dep in &config.depends_on {
            if !service_names.contains(dep) {
                return Err(DaemonError::MissingDependency {
                    service: name.clone(),
                    dependency: dep.clone(),
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
        for dep in &config.depends_on {
            // name depends on dep, so increment in_degree of name
            *in_degree.get_mut(name).unwrap() += 1;
            // dep has name as a dependent
            dependents.get_mut(dep).unwrap().push(name.clone());
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
        for dep in &config.depends_on {
            if !services.contains_key(dep) {
                return Err(DaemonError::MissingDependency {
                    service: service.to_string(),
                    dependency: dep.clone(),
                });
            }
            collect_deps(dep, services, needed)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RestartPolicy;

    fn make_service(deps: Vec<&str>) -> ServiceConfig {
        ServiceConfig {
            command: vec!["test".to_string()],
            working_dir: None,
            environment: vec![],
            env_file: None,
            restart: RestartPolicy::No,
            depends_on: deps.into_iter().map(String::from).collect(),
            healthcheck: None,
            watch: vec![],
            hooks: None,
            logs: None,
            user: None,
            group: None,
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
