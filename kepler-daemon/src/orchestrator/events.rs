//! Event handling for service lifecycle events and restart propagation

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use super::{OrchestratorError, ServiceOrchestrator};
use crate::config_actor::ConfigActorHandle;
use crate::deps::{check_dependency_satisfied, is_transient_satisfaction};
use crate::events::{RestartReason, ServiceEvent, ServiceEventMessage};
use crate::process::stop_service;

/// Tagged message containing service name and event message
#[derive(Debug, Clone)]
pub struct TaggedEventMessage {
    pub service_name: String,
    pub message: ServiceEventMessage,
}

/// Handler for service events that manages restart propagation
///
/// The ServiceEventHandler uses a channel to receive events from all services.
/// When a service emits a Restart event, it propagates the restart to dependent
/// services that have `on_dependency_restart` enabled.
pub struct ServiceEventHandler {
    /// Receiver for tagged events from all services
    event_rx: mpsc::Receiver<TaggedEventMessage>,
    /// Reference to the orchestrator for restart operations
    orchestrator: Arc<ServiceOrchestrator>,
    /// Config path for this handler
    config_path: PathBuf,
    /// Config actor handle
    handle: ConfigActorHandle,
}

impl ServiceEventHandler {
    /// Create a new event handler for a config
    pub fn new(
        event_rx: mpsc::Receiver<TaggedEventMessage>,
        orchestrator: Arc<ServiceOrchestrator>,
        config_path: PathBuf,
        handle: ConfigActorHandle,
    ) -> Self {
        Self {
            event_rx,
            orchestrator,
            config_path,
            handle,
        }
    }

    /// Run the event handler loop
    ///
    /// This loop processes events from all services and propagates restarts
    /// to dependent services when appropriate.
    pub async fn run(mut self) {
        info!("ServiceEventHandler started for {:?}", self.config_path);

        while let Some(tagged_msg) = self.event_rx.recv().await {
            let service_name = &tagged_msg.service_name;
            let event = &tagged_msg.message.event;

            debug!(
                "Event received for {}: {:?}",
                service_name, event
            );

            // Handle restart propagation
            if let ServiceEvent::Restart { .. } = event
                && let Err(e) = self.propagate_restart(service_name).await {
                    error!(
                        "Failed to propagate restart for {}: {}",
                        service_name, e
                    );
                }

            // Handle unhealthy → restart (on-unhealthy policy)
            if let ServiceEvent::Unhealthy = event
                && let Err(e) = self.handle_unhealthy_restart(service_name).await {
                    error!(
                        "Failed to handle unhealthy restart for {}: {}",
                        service_name, e
                    );
                }
        }

        info!("ServiceEventHandler stopped for {:?}", self.config_path);
    }

    /// Propagate restart to dependent services
    ///
    /// When a service restarts, this method:
    /// 1. Finds services that depend on the restarted service with `restart: true`
    /// 2. For each matching service: stops it, waits for the dependency condition, restarts it
    ///
    /// This follows Docker Compose behavior where `restart: true` in depends_on causes
    /// the dependent service to restart when its dependency restarts.
    async fn propagate_restart(&self, restarted_service: &str) -> Result<(), OrchestratorError> {
        let config = match self.handle.get_config().await {
            Some(c) => c,
            None => return Ok(()),
        };

        info!(
            "Checking restart propagation for dependents of {}",
            restarted_service
        );

        for (service_name, raw) in &config.services {
            // Skip the service that triggered the restart
            if service_name == restarted_service {
                continue;
            }

            // Check if this service depends on the restarted service with restart: true
            // This is the Docker Compose compatible check
            if !raw.depends_on.should_restart_on_dependency(restarted_service) {
                continue;
            }

            // Check if the service is running
            if !self.handle.is_service_running(service_name).await {
                continue;
            }

            info!(
                "Propagating restart from {} to {}",
                restarted_service, service_name
            );

            // Stop the dependent service
            if let Err(e) = stop_service(service_name, self.handle.clone(), None).await {
                warn!("Failed to stop {} for restart propagation: {}", service_name, e);
                continue;
            }

            // Wait for the dependency's condition to be met
            let dep_config = raw.depends_on
                .get(restarted_service)
                .unwrap_or_default();

            // Only use per-dependency timeout (no global fallback — wait indefinitely if unset)
            let deadline = dep_config.timeout.map(|t| Instant::now() + t);

            // Subscribe to state changes of the restarted dependency for instant notification
            let mut status_rx = self.handle.watch_dep(restarted_service);

            // Get config for transient exit filtering
            let config = self.handle.get_config().await;

            loop {
                let dep_satisfied = check_dependency_satisfied(restarted_service, &dep_config, &self.handle)
                    .await;
                if dep_satisfied {
                    // Check if this is a transient satisfaction (dep exited but will restart)
                    let is_transient = if let Some(ref cfg) = config
                        && let Some(dep_raw) = cfg.services.get(restarted_service)
                        && let Some(dep_state) = self.handle.get_service_state(restarted_service).await
                    {
                        let dep_restart = dep_raw.restart.as_static().cloned().unwrap_or_default();
                        is_transient_satisfaction(&dep_state, &dep_restart)
                    } else {
                        false
                    };

                    if !is_transient {
                        break;
                    }
                    // Transient — fall through to recv()
                }

                // Wait for next status change, with optional deadline
                let recv_result = if let Some(dl) = deadline {
                    let remaining = dl.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        warn!(
                            "Timeout waiting for {} to satisfy condition for {} restart propagation",
                            restarted_service, service_name
                        );
                        break;
                    }
                    match tokio::time::timeout(remaining, status_rx.recv()).await {
                        Ok(result) => result,
                        Err(_) => {
                            warn!(
                                "Timeout waiting for {} to satisfy condition for {} restart propagation",
                                restarted_service, service_name
                            );
                            break;
                        }
                    }
                } else {
                    // No timeout — wait indefinitely for next status change
                    status_rx.recv().await
                };

                match recv_result {
                    Some(_) => continue,
                    None => break, // Channel closed
                }
            }

            // Restart the dependent service
            if let Err(e) = self
                .orchestrator
                .restart_single_service_with_reason(
                    &self.config_path,
                    service_name,
                    RestartReason::DependencyRestart {
                        dependency: restarted_service.to_string(),
                    },
                )
                .await
            {
                error!(
                    "Failed to restart {} after {} restart: {}",
                    service_name, restarted_service, e
                );
            }
        }

        Ok(())
    }

    /// Handle unhealthy event: restart the service if its restart policy includes on-unhealthy
    async fn handle_unhealthy_restart(&self, service_name: &str) -> Result<(), OrchestratorError> {
        let config = match self.handle.get_config().await {
            Some(c) => c,
            None => return Ok(()),
        };

        let raw = match config.services.get(service_name) {
            Some(r) => r,
            None => return Ok(()),
        };

        let restart = raw.restart.as_static().cloned().unwrap_or_default();
        if !restart.should_restart_on_unhealthy() {
            return Ok(());
        }

        if !self.handle.is_service_running(service_name).await {
            return Ok(());
        }

        info!(
            "Restarting {} due to unhealthy status (on-unhealthy policy)",
            service_name
        );

        if let Err(e) = self
            .orchestrator
            .restart_single_service_with_reason(
                &self.config_path,
                service_name,
                RestartReason::Unhealthy,
            )
            .await
        {
            error!(
                "Failed to restart {} on unhealthy: {}",
                service_name, e
            );
        }

        Ok(())
    }
}

/// Spawn forwarders that receive events from service channels and forward them to the aggregator.
/// Returns JoinHandles for all forwarder tasks so they can be tracked and aborted during cleanup.
pub async fn spawn_event_forwarders(
    handle: &ConfigActorHandle,
    aggregate_tx: mpsc::Sender<TaggedEventMessage>,
) -> Vec<tokio::task::JoinHandle<()>> {
    let receivers = handle.get_all_event_receivers().await;
    let mut handles = Vec::with_capacity(receivers.len());

    for (service_name, mut rx) in receivers {
        let tx = aggregate_tx.clone();
        let name = service_name.clone();

        let handle = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                let tagged = TaggedEventMessage {
                    service_name: name.clone(),
                    message: msg,
                };
                if tx.send(tagged).await.is_err() {
                    break;
                }
            }
        });
        handles.push(handle);
    }

    handles
}
