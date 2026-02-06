//! Event handling for service lifecycle events and restart propagation

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use super::{OrchestratorError, ServiceOrchestrator};
use crate::config_actor::ConfigActorHandle;
use crate::deps::check_dependency_satisfied;
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

        for (service_name, service_config) in &config.services {
            // Skip the service that triggered the restart
            if service_name == restarted_service {
                continue;
            }

            // Check if this service depends on the restarted service with restart: true
            // This is the Docker Compose compatible check
            if !service_config
                .depends_on
                .should_restart_on_dependency(restarted_service)
            {
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
            let dep_config = service_config
                .depends_on
                .get(restarted_service)
                .unwrap_or_default();

            let start = Instant::now();
            loop {
                if check_dependency_satisfied(restarted_service, &dep_config.condition, &self.handle)
                    .await
                {
                    break;
                }

                // Check timeout if specified
                if let Some(timeout) = dep_config.timeout
                    && start.elapsed() > timeout {
                        warn!(
                            "Timeout waiting for {} to satisfy condition for {} restart propagation",
                            restarted_service, service_name
                        );
                        break;
                    }

                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
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
}

/// Spawn forwarders that receive events from service channels and forward them to the aggregator
pub async fn spawn_event_forwarders(
    handle: &ConfigActorHandle,
    aggregate_tx: mpsc::Sender<TaggedEventMessage>,
) {
    let receivers = handle.get_all_event_receivers().await;

    for (service_name, mut rx) in receivers {
        let tx = aggregate_tx.clone();
        let name = service_name.clone();

        tokio::spawn(async move {
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
    }
}
