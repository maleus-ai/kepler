//! ACL Lua authorizer — sandboxed Lua VM for per-request authorization.
//!
//! Separate from the config's main Lua state. Authorizers are rejection filters:
//! all matching authorizers must return `true` for the request to be allowed.
//!
//! ## VM lifecycle
//!
//! 1. Create VM with [`AclLuaVm::new`]
//! 2. Evaluate shared `kepler.acl.lua` block (if any)
//! 3. Compile authorizer chunks → [`mlua::RegistryKey`] handles
//! 4. On each request: call matching authorizers with `(request, caller)` tables
//! 5. On `recreate`: replace the entire VM (new worker thread)
//!
//! ## Sandbox
//!
//! Stricter than the config Lua state:
//! - No `require`, `load`, `loadstring`, `dofile`
//! - No `io`, `debug` libraries
//! - `os` limited to `os.clock()` and `os.time()`
//! - No `global` table (config-specific state sharing concept)
//! - Standard library tables frozen
//! - Timeout: warning at 100ms, hard cutoff at 1s

use std::collections::HashMap;
use std::sync::Arc;

use mlua::prelude::*;
use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::warn;

use kepler_protocol::protocol::Request;

/// Handle to the ACL Lua worker. Thread-safe, cloneable.
///
/// Normal auth requests take a read lock (concurrent).
/// Rebuild (recreate) takes a write lock (blocks auth until new VM is ready).
#[derive(Clone, Debug)]
pub struct AclLuaHandle {
    inner: Arc<RwLock<AclLuaWorkerInner>>,
}

#[derive(Debug)]
struct AclLuaWorkerInner {
    tx: mpsc::UnboundedSender<AclLuaMessage>,
}

/// Message sent to the ACL Lua worker thread.
enum AclLuaMessage {
    /// Evaluate a set of authorizer functions against a request.
    Check {
        authorizer_keys: Vec<RegistryKeyHandle>,
        context: AuthorizerContext,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Compile a new authorizer source and return the registry key handle.
    Compile {
        source: String,
        reply: oneshot::Sender<Result<RegistryKeyHandle, String>>,
    },
}

/// Shared handle to a Lua registry key. Cloneable via Arc.
///
/// The underlying `RegistryKey` is not Clone, so we wrap it in Arc.
/// The key is valid as long as the Lua VM that created it is alive.
#[derive(Clone)]
pub struct RegistryKeyHandle(Arc<mlua::RegistryKey>);

impl std::fmt::Debug for RegistryKeyHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("RegistryKeyHandle").finish()
    }
}

/// Context passed to authorizer functions as the `request` table.
#[derive(Debug, Clone)]
pub struct AuthorizerContext {
    /// The base right name (e.g., "start", "stop").
    pub action: &'static str,
    /// Caller UID.
    pub uid: u32,
    /// Caller GID.
    pub gid: u32,
    /// Caller username (resolved from UID, may be None).
    pub username: Option<String>,
    /// All caller GIDs (primary + supplementary).
    pub groups: Vec<u32>,
    /// Whether this is a token-based request.
    pub is_token: bool,
    /// Request-specific parameters (includes `services`).
    pub params: HashMap<String, ParamValue>,
}

/// Simple value type for authorizer parameters.
#[derive(Debug, Clone)]
pub enum ParamValue {
    Bool(bool),
    String(String),
    StringList(Vec<String>),
    StringMap(HashMap<String, String>),
}

// ---------------------------------------------------------------------------
// VM creation & sandbox
// ---------------------------------------------------------------------------

/// Create a sandboxed Lua VM for ACL authorizers.
///
/// Starts from the shared [`super::vm::create_lua_vm`] base, then applies
/// stricter sandboxing: removes `load`/`loadstring`/`dofile`/`loadfile`,
/// `io`, `debug`, `global`, and restricts `os` to `clock`/`time` only.
fn create_acl_lua_vm() -> LuaResult<Lua> {
    let lua = super::vm::create_lua_vm()?;
    let globals = lua.globals();

    // Remove additional dangerous globals (require already removed by base)
    for name in &["load", "loadstring", "dofile", "loadfile"] {
        globals.set(*name, LuaValue::Nil)?;
    }

    // Remove `global` — not applicable in ACL context
    globals.set("global", LuaValue::Nil)?;

    // Remove `io` and `debug` libraries entirely
    globals.set("io", LuaValue::Nil)?;
    globals.set("debug", LuaValue::Nil)?;

    // Restrict `os` to safe functions only (clock, time).
    // The base VM already froze the os table, so we need to replace it entirely.
    let restricted_os = lua.create_table()?;
    if let Ok(os_table) = globals.get::<LuaTable>("os") {
        if let Ok(clock) = os_table.get::<LuaFunction>("clock") {
            restricted_os.set("clock", clock)?;
        }
        if let Ok(time) = os_table.get::<LuaFunction>("time") {
            restricted_os.set("time", time)?;
        }
    }
    restricted_os.set_readonly(true);
    globals.set("os", restricted_os)?;

    Ok(lua)
}

/// Compile an authorizer source string into a callable function stored in the registry.
///
/// The source is wrapped in a function body: `function(request, caller) <source> end`
fn compile_authorizer(lua: &Lua, source: &str) -> LuaResult<mlua::RegistryKey> {
    let wrapped = format!(
        "return function(request, caller)\n{}\nend",
        source
    );
    let func: LuaFunction = lua.load(&wrapped).set_name("authorizer").eval()?;
    lua.create_registry_value(func)
}

/// Build the `request` Lua table from an AuthorizerContext.
fn build_request_table(lua: &Lua, ctx: &AuthorizerContext) -> LuaResult<LuaTable> {
    let tbl = lua.create_table()?;
    tbl.set("action", ctx.action)?;

    // params
    let params = lua.create_table()?;
    for (key, val) in &ctx.params {
        match val {
            ParamValue::Bool(b) => params.set(key.as_str(), *b)?,
            ParamValue::String(s) => params.set(key.as_str(), s.as_str())?,
            ParamValue::StringList(list) => {
                let list_tbl = lua.create_table()?;
                for (i, s) in list.iter().enumerate() {
                    list_tbl.set(i + 1, s.as_str())?;
                }
                list_tbl.set_readonly(true);
                params.set(key.as_str(), list_tbl)?;
            }
            ParamValue::StringMap(m) => {
                let map_tbl = lua.create_table()?;
                for (k, v) in m {
                    map_tbl.set(k.as_str(), v.as_str())?;
                }
                map_tbl.set_readonly(true);
                params.set(key.as_str(), map_tbl)?;
            }
        }
    }
    params.set_readonly(true);
    tbl.set("params", params)?;

    tbl.set_readonly(true);
    Ok(tbl)
}

/// Build the `caller` Lua table from an AuthorizerContext.
fn build_caller_table(lua: &Lua, ctx: &AuthorizerContext) -> LuaResult<LuaTable> {
    let tbl = lua.create_table()?;
    tbl.set("uid", ctx.uid)?;
    tbl.set("gid", ctx.gid)?;
    if let Some(ref name) = ctx.username {
        tbl.set("username", name.as_str())?;
    }
    tbl.set("token", ctx.is_token)?;

    let groups = lua.create_table()?;
    for (i, g) in ctx.groups.iter().enumerate() {
        groups.set(i + 1, *g)?;
    }
    groups.set_readonly(true);
    tbl.set("groups", groups)?;

    tbl.set_readonly(true);
    Ok(tbl)
}

// ---------------------------------------------------------------------------
// Worker thread
// ---------------------------------------------------------------------------

/// Run the ACL Lua worker loop on a blocking thread.
///
/// Processes messages sequentially: authorizer evaluation and compilation.
fn worker_loop(lua: Lua, mut rx: mpsc::UnboundedReceiver<AclLuaMessage>) {
    while let Some(msg) = rx.blocking_recv() {
        match msg {
            AclLuaMessage::Check { authorizer_keys, context, reply } => {
                let result = evaluate_authorizers(&lua, &authorizer_keys, &context);
                let _ = reply.send(result);
            }
            AclLuaMessage::Compile { source, reply } => {
                // Free Lua registry slots for RegistryKeys whose Arc refcount
                // has reached zero (e.g. revoked token authorizers from prior
                // service lifecycles). Done before compile so slots are reclaimed
                // before allocating new ones.
                lua.expire_registry_values();

                let result = compile_authorizer(&lua, &source)
                    .map(|key| RegistryKeyHandle(Arc::new(key)))
                    .map_err(|e| format!("Failed to compile token authorizer: {}", e));
                let _ = reply.send(result);
            }
        }
    }
}

/// Evaluate a list of authorizer functions against a request context.
///
/// All authorizers must return `true` for the request to be allowed.
/// Timeout: warning at 100ms, hard cutoff at 1s.
fn evaluate_authorizers(
    lua: &Lua,
    keys: &[RegistryKeyHandle],
    ctx: &AuthorizerContext,
) -> Result<(), String> {
    if keys.is_empty() {
        return Ok(());
    }

    let request_table = build_request_table(lua, ctx)
        .map_err(|e| format!("Failed to build request context: {}", e))?;
    let caller_table = build_caller_table(lua, ctx)
        .map_err(|e| format!("Failed to build caller context: {}", e))?;

    for (i, key) in keys.iter().enumerate() {
        // Set two-tier timeout: warn at 100ms, abort at 1s
        let start = std::time::Instant::now();
        let warned = std::sync::atomic::AtomicBool::new(false);
        lua.set_interrupt(move |_| {
            let elapsed = start.elapsed();
            if elapsed.as_secs() >= 1 {
                return Err(mlua::Error::RuntimeError(
                    "Authorization timeout: authorizer exceeded 1s limit".into(),
                ));
            }
            if elapsed.as_millis() >= 100 && !warned.swap(true, std::sync::atomic::Ordering::Relaxed) {
                tracing::warn!("ACL authorizer #{} is slow (>100ms)", i);
            }
            Ok(mlua::VmState::Continue)
        });

        let func: LuaFunction = lua.registry_value(&key.0)
            .map_err(|e| format!("Failed to retrieve authorizer #{}: {}", i, e))?;

        let result = func.call::<LuaValue>((request_table.clone(), caller_table.clone()));
        lua.remove_interrupt();

        match result {
            Ok(LuaValue::Boolean(true)) => {
                // Allowed — continue to next authorizer
            }
            Ok(LuaValue::Boolean(false)) => {
                return Err(format!("Authorizer #{} denied the request", i));
            }
            Ok(LuaValue::Nil) => {
                // nil = deny (fail-closed)
                return Err(format!("Authorizer #{} returned nil (denied)", i));
            }
            Ok(other) => {
                warn!("ACL authorizer #{} returned unexpected type: {:?}, treating as deny", i, other.type_name());
                return Err(format!("Authorizer #{} returned non-boolean value", i));
            }
            Err(e) => {
                warn!("ACL authorizer #{} failed: {}", i, e);
                return Err(format!("Authorizer #{} error: {}", i, e));
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

impl AclLuaHandle {
    /// Create a new ACL Lua worker.
    ///
    /// - `acl_lua`: Optional shared Lua code block (`kepler.acl.lua`), evaluated once.
    /// - `authorizer_sources`: Map of ID → Lua source for each authorizer to compile.
    ///
    /// Returns the handle and compiled authorizer registry keys (keyed by the same IDs).
    pub fn new(
        acl_lua: Option<&str>,
        authorizer_sources: &[(u32, &str)],
    ) -> Result<(Self, HashMap<u32, RegistryKeyHandle>), String> {
        let lua = create_acl_lua_vm()
            .map_err(|e| format!("Failed to create ACL Lua VM: {}", e))?;

        // Evaluate shared acl.lua block
        if let Some(code) = acl_lua {
            lua.load(code)
                .set_name("acl.lua")
                .exec()
                .map_err(|e| format!("Failed to evaluate kepler.acl.lua: {}", e))?;
        }

        // Freeze globals after acl.lua execution
        lua.globals().set_readonly(true);

        // Compile authorizer chunks
        let mut compiled = HashMap::new();
        for (id, source) in authorizer_sources {
            let key = compile_authorizer(&lua, source)
                .map_err(|e| format!("Failed to compile authorizer for ID {}: {}", id, e))?;
            compiled.insert(*id, RegistryKeyHandle(Arc::new(key)));
        }

        // Spawn worker thread
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::task::spawn_blocking(move || worker_loop(lua, rx));

        let inner = AclLuaWorkerInner { tx };
        let handle = AclLuaHandle {
            inner: Arc::new(RwLock::new(inner)),
        };

        Ok((handle, compiled))
    }

    /// Evaluate authorizers for a request. Returns `Ok(())` if allowed.
    ///
    /// Sends the request to the worker thread and awaits the result.
    pub async fn check(
        &self,
        authorizer_keys: Vec<RegistryKeyHandle>,
        context: AuthorizerContext,
    ) -> Result<(), String> {
        if authorizer_keys.is_empty() {
            return Ok(());
        }

        let (reply_tx, reply_rx) = oneshot::channel();
        let msg = AclLuaMessage::Check {
            authorizer_keys,
            context,
            reply: reply_tx,
        };

        // Read lock — concurrent auth requests allowed
        let inner = self.inner.read().await;
        inner.tx.send(msg).map_err(|_| "ACL Lua worker shut down".to_string())?;
        drop(inner); // Release read lock before awaiting reply

        reply_rx.await.map_err(|_| "ACL Lua worker dropped reply channel".to_string())?
    }

    /// Compile a new authorizer source in the existing VM.
    ///
    /// Used for token authorizers compiled at service start time.
    /// Takes a read lock — does not block concurrent auth checks.
    pub async fn compile(&self, source: &str) -> Result<RegistryKeyHandle, String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let msg = AclLuaMessage::Compile {
            source: source.to_string(),
            reply: reply_tx,
        };

        let inner = self.inner.read().await;
        inner.tx.send(msg).map_err(|_| "ACL Lua worker shut down".to_string())?;
        drop(inner);

        reply_rx.await.map_err(|_| "ACL Lua worker dropped reply channel".to_string())?
    }

    /// Rebuild the ACL Lua VM (for recreate).
    ///
    /// Takes a write lock, drops the old worker, creates a new one.
    pub async fn rebuild(
        &self,
        acl_lua: Option<&str>,
        authorizer_sources: &[(u32, &str)],
    ) -> Result<HashMap<u32, RegistryKeyHandle>, String> {
        let lua = create_acl_lua_vm()
            .map_err(|e| format!("Failed to create ACL Lua VM: {}", e))?;

        if let Some(code) = acl_lua {
            lua.load(code)
                .set_name("acl.lua")
                .exec()
                .map_err(|e| format!("Failed to evaluate kepler.acl.lua: {}", e))?;
        }

        lua.globals().set_readonly(true);

        let mut compiled = HashMap::new();
        for (id, source) in authorizer_sources {
            let key = compile_authorizer(&lua, source)
                .map_err(|e| format!("Failed to compile authorizer for ID {}: {}", id, e))?;
            compiled.insert(*id, RegistryKeyHandle(Arc::new(key)));
        }

        let (tx, rx) = mpsc::unbounded_channel();
        tokio::task::spawn_blocking(move || worker_loop(lua, rx));

        // Write lock — blocks all auth until swap is complete
        let mut inner = self.inner.write().await;
        *inner = AclLuaWorkerInner { tx };

        Ok(compiled)
    }
}

// ---------------------------------------------------------------------------
// Context building from Request
// ---------------------------------------------------------------------------

/// Build an [`AuthorizerContext`] from a protocol Request and caller info.
///
/// Returns `None` for rights-free requests (Ping, ListConfigs, etc.) since
/// those never reach the authorizer.
pub fn build_authorizer_context(
    request: &Request,
    uid: u32,
    gid: u32,
    username: Option<String>,
    groups: Vec<u32>,
    is_token: bool,
) -> Option<AuthorizerContext> {
    let (action, params) = match request {
        Request::Start {
            services,
            no_deps,
            override_envs,
            hardening,
            ..
        } => {
            let mut params = HashMap::new();
            params.insert("services".into(), ParamValue::StringList(services.clone()));
            params.insert("no_deps".into(), ParamValue::Bool(*no_deps));
            if let Some(h) = hardening {
                params.insert("hardening".into(), ParamValue::String(h.clone()));
            }
            if let Some(envs) = override_envs {
                params.insert("override_envs".into(), ParamValue::StringMap(envs.clone()));
            }
            ("start", params)
        }
        Request::Stop {
            services,
            clean,
            signal,
            ..
        } => {
            let mut params = HashMap::new();
            params.insert("services".into(), ParamValue::StringList(services.clone()));
            params.insert("clean".into(), ParamValue::Bool(*clean));
            if let Some(sig) = signal {
                params.insert("signal".into(), ParamValue::String(sig.clone()));
            }
            ("stop", params)
        }
        Request::Restart {
            services,
            no_deps,
            override_envs,
            ..
        } => {
            let mut params = HashMap::new();
            params.insert("services".into(), ParamValue::StringList(services.clone()));
            params.insert("no_deps".into(), ParamValue::Bool(*no_deps));
            if let Some(envs) = override_envs {
                params.insert("override_envs".into(), ParamValue::StringMap(envs.clone()));
            }
            ("restart", params)
        }
        Request::Recreate { hardening, .. } => {
            let mut params = HashMap::new();
            if let Some(h) = hardening {
                params.insert("hardening".into(), ParamValue::String(h.clone()));
            }
            ("recreate", params)
        }
        Request::LogsStream {
            services, filter, ..
        } => {
            let mut params = HashMap::new();
            params.insert("services".into(), ParamValue::StringList(services.clone()));
            if let Some(f) = filter {
                params.insert("filter".into(), ParamValue::String(f.clone()));
            }
            ("logs", params)
        }
        Request::Subscribe { services, .. } => {
            let mut params = HashMap::new();
            let svcs = services.clone().unwrap_or_default();
            params.insert("services".into(), ParamValue::StringList(svcs));
            ("subscribe", params)
        }
        Request::Inspect { .. } => ("inspect", HashMap::new()),
        Request::Status {
            config_path: Some(_),
        } => ("status", HashMap::new()),
        Request::SubscribeLogs { .. } => ("logs", HashMap::new()),
        Request::CheckQuiescence { .. } => ("quiescence", HashMap::new()),
        Request::CheckReadiness { .. } => ("readiness", HashMap::new()),
        Request::MonitorMetrics { .. } => ("monitor", HashMap::new()),
        // Rights-free requests — no authorizer evaluation
        Request::Ping
        | Request::ListConfigs
        | Request::Shutdown
        | Request::Prune { .. }
        | Request::Status { config_path: None }
        | Request::UserRights { .. } => return None,
    };

    Some(AuthorizerContext {
        action,
        uid,
        gid,
        username,
        groups,
        is_token,
        params,
    })
}

#[cfg(test)]
mod tests;
