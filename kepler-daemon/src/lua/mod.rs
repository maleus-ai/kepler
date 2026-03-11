//! Lua (Luau) scripting infrastructure for Kepler.
//!
//! - [`vm`] — Shared base VM setup (sandbox, json/yaml stdlib, frozen tables)
//! - [`templating_runtime`] — Config templating engine for `!lua` and `${{ }}$` blocks
//! - [`acl_runtime`] — ACL authorizer VM for per-request Lua authorization

pub mod acl_runtime;
pub mod templating_runtime;
pub mod vm;
