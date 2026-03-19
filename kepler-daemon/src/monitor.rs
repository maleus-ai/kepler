//! Resource monitoring for running services.
//!
//! Periodically collects CPU and memory metrics for each running service's
//! process tree and stores them in a SQLite database.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use rusqlite::Connection;
use sysinfo::{Pid, ProcessesToUpdate, System};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::config::{MonitorConfig, StorageMode};
use crate::config_actor::ConfigActorHandle;
use crate::containment::ContainmentManager;
use crate::query::{QueryDsl, Field, SqlFragment, SqlValue};
use crate::query::filter;
use kepler_protocol::protocol::MonitorMetricEntry;

/// A single metrics sample for one service.
struct ServiceMetrics {
    service: String,
    cpu_percent: f32,
    memory_rss: u64,
    memory_vss: u64,
    pids: Vec<u32>,
}

/// Spawn the monitor loop. Returns a JoinHandle that can be aborted on cleanup.
pub fn spawn_monitor(
    config: MonitorConfig,
    handle: ConfigActorHandle,
    state_dir: PathBuf,
    containment: ContainmentManager,
    storage_mode: StorageMode,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let conn = match open_monitor_db(&state_dir, storage_mode) {
            Ok(c) => Arc::new(Mutex::new(c)),
            Err(e) => {
                warn!("Failed to open monitor database: {}", e);
                return;
            }
        };

        // Apply retention on startup
        {
            let conn = conn.clone();
            let retention = config.retention;
            if let Err(e) = tokio::task::spawn_blocking(move || {
                let conn = conn.lock();
                apply_retention(&conn, retention)
            })
            .await
            .unwrap_or_else(|e| Err(e.into()))
            {
                warn!("Failed to apply retention: {}", e);
            }
        }

        let mut sys = System::new();
        let config_hash = handle.config_hash().to_string();

        loop {
            tokio::time::sleep(config.interval).await;

            let running = handle.get_running_services().await;
            if running.is_empty() {
                continue;
            }

            let now = chrono::Utc::now().timestamp_millis();
            let mut all_metrics = Vec::new();

            for service_name in &running {
                // Get PIDs from cgroup, or fall back to stored PID
                let mut pids = containment.enumerate_service_pids(&config_hash, service_name);
                if pids.is_empty() {
                    // Fallback: get the main PID from service state
                    if let Some(state) = handle.get_service_state(service_name).await {
                        if let Some(pid) = state.pid {
                            pids.push(pid);
                            // Also collect child processes via sysinfo
                            collect_descendants(&sys, pid, &mut pids);
                        }
                    }
                }

                if pids.is_empty() {
                    continue;
                }

                // Refresh only the processes we care about
                let pids_to_update: Vec<Pid> = pids.iter().map(|&p| Pid::from_u32(p)).collect();
                sys.refresh_processes(ProcessesToUpdate::Some(&pids_to_update), true);

                let mut total_cpu: f32 = 0.0;
                let mut total_rss: u64 = 0.0 as u64;
                let mut total_vss: u64 = 0;

                for &pid in &pids {
                    if let Some(proc_info) = sys.process(Pid::from_u32(pid)) {
                        total_cpu += proc_info.cpu_usage();
                        total_rss += proc_info.memory();
                        total_vss += proc_info.virtual_memory();
                    }
                }

                all_metrics.push(ServiceMetrics {
                    service: service_name.clone(),
                    cpu_percent: total_cpu,
                    memory_rss: total_rss,
                    memory_vss: total_vss,
                    pids: pids,
                });
            }

            if all_metrics.is_empty() {
                continue;
            }

            // Insert metrics in a blocking task
            let conn = conn.clone();
            if let Err(e) = tokio::task::spawn_blocking(move || {
                let conn = conn.lock();
                insert_metrics(&conn, now, &all_metrics)
            })
            .await
            .unwrap_or_else(|e| Err(e.into()))
            {
                debug!("Failed to insert monitor metrics: {}", e);
            }
        }
    })
}

/// Open or create the monitor SQLite database with appropriate journal mode and schema.
fn open_monitor_db(state_dir: &Path, mode: StorageMode) -> anyhow::Result<Connection> {
    let db_path = state_dir.join("monitor.db");
    let conn = Connection::open(&db_path)?;

    // Shared pragmas
    conn.execute_batch(
        "PRAGMA mmap_size = 0;
         PRAGMA cache_size = -8000;
         PRAGMA temp_store = MEMORY;
         PRAGMA page_size = 4096;
         PRAGMA journal_size_limit = 6291456;",
    )?;

    match mode {
        StorageMode::Local => {
            conn.execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = NORMAL;
                 PRAGMA wal_autocheckpoint = 1000;",
            )?;
        }
        StorageMode::Nfs => {
            conn.execute_batch(
                "PRAGMA journal_mode = DELETE;
                 PRAGMA synchronous = FULL;",
            )?;
        }
    }

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS metrics (
             timestamp INTEGER NOT NULL,
             service TEXT NOT NULL,
             cpu_percent REAL NOT NULL,
             memory_rss INTEGER NOT NULL,
             memory_vss INTEGER NOT NULL,
             pids TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_metrics_ts ON metrics(timestamp);
         CREATE INDEX IF NOT EXISTS idx_metrics_svc_ts ON metrics(service, timestamp);",
    )?;

    Ok(conn)
}

/// Apply retention policy: no retention → clear all; with retention → delete old rows.
fn apply_retention(conn: &Connection, retention: Option<Duration>) -> anyhow::Result<()> {
    match retention {
        None => {
            conn.execute("DELETE FROM metrics", [])?;
        }
        Some(retention) => {
            let cutoff = chrono::Utc::now().timestamp_millis() - retention.as_millis() as i64;
            conn.execute("DELETE FROM metrics WHERE timestamp < ?1", [cutoff])?;
        }
    }
    Ok(())
}

/// Collect descendant PIDs by walking sysinfo's process tree.
fn collect_descendants(sys: &System, parent_pid: u32, result: &mut Vec<u32>) {
    let parent = Pid::from_u32(parent_pid);
    for (pid, proc_info) in sys.processes() {
        if let Some(ppid) = proc_info.parent() {
            if ppid == parent && !result.contains(&pid.as_u32()) {
                let child_pid = pid.as_u32();
                result.push(child_pid);
                collect_descendants(sys, child_pid, result);
            }
        }
    }
}

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

/// Batch-insert metrics into SQLite in a single transaction.
fn insert_metrics(
    conn: &Connection,
    timestamp: i64,
    metrics: &[ServiceMetrics],
) -> anyhow::Result<()> {
    let tx = conn.unchecked_transaction()?;

    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO metrics (timestamp, service, cpu_percent, memory_rss, memory_vss, pids)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;

        for m in metrics {
            let pids_json = serde_json::to_string(&m.pids).unwrap_or_else(|_| "[]".to_string());
            stmt.execute(rusqlite::params![
                timestamp,
                m.service,
                m.cpu_percent,
                m.memory_rss,
                m.memory_vss,
                pids_json,
            ])?;
        }
    }

    tx.commit()?;
    Ok(())
}
