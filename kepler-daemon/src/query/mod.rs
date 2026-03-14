//! Shared query infrastructure: DSL parser and SQL safety layer.
//!
//! - [`dsl`] — Datadog-inspired query DSL → SQL with bind parameters
//! - [`filter`] — Authorizer-based safety for user-provided SQL expressions

pub mod dsl;
pub mod filter;

// Re-export DSL types for convenience
pub use dsl::{ColumnType, Field, QueryDsl, QueryDslBuilder, SqlFragment, SqlValue, wildcard_to_like};
