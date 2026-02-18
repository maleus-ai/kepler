//! Service lifecycle events for event-driven architecture.
//!
//! This module provides the event types and channel primitives for the SPSC
//! (Single Producer Single Consumer) event-driven architecture. Each service
//! gets its own dedicated event channel for isolation and backpressure control.

use chrono::{DateTime, Utc};
use tokio::sync::mpsc;

/// Reason for a service restart
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartReason {
    /// Restart triggered by file watcher
    Watch,
    /// Restart triggered by process failure
    Failure { exit_code: Option<i32> },
    /// Manual restart requested
    Manual,
    /// Restart triggered by a dependency restarting
    DependencyRestart { dependency: String },
}

/// Health check status
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthStatus {
    /// Health check passed
    Success,
    /// Health check failed
    Failure { consecutive_failures: u32 },
}

/// Service lifecycle events
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceEvent {
    /// Service initialization (on_init hook about to run)
    Init,
    /// Service starting (pre_start hook about to run)
    Start,
    /// Service restarting
    Restart { reason: RestartReason },
    /// Process exited
    Exit { code: Option<i32> },
    /// Service stopping (pre_stop hook about to run)
    Stop,
    /// Service cleanup (pre_cleanup hook about to run)
    Cleanup,
    /// Health check result
    Healthcheck { status: HealthStatus },
    /// Service transitioned to healthy state
    Healthy,
    /// Service transitioned to unhealthy state
    Unhealthy,
}

impl ServiceEvent {
    /// Get a string representation of the event type
    pub fn as_str(&self) -> &'static str {
        match self {
            ServiceEvent::Init => "init",
            ServiceEvent::Start => "start",
            ServiceEvent::Restart { .. } => "restart",
            ServiceEvent::Exit { .. } => "exit",
            ServiceEvent::Stop => "stop",
            ServiceEvent::Cleanup => "cleanup",
            ServiceEvent::Healthcheck { .. } => "healthcheck",
            ServiceEvent::Healthy => "healthy",
            ServiceEvent::Unhealthy => "unhealthy",
        }
    }
}

/// Event message sent through service event channels
#[derive(Debug, Clone)]
pub struct ServiceEventMessage {
    /// The event that occurred
    pub event: ServiceEvent,
    /// Timestamp when the event occurred
    pub timestamp: DateTime<Utc>,
}

impl ServiceEventMessage {
    /// Create a new event message with the current timestamp
    pub fn new(event: ServiceEvent) -> Self {
        Self {
            event,
            timestamp: Utc::now(),
        }
    }
}

/// Sender half of a service event channel - used to emit events for a specific service
pub type ServiceEventSender = mpsc::Sender<ServiceEventMessage>;

/// Receiver half of a service event channel - owned by orchestrator for a specific service
pub type ServiceEventReceiver = mpsc::Receiver<ServiceEventMessage>;

/// Default channel capacity per service
pub const SERVICE_EVENT_CHANNEL_CAPACITY: usize = 100;

/// Create a new service event channel pair
///
/// Returns (sender, receiver) pair where:
/// - sender: Used by the service/orchestrator to emit events
/// - receiver: Used by the event handler to process events
pub fn service_event_channel() -> (ServiceEventSender, ServiceEventReceiver) {
    mpsc::channel(SERVICE_EVENT_CHANNEL_CAPACITY)
}

#[cfg(test)]
mod tests;
