//! Authorizer-based safety for user-provided SQL filter expressions.
//!
//! Instead of parsing a custom DSL, user WHERE fragments are injected directly
//! into SQL queries. Safety is enforced by SQLite's authorizer API combined with
//! resource limits and a progress-handler timeout.
//!
//! Defense layers:
//! 1. Read-only connection (SQLITE_OPEN_READ_ONLY + PRAGMA query_only)
//! 2. Authorizer: deny-by-default, allow only SELECT on `logs` + whitelisted functions
//! 3. Resource limits (expression depth, LIKE pattern length, SQL length, etc.)
//! 4. Progress handler timeout (circuit breaker for runaway queries)

use std::time::{Duration, Instant};

/// Default query timeout for filtered queries.
pub const FILTER_QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum filter string length (bytes).
const MAX_FILTER_LENGTH: usize = 4096;

/// Allowed SQL functions in user filter expressions.
///
/// Deny-by-default: any function not listed here is rejected by the authorizer.
/// This blocks dangerous functions like `randomblob`, `zeroblob`, `load_extension`,
/// `printf`, `writefile`, `readfile`, etc.
const ALLOWED_FUNCTIONS: &[&str] = &[
    // JSON
    "json", "json_extract", "json_type", "json_valid",
    "json_array_length", "json_each", "json_tree",
    "json_object", "json_array", "json_quote",
    // String
    "length", "lower", "upper", "trim", "ltrim", "rtrim",
    "substr", "substring", "replace", "instr",
    "like", "glob",
    // Comparison / logic
    "coalesce", "ifnull", "iif", "nullif", "typeof",
    "min", "max",
    // Numeric
    "abs", "round",
    // Aggregate
    "count", "sum", "total", "avg", "group_concat",
];

/// Validate a user-provided filter string before injection.
pub fn validate_filter(filter: &str) -> Result<(), String> {
    let filter = filter.trim();
    if filter.is_empty() {
        return Err("empty filter expression".to_string());
    }
    if filter.len() > MAX_FILTER_LENGTH {
        return Err(format!(
            "filter too long ({} bytes, max {})",
            filter.len(),
            MAX_FILTER_LENGTH
        ));
    }
    // Reject parameter placeholders — user filters are self-contained expressions,
    // not parameterized queries. '?' would conflict with internal bind parameters.
    if filter.contains('?') {
        return Err("filter must not contain '?' parameter placeholders".to_string());
    }
    Ok(())
}

/// Apply authorizer, resource limits, and progress handler to a connection
/// for safely executing queries with user-provided filter expressions.
///
/// Must be called BEFORE `conn.prepare()` — the authorizer validates at
/// compile time.
pub fn apply_filter_safeguards(
    conn: &rusqlite::Connection,
    timeout: Duration,
) {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};

    // Authorizer: deny-by-default
    conn.authorizer(Some(
        move |ctx: AuthContext<'_>| -> Authorization {
            match ctx.action {
                // Allow the SELECT operation itself
                AuthAction::Select => Authorization::Allow,

                // Allow reading columns from the `logs` table only
                AuthAction::Read { table_name, .. } if table_name == "logs" => {
                    Authorization::Allow
                }

                // Allow whitelisted functions only
                AuthAction::Function { function_name } => {
                    if ALLOWED_FUNCTIONS
                        .iter()
                        .any(|&f| f.eq_ignore_ascii_case(function_name))
                    {
                        Authorization::Allow
                    } else {
                        Authorization::Deny
                    }
                }

                // Deny everything else: writes, DDL, ATTACH, PRAGMA,
                // reads on other tables (sqlite_master, etc.), recursive CTEs
                _ => Authorization::Deny,
            }
        },
    ));

    // Resource limits (defense-in-depth for DoS)
    use rusqlite::limits::Limit;
    let _ = conn.set_limit(Limit::SQLITE_LIMIT_EXPR_DEPTH, 20);
    let _ = conn.set_limit(Limit::SQLITE_LIMIT_LIKE_PATTERN_LENGTH, 100);
    let _ = conn.set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, 10_000);
    let _ = conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, 1_000_000);
    let _ = conn.set_limit(Limit::SQLITE_LIMIT_COMPOUND_SELECT, 2);
    let _ = conn.set_limit(Limit::SQLITE_LIMIT_FUNCTION_ARG, 8);
    let _ = conn.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0);

    // Progress handler: abort queries exceeding the timeout
    let start = Instant::now();
    conn.progress_handler(1000, Some(move || start.elapsed() > timeout));
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE logs (
                id        INTEGER PRIMARY KEY,
                timestamp INTEGER NOT NULL,
                service   TEXT    NOT NULL,
                hook      TEXT,
                level     TEXT    NOT NULL,
                line      TEXT    NOT NULL,
                attributes TEXT
            )",
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO logs VALUES (1, 1000, 'web', NULL, 'out', 'hello world', NULL);
             INSERT INTO logs VALUES (2, 1001, 'api', NULL, 'err', 'status 500', '{\"status\": 500}');",
        )
        .unwrap();
        conn
    }

    // ---- validate_filter ----

    #[test]
    fn validate_empty() {
        assert!(validate_filter("").is_err());
        assert!(validate_filter("  ").is_err());
    }

    #[test]
    fn validate_too_long() {
        let long = "x".repeat(MAX_FILTER_LENGTH + 1);
        assert!(validate_filter(&long).is_err());
    }

    #[test]
    fn validate_rejects_param_placeholders() {
        assert!(validate_filter("level = ?1").is_err());
        assert!(validate_filter("level = ?").is_err());
    }

    #[test]
    fn validate_ok() {
        assert!(validate_filter("level = 'err'").is_ok());
        assert!(validate_filter("service = 'web' AND level = 'out'").is_ok());
    }

    // ---- authorizer: allowed operations ----

    #[test]
    fn authorizer_allows_select_on_logs() {
        let conn = setup_db();
        apply_filter_safeguards(&conn, Duration::from_secs(5));

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM logs WHERE level = 'out'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn authorizer_allows_json_extract() {
        let conn = setup_db();
        apply_filter_safeguards(&conn, Duration::from_secs(5));

        // Query JSON attributes column
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM logs WHERE attributes IS NOT NULL AND json_extract(attributes, '$.status') = 500",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn authorizer_allows_string_functions() {
        let conn = setup_db();
        apply_filter_safeguards(&conn, Duration::from_secs(5));

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM logs WHERE lower(service) = 'web'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn authorizer_allows_like() {
        let conn = setup_db();
        apply_filter_safeguards(&conn, Duration::from_secs(5));

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM logs WHERE line LIKE '%hello%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    // ---- authorizer: denied operations ----

    #[test]
    fn authorizer_denies_other_tables() {
        let conn = setup_db();
        conn.execute_batch("CREATE TABLE secrets (data TEXT)")
            .unwrap();
        apply_filter_safeguards(&conn, Duration::from_secs(5));

        assert!(conn.prepare("SELECT * FROM secrets").is_err());
    }

    #[test]
    fn authorizer_denies_sqlite_master() {
        let conn = setup_db();
        apply_filter_safeguards(&conn, Duration::from_secs(5));

        assert!(conn
            .prepare(
                "SELECT id FROM logs WHERE (SELECT sql FROM sqlite_master LIMIT 1) IS NOT NULL"
            )
            .is_err());
    }

    #[test]
    fn authorizer_denies_randomblob() {
        let conn = setup_db();
        apply_filter_safeguards(&conn, Duration::from_secs(5));

        assert!(conn
            .prepare("SELECT id FROM logs WHERE randomblob(1000) IS NOT NULL")
            .is_err());
    }

    #[test]
    fn authorizer_denies_zeroblob() {
        let conn = setup_db();
        apply_filter_safeguards(&conn, Duration::from_secs(5));

        assert!(conn
            .prepare("SELECT id FROM logs WHERE zeroblob(1000) IS NOT NULL")
            .is_err());
    }

    #[test]
    fn authorizer_denies_insert() {
        let conn = setup_db();
        apply_filter_safeguards(&conn, Duration::from_secs(5));

        assert!(conn
            .prepare("INSERT INTO logs VALUES (3, 0, 'x', NULL, 'out', 'x', 0)")
            .is_err());
    }

    #[test]
    fn authorizer_denies_delete() {
        let conn = setup_db();
        apply_filter_safeguards(&conn, Duration::from_secs(5));

        assert!(conn.prepare("DELETE FROM logs").is_err());
    }

    #[test]
    fn authorizer_denies_attach() {
        let conn = setup_db();
        apply_filter_safeguards(&conn, Duration::from_secs(5));

        assert!(conn
            .prepare("ATTACH DATABASE '/tmp/evil.db' AS evil")
            .is_err());
    }

    #[test]
    fn authorizer_denies_pragma() {
        let conn = setup_db();
        apply_filter_safeguards(&conn, Duration::from_secs(5));

        assert!(conn.execute_batch("PRAGMA table_info(logs)").is_err());
    }

    // ---- resource limits ----

    #[test]
    fn expr_depth_limit() {
        let conn = setup_db();
        apply_filter_safeguards(&conn, Duration::from_secs(5));

        // Build a deeply nested expression: coalesce(coalesce(...(1)...))
        // Each function call adds real depth to the parse tree.
        let mut expr = "1".to_string();
        for _ in 0..25 {
            expr = format!("coalesce({})", expr);
        }
        let sql = format!("SELECT id FROM logs WHERE {} IS NOT NULL", expr);
        assert!(conn.prepare(&sql).is_err());
    }
}
