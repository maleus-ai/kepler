//! End-to-end testing harness for Kepler
//!
//! This crate provides utilities for running end-to-end tests against the
//! Kepler CLI and daemon. It handles binary discovery, process spawning,
//! and test isolation through the `KEPLER_DAEMON_PATH` environment variable.

pub mod harness;

pub use harness::{E2eHarness, E2eResult, E2eError};
