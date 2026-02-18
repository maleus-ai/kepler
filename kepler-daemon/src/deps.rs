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
mod tests;
