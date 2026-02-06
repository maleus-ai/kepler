//! Stateless log reader for querying logs from disk

use chrono::{TimeZone, Utc};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{
    LogLine, LogStream, MergedLogIterator, ReverseMergedLogIterator,
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES,
};

/// Stateless log reader for querying logs from disk.
///
/// Created on-demand for log queries. Uses merge-sort strategy to combine
/// multiple log files chronologically.
///
/// This reader handles single files per service/stream (no rotation files).
/// Log files use truncation instead of rotation for simplicity.
pub struct LogReader {
    logs_dir: PathBuf,
}

impl LogReader {
    /// Create a new LogReader for the given logs directory.
    pub fn new(logs_dir: PathBuf) -> Self {
        Self { logs_dir }
    }

    /// Get the last N entries, optionally filtered by service.
    /// Uses efficient reverse reading - only reads as much as needed from the end of files.
    pub fn tail(&self, count: usize, service: Option<&str>) -> Vec<LogLine> {
        let count = count.min(DEFAULT_MAX_LINES);

        // Collect files grouped by service for reverse iterator
        let files_by_service = self.collect_files_by_service(service);

        if files_by_service.is_empty() {
            return Vec::new();
        }

        // Use reverse iterator - reads from end of files, newest first
        let mut entries: Vec<LogLine> = ReverseMergedLogIterator::new(files_by_service)
            .take(count)
            .collect();

        // Reverse to get chronological order (oldest to newest)
        entries.reverse();
        entries
    }

    /// Get the last N entries with bounded reading to prevent OOM
    /// Falls back to full read + sort approach with byte limits
    pub fn tail_bounded(
        &self,
        count: usize,
        service: Option<&str>,
        max_bytes: Option<usize>,
    ) -> Vec<LogLine> {
        // If no byte limit specified, use efficient reverse reading
        if max_bytes.is_none() {
            return self.tail(count, service);
        }

        // With byte limit, use the bounded read approach
        let count = count.min(DEFAULT_MAX_LINES);
        let max_bytes = max_bytes.unwrap_or(DEFAULT_MAX_BYTES);

        // Collect all relevant log files
        let files = self.collect_log_files(service);

        if files.is_empty() {
            return Vec::new();
        }

        // Calculate per-file byte limit
        let per_file_max_bytes = (max_bytes / files.len()).max(1024 * 1024);

        // Read entries from all files
        let mut entries = Vec::with_capacity(count);
        for (path, svc, stream) in files {
            let file_entries = self.read_log_file_bounded(&path, &svc, stream, Some(per_file_max_bytes), None);
            entries.extend(file_entries);
        }

        // Sort by timestamp
        entries.sort_by_key(|e| e.timestamp);

        // Return last N
        let start = entries.len().saturating_sub(count);
        entries.into_iter().skip(start).collect()
    }

    /// Get logs with true pagination
    pub fn get_paginated(
        &self,
        service: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> (Vec<LogLine>, usize) {
        let files = self.collect_log_files(service);

        if files.is_empty() {
            return (Vec::new(), 0);
        }

        // Read all entries and count
        let mut all_entries: Vec<LogLine> = Vec::new();
        for (path, svc, stream) in files {
            let entries = self.read_log_file(&path, &svc, stream);
            all_entries.extend(entries);
        }

        // Sort by timestamp
        all_entries.sort_by_key(|e| e.timestamp);

        let total = all_entries.len();

        if offset >= total {
            return (Vec::new(), total);
        }

        // Apply offset and limit
        let entries: Vec<LogLine> = all_entries
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect();

        (entries, total)
    }

    /// Collect all log files, optionally filtered by service
    /// Returns (path, service_name, stream)
    pub(crate) fn collect_log_files(&self, service: Option<&str>) -> Vec<(PathBuf, String, LogStream)> {
        let mut files = Vec::new();

        if let Some(svc) = service {
            // Collect files for specific service
            self.collect_service_files(&mut files, svc);
        } else {
            // Collect files for all services directly from directory listing
            if let Ok(entries) = fs::read_dir(&self.logs_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if let Some((svc, stream)) = self.parse_log_filename(&path) {
                        files.push((path, svc, stream));
                    }
                }
            }
        }

        files
    }

    /// Collect all log files for a specific service (both streams).
    /// With truncation instead of rotation, there's only one file per service/stream.
    fn collect_service_files(&self, files: &mut Vec<(PathBuf, String, LogStream)>, service: &str) {
        let safe_name = service.replace(['/', '\\', ':', '[', ']'], "_");

        for stream in [LogStream::Stdout, LogStream::Stderr] {
            let log_file = self.logs_dir.join(format!("{}.{}", safe_name, stream.extension()));

            if log_file.exists() {
                files.push((log_file, service.to_string(), stream));
            }
        }
    }

    /// Collect files grouped by service/stream for the reverse iterator.
    /// Returns: Vec<(files_for_this_stream, service_name, stream)>
    /// With truncation instead of rotation, each group has only one file.
    pub(crate) fn collect_files_by_service(&self, service: Option<&str>) -> Vec<(Vec<PathBuf>, String, LogStream)> {
        let mut result: Vec<(Vec<PathBuf>, String, LogStream)> = Vec::new();

        let services: Vec<String> = if let Some(svc) = service {
            vec![svc.to_string()]
        } else {
            // Discover all services from log files
            self.discover_services()
        };

        for svc in services {
            let safe_name = svc.replace(['/', '\\', ':', '[', ']'], "_");

            for stream in [LogStream::Stdout, LogStream::Stderr] {
                let log_file = self.logs_dir.join(format!("{}.{}", safe_name, stream.extension()));

                if log_file.exists() {
                    result.push((vec![log_file], svc.clone(), stream));
                }
            }
        }

        result
    }

    /// Discover all services that have log files
    fn discover_services(&self) -> Vec<String> {
        let mut services = std::collections::HashSet::new();

        if let Ok(entries) = fs::read_dir(&self.logs_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some((svc, _)) = self.parse_log_filename(&path) {
                    services.insert(svc);
                }
            }
        }

        services.into_iter().collect()
    }

    /// Parse log filename to extract service name and stream
    /// Returns (service_name, stream) if valid
    fn parse_log_filename(&self, path: &Path) -> Option<(String, LogStream)> {
        let filename = path.file_name()?.to_str()?;

        // Parse: service.stdout.log or service.stderr.log
        filename
            .strip_suffix(".stdout.log")
            .map(|service| (service.to_string(), LogStream::Stdout))
            .or_else(|| {
                filename
                    .strip_suffix(".stderr.log")
                    .map(|service| (service.to_string(), LogStream::Stderr))
            })
    }

    /// Read all entries from a log file
    fn read_log_file(&self, path: &PathBuf, service: &str, stream: LogStream) -> Vec<LogLine> {
        self.read_log_file_bounded(path, service, stream, None, None)
    }

    /// Read log entries from a file with optional bounds
    pub(crate) fn read_log_file_bounded(
        &self,
        path: &PathBuf,
        service: &str,
        stream: LogStream,
        max_bytes: Option<usize>,
        max_lines: Option<usize>,
    ) -> Vec<LogLine> {
        let mut entries = Vec::with_capacity(256);
        let file = match File::open(path) {
            Ok(f) => f,
            Err(_) => return entries,
        };

        let metadata = match file.metadata() {
            Ok(m) => m,
            Err(_) => return entries,
        };

        let file_size = metadata.len() as usize;
        let max_bytes = max_bytes.unwrap_or(DEFAULT_MAX_BYTES);

        // Pre-allocate service as Arc<str> once for this file - cheap cloning for all lines
        let service_arc: Arc<str> = Arc::from(service);
        // Reusable line buffer - avoids allocation per line
        let mut line_buf = String::with_capacity(256);

        // If file is larger than max_bytes, read only the last max_bytes
        if file_size > max_bytes {
            use std::io::{Seek, SeekFrom};
            let mut file = file;
            let start_pos = file_size - max_bytes;

            if file.seek(SeekFrom::Start(start_pos as u64)).is_err() {
                return entries;
            }

            let mut reader = BufReader::new(file);

            // Skip the first partial line
            if reader.read_line(&mut line_buf).is_err() {
                return entries;
            }
            line_buf.clear();

            // Read remaining lines using reusable buffer
            while reader.read_line(&mut line_buf).unwrap_or(0) > 0 {
                if let Some(entry) = Self::parse_log_line_arc(&line_buf, &service_arc, stream) {
                    entries.push(entry);
                }
                line_buf.clear();
            }
        } else {
            // File is small enough, read everything
            let mut reader = BufReader::new(file);
            while reader.read_line(&mut line_buf).unwrap_or(0) > 0 {
                if let Some(entry) = Self::parse_log_line_arc(&line_buf, &service_arc, stream) {
                    entries.push(entry);
                }
                line_buf.clear();
            }
        }

        // Apply max_lines limit if specified
        if let Some(max) = max_lines {
            let len = entries.len();
            if len > max {
                entries = entries.into_iter().skip(len - max).collect();
            }
        }

        entries
    }

    /// Parse a log line with Arc<str> service (cheap clone - just reference count increment)
    /// New format: "TIMESTAMP\tMESSAGE"
    /// Note: line may contain trailing newline from read_line()
    pub fn parse_log_line_arc(line: &str, service: &Arc<str>, stream: LogStream) -> Option<LogLine> {
        let line = line.trim_end_matches('\n').trim_end_matches('\r');
        let mut parts = line.splitn(2, '\t');

        let timestamp_str = parts.next()?;
        let content = parts.next().unwrap_or("");

        let timestamp = timestamp_str.parse::<i64>().ok()?;

        Some(LogLine {
            service: Arc::clone(service), // Cheap: just increment reference count
            line: content.to_string(),
            timestamp: Utc.timestamp_millis_opt(timestamp).single()?,
            stream,
        })
    }

    /// Clear all log files
    pub fn clear(&self) {
        if let Ok(entries) = fs::read_dir(&self.logs_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

                // Delete .stdout.log and .stderr.log files and their rotated versions
                if filename.contains(".stdout.log") || filename.contains(".stderr.log") {
                    let _ = fs::remove_file(&path);
                }
            }
        }
    }

    /// Clear logs for a specific service
    pub fn clear_service(&self, service: &str) {
        let safe_name = service.replace(['/', '\\', ':', '[', ']'], "_");

        for stream in [LogStream::Stdout, LogStream::Stderr] {
            let log_file = self.logs_dir.join(format!("{}.{}", safe_name, stream.extension()));
            let _ = fs::remove_file(&log_file);
        }
    }

    /// Clear logs for services matching a prefix
    pub fn clear_service_prefix(&self, prefix: &str) {
        let safe_prefix = prefix.replace(['/', '\\', ':', '[', ']'], "_");

        if let Ok(entries) = fs::read_dir(&self.logs_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

                if filename.starts_with(&safe_prefix) {
                    let _ = fs::remove_file(&path);
                }
            }
        }
    }

    /// Get logs directory
    pub fn logs_dir(&self) -> &Path {
        &self.logs_dir
    }

    /// Create a merged iterator for streaming logs in chronological order (oldest first)
    /// This is memory-efficient as it only keeps one entry per file in memory at a time.
    pub fn iter(&self, service: Option<&str>) -> MergedLogIterator {
        let files = self.collect_log_files(service);
        MergedLogIterator::new(files)
    }

    /// Get the first N entries in chronological order using the efficient iterator
    /// Unlike tail(), this doesn't need to read all entries - it stops after N.
    pub fn head(&self, count: usize, service: Option<&str>) -> Vec<LogLine> {
        self.iter(service).take(count).collect()
    }
}
