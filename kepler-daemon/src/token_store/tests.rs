use super::*;

#[test]
fn token_generate_produces_unique_tokens() {
    let t1 = Token::generate().unwrap();
    let t2 = Token::generate().unwrap();
    assert_ne!(t1, t2);
}

#[test]
fn token_constant_time_eq() {
    let bytes = [0xAB; 32];
    let t1 = Token::from_bytes(bytes);
    let t2 = Token::from_bytes(bytes);
    assert_eq!(t1, t2);

    let mut different = bytes;
    different[31] = 0xCD;
    let t3 = Token::from_bytes(different);
    assert_ne!(t1, t3);
}

#[test]
fn token_hex_round_trip() {
    let token = Token::generate().unwrap();
    let hex_str = token.to_hex();
    assert_eq!(hex_str.len(), 64);

    let decoded = Token::from_hex(&hex_str).expect("should decode");
    assert_eq!(token, decoded);
}

#[test]
fn token_from_hex_rejects_invalid() {
    assert!(Token::from_hex("not-hex").is_none());
    assert!(Token::from_hex("").is_none());
    assert!(Token::from_hex("aabb").is_none()); // too short
    assert!(Token::from_hex(&"ab".repeat(33)).is_none()); // too long
}

#[tokio::test]
async fn register_and_get() {
    let store = TokenStore::new();
    let token = Token::generate().unwrap();
    let ctx = TokenContext {
        allow: ["service:start"].into(),
        max_hardening: HardeningLevel::None,
        service: "web".to_string(),
        config_path: "/test/kepler.yaml".into(),
    };
    store.register(token, ctx).await;

    let retrieved = store.get(&token).await.unwrap();
    assert_eq!(retrieved.service, "web");
    assert!(retrieved.allow.contains("service:start"));
}

#[tokio::test]
async fn unknown_token_returns_none() {
    let store = TokenStore::new();
    let token = Token::generate().unwrap();
    assert!(store.get(&token).await.is_none());
}

#[tokio::test]
async fn revoke_for_service_removes_matching() {
    let store = TokenStore::new();
    let t1 = Token::generate().unwrap();
    let t2 = Token::generate().unwrap();
    let ctx1 = TokenContext {
        allow: HashSet::new(),
        max_hardening: HardeningLevel::None,
        service: "web".to_string(),
        config_path: "/test/kepler.yaml".into(),
    };
    let ctx2 = TokenContext {
        allow: HashSet::new(),
        max_hardening: HardeningLevel::None,
        service: "worker".to_string(),
        config_path: "/test/kepler.yaml".into(),
    };
    store.register(t1, ctx1).await;
    store.register(t2, ctx2).await;

    // Revoke entries for "web" service
    store.revoke_for_service(std::path::Path::new("/test/kepler.yaml"), "web").await;

    // web entry revoked, worker entry still valid
    assert!(store.get(&t1).await.is_none());
    assert!(store.get(&t2).await.is_some());
}

#[tokio::test]
async fn revoke_for_config_removes_all_matching() {
    let store = TokenStore::new();
    let t1 = Token::generate().unwrap();
    let t2 = Token::generate().unwrap();
    let t3 = Token::generate().unwrap();
    let ctx1 = TokenContext {
        allow: HashSet::new(),
        max_hardening: HardeningLevel::None,
        service: "web".to_string(),
        config_path: "/test/kepler.yaml".into(),
    };
    let ctx2 = TokenContext {
        allow: HashSet::new(),
        max_hardening: HardeningLevel::None,
        service: "worker".to_string(),
        config_path: "/test/kepler.yaml".into(),
    };
    let ctx3 = TokenContext {
        allow: HashSet::new(),
        max_hardening: HardeningLevel::None,
        service: "other".to_string(),
        config_path: "/other/kepler.yaml".into(),
    };
    store.register(t1, ctx1).await;
    store.register(t2, ctx2).await;
    store.register(t3, ctx3).await;

    // Revoke all entries for /test/kepler.yaml
    store.revoke_for_config(std::path::Path::new("/test/kepler.yaml")).await;

    assert!(store.get(&t1).await.is_none());
    assert!(store.get(&t2).await.is_none());
    // Other config unaffected
    assert!(store.get(&t3).await.is_some());
}

#[tokio::test]
async fn concurrent_registration() {
    let store = Arc::new(TokenStore::new());
    let mut handles = Vec::new();
    let mut tokens = Vec::new();

    for i in 0..50u32 {
        let token = Token::generate().unwrap();
        tokens.push(token);
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            let ctx = TokenContext {
                allow: HashSet::new(),
                max_hardening: HardeningLevel::None,
                service: format!("svc-{}", i),
                config_path: "/test/kepler.yaml".into(),
            };
            store.register(token, ctx).await;
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    // All entries should be retrievable
    for token in &tokens {
        assert!(store.get(token).await.is_some());
    }
}

#[tokio::test]
async fn concurrent_register_and_get() {
    let store = Arc::new(TokenStore::new());
    let mut handles = Vec::new();

    for i in 0..50u32 {
        let store = store.clone();
        let token = Token::generate().unwrap();
        handles.push(tokio::spawn(async move {
            let ctx = TokenContext {
                allow: HashSet::new(),
                max_hardening: HardeningLevel::None,
                service: format!("svc-{}", i),
                config_path: "/test/kepler.yaml".into(),
            };
            store.register(token, ctx).await;
            // Immediately read it back
            let retrieved = store.get(&token).await;
            assert!(retrieved.is_some(), "entry should be retrievable immediately after registration");
        }));
    }

    for h in handles {
        h.await.unwrap();
    }
}

#[tokio::test]
async fn concurrent_revoke_for_service_during_reads() {
    let store = Arc::new(TokenStore::new());

    // Register entries for multiple services
    for i in 0..20u32 {
        let ctx = TokenContext {
            allow: HashSet::new(),
            max_hardening: HardeningLevel::None,
            service: format!("svc-{}", i),
            config_path: "/test/kepler.yaml".into(),
        };
        store.register(Token::generate().unwrap(), ctx).await;
    }

    let mut handles = Vec::new();

    // Revoke by service concurrently while reading
    for i in 0..20 {
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            store.revoke_for_service(
                std::path::Path::new("/test/kepler.yaml"),
                &format!("svc-{}", i),
            ).await;
        }));
    }

    // Concurrent reads
    for _ in 0..20 {
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            // Reading a nonexistent token should return None, not panic
            store.get(&Token::generate().unwrap()).await;
        }));
    }

    for h in handles {
        h.await.unwrap();
    }
}

// === ServiceTokenGuard tests ===

#[tokio::test]
async fn guard_new_registers_token() {
    let store: SharedTokenStore = Arc::new(TokenStore::new());
    let ctx = TokenContext {
        allow: ["service:start"].into(),
        max_hardening: HardeningLevel::None,
        service: "web".to_string(),
        config_path: "/test/kepler.yaml".into(),
    };
    let guard = ServiceTokenGuard::new(store.clone(), ctx).await.unwrap();

    // Token should be registered in the store
    let token = guard.token().unwrap();
    let retrieved = store.get(&token).await;
    assert!(retrieved.is_some(), "token should be in store after guard creation");
    assert_eq!(retrieved.unwrap().service, "web");

    // Clean up
    guard.revoke().await;
}

#[tokio::test]
async fn guard_revoke_removes_token() {
    let store: SharedTokenStore = Arc::new(TokenStore::new());
    let ctx = TokenContext {
        allow: HashSet::new(),
        max_hardening: HardeningLevel::None,
        service: "web".to_string(),
        config_path: "/test/kepler.yaml".into(),
    };
    let guard = ServiceTokenGuard::new(store.clone(), ctx).await.unwrap();
    let token = guard.token().unwrap();

    // Token should be registered
    assert!(store.get(&token).await.is_some());

    // Revoke should remove it
    guard.revoke().await;
    assert!(store.get(&token).await.is_none(), "token should be removed after revoke");
}

#[tokio::test]
async fn guard_token_hex_returns_valid_hex() {
    let store: SharedTokenStore = Arc::new(TokenStore::new());
    let ctx = TokenContext {
        allow: HashSet::new(),
        max_hardening: HardeningLevel::None,
        service: "web".to_string(),
        config_path: "/test/kepler.yaml".into(),
    };
    let guard = ServiceTokenGuard::new(store.clone(), ctx).await.unwrap();

    let hex = guard.token_hex().unwrap();
    assert_eq!(hex.len(), 64, "hex should be 64 chars (32 bytes)");

    // Should round-trip
    let decoded = Token::from_hex(&hex);
    assert!(decoded.is_some());
    assert_eq!(decoded.unwrap(), guard.token().unwrap());

    guard.revoke().await;
}

#[tokio::test]
async fn guard_drop_cleans_up_token() {
    let store: SharedTokenStore = Arc::new(TokenStore::new());
    let ctx = TokenContext {
        allow: HashSet::new(),
        max_hardening: HardeningLevel::None,
        service: "web".to_string(),
        config_path: "/test/kepler.yaml".into(),
    };
    let guard = ServiceTokenGuard::new(store.clone(), ctx).await.unwrap();
    let token = guard.token().unwrap();

    // Token should be registered
    assert!(store.get(&token).await.is_some());

    // Drop the guard without calling revoke()
    drop(guard);

    // Give the background task a moment to run
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    // Token should be cleaned up by Drop safety net
    assert!(store.get(&token).await.is_none(), "Drop should clean up unreleased token");
}

#[tokio::test]
async fn guard_generates_unique_tokens() {
    let store: SharedTokenStore = Arc::new(TokenStore::new());
    let ctx1 = TokenContext {
        allow: HashSet::new(),
        max_hardening: HardeningLevel::None,
        service: "web".to_string(),
        config_path: "/test/kepler.yaml".into(),
    };
    let ctx2 = TokenContext {
        allow: HashSet::new(),
        max_hardening: HardeningLevel::None,
        service: "worker".to_string(),
        config_path: "/test/kepler.yaml".into(),
    };
    let guard1 = ServiceTokenGuard::new(store.clone(), ctx1).await.unwrap();
    let guard2 = ServiceTokenGuard::new(store.clone(), ctx2).await.unwrap();

    assert_ne!(guard1.token().unwrap(), guard2.token().unwrap(), "each guard should have a unique token");
    assert_ne!(guard1.token_hex().unwrap(), guard2.token_hex().unwrap());

    guard1.revoke().await;
    guard2.revoke().await;
}

#[tokio::test]
async fn guard_revoke_does_not_affect_other_tokens() {
    let store: SharedTokenStore = Arc::new(TokenStore::new());
    let ctx1 = TokenContext {
        allow: HashSet::new(),
        max_hardening: HardeningLevel::None,
        service: "web".to_string(),
        config_path: "/test/kepler.yaml".into(),
    };
    let ctx2 = TokenContext {
        allow: HashSet::new(),
        max_hardening: HardeningLevel::None,
        service: "worker".to_string(),
        config_path: "/test/kepler.yaml".into(),
    };
    let guard1 = ServiceTokenGuard::new(store.clone(), ctx1).await.unwrap();
    let guard2 = ServiceTokenGuard::new(store.clone(), ctx2).await.unwrap();
    let token2 = guard2.token().unwrap();

    // Revoking guard1 should not affect guard2
    guard1.revoke().await;
    assert!(store.get(&token2).await.is_some(), "revoking one guard should not affect others");

    guard2.revoke().await;
}

#[tokio::test]
async fn guard_preserves_token_context_scopes() {
    let store: SharedTokenStore = Arc::new(TokenStore::new());
    let allow: HashSet<&'static str> = ["service:start", "service:stop", "config:status"]
        .into();
    let ctx = TokenContext {
        allow: allow.clone(),
        max_hardening: HardeningLevel::NoRoot,
        service: "web".to_string(),
        config_path: "/test/kepler.yaml".into(),
    };
    let guard = ServiceTokenGuard::new(store.clone(), ctx).await.unwrap();

    let retrieved = store.get(&guard.token().unwrap()).await.unwrap();
    assert_eq!(retrieved.allow, allow);
    assert_eq!(retrieved.max_hardening, HardeningLevel::NoRoot);
    assert_eq!(retrieved.service, "web");

    guard.revoke().await;
}

// =========================================================================
// Missing tests from review (M-TOK-1 through M-TOK-4, N-1)
// =========================================================================

#[tokio::test]
async fn register_same_token_twice_returns_first() {
    let store = TokenStore::new();
    let token = Token::from_bytes([0xAA; 32]);
    let ctx1 = TokenContext {
        allow: ["service:start"].into(),
        max_hardening: HardeningLevel::None,
        service: "web".to_string(),
        config_path: "/test/kepler.yaml".into(),
    };
    let ctx2 = TokenContext {
        allow: ["config:status"].into(),
        max_hardening: HardeningLevel::Strict,
        service: "worker".to_string(),
        config_path: "/test/kepler.yaml".into(),
    };
    store.register(token, ctx1).await;
    store.register(token, ctx2).await;

    // Linear scan returns the first match
    let retrieved = store.get(&token).await.unwrap();
    assert_eq!(retrieved.service, "web");
}

#[tokio::test]
async fn revoke_for_service_non_matching_config_path() {
    let store = TokenStore::new();
    let token = Token::generate().unwrap();
    let ctx = TokenContext {
        allow: HashSet::new(),
        max_hardening: HardeningLevel::None,
        service: "web".to_string(),
        config_path: "/test/kepler.yaml".into(),
    };
    store.register(token, ctx).await;

    // Revoke with a different config path — should not remove the token
    store.revoke_for_service(
        std::path::Path::new("/other/kepler.yaml"),
        "web",
    ).await;

    assert!(store.get(&token).await.is_some());
}

#[tokio::test]
async fn len_and_is_empty() {
    let store = TokenStore::new();
    assert!(store.is_empty().await);
    assert_eq!(store.len().await, 0);

    let t1 = Token::generate().unwrap();
    let ctx = TokenContext {
        allow: HashSet::new(),
        max_hardening: HardeningLevel::None,
        service: "web".to_string(),
        config_path: "/test/kepler.yaml".into(),
    };
    store.register(t1, ctx).await;

    assert!(!store.is_empty().await);
    assert_eq!(store.len().await, 1);

    let t2 = Token::generate().unwrap();
    let ctx2 = TokenContext {
        allow: HashSet::new(),
        max_hardening: HardeningLevel::None,
        service: "worker".to_string(),
        config_path: "/test/kepler.yaml".into(),
    };
    store.register(t2, ctx2).await;
    assert_eq!(store.len().await, 2);

    store.revoke_for_config(std::path::Path::new("/test/kepler.yaml")).await;
    assert!(store.is_empty().await);
    assert_eq!(store.len().await, 0);
}

#[test]
fn token_debug_does_not_reveal_bytes() {
    let token = Token::from_bytes([0xAB; 32]);
    let debug = format!("{:?}", token);
    assert_eq!(debug, "Token(***)");
    assert!(!debug.contains("ab"), "Debug should not reveal token bytes");
    assert!(!debug.contains("AB"), "Debug should not reveal token bytes");
}

#[tokio::test]
async fn revoked_token_no_longer_resolves() {
    let store = TokenStore::new();
    let token = Token::generate().unwrap();
    let ctx = TokenContext {
        allow: ["service:start"].into(),
        max_hardening: HardeningLevel::None,
        service: "web".to_string(),
        config_path: "/test/kepler.yaml".into(),
    };
    store.register(token, ctx).await;

    // Token works before revocation
    assert!(store.get(&token).await.is_some());

    // Revoke
    store.revoke_for_service(
        std::path::Path::new("/test/kepler.yaml"),
        "web",
    ).await;

    // Token no longer works
    assert!(store.get(&token).await.is_none());
}

#[test]
fn token_from_hex_accepts_uppercase() {
    let token = Token::from_bytes([0xAB; 32]);
    let hex_lower = token.to_hex();
    let hex_upper = hex_lower.to_uppercase();

    let decoded = Token::from_hex(&hex_upper);
    assert!(decoded.is_some());
    assert_eq!(decoded.unwrap(), token);
}

#[tokio::test]
async fn all_zero_token_not_registered() {
    let store = TokenStore::new();
    let zero_token = Token::from_bytes([0u8; 32]);

    // An all-zero token should not match anything in an empty store
    assert!(store.get(&zero_token).await.is_none());
}
