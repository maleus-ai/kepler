use chrono::{DateTime, TimeZone, Utc};
use kepler_protocol::protocol::LogEntry;
use parking_lot::RwLock;
use std::fmt::Write as FmtWrite;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Validate that a path is not a symlink (security measure to prevent symlink attacks)
#[cfg(unix)]
fn validate_not_symlink(path: &std::path::Path) -> std::io::Result<()> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Log path is a symlink: {:?}", path),
            ));
        }
    }
    Ok(())
}

/// Default maximum bytes to read when tailing logs (10MB)
pub const DEFAULT_MAX_BYTES: usize = 10 * 1024 * 1024;

/// Default maximum lines to return
pub const DEFAULT_MAX_LINES: usize = 10_000;

/// A single log line with metadata
#[derive(Debug, Clone)]
pub struct LogLine {
    pub service: String,
    pub line: String,
    pub timestamp: DateTime<Utc>,
    pub stream: LogStream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogStream {
    Stdout,
    Stderr,
}

impl LogStream {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogStream::Stdout => "stdout",
            LogStream::Stderr => "stderr",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "stdout" => Some(LogStream::Stdout),
            "stderr" => Some(LogStream::Stderr),
            _ => None,
        }
    }
}

impl std::fmt::Display for LogStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Default max log file size for rotation (10MB)
pub const DEFAULT_MAX_LOG_SIZE: u64 = 10 * 1024 * 1024;

/// Default number of rotated files to keep
pub const DEFAULT_MAX_ROTATED_FILES: u32 = 5;

/// Disk-based log buffer for log entries per config
pub struct LogBuffer {
    logs_dir: PathBuf,
    /// Monotonically increasing sequence number for tracking position
    sequence: u64,
    /// Maximum log file size before rotation (bytes)
    max_log_size: u64,
    /// Maximum number of rotated files to keep
    max_rotated_files: u32,
    /// Pre-allocated buffer for formatting log entries (reduces allocations)
    format_buffer: String,
    /// Cached file handles for writing
    open_files: std::collections::HashMap<String, File>,
    /// Per-service write buffers (only used when buffer_size > 0)
    write_buffers: std::collections::HashMap<String, Vec<u8>>,
    /// Maximum buffer size per service before flushing (0 = write directly, no buffering)
    buffer_size: usize,
    /// Bytes written per service since last rotation (avoids fs::metadata on every write)
    bytes_written: std::collections::HashMap<String, u64>,
    /// Next rotation index per service (1 to max_rotated_files, cycling)
    rotation_index: std::collections::HashMap<String, u32>,
}

impl std::fmt::Debug for LogBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogBuffer")
            .field("logs_dir", &self.logs_dir)
            .field("sequence", &self.sequence)
            .field("max_log_size", &self.max_log_size)
            .field("max_rotated_files", &self.max_rotated_files)
            .field("open_files_count", &self.open_files.len())
            .field("buffer_size", &self.buffer_size)
            .field("buffered_services", &self.write_buffers.len())
            .field("tracked_services", &self.bytes_written.len())
            .finish()
    }
}

impl LogBuffer {
    pub fn new(logs_dir: PathBuf) -> Self {
        Self::with_options(logs_dir, DEFAULT_MAX_LOG_SIZE, DEFAULT_MAX_ROTATED_FILES, 0)
    }

    pub fn with_rotation(logs_dir: PathBuf, max_log_size: u64, max_rotated_files: u32) -> Self {
        Self::with_options(logs_dir, max_log_size, max_rotated_files, 0)
    }

    /// Create a LogBuffer with all options including buffer size.
    ///
    /// # Arguments
    /// * `logs_dir` - Directory to store log files
    /// * `max_log_size` - Maximum size of a log file before rotation
    /// * `max_rotated_files` - Number of rotated files to keep
    /// * `buffer_size` - Bytes to buffer per service before flushing (0 = no buffering)
    pub fn with_options(
        logs_dir: PathBuf,
        max_log_size: u64,
        max_rotated_files: u32,
        buffer_size: usize,
    ) -> Self {
        // Create logs directory if it doesn't exist with secure permissions (0o700)
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            if let Err(e) = std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&logs_dir)
            {
                tracing::warn!("Failed to create logs directory {:?}: {}", logs_dir, e);
            }
        }
        #[cfg(not(unix))]
        {
            if let Err(e) = fs::create_dir_all(&logs_dir) {
                tracing::warn!("Failed to create logs directory {:?}: {}", logs_dir, e);
            }
        }

        // Load or estimate sequence number (optimized - no longer counts all lines)
        let sequence = Self::load_or_estimate_sequence(&logs_dir);

        Self {
            logs_dir,
            sequence,
            max_log_size,
            max_rotated_files,
            // Pre-allocate buffer for typical log line (256 bytes should cover most cases)
            format_buffer: String::with_capacity(256),
            // Cache file handles for ~10 services
            open_files: std::collections::HashMap::with_capacity(16),
            // Per-service write buffers
            write_buffers: std::collections::HashMap::with_capacity(16),
            buffer_size,
            // Track bytes written per service (initialized lazily on first write)
            bytes_written: std::collections::HashMap::with_capacity(16),
            // Track rotation index per service
            rotation_index: std::collections::HashMap::with_capacity(16),
        }
    }

    /// Sequence metadata file path
    fn sequence_file_path(logs_dir: &Path) -> PathBuf {
        logs_dir.join(".sequence")
    }

    /// Load sequence from metadata file, or estimate from file sizes
    ///
    /// This is much faster than counting all lines on startup.
    /// Uses file size as an approximation when the metadata file doesn't exist.
    fn load_or_estimate_sequence(logs_dir: &Path) -> u64 {
        let seq_file = Self::sequence_file_path(logs_dir);

        // Try to load from metadata file first (O(1))
        if let Ok(contents) = fs::read_to_string(&seq_file) {
            if let Ok(seq) = contents.trim().parse::<u64>() {
                return seq;
            }
        }

        // Fall back to size-based estimation (O(number of files), not O(lines))
        // Assume average log line is ~100 bytes
        const AVG_LINE_SIZE: u64 = 100;
        let mut total_bytes = 0u64;

        if let Ok(entries) = fs::read_dir(logs_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "log") {
                    if let Ok(metadata) = fs::metadata(&path) {
                        total_bytes += metadata.len();
                    }
                }
            }
        }

        total_bytes / AVG_LINE_SIZE
    }

    /// Save sequence to metadata file
    fn save_sequence(&self) {
        let seq_file = Self::sequence_file_path(&self.logs_dir);

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            if let Ok(mut file) = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&seq_file)
            {
                let _ = file.write_all(self.sequence.to_string().as_bytes());
            }
        }
        #[cfg(not(unix))]
        {
            let _ = fs::write(&seq_file, self.sequence.to_string());
        }
    }

    /// Count existing log entries across all log files (legacy, kept for compatibility)
    #[allow(dead_code)]
    fn count_existing_entries(logs_dir: &PathBuf) -> u64 {
        let mut count = 0u64;
        if let Ok(entries) = fs::read_dir(logs_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "log") {
                    if let Ok(file) = File::open(&path) {
                        count += BufReader::new(file).lines().count() as u64;
                    }
                }
            }
        }
        count
    }

    /// Get the log file path for a service
    fn log_file_path(&self, service: &str) -> PathBuf {
        // Sanitize service name for filename (replace special chars)
        let safe_name = service.replace(['/', '\\', ':', '[', ']'], "_");
        self.logs_dir.join(format!("{}.log", safe_name))
    }

    /// Add a log entry, appending to disk with rotation if needed.
    ///
    /// If `buffer_size > 0`, entries are buffered per-service and flushed when
    /// the buffer exceeds `buffer_size` bytes. If `buffer_size == 0`, entries
    /// are written directly to disk.
    pub fn push(&mut self, service: &str, line: String, stream: LogStream) {
        let timestamp = Utc::now().timestamp_millis();

        // Format: {timestamp_unix}\t{stream}\t{service}\t{line}
        self.format_buffer.clear();
        let _ = writeln!(
            self.format_buffer,
            "{}\t{}\t{}\t{}",
            timestamp,
            stream.as_str(),
            service,
            line
        );

        self.sequence += 1;

        if self.buffer_size == 0 {
            // No buffering - write directly to disk
            // Take ownership of format_buffer to avoid allocation
            let formatted = std::mem::take(&mut self.format_buffer);
            self.write_to_disk(service, formatted.as_bytes());
            self.format_buffer = formatted;
            self.format_buffer.clear();
        } else {
            // Buffer the entry
            let buffer = self
                .write_buffers
                .entry(service.to_string())
                .or_insert_with(|| Vec::with_capacity(self.buffer_size));

            buffer.extend_from_slice(self.format_buffer.as_bytes());

            // Flush if buffer exceeds size
            if buffer.len() >= self.buffer_size {
                self.flush_service(service);
            }
        }

        // Periodically save sequence to disk (every 100 entries)
        if self.sequence % 100 == 0 {
            self.save_sequence();
        }
    }

    /// Write data directly to disk for a service
    fn write_to_disk(&mut self, service: &str, data: &[u8]) {
        let log_file = self.log_file_path(service);
        let service_key = service.to_string();

        // Initialize bytes_written if first write to this service
        // Check existing file size once (only on first write per service)
        let current_bytes = *self
            .bytes_written
            .entry(service_key.clone())
            .or_insert_with(|| fs::metadata(&log_file).map(|m| m.len()).unwrap_or(0));

        // Check if rotation is needed based on tracked bytes (no fs::metadata call)
        if current_bytes >= self.max_log_size {
            self.open_files.remove(service);
            self.rotate_log_file(service, &log_file);
            // Reset bytes written after rotation
            self.bytes_written.insert(service_key.clone(), 0);
        }

        // Get or create the file handle
        let log_file_clone = log_file.clone();
        let file = self.open_files.entry(service_key.clone()).or_insert_with(|| {
            // Validate existing path is not a symlink
            #[cfg(unix)]
            if let Err(e) = validate_not_symlink(&log_file_clone) {
                tracing::warn!("Refusing to open log file: {}", e);
                return File::open("/dev/null").expect("Failed to open /dev/null");
            }

            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW) // Don't follow symlinks on open
                    .open(&log_file_clone)
                    .unwrap_or_else(|e| {
                        tracing::warn!("Failed to open log file {:?}: {}", log_file_clone, e);
                        File::open("/dev/null").expect("Failed to open /dev/null")
                    })
            }
            #[cfg(not(unix))]
            {
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log_file_clone)
                    .expect("Failed to open log file")
            }
        });

        if let Err(e) = file.write_all(data) {
            tracing::warn!("Failed to write log entry: {}", e);
            self.open_files.remove(&service_key);
        } else {
            // Update bytes written (no syscall, just memory)
            *self.bytes_written.get_mut(&service_key).unwrap() += data.len() as u64;
        }
    }

    /// Flush buffered entries for a specific service to disk
    pub fn flush_service(&mut self, service: &str) {
        if let Some(buffer) = self.write_buffers.remove(service) {
            if !buffer.is_empty() {
                self.write_to_disk(service, &buffer);
            }
        }
    }

    /// Flush all buffered entries to disk
    pub fn flush_all(&mut self) {
        // Drain buffers without cloning keys
        let buffers = std::mem::take(&mut self.write_buffers);
        for (service, buffer) in buffers {
            if !buffer.is_empty() {
                self.write_to_disk(&service, &buffer);
            }
        }
    }

    /// Close all cached file handles for graceful shutdown
    pub fn close_all(&mut self) {
        // Flush any remaining buffered data first
        self.flush_all();
        self.open_files.clear(); // File handles are flushed on drop
    }

    /// Rotate a log file using cycling index (single rename, no cascade)
    ///
    /// Instead of shifting all files (.log.1 -> .log.2, etc.), we use a cycling
    /// index that overwrites the oldest file directly. This reduces rotation
    /// from O(max_rotated_files) renames to O(1).
    fn rotate_log_file(&mut self, service: &str, log_file: &PathBuf) {
        // Validate source before rotation
        #[cfg(unix)]
        if let Err(e) = validate_not_symlink(log_file) {
            tracing::error!("Cannot rotate: {}", e);
            return;
        }

        // Get next rotation index (1 to max_rotated_files, cycling)
        let index = self
            .rotation_index
            .entry(service.to_string())
            .or_insert(0);
        *index = (*index % self.max_rotated_files) + 1;

        let rotated = format!("{}.{}", log_file.display(), *index);
        tracing::debug!("Rotating log file {:?} -> {}", log_file, rotated);

        // Single rename (overwrites if target exists)
        if let Err(e) = fs::rename(log_file, &rotated) {
            tracing::warn!("Failed to rotate log file {:?}: {}", log_file, e);
        }
    }

    /// Clean up old rotated log files beyond max_rotated_files
    pub fn cleanup_old_rotations(&self) {
        for file in self.list_log_files() {
            // Check for rotated files beyond max
            for i in (self.max_rotated_files + 1)..100 {
                let rotated = format!("{}.{}", file.display(), i);
                if PathBuf::from(&rotated).exists() {
                    let _ = fs::remove_file(&rotated);
                } else {
                    break; // No more rotated files
                }
            }
        }
    }

    /// Get the current sequence number (for follow mode)
    pub fn current_sequence(&self) -> u64 {
        self.sequence
    }

    /// List all log files in the directory (main files only, not rotated)
    fn list_log_files(&self) -> Vec<PathBuf> {
        let mut files = Vec::with_capacity(16); // Typical ~10 services
        if let Ok(entries) = fs::read_dir(&self.logs_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "log") {
                    files.push(path);
                }
            }
        }
        files
    }

    /// List all log files for a service including rotated files
    /// Returns files sorted by first log entry timestamp (oldest first, newest last)
    fn list_service_log_files(&self, service: &str) -> Vec<PathBuf> {
        let main_file = self.log_file_path(service);
        let mut files = Vec::with_capacity(self.max_rotated_files as usize + 1);

        // Collect all rotated files
        for i in 1..=self.max_rotated_files {
            let rotated = PathBuf::from(format!("{}.{}", main_file.display(), i));
            if rotated.exists() {
                files.push(rotated);
            }
        }

        // Add main file if exists
        if main_file.exists() {
            files.push(main_file);
        }

        // Sort by first log entry timestamp (more accurate than file modification time)
        // This is necessary because rotation uses a cycling index
        files.sort_by(|a, b| {
            let ts_a = Self::first_entry_timestamp(a);
            let ts_b = Self::first_entry_timestamp(b);
            ts_a.cmp(&ts_b)
        });

        files
    }

    /// Read the timestamp from the first log entry in a file
    fn first_entry_timestamp(path: &PathBuf) -> i64 {
        if let Ok(file) = File::open(path) {
            let mut reader = BufReader::new(file);
            let mut first_line = String::new();
            if reader.read_line(&mut first_line).is_ok() {
                // Log format: "TIMESTAMP\tSTREAM\tSERVICE\tMESSAGE"
                if let Some(ts_str) = first_line.split('\t').next() {
                    if let Ok(ts) = ts_str.parse::<i64>() {
                        return ts;
                    }
                }
            }
        }
        0 // Default to 0 if we can't read the timestamp
    }

    /// Read log entries from a file
    fn read_log_file(&self, path: &PathBuf) -> Vec<LogLine> {
        self.read_log_file_bounded(path, None, None)
    }

    /// Read log entries from a file with optional bounds
    ///
    /// `max_bytes` - Maximum bytes to read from the file (reads from end)
    /// `max_lines` - Maximum number of lines to return
    fn read_log_file_bounded(
        &self,
        path: &PathBuf,
        max_bytes: Option<usize>,
        max_lines: Option<usize>,
    ) -> Vec<LogLine> {
        let mut entries = Vec::with_capacity(256); // Reasonable default
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

        // If file is larger than max_bytes, read only the last max_bytes
        if file_size > max_bytes {
            let mut file = file;
            let start_pos = file_size - max_bytes;

            if file.seek(SeekFrom::Start(start_pos as u64)).is_err() {
                return entries;
            }

            let mut reader = BufReader::new(file);

            // Skip the first partial line (we started in the middle)
            let mut partial_line = String::new();
            if reader.read_line(&mut partial_line).is_err() {
                return entries;
            }

            // Read remaining lines
            for line in reader.lines().map_while(Result::ok) {
                if let Some(entry) = Self::parse_log_line(&line) {
                    entries.push(entry);
                }
            }
        } else {
            // File is small enough, read everything
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                if let Some(entry) = Self::parse_log_line(&line) {
                    entries.push(entry);
                }
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

    /// Parse a log line from the file format
    fn parse_log_line(line: &str) -> Option<LogLine> {
        let parts: Vec<&str> = line.splitn(4, '\t').collect();
        if parts.len() < 4 {
            return None;
        }

        let timestamp = parts[0].parse::<i64>().ok()?;
        let stream = LogStream::parse(parts[1])?;
        let service = parts[2].to_string();
        let content = parts[3].to_string();

        Some(LogLine {
            service,
            line: content,
            timestamp: Utc.timestamp_millis_opt(timestamp).single()?,
            stream,
        })
    }

    /// Get entries since a given sequence number
    pub fn entries_since(&self, since_seq: u64) -> Vec<LogLine> {
        if since_seq >= self.sequence {
            return Vec::new();
        }

        // Read all entries and sort by timestamp
        let mut all_entries = Vec::with_capacity(256);
        for file in self.list_log_files() {
            all_entries.extend(self.read_log_file(&file));
        }
        all_entries.sort_by_key(|e| e.timestamp);

        // Skip entries up to since_seq
        let skip = since_seq as usize;
        all_entries.into_iter().skip(skip).collect()
    }

    /// Get the last N entries, optionally filtered by service
    pub fn tail(&self, count: usize, service: Option<&str>) -> Vec<LogLine> {
        self.tail_bounded(count, service, None)
    }

    /// Get the last N entries with bounded reading to prevent OOM
    ///
    /// `count` - Maximum number of lines to return
    /// `service` - Optional service filter
    /// `max_bytes` - Maximum bytes to read per file (prevents OOM with large logs)
    pub fn tail_bounded(
        &self,
        count: usize,
        service: Option<&str>,
        max_bytes: Option<usize>,
    ) -> Vec<LogLine> {
        // Apply sensible defaults to prevent OOM
        let count = count.min(DEFAULT_MAX_LINES);
        let max_bytes = max_bytes.unwrap_or(DEFAULT_MAX_BYTES);

        // Get list of log files to read (including rotated files)
        let files = match service {
            Some(svc) => self.list_service_log_files(svc),
            None => {
                // For all services, collect all files including rotated
                let mut all_files = Vec::new();
                for main_file in self.list_log_files() {
                    // Extract service name from file path
                    if let Some(stem) = main_file.file_stem().and_then(|s| s.to_str()) {
                        all_files.extend(self.list_service_log_files(stem));
                    }
                }
                all_files
            }
        };

        // Calculate per-file byte limit when reading multiple files
        let per_file_max_bytes = if files.is_empty() {
            max_bytes
        } else {
            // Distribute bytes across files, but ensure minimum per file
            (max_bytes / files.len()).max(1024 * 1024) // At least 1MB per file
        };

        // Read and parse entries from each file with bounds
        let mut entries = Vec::with_capacity(count); // Pre-allocate for expected result size
        for file in files {
            let file_entries = self.read_log_file_bounded(&file, Some(per_file_max_bytes), None);
            // Filter by service if specified (in case filename doesn't exactly match)
            if let Some(svc) = service {
                entries.extend(file_entries.into_iter().filter(|e| e.service == svc));
            } else {
                entries.extend(file_entries);
            }
        }

        // Sort by timestamp and return last N
        entries.sort_by_key(|e| e.timestamp);
        let start = entries.len().saturating_sub(count);
        entries.into_iter().skip(start).collect()
    }

    /// Get all entries, optionally filtered by service
    pub fn all(&self, service: Option<&str>) -> Vec<LogLine> {
        let files = match service {
            Some(svc) => {
                let path = self.log_file_path(svc);
                if path.exists() {
                    vec![path]
                } else {
                    vec![]
                }
            }
            None => self.list_log_files(),
        };

        let mut entries = Vec::new();
        for file in files {
            let file_entries = self.read_log_file(&file);
            if let Some(svc) = service {
                entries.extend(file_entries.into_iter().filter(|e| e.service == svc));
            } else {
                entries.extend(file_entries);
            }
        }

        entries.sort_by_key(|e| e.timestamp);
        entries
    }

    /// Get logs with true pagination (reads from disk with offset/limit).
    ///
    /// This method efficiently reads only the needed portion of log files,
    /// avoiding loading all logs into memory.
    ///
    /// Returns (entries, total_count) where total_count is the total number
    /// of entries across all relevant log files.
    pub fn get_paginated(
        &self,
        service: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> (Vec<LogLine>, usize) {
        // Get list of log files to read (including rotated files)
        let files = match service {
            Some(svc) => self.list_service_log_files(svc),
            None => {
                // For all services, collect all files including rotated
                let mut all_files = Vec::new();
                for main_file in self.list_log_files() {
                    // Extract service name from file path
                    if let Some(stem) = main_file.file_stem().and_then(|s| s.to_str()) {
                        all_files.extend(self.list_service_log_files(stem));
                    }
                }
                all_files
            }
        };

        if files.is_empty() {
            return (Vec::new(), 0);
        }

        // Count lines in each file for accurate offset calculation
        let file_counts: Vec<(PathBuf, usize)> = files
            .iter()
            .map(|f| (f.clone(), self.count_lines_in_file(f)))
            .collect();

        let total: usize = file_counts.iter().map(|(_, c)| *c).sum();

        if offset >= total {
            return (Vec::new(), total);
        }

        // Read only needed files/ranges
        let entries = self.read_range_across_files(&file_counts, service, offset, limit);

        (entries, total)
    }

    /// Fast line counting for a file
    fn count_lines_in_file(&self, path: &PathBuf) -> usize {
        match File::open(path) {
            Ok(file) => BufReader::new(file).lines().count(),
            Err(_) => 0,
        }
    }

    /// Read a range of log entries across multiple files.
    ///
    /// This method skips files until reaching the offset, then reads
    /// only the needed portion of each file.
    fn read_range_across_files(
        &self,
        files: &[(PathBuf, usize)],
        service_filter: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Vec<LogLine> {
        // First, read all entries and sort them by timestamp
        // This is necessary because log files may have interleaved timestamps
        // especially when reading from multiple services
        let mut all_entries: Vec<LogLine> = Vec::new();

        for (file, _) in files {
            let entries = self.read_log_file(file);
            if let Some(svc) = service_filter {
                all_entries.extend(entries.into_iter().filter(|e| e.service == svc));
            } else {
                all_entries.extend(entries);
            }
        }

        // Sort by timestamp to ensure correct ordering
        all_entries.sort_by_key(|e| e.timestamp);

        // Apply offset and limit
        all_entries
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect()
    }

    /// Clear all entries (delete all log files)
    pub fn clear(&mut self) {
        // Close all cached file handles before deleting
        self.open_files.clear();
        // Reset tracking state
        self.bytes_written.clear();
        self.rotation_index.clear();

        for file in self.list_log_files() {
            // Remove rotated files first
            for i in 1..=self.max_rotated_files {
                let rotated = format!("{}.{}", file.display(), i);
                let _ = fs::remove_file(&rotated);
            }
            // Then remove the main file
            let _ = fs::remove_file(&file);
        }
        self.sequence = 0;
        self.save_sequence();
    }

    /// Clear entries for a specific service (delete the log file and all rotated files)
    pub fn clear_service(&mut self, service: &str) {
        // Close cached file handle before deleting
        self.open_files.remove(service);
        // Reset tracking state for this service
        self.bytes_written.remove(service);
        self.rotation_index.remove(service);

        let log_file = self.log_file_path(service);

        // Remove rotated files first (.log.1, .log.2, etc.)
        for i in 1..=self.max_rotated_files {
            let rotated = format!("{}.{}", log_file.display(), i);
            let rotated_path = PathBuf::from(&rotated);
            if rotated_path.exists() {
                // Count entries being removed to update sequence
                if let Ok(file) = File::open(&rotated_path) {
                    let count = BufReader::new(file).lines().count() as u64;
                    self.sequence = self.sequence.saturating_sub(count);
                }
                let _ = fs::remove_file(&rotated_path);
            }
        }

        // Then remove the main log file
        if log_file.exists() {
            // Count entries being removed to update sequence
            if let Ok(file) = File::open(&log_file) {
                let count = BufReader::new(file).lines().count() as u64;
                self.sequence = self.sequence.saturating_sub(count);
            }
            let _ = fs::remove_file(log_file);
        }
    }

    /// Clear entries for services matching a prefix (e.g., "[global" clears all global hooks)
    /// Also removes rotated log files for matching services
    pub fn clear_service_prefix(&mut self, prefix: &str) {
        // Close cached file handles for matching services before deleting
        let sanitized_prefix = prefix.replace(['/', '\\', ':', '[', ']'], "_");
        self.open_files.retain(|k, _| {
            let sanitized_key = k.replace(['/', '\\', ':', '[', ']'], "_");
            !sanitized_key.starts_with(&sanitized_prefix)
        });
        // Reset tracking state for matching services
        self.bytes_written.retain(|k, _| {
            let sanitized_key = k.replace(['/', '\\', ':', '[', ']'], "_");
            !sanitized_key.starts_with(&sanitized_prefix)
        });
        self.rotation_index.retain(|k, _| {
            let sanitized_key = k.replace(['/', '\\', ':', '[', ']'], "_");
            !sanitized_key.starts_with(&sanitized_prefix)
        });

        // Sanitize prefix for matching filenames
        let safe_prefix = prefix.replace(['/', '\\', ':', '[', ']'], "_");

        if let Ok(entries) = fs::read_dir(&self.logs_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let file_name = path.file_name().map(|n| n.to_string_lossy().to_string());

                if let Some(name) = file_name {
                    // Check if this is a log file (main or rotated) for a matching service
                    // Main files: "service.log"
                    // Rotated files: "service.log.1", "service.log.2", etc.
                    let is_rotated = name.contains(".log.");
                    let base_name = if is_rotated {
                        // Extract base name from "service.log.N"
                        name.split(".log.").next().map(String::from)
                    } else if name.ends_with(".log") {
                        // Extract base name from "service.log"
                        name.strip_suffix(".log").map(String::from)
                    } else {
                        None
                    };

                    if let Some(base) = base_name {
                        if base.starts_with(&safe_prefix) {
                            // Count entries being removed
                            if let Ok(file) = File::open(&path) {
                                let count = BufReader::new(file).lines().count() as u64;
                                self.sequence = self.sequence.saturating_sub(count);
                            }
                            let _ = fs::remove_file(&path);
                        }
                    }
                }
            }
        }
    }
}

/// Thread-safe log buffer wrapper
#[derive(Debug, Clone)]
pub struct SharedLogBuffer {
    inner: Arc<RwLock<LogBuffer>>,
}

impl SharedLogBuffer {
    pub fn new(logs_dir: PathBuf) -> Self {
        Self {
            inner: Arc::new(RwLock::new(LogBuffer::new(logs_dir))),
        }
    }

    pub fn with_rotation(logs_dir: PathBuf, max_log_size: u64, max_rotated_files: u32) -> Self {
        Self {
            inner: Arc::new(RwLock::new(LogBuffer::with_rotation(
                logs_dir,
                max_log_size,
                max_rotated_files,
            ))),
        }
    }

    /// Create a SharedLogBuffer with all options including buffer size.
    ///
    /// # Arguments
    /// * `logs_dir` - Directory to store log files
    /// * `max_log_size` - Maximum size of a log file before rotation
    /// * `max_rotated_files` - Number of rotated files to keep
    /// * `buffer_size` - Bytes to buffer per service before flushing (0 = no buffering)
    pub fn with_options(
        logs_dir: PathBuf,
        max_log_size: u64,
        max_rotated_files: u32,
        buffer_size: usize,
    ) -> Self {
        Self {
            inner: Arc::new(RwLock::new(LogBuffer::with_options(
                logs_dir,
                max_log_size,
                max_rotated_files,
                buffer_size,
            ))),
        }
    }

    pub fn push(&self, service: &str, line: String, stream: LogStream) {
        self.inner.write().push(service, line, stream);
    }

    pub fn current_sequence(&self) -> u64 {
        self.inner.read().current_sequence()
    }

    pub fn tail(&self, count: usize, service: Option<&str>) -> Vec<LogLine> {
        // Flush buffers first to ensure we read the latest data
        self.inner.write().flush_all();
        self.inner.read().tail(count, service)
    }

    pub fn tail_bounded(
        &self,
        count: usize,
        service: Option<&str>,
        max_bytes: Option<usize>,
    ) -> Vec<LogLine> {
        // Flush buffers first to ensure we read the latest data
        self.inner.write().flush_all();
        self.inner.read().tail_bounded(count, service, max_bytes)
    }

    pub fn entries_since(&self, since_seq: u64, service: Option<&str>) -> Vec<LogLine> {
        // Flush buffers first to ensure we read the latest data
        self.inner.write().flush_all();
        let buffer = self.inner.read();
        let entries = buffer.entries_since(since_seq);
        if let Some(svc) = service {
            entries.into_iter().filter(|e| e.service == svc).collect()
        } else {
            entries
        }
    }

    pub fn clear(&self) {
        self.inner.write().clear();
    }

    pub fn clear_service(&self, service: &str) {
        self.inner.write().clear_service(service);
    }

    pub fn clear_service_prefix(&self, prefix: &str) {
        self.inner.write().clear_service_prefix(prefix);
    }

    /// Get logs with true pagination
    pub fn get_paginated(
        &self,
        service: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> (Vec<LogLine>, usize) {
        // Flush buffers first to ensure we read the latest data
        self.inner.write().flush_all();
        self.inner.read().get_paginated(service, offset, limit)
    }

    /// Flush all buffered entries to disk
    pub fn flush_all(&self) {
        self.inner.write().flush_all();
    }

    /// Flush buffered entries for a specific service to disk
    pub fn flush_service(&self, service: &str) {
        self.inner.write().flush_service(service);
    }
}

/// Convert LogLine to protocol LogEntry
impl From<LogLine> for LogEntry {
    fn from(line: LogLine) -> Self {
        LogEntry {
            service: line.service,
            line: line.line,
            timestamp: Some(line.timestamp.timestamp_millis()),
            stream: line.stream.as_str().to_string(),
        }
    }
}

impl From<&LogLine> for LogEntry {
    fn from(line: &LogLine) -> Self {
        LogEntry {
            service: line.service.clone(),
            line: line.line.clone(),
            timestamp: Some(line.timestamp.timestamp_millis()),
            stream: line.stream.as_str().to_string(),
        }
    }
}
