//! Async utilities to wait for state transitions

use kepler_daemon::state::{ServiceStatus, SharedDaemonState};
use std::path::Path;
use std::time::Duration;
use tokio::time::{sleep, Instant};

/// Error type for wait operations
#[derive(Debug)]
pub enum WaitError {
    Timeout,
    ServiceNotFound,
    ConfigNotFound,
}

impl std::fmt::Display for WaitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WaitError::Timeout => write!(f, "Wait operation timed out"),
            WaitError::ServiceNotFound => write!(f, "Service not found"),
            WaitError::ConfigNotFound => write!(f, "Config not found"),
        }
    }
}

impl std::error::Error for WaitError {}

/// Wait for a service to reach the Healthy status
pub async fn wait_for_healthy(
    state: &SharedDaemonState,
    config_path: &Path,
    service_name: &str,
    timeout: Duration,
) -> Result<(), WaitError> {
    wait_for_status(
        state,
        config_path,
        service_name,
        ServiceStatus::Healthy,
        timeout,
    )
    .await
}

/// Wait for a service to reach the Unhealthy status
pub async fn wait_for_unhealthy(
    state: &SharedDaemonState,
    config_path: &Path,
    service_name: &str,
    timeout: Duration,
) -> Result<(), WaitError> {
    wait_for_status(
        state,
        config_path,
        service_name,
        ServiceStatus::Unhealthy,
        timeout,
    )
    .await
}

/// Wait for a service to reach the Running status
pub async fn wait_for_running(
    state: &SharedDaemonState,
    config_path: &Path,
    service_name: &str,
    timeout: Duration,
) -> Result<(), WaitError> {
    wait_for_status(
        state,
        config_path,
        service_name,
        ServiceStatus::Running,
        timeout,
    )
    .await
}

/// Wait for a service to reach the Stopped status
pub async fn wait_for_stopped(
    state: &SharedDaemonState,
    config_path: &Path,
    service_name: &str,
    timeout: Duration,
) -> Result<(), WaitError> {
    wait_for_status(
        state,
        config_path,
        service_name,
        ServiceStatus::Stopped,
        timeout,
    )
    .await
}

/// Wait for a service to reach a specific status
pub async fn wait_for_status(
    state: &SharedDaemonState,
    config_path: &Path,
    service_name: &str,
    expected_status: ServiceStatus,
    timeout: Duration,
) -> Result<(), WaitError> {
    let start = Instant::now();
    let config_path = config_path.to_path_buf();

    while start.elapsed() < timeout {
        let current_status = {
            let state = state.read();
            if let Some(config_state) = state.configs.get(&config_path) {
                if let Some(service_state) = config_state.services.get(service_name) {
                    Some(service_state.status)
                } else {
                    return Err(WaitError::ServiceNotFound);
                }
            } else {
                return Err(WaitError::ConfigNotFound);
            }
        };

        if let Some(status) = current_status {
            if status == expected_status {
                return Ok(());
            }
        }

        sleep(Duration::from_millis(50)).await;
    }

    Err(WaitError::Timeout)
}

/// Wait for a service to be in any running state (Running, Healthy, or Unhealthy)
pub async fn wait_for_any_running(
    state: &SharedDaemonState,
    config_path: &Path,
    service_name: &str,
    timeout: Duration,
) -> Result<ServiceStatus, WaitError> {
    let start = Instant::now();
    let config_path = config_path.to_path_buf();

    while start.elapsed() < timeout {
        let current_status = {
            let state = state.read();
            if let Some(config_state) = state.configs.get(&config_path) {
                if let Some(service_state) = config_state.services.get(service_name) {
                    Some(service_state.status)
                } else {
                    return Err(WaitError::ServiceNotFound);
                }
            } else {
                return Err(WaitError::ConfigNotFound);
            }
        };

        if let Some(status) = current_status {
            if status.is_running() {
                return Ok(status);
            }
        }

        sleep(Duration::from_millis(50)).await;
    }

    Err(WaitError::Timeout)
}

/// Wait for a status transition (status changes from one value to another)
pub async fn wait_for_transition(
    state: &SharedDaemonState,
    config_path: &Path,
    service_name: &str,
    from_status: ServiceStatus,
    to_status: ServiceStatus,
    timeout: Duration,
) -> Result<(), WaitError> {
    let start = Instant::now();
    let config_path = config_path.to_path_buf();
    let mut seen_from = false;

    while start.elapsed() < timeout {
        let current_status = {
            let state = state.read();
            if let Some(config_state) = state.configs.get(&config_path) {
                if let Some(service_state) = config_state.services.get(service_name) {
                    Some(service_state.status)
                } else {
                    return Err(WaitError::ServiceNotFound);
                }
            } else {
                return Err(WaitError::ConfigNotFound);
            }
        };

        if let Some(status) = current_status {
            if status == from_status {
                seen_from = true;
            } else if seen_from && status == to_status {
                return Ok(());
            }
        }

        sleep(Duration::from_millis(50)).await;
    }

    Err(WaitError::Timeout)
}

/// Wait for health check failures to reach a specific count
pub async fn wait_for_health_check_failures(
    state: &SharedDaemonState,
    config_path: &Path,
    service_name: &str,
    expected_failures: u32,
    timeout: Duration,
) -> Result<(), WaitError> {
    let start = Instant::now();
    let config_path = config_path.to_path_buf();

    while start.elapsed() < timeout {
        let failures = {
            let state = state.read();
            if let Some(config_state) = state.configs.get(&config_path) {
                if let Some(service_state) = config_state.services.get(service_name) {
                    Some(service_state.health_check_failures)
                } else {
                    return Err(WaitError::ServiceNotFound);
                }
            } else {
                return Err(WaitError::ConfigNotFound);
            }
        };

        if let Some(count) = failures {
            if count >= expected_failures {
                return Ok(());
            }
        }

        sleep(Duration::from_millis(50)).await;
    }

    Err(WaitError::Timeout)
}

/// Get the current status of a service
pub fn get_service_status(
    state: &SharedDaemonState,
    config_path: &Path,
    service_name: &str,
) -> Result<ServiceStatus, WaitError> {
    let config_path = config_path.to_path_buf();
    let state = state.read();

    if let Some(config_state) = state.configs.get(&config_path) {
        if let Some(service_state) = config_state.services.get(service_name) {
            Ok(service_state.status)
        } else {
            Err(WaitError::ServiceNotFound)
        }
    } else {
        Err(WaitError::ConfigNotFound)
    }
}

/// Check if a service exists in the state
pub fn service_exists(
    state: &SharedDaemonState,
    config_path: &Path,
    service_name: &str,
) -> bool {
    let config_path = config_path.to_path_buf();
    let state = state.read();

    if let Some(config_state) = state.configs.get(&config_path) {
        config_state.services.contains_key(service_name)
    } else {
        false
    }
}
