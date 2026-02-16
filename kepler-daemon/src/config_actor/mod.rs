//! Per-config actor for parallel processing.
//!
//! This module provides a per-config actor pattern that allows parallel processing
//! of commands for different configs. Each loaded config gets its own actor with
//! its own channel, eliminating the single-channel bottleneck.
//!
//! ## Module Structure
//!
//! - `actor` - ConfigActor struct and implementation (manages config state)
//! - `handle` - ConfigActorHandle (cheap-to-clone interface for sending commands)
//! - `command` - ConfigCommand enum (all command variants for actor communication)
//! - `context` - ServiceContext and related types

mod actor;
mod command;
pub mod context;
mod handle;

pub use actor::ConfigActor;
pub use command::ConfigCommand;
pub use context::{ConfigEvent, DiagnosticCounts, HealthCheckUpdate, ServiceContext, ServiceStatusChange, TaskHandleType};
pub use handle::ConfigActorHandle;
