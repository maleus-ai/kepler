//! SQL-based log reader. Opens fresh read-only connections per query.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::store::open_readonly;
use super::LogLine;
use crate::config::StorageMode;
use crate::query::{SqlFragment, SqlValue};
use crate::query::filter;

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
    pub fn tail(&self, count: usize, services: &[String], no_hooks: bool, filter: Option<&SqlFragment>, after_ts: Option<i64>, before_ts: Option<i64>) -> Vec<LogLine> {
        // Validate raw SQL filters before doing any I/O (DSL-generated ones are safe)
        if let Some(frag) = filter {
            if frag.params.is_empty() {
                if filter::validate_filter(&frag.sql).is_err() {
                    return Vec::new();
                }
            }
        }

        let conn = match open_readonly(&self.db_path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        // Apply authorizer + resource limits when user filter is present
        if filter.is_some() {
            filter::apply_filter_safeguards(&conn, filter::FILTER_QUERY_TIMEOUT, &["logs"]);
        }

        let (where_clause, bind) = build_filter(services, no_hooks, filter, after_ts, before_ts);
        let sql = format!(
            "SELECT * FROM (SELECT id, timestamp, service, hook, level, line, attributes \
             FROM logs {where_clause} ORDER BY id DESC LIMIT ?{next}) ORDER BY id ASC",
            where_clause = where_clause,
            next = bind.len() + 1,
        );

        query_log_lines(&conn, &sql, &bind, Some(count as i64)).unwrap_or_default()
    }

    /// Get the first N entries in chronological order.
    pub fn head(&self, count: usize, services: &[String], no_hooks: bool, filter: Option<&SqlFragment>, after_ts: Option<i64>, before_ts: Option<i64>) -> Vec<LogLine> {
        // Validate raw SQL filters before doing any I/O (DSL-generated ones are safe)
        if let Some(frag) = filter {
            if frag.params.is_empty() {
                if filter::validate_filter(&frag.sql).is_err() {
                    return Vec::new();
                }
            }
        }

        let conn = match open_readonly(&self.db_path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        // Apply authorizer + resource limits when user filter is present
        if filter.is_some() {
            filter::apply_filter_safeguards(&conn, filter::FILTER_QUERY_TIMEOUT, &["logs"]);
        }

        let (where_clause, bind) = build_filter(services, no_hooks, filter, after_ts, before_ts);
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
    /// When `filter` is provided as a [`SqlFragment`], its SQL and bind parameters
    /// are integrated into the query. The SQLite authorizer and resource limits
    /// are applied for safety.
    pub fn after(
        &self,
        after_id: i64,
        limit: usize,
        services: &[String],
        no_hooks: bool,
        filter: Option<&SqlFragment>,
        after_ts: Option<i64>,
        before_ts: Option<i64>,
    ) -> Result<(Vec<LogLine>, bool), String> {
        // Validate raw SQL filters before doing any I/O (DSL-generated ones are safe)
        if let Some(frag) = filter {
            if frag.params.is_empty() {
                filter::validate_filter(&frag.sql)?;
            }
        }

        let conn = match open_readonly(&self.db_path) {
            Ok(c) => c,
            Err(_) => return Ok((Vec::new(), false)),
        };

        // Apply authorizer + resource limits when user filter is present
        if filter.is_some() {
            filter::apply_filter_safeguards(&conn, filter::FILTER_QUERY_TIMEOUT, &["logs"]);
        }

        let (filter_clause, mut bind) = build_filter(services, no_hooks, filter, after_ts, before_ts);
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
        filter: Option<&SqlFragment>,
        after_ts: Option<i64>,
        before_ts: Option<i64>,
    ) -> Vec<LogLine> {
        let entries = self.tail(count, services, no_hooks, filter, after_ts, before_ts);
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

    /// Clear hook logs for a service.
    pub fn clear_service_hooks(&self, store: &super::store::LogStoreHandle, service: &str) {
        store.clear_service_hooks(service);
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
    Real(f64),
}

impl From<SqlValue> for BindValue {
    fn from(v: SqlValue) -> Self {
        match v {
            SqlValue::Text(s) => BindValue::Text(s),
            SqlValue::Integer(i) => BindValue::Int(i),
            SqlValue::Real(f) => BindValue::Real(f),
        }
    }
}

fn build_filter(
    services: &[String],
    no_hooks: bool,
    filter: Option<&SqlFragment>,
    after_ts: Option<i64>,
    before_ts: Option<i64>,
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
        conditions.push("hook IS NULL".to_string());
    }

    // Timestamp bounds — injected server-side, no `logs:search` right required.
    if let Some(ts) = after_ts {
        bind.push(BindValue::Int(ts));
        conditions.push(format!("timestamp >= ?{}", bind.len()));
    }
    if let Some(ts) = before_ts {
        bind.push(BindValue::Int(ts));
        conditions.push(format!("timestamp <= ?{}", bind.len()));
    }

    // SQL filter fragment — either DSL-generated (with bind params) or raw SQL.
    // Bind param placeholders (?N) are shifted by the number of existing params.
    if let Some(frag) = filter {
        let shifted_sql = frag.sql_with_offset(bind.len());
        conditions.push(format!("({})", shifted_sql));
        for param in &frag.params {
            bind.push(BindValue::from(param.clone()));
        }
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
                BindValue::Real(f) => Box::new(*f),
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
