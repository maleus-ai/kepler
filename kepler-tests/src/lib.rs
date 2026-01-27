//! Test utilities for the kepler workspace
//!
//! This crate provides helper functions and test harnesses for testing
//! the kepler daemon functionality including health checks, hooks,
//! config parsing, and service lifecycle management.

pub mod helpers;

pub use helpers::config_builder::{TestConfigBuilder, TestHealthCheckBuilder, TestServiceBuilder};
pub use helpers::daemon_harness::TestDaemonHarness;
pub use helpers::marker_files::MarkerFileHelper;
pub use helpers::wait_utils::{wait_for_healthy, wait_for_status, wait_for_unhealthy};
