//! Async utilities to wait for state transitions

use kepler_daemon::state::ServiceStatus;
use kepler_daemon::state_actor::StateHandle;
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
    state: &StateHandle,
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
    state: &StateHandle,
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
    state: &StateHandle,
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
    state: &StateHandle,
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
    state: &StateHandle,
    config_path: &Path,
    service_name: &str,
    expected_status: ServiceStatus,
    timeout: Duration,
) -> Result<(), WaitError> {
    let start = Instant::now();
    let config_path = config_path.to_path_buf();

    while start.elapsed() < timeout {
        let service_state = state
            .get_service_state(config_path.clone(), service_name.to_string())
            .await;

        match service_state {
            Some(s) => {
                if s.status == expected_status {
                    return Ok(());
                }
            }
            None => {
                // Check if config exists
                if state.get_config(config_path.clone()).await.is_none() {
                    return Err(WaitError::ConfigNotFound);
                }
                return Err(WaitError::ServiceNotFound);
            }
        }

        sleep(Duration::from_millis(50)).await;
    }

    Err(WaitError::Timeout)
}

/// Wait for a service to be in any running state (Running, Healthy, or Unhealthy)
pub async fn wait_for_any_running(
    state: &StateHandle,
    config_path: &Path,
    service_name: &str,
    timeout: Duration,
) -> Result<ServiceStatus, WaitError> {
    let start = Instant::now();
    let config_path = config_path.to_path_buf();

    while start.elapsed() < timeout {
        let service_state = state
            .get_service_state(config_path.clone(), service_name.to_string())
            .await;

        match service_state {
            Some(s) => {
                if s.status.is_running() {
                    return Ok(s.status);
                }
            }
            None => {
                // Check if config exists
                if state.get_config(config_path.clone()).await.is_none() {
                    return Err(WaitError::ConfigNotFound);
                }
                return Err(WaitError::ServiceNotFound);
            }
        }

        sleep(Duration::from_millis(50)).await;
    }

    Err(WaitError::Timeout)
}

/// Wait for a status transition (status changes from one value to another)
pub async fn wait_for_transition(
    state: &StateHandle,
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
        let service_state = state
            .get_service_state(config_path.clone(), service_name.to_string())
            .await;

        match service_state {
            Some(s) => {
                if s.status == from_status {
                    seen_from = true;
                } else if seen_from && s.status == to_status {
                    return Ok(());
                }
            }
            None => {
                // Check if config exists
                if state.get_config(config_path.clone()).await.is_none() {
                    return Err(WaitError::ConfigNotFound);
                }
                return Err(WaitError::ServiceNotFound);
            }
        }

        sleep(Duration::from_millis(50)).await;
    }

    Err(WaitError::Timeout)
}

/// Wait for health check failures to reach a specific count
pub async fn wait_for_health_check_failures(
    state: &StateHandle,
    config_path: &Path,
    service_name: &str,
    expected_failures: u32,
    timeout: Duration,
) -> Result<(), WaitError> {
    let start = Instant::now();
    let config_path = config_path.to_path_buf();

    while start.elapsed() < timeout {
        let service_state = state
            .get_service_state(config_path.clone(), service_name.to_string())
            .await;

        match service_state {
            Some(s) => {
                if s.health_check_failures >= expected_failures {
                    return Ok(());
                }
            }
            None => {
                // Check if config exists
                if state.get_config(config_path.clone()).await.is_none() {
                    return Err(WaitError::ConfigNotFound);
                }
                return Err(WaitError::ServiceNotFound);
            }
        }

        sleep(Duration::from_millis(50)).await;
    }

    Err(WaitError::Timeout)
}

/// Get the current status of a service
pub async fn get_service_status(
    state: &StateHandle,
    config_path: &Path,
    service_name: &str,
) -> Result<ServiceStatus, WaitError> {
    let config_path = config_path.to_path_buf();

    let service_state = state
        .get_service_state(config_path.clone(), service_name.to_string())
        .await;

    match service_state {
        Some(s) => Ok(s.status),
        None => {
            // Check if config exists
            if state.get_config(config_path).await.is_none() {
                Err(WaitError::ConfigNotFound)
            } else {
                Err(WaitError::ServiceNotFound)
            }
        }
    }
}

/// Check if a service exists in the state
pub async fn service_exists(
    state: &StateHandle,
    config_path: &Path,
    service_name: &str,
) -> bool {
    state
        .get_service_state(config_path.to_path_buf(), service_name.to_string())
        .await
        .is_some()
}
