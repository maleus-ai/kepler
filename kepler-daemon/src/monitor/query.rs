//! Read-only query functions for the monitor database.
//!
//! Each query opens a fresh `SQLITE_OPEN_READ_ONLY` connection via
//! [`open_monitor_db_readonly`]. This is required for NFS correctness
//! (where WAL is not available) and is cheap on local WAL mode.

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

use crate::query::{QueryDsl, Field, SqlFragment, SqlValue};
use crate::query::filter;
use kepler_protocol::protocol::MonitorMetricEntry;

/// Open the monitor database read-only. Opens a fresh connection per call
/// (required for NFS correctness, cheap on local WAL).
/// Returns error if `monitor.db` doesn't exist.
pub fn open_monitor_db_readonly(state_dir: &Path) -> anyhow::Result<Connection> {
    let db_path = state_dir.join("monitor.db");
    if !db_path.exists() {
        anyhow::bail!("monitor database not found at {}", db_path.display());
    }
    let conn = Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch(
        "PRAGMA query_only = ON;
         PRAGMA mmap_size = 0;",
    )?;
    Ok(conn)
}

/// Query the latest metric row per service (or for one service).
pub fn query_latest(
    conn: &Connection,
    service: Option<&str>,
) -> anyhow::Result<Vec<MonitorMetricEntry>> {
    let entries = match service {
        Some(svc) => {
            let mut stmt = conn.prepare(
                "SELECT timestamp, service, cpu_percent, memory_rss, memory_vss, pids
                 FROM metrics WHERE service = ?1
                 ORDER BY timestamp DESC LIMIT 1",
            )?;
            stmt.query_map([svc], parse_metric_row)?
                .filter_map(|r| r.ok())
                .collect()
        }
        None => {
            let mut stmt = conn.prepare(
                "SELECT m.timestamp, m.service, m.cpu_percent, m.memory_rss, m.memory_vss, m.pids
                 FROM metrics m
                 INNER JOIN (SELECT service, MAX(timestamp) as max_ts FROM metrics GROUP BY service) latest
                 ON m.service = latest.service AND m.timestamp = latest.max_ts",
            )?;
            stmt.query_map([], parse_metric_row)?
                .filter_map(|r| r.ok())
                .collect()
        }
    };
    Ok(entries)
}

/// Returns (SELECT clause, GROUP BY clause) for raw or bucketed queries.
fn bucket_select_clause(bucket_ms: Option<i64>) -> (String, String) {
    match bucket_ms {
        Some(b) if b > 0 => (
            format!(
                "SELECT (timestamp / {b}) * {b} AS timestamp, service, \
                 AVG(cpu_percent) AS cpu_percent, MAX(memory_rss) AS memory_rss, \
                 MAX(memory_vss) AS memory_vss, '[]' AS pids"
            ),
            "GROUP BY 1, service".to_string(),
        ),
        _ => (
            "SELECT timestamp, service, cpu_percent, memory_rss, memory_vss, pids".to_string(),
            String::new(),
        ),
    }
}

/// Query metric rows since a given timestamp, ordered chronologically.
pub fn query_history(
    conn: &Connection,
    service: Option<&str>,
    since: i64,
    limit: Option<usize>,
    bucket_ms: Option<i64>,
    before_ts: Option<i64>,
) -> anyhow::Result<Vec<MonitorMetricEntry>> {
    let limit = limit.unwrap_or(10_000);
    let (select, group_by) = bucket_select_clause(bucket_ms);

    let mut conditions = vec!["timestamp >= ?1".to_string()];
    let mut bind: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(since)];

    if let Some(svc) = service {
        bind.push(Box::new(svc.to_string()));
        conditions.push(format!("service = ?{}", bind.len()));
    }
    if let Some(ts) = before_ts {
        bind.push(Box::new(ts));
        conditions.push(format!("timestamp <= ?{}", bind.len()));
    }

    bind.push(Box::new(limit as i64));
    let limit_param = bind.len();

    let where_clause = format!("WHERE {}", conditions.join(" AND "));
    let sql = format!(
        "{} FROM metrics {} {} ORDER BY 1 ASC LIMIT ?{}",
        select, where_clause, group_by, limit_param,
    );

    let mut stmt = conn.prepare(&sql)?;
    let params_refs: Vec<&dyn rusqlite::types::ToSql> = bind.iter().map(|p| p.as_ref()).collect();
    let entries: Vec<MonitorMetricEntry> = stmt
        .query_map(params_refs.as_slice(), parse_metric_row)?
        .filter_map(|r| r.ok())
        .collect();
    Ok(entries)
}

/// Build the monitor-specific query DSL schema.
pub fn monitor_query_dsl() -> QueryDsl {
    QueryDsl::builder()
        .field(Field::text("service"))
        .field(Field::int("timestamp"))
        .field(Field::real("cpu_percent"))
        .field(Field::int("memory_rss"))
        .field(Field::int("memory_vss"))
        .build()
}

/// Query metrics with a DSL or raw SQL filter.
///
/// Builds a WHERE clause combining optional `service`, optional `since`,
/// and the user-provided filter fragment. Applies safety safeguards
/// (authorizer, resource limits, timeout) before execution.
pub fn query_filtered(
    conn: &Connection,
    service: Option<&str>,
    since: Option<i64>,
    limit: Option<usize>,
    frag: &SqlFragment,
    bucket_ms: Option<i64>,
    after_ts: Option<i64>,
    before_ts: Option<i64>,
) -> anyhow::Result<Vec<MonitorMetricEntry>> {
    let limit = limit.unwrap_or(10_000);
    let (select, group_by) = bucket_select_clause(bucket_ms);
    let mut conditions = Vec::new();
    let mut bind: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(svc) = service {
        bind.push(Box::new(svc.to_string()));
        conditions.push(format!("service = ?{}", bind.len()));
    }
    if let Some(since_ts) = since {
        bind.push(Box::new(since_ts));
        conditions.push(format!("timestamp >= ?{}", bind.len()));
    }

    // Timestamp bounds — injected server-side, no `monitor:search` right required.
    if let Some(ts) = after_ts {
        bind.push(Box::new(ts));
        conditions.push(format!("timestamp >= ?{}", bind.len()));
    }
    if let Some(ts) = before_ts {
        bind.push(Box::new(ts));
        conditions.push(format!("timestamp <= ?{}", bind.len()));
    }

    // Integrate filter fragment with shifted bind params
    let shifted_sql = frag.sql_with_offset(bind.len());
    conditions.push(format!("({})", shifted_sql));
    for param in &frag.params {
        match param {
            SqlValue::Text(s) => bind.push(Box::new(s.clone())),
            SqlValue::Integer(i) => bind.push(Box::new(*i)),
            SqlValue::Real(f) => bind.push(Box::new(*f)),
        }
    }

    bind.push(Box::new(limit as i64));
    let limit_param = bind.len();

    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };

    let sql = format!(
        "{} FROM metrics {} {} ORDER BY 1 ASC LIMIT ?{}",
        select, where_clause, group_by, limit_param,
    );

    // Apply safety safeguards for the user-provided filter
    filter::apply_filter_safeguards(conn, filter::FILTER_QUERY_TIMEOUT, &["metrics"]);

    let mut stmt = conn.prepare(&sql)?;
    let params_refs: Vec<&dyn rusqlite::types::ToSql> = bind.iter().map(|p| p.as_ref()).collect();
    let entries: Vec<MonitorMetricEntry> = stmt
        .query_map(params_refs.as_slice(), parse_metric_row)?
        .filter_map(|r| r.ok())
        .collect();
    Ok(entries)
}

/// Map a SQLite row to a MonitorMetricEntry.
fn parse_metric_row(row: &rusqlite::Row) -> rusqlite::Result<MonitorMetricEntry> {
    let pids_json: String = row.get(5)?;
    let pids: Vec<u32> = serde_json::from_str(&pids_json).unwrap_or_default();
    Ok(MonitorMetricEntry {
        timestamp: row.get(0)?,
        service: row.get(1)?,
        cpu_percent: row.get(2)?,
        memory_rss: row.get(3)?,
        memory_vss: row.get(4)?,
        pids,
    })
}
