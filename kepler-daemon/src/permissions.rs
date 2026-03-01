//! Permission scopes for the token-based authorization system.
//!
//! Scopes are CLI-level concepts that map to what users see and type.
//! The daemon enforces them at the protocol level by mapping each incoming
//! Request to its required scope(s).
//!
//! Scope hierarchy:
//! - Category (level 1): `service`, `config`
//! - Command (level 2): `service:start`, `service:stop`, `config:status`, etc.
//!
//! Implication rules:
//! 1. Category implies all commands: `service` → `service:start`, `service:restart`, `service:clean`, etc.
//! 2. Command-level implication: `service:clean` → `service:stop`
//! 3. Wildcards: `service:*` = all service ops, `*` = everything
//!
//! Note: Daemon operations (shutdown, prune) are root-only and do not use scopes.

use std::collections::HashSet;
use std::sync::LazyLock;

use kepler_protocol::protocol::Request;

/// Known category scopes (level 1).
pub const CATEGORY_SCOPES: &[&str] = &["service", "config"];

/// Known command scopes (level 2).
pub const COMMAND_SCOPES: &[&str] = &[
    "service:start",
    "service:stop",
    "service:restart",
    "service:clean",
    "service:logs",
    "config:status",
    "config:inspect",
    "config:recreate",
    "config:hardening",
    "config:env",
];

/// All known scopes (categories + commands), computed once.
static ALL_KNOWN_SCOPES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    let mut set = HashSet::new();
    for s in CATEGORY_SCOPES {
        set.insert(*s);
    }
    for s in COMMAND_SCOPES {
        set.insert(*s);
    }
    set
});

/// Look up a scope string in the static scope tables, returning the
/// `&'static str` reference if found. Returns `None` for unknown scopes.
pub fn intern_scope(scope: &str) -> Option<&'static str> {
    for s in CATEGORY_SCOPES {
        if *s == scope {
            return Some(s);
        }
    }
    for s in COMMAND_SCOPES {
        if *s == scope {
            return Some(s);
        }
    }
    None
}

/// Validate scope strings.
///
/// Each scope must be either a known scope or a valid wildcard pattern.
/// Accepts both `&[String]` and `&[&str]` via `AsRef<str>`.
pub fn validate_scopes<S: AsRef<str>>(scopes: &[S]) -> Result<(), String> {
    let known = &*ALL_KNOWN_SCOPES;
    for scope in scopes {
        let scope = scope.as_ref();
        if scope == "*" {
            continue;
        }
        if let Some(prefix) = scope.strip_suffix(":*") {
            // Check that the prefix matches a known category or command
            let valid_prefix = CATEGORY_SCOPES.contains(&prefix);
            if !valid_prefix {
                return Err(format!("unknown scope wildcard '{}': prefix '{}' is not a known category", scope, prefix));
            }
            continue;
        }
        if !known.contains(scope) {
            return Err(format!("unknown scope '{}'. Valid scopes: {:?}", scope, known));
        }
    }
    Ok(())
}

/// Expand scopes by applying implication rules.
///
/// - `*` → all known scopes (the literal `"*"` is NOT preserved in the output)
/// - Category → all its commands
/// - `category:*` → same as category
/// - Command stays as-is
/// - `service:clean` → `service:stop` (command-level implication)
///
/// Wildcards (`"*"`, `"category:*"`) are always expanded into their concrete
/// scopes and are never present as literals in the returned set. Both ACL
/// scopes and token scopes are expanded before any intersection, so the
/// literal `"*"` is never used in runtime checks.
///
/// Validates all scopes before expanding. Returns an error if any scope is unknown.
pub fn expand_scopes<S: AsRef<str>>(scopes: &HashSet<S>) -> Result<HashSet<&'static str>, String> {
    // Validate all scopes are known before expanding
    let scope_list: Vec<&str> = scopes.iter().map(|s| s.as_ref()).collect();
    validate_scopes(&scope_list)?;

    Ok(expand_scopes_unchecked(scopes))
}

/// Internal: expand scopes without validation (for use in tests and pre-validated contexts).
fn expand_scopes_unchecked<S: AsRef<str>>(scopes: &HashSet<S>) -> HashSet<&'static str> {
    let mut expanded = HashSet::new();

    for scope_s in scopes {
        let scope = scope_s.as_ref();
        if scope == "*" {
            // Grant everything
            for s in CATEGORY_SCOPES {
                expanded.insert(*s);
            }
            for s in COMMAND_SCOPES {
                expanded.insert(*s);
            }
            return expanded;
        }

        if scope.ends_with(":*") {
            let prefix = &scope[..scope.len() - 2];
            // Expand wildcard: add prefix + all descendants
            if let Some(interned) = intern_scope(prefix) {
                expanded.insert(interned);
            }
            let prefix_colon = format!("{}:", prefix);
            for s in COMMAND_SCOPES {
                if s.starts_with(&prefix_colon) || *s == prefix {
                    expanded.insert(*s);
                }
            }
            continue;
        }

        if let Some(interned) = intern_scope(scope) {
            expanded.insert(interned);
        }

        let parts: Vec<&str> = scope.split(':').collect();
        match parts.len() {
            1 => {
                // Category → all descendants
                let prefix = format!("{}:", scope);
                for s in COMMAND_SCOPES {
                    if s.starts_with(&prefix) {
                        expanded.insert(*s);
                    }
                }
            }
            _ => {
                // Command (level 2) — no extra expansion
            }
        }
    }

    // Command-level implication: service:clean → service:stop
    if expanded.contains("service:clean") {
        expanded.insert("service:stop");
    }

    expanded
}

/// Map a protocol Request to its required scope(s).
///
/// Returns a list of scope strings that the caller must have.
/// Empty list means the request is always allowed.
pub fn required_scopes(request: &Request) -> Vec<&'static str> {
    match request {
        Request::Start {
            hardening,
            override_envs,
            ..
        } => {
            let mut scopes = vec!["service:start"];
            if hardening.is_some() {
                scopes.push("config:hardening");
            }
            if override_envs.is_some() {
                scopes.push("config:env");
            }
            scopes
        }
        Request::Stop { clean, .. } => {
            if *clean {
                vec!["service:clean"]
            } else {
                vec!["service:stop"]
            }
        }
        Request::Restart { override_envs, .. } => {
            let mut scopes = vec!["service:restart"];
            if override_envs.is_some() {
                scopes.push("config:env");
            }
            scopes
        }
        Request::Recreate { hardening, .. } => {
            let mut scopes = vec!["config:recreate"];
            if hardening.is_some() {
                scopes.push("config:hardening");
            }
            scopes
        }
        Request::Inspect { .. } => vec!["config:inspect"],
        Request::Status { config_path } => {
            if config_path.is_some() {
                vec!["config:status"]
            } else {
                // Global status — always allowed, results filtered
                vec![]
            }
        }
        Request::Logs { .. } | Request::LogsChunk { .. } | Request::LogsCursor { .. } => {
            vec!["service:logs"]
        }
        // Daemon operations are root-only (enforced in handler), not scope-gated
        Request::Prune { .. } => vec![],
        Request::Shutdown => vec![],
        // Always allowed
        Request::Ping => vec![],
        Request::ListConfigs => vec![],
        // Subscribe: allowed if caller has any service: scope
        Request::Subscribe { .. } => vec![],
    }
}

/// Check if a Subscribe request should be allowed for the given granted scopes.
pub fn check_subscribe_access(granted: &HashSet<&'static str>) -> bool {
    granted.iter().any(|s| *s == "service" || s.starts_with("service:"))
}

/// Check if all required scopes for a request are satisfied by the granted (expanded) set.
pub fn check_scopes(granted: &HashSet<&'static str>, request: &Request) -> Result<(), String> {
    // Subscribe has special logic
    if matches!(request, Request::Subscribe { .. }) {
        if !check_subscribe_access(granted) {
            return Err("permission denied: Subscribe requires at least one 'service:' scope".to_string());
        }
        return Ok(());
    }

    let required = required_scopes(request);
    if required.is_empty() {
        return Ok(());
    }

    for scope in &required {
        if !granted.contains(*scope) {
            return Err(format!(
                "permission denied: missing required scope '{}'",
                scope
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
