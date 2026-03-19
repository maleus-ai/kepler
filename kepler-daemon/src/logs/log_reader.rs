//! SQL-based log reader. Opens fresh read-only connections per query.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::store::open_readonly;
use super::LogLine;
use crate::config::StorageMode;

/// SQL-based log reader. Creates a fresh read-only connection per query
/// (required for NFS correctness, cheap on local WAL).
pub struct LogReader {
    db_path: PathBuf,
    _storage_mode: StorageMode,
}

impl LogReader {
    pub fn new(db_path: PathBuf, storage_mode: StorageMode) -> Self {
        Self { db_path, _storage_mode: storage_mode }
    }

    /// Get the last N entries (newest, returned in chronological order).
    pub fn tail(&self, count: usize, services: &[String], no_hooks: bool) -> Vec<LogLine> {
        let conn = match open_readonly(&self.db_path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        let (where_clause, bind) = build_filter(services, no_hooks, None);
        let sql = format!(
            "SELECT * FROM (SELECT id, timestamp, service, hook, level, line, attributes \
             FROM logs {where_clause} ORDER BY id DESC LIMIT ?{next}) ORDER BY id ASC",
            where_clause = where_clause,
            next = bind.len() + 1,
        );

        query_log_lines(&conn, &sql, &bind, Some(count as i64)).unwrap_or_default()
    }

    /// Get the first N entries in chronological order.
    pub fn head(&self, count: usize, services: &[String], no_hooks: bool) -> Vec<LogLine> {
        let conn = match open_readonly(&self.db_path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        let (where_clause, bind) = build_filter(services, no_hooks, None);
        let sql = format!(
            "SELECT id, timestamp, service, hook, level, line, attributes \
             FROM logs {where_clause} ORDER BY id ASC LIMIT ?{next}",
            where_clause = where_clause,
            next = bind.len() + 1,
        );

        query_log_lines(&conn, &sql, &bind, Some(count as i64)).unwrap_or_default()
    }

    /// Position-based pagination: get entries after `after_id`.
    /// Returns `(entries, has_more)`.
    ///
    /// When `filter` is provided, the raw SQL WHERE fragment is injected into
    /// the query, protected by the SQLite authorizer and resource limits.
    pub fn after(
        &self,
        after_id: i64,
        limit: usize,
        services: &[String],
        no_hooks: bool,
        filter: Option<&str>,
    ) -> Result<(Vec<LogLine>, bool), String> {
        // Validate filter before doing any I/O
        if let Some(f) = filter {
            super::filter::validate_filter(f)?;
        }

        let conn = match open_readonly(&self.db_path) {
            Ok(c) => c,
            Err(_) => return Ok((Vec::new(), false)),
        };

        // Apply authorizer + resource limits when user filter is present
        if filter.is_some() {
            super::filter::apply_filter_safeguards(&conn, super::filter::FILTER_QUERY_TIMEOUT);
        }

        let (filter_clause, mut bind) = build_filter(services, no_hooks, filter);
        // Combine id > ? with existing filters
        let where_clause = if filter_clause.is_empty() {
            "WHERE id > ?1".to_string()
        } else {
            format!("{} AND id > ?{}", filter_clause, bind.len() + 1)
        };
        bind.push(BindValue::Int(after_id));

        let sql = format!(
            "SELECT id, timestamp, service, hook, level, line, attributes \
             FROM logs {} ORDER BY id ASC LIMIT ?{}",
            where_clause,
            bind.len() + 1,
        );

        // Fetch limit+1 to detect has_more.
        // Query execution errors (e.g. "no such table" during startup race)
        // are treated as empty — only filter validation errors are propagated.
        let mut entries = query_log_lines(&conn, &sql, &bind, Some((limit + 1) as i64))
            .unwrap_or_default();
        let has_more = entries.len() > limit;
        entries.truncate(limit);
        Ok((entries, has_more))
    }

    /// Tail with a byte-budget: returns up to `count` entries but stops early
    /// if cumulative payload exceeds `max_bytes` (if specified).
    pub fn tail_bounded(
        &self,
        count: usize,
        services: &[String],
        max_bytes: Option<usize>,
        no_hooks: bool,
    ) -> Vec<LogLine> {
        let entries = self.tail(count, services, no_hooks);
        match max_bytes {
            Some(budget) => {
                let mut total = 0usize;
                entries
                    .into_iter()
                    .take_while(|e| {
                        total += e.line.len() + e.service.len() + 32;
                        total <= budget
                    })
                    .collect()
            }
            None => entries,
        }
    }

    /// Get max rowid (for follow-mode position tracking). Returns 0 on empty/error.
    pub fn max_id(&self) -> i64 {
        let conn = match open_readonly(&self.db_path) {
            Ok(c) => c,
            Err(_) => return 0,
        };
        conn.query_row("SELECT COALESCE(MAX(id), 0) FROM logs", [], |row| row.get(0))
            .unwrap_or(0)
    }

    /// Clear all logs (sends DELETE through a write connection).
    pub fn clear(&self, store: &super::store::LogStoreHandle) {
        store.clear();
    }

    /// Clear logs for a service.
    pub fn clear_service(&self, store: &super::store::LogStoreHandle, service: &str) {
        store.clear_service(service);
    }

    /// Clear hook logs for a service prefix.
    pub fn clear_service_prefix(&self, store: &super::store::LogStoreHandle, prefix: &str) {
        store.clear_service_prefix(prefix);
    }

    /// Get the database path.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }
}

// ============================================================================
// Query helpers
// ============================================================================

/// Internal bind-value type for query parameters.
enum BindValue {
    Text(String),
    Int(i64),
}

fn build_filter(
    services: &[String],
    no_hooks: bool,
    filter: Option<&str>,
) -> (String, Vec<BindValue>) {
    let mut conditions = Vec::new();
    let mut bind = Vec::new();

    match services.len() {
        0 => {} // no service filter — return all
        1 => {
            bind.push(BindValue::Text(services[0].clone()));
            conditions.push(format!("service = ?{}", bind.len()));
        }
        _ => {
            let placeholders: Vec<String> = services.iter().map(|svc| {
                bind.push(BindValue::Text(svc.clone()));
                format!("?{}", bind.len())
            }).collect();
            conditions.push(format!("service IN ({})", placeholders.join(", ")));
        }
    }

    if no_hooks {
        // Hook logs use dotted service names like "web.pre_start.0"
        conditions.push("service NOT LIKE '%.%'".to_string());
    }

    // Raw SQL WHERE fragment — safety enforced by the SQLite authorizer
    // applied to the connection before prepare().
    if let Some(f) = filter {
        conditions.push(format!("({})", f));
    }

    if conditions.is_empty() {
        (String::new(), bind)
    } else {
        (format!("WHERE {}", conditions.join(" AND ")), bind)
    }
}

fn query_log_lines(
    conn: &rusqlite::Connection,
    sql: &str,
    bind: &[BindValue],
    limit: Option<i64>,
) -> Result<Vec<LogLine>, String> {
    let mut stmt = conn
        .prepare_cached(sql)
        .map_err(|e| format!("failed to prepare log query: {}", e))?;

    // Build parameter list: bind values + optional limit
    let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = bind
        .iter()
        .map(|v| -> Box<dyn rusqlite::types::ToSql> {
            match v {
                BindValue::Text(s) => Box::new(s.clone()),
                BindValue::Int(i) => Box::new(*i),
            }
        })
        .collect();
    if let Some(lim) = limit {
        params_vec.push(Box::new(lim));
    }
    let params_refs: Vec<&dyn rusqlite::types::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();

    let rows = stmt
        .query_map(params_refs.as_slice(), |row| {
            let id: i64 = row.get(0)?;
            let timestamp: i64 = row.get(1)?;
            let service: String = row.get(2)?;
            let hook: Option<String> = row.get(3)?;
            let level_str: String = row.get(4)?;
            let line: String = row.get(5)?;
            let attributes: Option<String> = row.get(6)?;

            Ok(LogLine {
                id,
                timestamp,
                service: Arc::from(service.as_str()),
                hook: hook.map(|h| Arc::from(h.as_str())),
                level: Arc::from(level_str.as_str()),
                line,
                attributes,
            })
        })
        .map_err(|e| format!("failed to query logs: {}", e))?;

    Ok(rows.filter_map(|r| r.ok()).collect())
}
