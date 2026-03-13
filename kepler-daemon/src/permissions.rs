//! Rights-based permission system for the token and ACL authorization system.
//!
//! Rights are flat, explicit identifiers with no implications or hidden expansions.
//!
//! - **Base rights**: Each gates exactly one Request type.
//! - **Sub-rights**: Gate optional features within a request. Use `base:feature` syntax.
//!   A sub-right is meaningless without its base right.
//! - **Aliases**: Named bundles expanded at config parse time. Only `all` is built-in.
//!
//! Note: Daemon operations (shutdown, prune) are root-only and do not use rights.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use kepler_protocol::protocol::Request;

/// Base rights — each gates exactly one request type.
pub const BASE_RIGHTS: &[&str] = &[
    "start",
    "stop",
    "restart",
    "recreate",
    "status",
    "inspect",
    "logs",
    "subscribe",
    "quiescence",
    "readiness",
    "monitor",
];

/// Sub-rights — gate optional features within a request.
/// Format: "base:feature"
pub const SUB_RIGHTS: &[&str] = &[
    "start:env-override",
    "start:hardening",
    "start:no-deps",
    "stop:clean",
    "stop:signal",
    "restart:env-override",
    "restart:no-deps",
    "recreate:hardening",
    "logs:search",
];

/// All known rights (base + sub), computed once.
///
/// Returns a reference to a lazily-initialized set of all base + sub rights.
pub fn all_rights() -> &'static HashSet<&'static str> {
    &ALL_RIGHTS
}

static ALL_RIGHTS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    let mut set = HashSet::new();
    for r in BASE_RIGHTS {
        set.insert(*r);
    }
    for r in SUB_RIGHTS {
        set.insert(*r);
    }
    set
});

/// Built-in aliases. Only `all` is built-in.
static BUILTIN_ALIASES: LazyLock<HashMap<&'static str, Vec<&'static str>>> = LazyLock::new(|| {
    let mut map = HashMap::new();
    let mut all: Vec<&'static str> = Vec::new();
    for r in BASE_RIGHTS {
        all.push(r);
    }
    for r in SUB_RIGHTS {
        all.push(r);
    }
    map.insert("all", all);
    map
});

/// Look up a right string in the static rights tables, returning the
/// `&'static str` reference if found. Returns `None` for unknown rights.
pub fn intern_right(right: &str) -> Option<&'static str> {
    for r in BASE_RIGHTS {
        if *r == right {
            return Some(r);
        }
    }
    SUB_RIGHTS.iter().find(|&&r| r == right).copied()
}

/// Required rights for a request.
pub struct RequiredRights {
    /// Base right required for this request.
    pub base: &'static str,
    /// Sub-rights required based on request parameters.
    pub sub_rights: Vec<&'static str>,
}

/// Map a protocol Request to its required rights.
///
/// Returns `None` for requests that don't require any rights (Ping, ListConfigs, etc.).
/// Returns `Some(RequiredRights)` with the base right and any sub-rights needed.
pub fn required_rights(request: &Request) -> Option<RequiredRights> {
    match request {
        Request::Start {
            override_envs,
            hardening,
            no_deps,
            ..
        } => Some(RequiredRights {
            base: "start",
            sub_rights: {
                let mut s = vec![];
                if override_envs.is_some() {
                    s.push("start:env-override");
                }
                if hardening.is_some() {
                    s.push("start:hardening");
                }
                if *no_deps {
                    s.push("start:no-deps");
                }
                s
            },
        }),
        Request::Stop {
            clean, signal, ..
        } => Some(RequiredRights {
            base: "stop",
            sub_rights: {
                let mut s = vec![];
                if *clean {
                    s.push("stop:clean");
                }
                if signal.is_some() {
                    s.push("stop:signal");
                }
                s
            },
        }),
        Request::Restart {
            override_envs,
            no_deps,
            ..
        } => Some(RequiredRights {
            base: "restart",
            sub_rights: {
                let mut s = vec![];
                if override_envs.is_some() {
                    s.push("restart:env-override");
                }
                if *no_deps {
                    s.push("restart:no-deps");
                }
                s
            },
        }),
        Request::Recreate { hardening, .. } => Some(RequiredRights {
            base: "recreate",
            sub_rights: if hardening.is_some() {
                vec!["recreate:hardening"]
            } else {
                vec![]
            },
        }),
        Request::LogsStream { filter, .. } => Some(RequiredRights {
            base: "logs",
            sub_rights: if filter.is_some() {
                vec!["logs:search"]
            } else {
                vec![]
            },
        }),
        // SubscribeLogs: same base right as logs, no sub-rights
        Request::SubscribeLogs { .. } => Some(RequiredRights {
            base: "logs",
            sub_rights: vec![],
        }),
        // Simple requests: base right only, no sub-rights
        Request::Subscribe { .. } => Some(RequiredRights {
            base: "subscribe",
            sub_rights: vec![],
        }),
        Request::CheckQuiescence { .. } => Some(RequiredRights {
            base: "quiescence",
            sub_rights: vec![],
        }),
        Request::CheckReadiness { .. } => Some(RequiredRights {
            base: "readiness",
            sub_rights: vec![],
        }),
        Request::Inspect { .. } => Some(RequiredRights {
            base: "inspect",
            sub_rights: vec![],
        }),
        Request::MonitorMetrics { .. } => Some(RequiredRights {
            base: "monitor",
            sub_rights: vec![],
        }),
        Request::Status {
            config_path: Some(_),
        } => Some(RequiredRights {
            base: "status",
            sub_rights: vec![],
        }),
        // No rights required — explicit arms to ensure compile-time error on new variants
        Request::Ping => None,
        Request::ListConfigs => None,
        Request::Shutdown => None,
        Request::Prune { .. } => None,
        Request::Status { config_path: None } => None,
        // UserRights computes effective rights — requires FS read access but no ACL gate
        Request::UserRights { .. } => None,
    }
}

/// Check if all required rights for a request are satisfied by the granted set.
pub fn check_rights(granted: &HashSet<&'static str>, request: &Request) -> Result<(), String> {
    let required = match required_rights(request) {
        Some(r) => r,
        None => return Ok(()),
    };

    if !granted.contains(required.base) {
        return Err(format!(
            "permission denied: missing required right '{}'",
            required.base
        ));
    }

    for sub in &required.sub_rights {
        if !granted.contains(*sub) {
            return Err(format!(
                "permission denied: missing required sub-right '{}'",
                sub
            ));
        }
    }

    Ok(())
}

/// Validate right and alias strings at config parse time.
///
/// Each entry must be either a known right (base or sub), a built-in alias,
/// or a user-defined alias name.
pub fn validate_rights<S: AsRef<str>>(
    rights: &[S],
    user_aliases: &HashMap<String, Vec<String>>,
) -> Result<(), String> {
    let known = &*ALL_RIGHTS;
    let builtins = &*BUILTIN_ALIASES;

    for right in rights {
        let right = right.as_ref();
        if known.contains(right) {
            continue;
        }
        if builtins.contains_key(right) {
            continue;
        }
        if user_aliases.contains_key(right) {
            continue;
        }
        return Err(format!("unknown right or alias '{}'", right));
    }
    Ok(())
}

/// Resolve aliases recursively, expanding into a flat set of rights.
///
/// - Built-in aliases (`all`) are expanded.
/// - User-defined aliases are expanded with a depth limit of 2.
/// - Cycle detection rejects circular references at parse time.
/// - Empty aliases expand to nothing.
pub fn resolve_aliases(
    allow: &[String],
    user_aliases: &HashMap<String, Vec<String>>,
) -> Result<HashSet<&'static str>, String> {
    let mut result = HashSet::new();
    for entry in allow {
        resolve_alias_entry(entry, user_aliases, &mut result, 0, &mut vec![])?;
    }
    Ok(result)
}

fn resolve_alias_entry(
    entry: &str,
    user_aliases: &HashMap<String, Vec<String>>,
    result: &mut HashSet<&'static str>,
    depth: usize,
    chain: &mut Vec<String>,
) -> Result<(), String> {
    // Direct right — intern and add
    if let Some(interned) = intern_right(entry) {
        result.insert(interned);
        return Ok(());
    }

    // Built-in alias (e.g., "all")
    let builtins = &*BUILTIN_ALIASES;
    if let Some(expanded) = builtins.get(entry) {
        for r in expanded {
            result.insert(r);
        }
        return Ok(());
    }

    // User-defined alias
    if let Some(entries) = user_aliases.get(entry) {
        // Cycle detection
        if chain.contains(&entry.to_string()) {
            return Err(format!(
                "recursive alias detected: {} -> {}",
                chain.join(" -> "),
                entry
            ));
        }

        // Depth limit (2 levels of alias-to-alias)
        if depth >= 2 {
            return Err(format!(
                "alias depth limit exceeded (max 2): {} -> {}",
                chain.join(" -> "),
                entry
            ));
        }

        chain.push(entry.to_string());
        for sub_entry in entries {
            resolve_alias_entry(sub_entry, user_aliases, result, depth + 1, chain)?;
        }
        chain.pop();
        return Ok(());
    }

    Err(format!("unknown right or alias '{}'", entry))
}

/// Validate user-defined aliases at config parse time.
///
/// Checks:
/// - No shadowing of built-in aliases (`all`), base rights, or sub-rights
/// - No cycles
/// - Depth limit of 2
/// - All referenced entries are valid
pub fn validate_user_aliases(user_aliases: &HashMap<String, Vec<String>>) -> Result<(), String> {
    let known = &*ALL_RIGHTS;
    let builtins = &*BUILTIN_ALIASES;

    // Check for shadowing
    for name in user_aliases.keys() {
        if known.contains(name.as_str()) {
            return Err(format!(
                "alias '{}' shadows a built-in right",
                name
            ));
        }
        if builtins.contains_key(name.as_str()) {
            return Err(format!(
                "alias '{}' shadows a built-in alias",
                name
            ));
        }
    }

    // Check for orphan sub-rights (warning-worthy, but not an error — just validate resolution)
    // Validate all entries resolve correctly (catches cycles, depth limits, unknown references)
    for (name, entries) in user_aliases {
        let mut result = HashSet::new();
        let mut chain = vec![name.clone()];
        for entry in entries {
            resolve_alias_entry(entry, user_aliases, &mut result, 1, &mut chain)?;
        }
    }

    Ok(())
}

/// Check for orphan sub-rights (sub-right granted without its base right).
/// Returns a list of warning messages.
pub fn check_orphan_sub_rights(rights: &HashSet<&'static str>) -> Vec<String> {
    let mut warnings = Vec::new();
    for sub in SUB_RIGHTS {
        if rights.contains(*sub) {
            let base = sub.split(':').next().unwrap();
            if !rights.contains(base) {
                warnings.push(format!(
                    "'{}' granted without '{}' — has no effect",
                    sub, base
                ));
            }
        }
    }
    warnings
}

/// Expand an `allow` list into a set of resolved rights.
///
/// This is the main entry point for ACL resolution:
/// 1. Validate all entries
/// 2. Resolve aliases recursively
/// 3. Return flat set of base rights + sub-rights
pub fn expand_allow(
    allow: &[String],
    user_aliases: &HashMap<String, Vec<String>>,
) -> Result<HashSet<&'static str>, String> {
    validate_rights(allow, user_aliases)?;
    resolve_aliases(allow, user_aliases)
}

#[cfg(test)]
mod tests;
