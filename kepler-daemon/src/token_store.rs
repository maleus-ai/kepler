//! Token-based permission store for spawned service processes.
//!
//! Each spawned service process receives a CSPRNG bearer token via the
//! `KEPLER_TOKEN` environment variable. The daemon identifies connecting
//! processes by the token they present and looks up their permissions.
//!
//! Token lookup uses linear scan with constant-time equality (via `subtle`)
//! to prevent timing side-channels.

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use subtle::ConstantTimeEq;
use tokio::sync::RwLock;

use crate::hardening::HardeningLevel;

/// 256-bit bearer token for process authentication.
///
/// - Generated via OS CSPRNG (`getrandom` crate)
/// - Compared with constant-time equality to prevent timing attacks
/// - Serialized as hex for the `KEPLER_TOKEN` environment variable
#[derive(Clone, Copy)]
pub struct Token([u8; 32]);

impl Token {
    /// Generate a new cryptographically random token.
    ///
    /// Returns an error if the OS CSPRNG is unavailable.
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut bytes = [0u8; 32];
        getrandom::getrandom(&mut bytes)?;
        Ok(Token(bytes))
    }

    /// Construct a token from raw bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Token(bytes)
    }

    /// Return the raw bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Encode the token as a lowercase hex string (64 chars).
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Decode a hex string into a Token. Returns `None` on invalid input.
    pub fn from_hex(s: &str) -> Option<Self> {
        let bytes = hex::decode(s).ok()?;
        if bytes.len() != 32 {
            return None;
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Some(Token(arr))
    }
}

/// Constant-time equality comparison to prevent timing side-channels.
impl PartialEq for Token {
    fn eq(&self, other: &Self) -> bool {
        self.0.ct_eq(&other.0).into()
    }
}

impl Eq for Token {}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never log the actual token value
        f.write_str("Token(***)")
    }
}

/// Context stored for each registered token.
#[derive(Debug, Clone)]
pub struct TokenContext {
    /// Expanded capability scopes granted to this process.
    pub allow: HashSet<&'static str>,
    /// Hardening floor — spawned configs can't go below this level.
    pub max_hardening: HardeningLevel,
    /// Service name that owns this registration.
    pub service: String,
    /// Config path (for revocation lifecycle only).
    pub config_path: PathBuf,
}

/// In-memory store mapping tokens to their permission contexts.
///
/// Uses a `Vec` with linear scan and constant-time equality comparison
/// to prevent timing side-channels. The number of active tokens is bounded
/// by the number of services with `permissions` fields, so linear scan is
/// negligible in practice.
///
/// `TokenContext` is wrapped in `Arc` to avoid cloning the inner `HashSet`
/// on every lookup — `Arc::clone` is a cheap reference count increment.
pub struct TokenStore {
    entries: RwLock<Vec<(Token, Arc<TokenContext>)>>,
}

impl Default for TokenStore {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenStore {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(Vec::new()),
        }
    }

    /// Register a token with the given permission context.
    pub async fn register(&self, token: Token, context: TokenContext) {
        self.entries.write().await.push((token, Arc::new(context)));
    }

    /// Look up a token's permission context using constant-time comparison.
    /// Returns None if unregistered.
    pub async fn get(&self, token: &Token) -> Option<Arc<TokenContext>> {
        let entries = self.entries.read().await;
        for (stored, ctx) in entries.iter() {
            if stored == token {
                return Some(Arc::clone(ctx));
            }
        }
        None
    }

    /// Revoke all registrations for a specific service (identified by config_path + service_name).
    /// Used when a service process exits.
    pub async fn revoke_for_service(&self, config_path: &Path, service_name: &str) {
        let mut entries = self.entries.write().await;
        entries.retain(|(_, ctx)| {
            !(ctx.config_path == config_path && ctx.service == service_name)
        });
    }

    /// Revoke all registrations for a config path.
    /// Used when a config is unloaded (stop --clean).
    pub async fn revoke_for_config(&self, config_path: &Path) {
        let mut entries = self.entries.write().await;
        entries.retain(|(_, ctx)| ctx.config_path != config_path);
    }

    /// Return the number of active registrations.
    pub async fn len(&self) -> usize {
        self.entries.read().await.len()
    }

    /// Check if the store is empty.
    pub async fn is_empty(&self) -> bool {
        self.entries.read().await.is_empty()
    }
}

/// Shared token store reference.
pub type SharedTokenStore = Arc<TokenStore>;

/// RAII guard for a service's bearer token.
///
/// Registers the token on creation and revokes it on explicit `revoke()`.
/// If dropped without calling `revoke()`, spawns a background revocation
/// task as a safety net and logs a warning.
pub struct ServiceTokenGuard {
    token: Option<Token>,
    store: SharedTokenStore,
    service: String,
}

impl ServiceTokenGuard {
    /// Generate a new token, register it in the store, and return the guard.
    pub async fn new(
        store: SharedTokenStore,
        context: TokenContext,
    ) -> Result<Self, getrandom::Error> {
        let token = Token::generate()?;
        let service = context.service.clone();
        store.register(token, context).await;
        Ok(Self {
            token: Some(token),
            store,
            service,
        })
    }

    /// Return the token as a hex string for injection into `KEPLER_TOKEN`.
    ///
    /// Returns an error if the token has already been revoked.
    pub fn token_hex(&self) -> Result<String, &'static str> {
        self.token.map(|t| t.to_hex()).ok_or("token already revoked")
    }

    /// Return the raw token.
    ///
    /// Returns an error if the token has already been revoked.
    pub fn token(&self) -> Result<Token, &'static str> {
        self.token.ok_or("token already revoked")
    }

    /// Explicitly revoke the token. Preferred over letting Drop handle it.
    pub async fn revoke(mut self) {
        if let Some(token) = self.token.take() {
            let mut entries = self.store.entries.write().await;
            entries.retain(|(t, _)| t != &token);
        }
    }
}

impl Drop for ServiceTokenGuard {
    fn drop(&mut self) {
        if let Some(token) = self.token.take() {
            tracing::warn!(
                "ServiceTokenGuard for '{}' dropped without explicit revoke — spawning background cleanup",
                self.service,
            );
            let store = self.store.clone();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let mut entries = store.entries.write().await;
                    entries.retain(|(t, _)| t != &token);
                });
            } else {
                tracing::error!(
                    "No tokio runtime in ServiceTokenGuard::drop for '{}' — token leaked",
                    self.service,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests;
