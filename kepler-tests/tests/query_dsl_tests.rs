// MutexGuard held across await is intentional for env safety in tests
#![allow(clippy::await_holding_lock)]
//! Integration tests for the query DSL filter.
//!
//! Tests the full pipeline: insert logs via LogWriter → flush to SQLite →
//! query via SqliteLogReader with DSL-generated SQL filters.

use kepler_daemon::config::StorageMode;
use kepler_daemon::logs::{LogStoreHandle, LogWriter, SqliteLogReader, log_query_dsl};
use std::time::Duration;
use tempfile::TempDir;

/// Create a log store and reader in a temporary directory.
fn setup_log_store(temp_dir: &std::path::Path) -> (LogStoreHandle, SqliteLogReader) {
    let db_dir = temp_dir.join("logs");
    std::fs::create_dir_all(&db_dir).unwrap();
    let db_path = db_dir.join("logs.db");
    let store = LogStoreHandle::spawn(
        db_path.clone(),
        Duration::from_millis(50),
        4096,
        StorageMode::Local,
        None,
    );
    let reader = SqliteLogReader::new(db_path, StorageMode::Local);
    (store, reader)
}

/// Write a log entry, flush synchronously, and return immediately.
fn write_log(store: &LogStoreHandle, service: &str, line: &str, level: &'static str) {
    let writer = LogWriter::new(store, service, level);
    writer.write(line);
    store.wait_flush_sync();
}

/// Write a JSON log entry (with attributes) via the log store.
fn write_json_log(store: &LogStoreHandle, service: &str, json: &str, level: &'static str) {
    let writer = LogWriter::new(store, service, level);
    writer.write(json);
    store.wait_flush_sync();
}

/// Query logs using a DSL filter expression and return matching lines.
fn query_with_dsl(reader: &SqliteLogReader, dsl: &str) -> Vec<String> {
    let frag = log_query_dsl().parse(dsl, 0).expect("DSL parse failed");
    let (entries, _) = reader
        .after(0, 10000, &[], false, Some(&frag), None, None)
        .expect("query failed");
    entries.into_iter().map(|e| e.line).collect()
}

/// Query logs using a DSL filter and return service names of matching entries.
fn query_services(reader: &SqliteLogReader, dsl: &str) -> Vec<String> {
    let frag = log_query_dsl().parse(dsl, 0).expect("DSL parse failed");
    let (entries, _) = reader
        .after(0, 10000, &[], false, Some(&frag), None, None)
        .expect("query failed");
    entries.into_iter().map(|e| e.service.to_string()).collect()
}

// ============================================================================
// Full-text search tests
// ============================================================================

#[test]
fn dsl_fulltext_single_word() {
    let temp = TempDir::new().unwrap();
    let (store, reader) = setup_log_store(temp.path());

    write_log(&store, "web", "server started successfully", "out");
    write_log(&store, "web", "connection error occurred", "err");
    write_log(&store, "api", "request processed", "out");

    let results = query_with_dsl(&reader, "error");
    assert_eq!(results.len(), 1);
    assert!(results[0].contains("error"));
}

#[test]
fn dsl_fulltext_quoted_phrase() {
    let temp = TempDir::new().unwrap();
    let (store, reader) = setup_log_store(temp.path());

    write_log(&store, "web", "connection error occurred", "err");
    write_log(&store, "web", "error in connection pool", "err");

    let results = query_with_dsl(&reader, r#""connection error""#);
    assert_eq!(results.len(), 1);
    assert!(results[0].contains("connection error"));
}

#[test]
fn dsl_fulltext_wildcard() {
    let temp = TempDir::new().unwrap();
    let (store, reader) = setup_log_store(temp.path());

    write_log(&store, "web", "processing request", "out");
    write_log(&store, "web", "process completed", "out");
    write_log(&store, "web", "other message", "out");

    let results = query_with_dsl(&reader, "process*");
    assert_eq!(results.len(), 2);
}

// ============================================================================
// Service / level field filter tests
// ============================================================================

#[test]
fn dsl_service_filter() {
    let temp = TempDir::new().unwrap();
    let (store, reader) = setup_log_store(temp.path());

    write_log(&store, "web", "web message", "out");
    write_log(&store, "api", "api message", "out");
    write_log(&store, "worker", "worker message", "out");

    let services = query_services(&reader, "@service:api");
    assert_eq!(services, vec!["api"]);
}

#[test]
fn dsl_service_wildcard_filter() {
    let temp = TempDir::new().unwrap();
    let (store, reader) = setup_log_store(temp.path());

    write_log(&store, "web-frontend", "fe message", "out");
    write_log(&store, "web-backend", "be message", "out");
    write_log(&store, "api", "api message", "out");

    let services = query_services(&reader, "@service:web*");
    assert_eq!(services, vec!["web-frontend", "web-backend"]);
}

#[test]
fn dsl_level_filter() {
    let temp = TempDir::new().unwrap();
    let (store, reader) = setup_log_store(temp.path());

    write_log(&store, "web", "info message", "out");
    write_log(&store, "web", "error message", "err");
    write_log(&store, "web", "another info", "out");

    let results = query_with_dsl(&reader, "@level:err");
    assert_eq!(results.len(), 1);
    assert!(results[0].contains("error"));
}

// ============================================================================
// Boolean operator tests
// ============================================================================

#[test]
fn dsl_and_operator() {
    let temp = TempDir::new().unwrap();
    let (store, reader) = setup_log_store(temp.path());

    write_log(&store, "web", "web error", "err");
    write_log(&store, "api", "api error", "err");
    write_log(&store, "web", "web info", "out");

    let results = query_with_dsl(&reader, "@service:web AND @level:err");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], "web error");
}

#[test]
fn dsl_implicit_and() {
    let temp = TempDir::new().unwrap();
    let (store, reader) = setup_log_store(temp.path());

    write_log(&store, "web", "web error", "err");
    write_log(&store, "api", "api error", "err");
    write_log(&store, "web", "web info", "out");

    let results = query_with_dsl(&reader, "@service:web @level:err");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], "web error");
}

#[test]
fn dsl_or_operator() {
    let temp = TempDir::new().unwrap();
    let (store, reader) = setup_log_store(temp.path());

    write_log(&store, "web", "web message", "out");
    write_log(&store, "api", "api message", "out");
    write_log(&store, "worker", "worker message", "out");

    let services = query_services(&reader, "@service:web OR @service:api");
    assert_eq!(services, vec!["web", "api"]);
}

#[test]
fn dsl_not_operator() {
    let temp = TempDir::new().unwrap();
    let (store, reader) = setup_log_store(temp.path());

    write_log(&store, "web", "web message", "out");
    write_log(&store, "api", "api message", "out");
    write_log(&store, "worker", "worker message", "out");

    let services = query_services(&reader, "NOT @service:web");
    assert_eq!(services, vec!["api", "worker"]);
}

#[test]
fn dsl_minus_negation() {
    let temp = TempDir::new().unwrap();
    let (store, reader) = setup_log_store(temp.path());

    write_log(&store, "web", "web error", "err");
    write_log(&store, "api", "api error", "err");
    write_log(&store, "api", "api info", "out");

    let results = query_with_dsl(&reader, "@service:api -@level:err");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], "api info");
}

// ============================================================================
// Grouping tests
// ============================================================================

#[test]
fn dsl_grouped_or_with_and() {
    let temp = TempDir::new().unwrap();
    let (store, reader) = setup_log_store(temp.path());

    write_log(&store, "web", "web error", "err");
    write_log(&store, "api", "api error", "err");
    write_log(&store, "worker", "worker error", "err");
    write_log(&store, "web", "web info", "out");

    let results = query_with_dsl(&reader, "(@service:web OR @service:api) AND @level:err");
    assert_eq!(results.len(), 2);
    assert!(results.contains(&"web error".to_string()));
    assert!(results.contains(&"api error".to_string()));
}

// ============================================================================
// JSON attribute filter tests
// ============================================================================

#[test]
fn dsl_attribute_exact_match() {
    let temp = TempDir::new().unwrap();
    let (store, reader) = setup_log_store(temp.path());

    write_json_log(
        &store, "api",
        r#"{"msg": "request ok", "status": 200, "method": "GET"}"#,
        "out",
    );
    write_json_log(
        &store, "api",
        r#"{"msg": "server error", "status": 500, "method": "POST"}"#,
        "err",
    );
    write_json_log(
        &store, "api",
        r#"{"msg": "not found", "status": 404, "method": "GET"}"#,
        "out",
    );

    // Query by numeric attribute
    let results = query_with_dsl(&reader, "@status:500");
    assert_eq!(results.len(), 1);
    assert!(results[0].contains("server error"));
}

#[test]
fn dsl_attribute_comparison() {
    let temp = TempDir::new().unwrap();
    let (store, reader) = setup_log_store(temp.path());

    write_json_log(
        &store, "api",
        r#"{"msg": "fast request", "latency": 10.5}"#,
        "out",
    );
    write_json_log(
        &store, "api",
        r#"{"msg": "medium request", "latency": 75.0}"#,
        "out",
    );
    write_json_log(
        &store, "api",
        r#"{"msg": "slow request", "latency": 250.3}"#,
        "out",
    );

    // latency > 100
    let results = query_with_dsl(&reader, "@latency:>100");
    assert_eq!(results.len(), 1);
    assert!(results[0].contains("slow request"));

    // latency <= 75
    let results = query_with_dsl(&reader, "@latency:<=75");
    assert_eq!(results.len(), 2);
}

#[test]
fn dsl_attribute_string_match() {
    let temp = TempDir::new().unwrap();
    let (store, reader) = setup_log_store(temp.path());

    write_json_log(
        &store, "worker",
        r#"{"msg": "job started", "queue": "emails", "priority": "high"}"#,
        "out",
    );
    write_json_log(
        &store, "worker",
        r#"{"msg": "job started", "queue": "reports", "priority": "low"}"#,
        "out",
    );

    let results = query_with_dsl(&reader, "@queue:emails");
    assert_eq!(results.len(), 1);
    assert!(results[0].contains("job started"));
}

#[test]
fn dsl_attribute_wildcard() {
    let temp = TempDir::new().unwrap();
    let (store, reader) = setup_log_store(temp.path());

    write_json_log(
        &store, "api",
        r#"{"msg": "req 1", "user_agent": "Mozilla/5.0"}"#,
        "out",
    );
    write_json_log(
        &store, "api",
        r#"{"msg": "req 2", "user_agent": "curl/7.64"}"#,
        "out",
    );
    write_json_log(
        &store, "api",
        r#"{"msg": "req 3", "user_agent": "Mozillabot"}"#,
        "out",
    );

    let results = query_with_dsl(&reader, "@user_agent:Mozilla*");
    assert_eq!(results.len(), 2);
}

// ============================================================================
// Complex query tests
// ============================================================================

#[test]
fn dsl_complex_combined_query() {
    let temp = TempDir::new().unwrap();
    let (store, reader) = setup_log_store(temp.path());

    write_json_log(
        &store, "api",
        r#"{"msg": "user request ok", "status": 200, "user": "alice"}"#,
        "out",
    );
    write_json_log(
        &store, "api",
        r#"{"msg": "bot request", "status": 200, "user": "bot-crawler"}"#,
        "out",
    );
    write_json_log(
        &store, "api",
        r#"{"msg": "server error", "status": 500, "user": "bob"}"#,
        "err",
    );
    write_json_log(
        &store, "web",
        r#"{"msg": "static file served", "status": 200}"#,
        "out",
    );

    // @service:api AND status >= 200 AND NOT user starting with "bot"
    let results = query_with_dsl(&reader, "@service:api @status:>=200 -@user:bot*");
    assert_eq!(results.len(), 2);
    assert!(results.iter().any(|l| l.contains("user request ok")));
    assert!(results.iter().any(|l| l.contains("server error")));
}

#[test]
fn dsl_fulltext_with_service_filter() {
    let temp = TempDir::new().unwrap();
    let (store, reader) = setup_log_store(temp.path());

    write_log(&store, "web", "connection timeout occurred", "err");
    write_log(&store, "api", "connection timeout occurred", "err");
    write_log(&store, "web", "all good", "out");

    // Full-text "timeout" AND @service:web
    let results = query_with_dsl(&reader, "timeout @service:web");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], "connection timeout occurred");
}

// ============================================================================
// Error handling tests
// ============================================================================

#[test]
fn dsl_empty_filter_error() {
    let dsl = log_query_dsl();
    assert!(dsl.parse("", 0).is_err());
    assert!(dsl.parse("   ", 0).is_err());
}

#[test]
fn dsl_syntax_error() {
    let dsl = log_query_dsl();
    assert!(dsl.parse("AND", 0).is_err());
    assert!(dsl.parse("OR", 0).is_err());
    assert!(dsl.parse("service:", 0).is_err());
    assert!(dsl.parse("@:value", 0).is_err());
    assert!(dsl.parse("(@service:web", 0).is_err());
}

// ============================================================================
// DSL → SQL authorizer safety test
// ============================================================================

#[test]
fn dsl_generated_sql_passes_authorizer() {
    let temp = TempDir::new().unwrap();
    let (store, reader) = setup_log_store(temp.path());

    write_log(&store, "web", "hello world", "out");
    write_json_log(
        &store, "api",
        r#"{"msg": "test", "count": 42}"#,
        "out",
    );

    // Verify various DSL expressions produce safe SQL that passes through
    // the reader's authorizer (applied inside reader.after()).
    let dsl_queries = [
        "hello",
        r#""hello world""#,
        "@service:web",
        "@level:out OR @level:err",
        "@service:web AND @level:out",
        "NOT @service:api",
        "-@level:err",
        "(@service:web OR @service:api)",
        "@count:>10",
        "@count:<=100",
        "@service:w*",
        "error*",
    ];

    let dsl = log_query_dsl();
    for query in &dsl_queries {
        let frag = dsl.parse(query, 0).unwrap();
        let result = reader.after(0, 100, &[], false, Some(&frag), None, None);
        assert!(
            result.is_ok(),
            "DSL '{}' generated SQL that failed authorizer: {:?}",
            query,
            result.err()
        );
    }
}
