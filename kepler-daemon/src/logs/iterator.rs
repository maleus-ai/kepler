//! Forward log iterator using min-heap (oldest entries first)

use std::collections::{BinaryHeap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;

use super::{LogLine, LogReader, LogStream};

/// Entry in the priority queue for merge-sort iteration
#[derive(Debug)]
struct HeapEntry {
    timestamp: i64,
    log_line: LogLine,
    source_idx: usize,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.timestamp == other.timestamp
    }
}

impl Eq for HeapEntry {}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reverse ordering for min-heap (smallest timestamp first)
        other.timestamp.cmp(&self.timestamp)
    }
}

/// Iterator that merges multiple log sources chronologically (oldest first)
pub struct MergedLogIterator {
    readers: Vec<BufReader<File>>,
    /// Service names as Arc<str> for cheap cloning
    services: Vec<Arc<str>>,
    streams: Vec<LogStream>,
    heap: BinaryHeap<HeapEntry>,
    /// Reusable line buffer to avoid allocation per read
    line_buf: String,
    /// File paths (needed for position export)
    paths: Vec<PathBuf>,
    /// Byte offset before the last read_next_into_heap() per source
    pre_read_positions: Vec<u64>,
}

impl MergedLogIterator {
    /// Create a new merged iterator from a list of log files
    pub fn new(files: Vec<(PathBuf, String, LogStream)>) -> Self {
        let capacity = files.len();
        let mut readers = Vec::with_capacity(capacity);
        let mut services: Vec<Arc<str>> = Vec::with_capacity(capacity);
        let mut streams = Vec::with_capacity(capacity);
        let mut paths = Vec::with_capacity(capacity);
        let heap = BinaryHeap::new();

        for (path, service, stream) in files.into_iter() {
            if let Ok(file) = File::open(&path) {
                let reader = BufReader::new(file);
                readers.push(reader);
                services.push(Arc::from(service.as_str()));
                streams.push(stream);
                paths.push(path);
            } else {
                continue;
            }
        }

        let source_count = readers.len();
        let pre_read_positions = vec![0u64; source_count];

        // Initialize heap with first entry from each reader
        let mut iter = Self {
            readers,
            services,
            streams,
            heap,
            line_buf: String::with_capacity(256),
            paths,
            pre_read_positions,
        };

        for i in 0..iter.readers.len() {
            iter.read_next_into_heap(i);
        }

        iter
    }

    /// Create a merged iterator that resumes from saved byte positions.
    ///
    /// Each file is seeked to the position from `positions` (or 0 if not present).
    /// If the saved position exceeds the current file size (truncation), resets to 0.
    pub fn with_positions(
        files: Vec<(PathBuf, String, LogStream)>,
        positions: &HashMap<PathBuf, u64>,
    ) -> Self {
        let capacity = files.len();
        let mut readers = Vec::with_capacity(capacity);
        let mut services: Vec<Arc<str>> = Vec::with_capacity(capacity);
        let mut streams = Vec::with_capacity(capacity);
        let mut paths = Vec::with_capacity(capacity);
        let mut pre_read_positions = Vec::with_capacity(capacity);
        let heap = BinaryHeap::new();

        for (path, service, stream) in files.into_iter() {
            let file = match File::open(&path) {
                Ok(f) => f,
                Err(_) => continue,
            };

            let mut seek_pos = positions.get(&path).copied().unwrap_or(0);

            // Handle truncation: if saved position > current file size, reset to 0
            if let Ok(metadata) = file.metadata() {
                if seek_pos > metadata.len() {
                    seek_pos = 0;
                }
            }

            let mut reader = BufReader::new(file);
            if seek_pos > 0 {
                if reader.seek(SeekFrom::Start(seek_pos)).is_err() {
                    continue;
                }
            }

            readers.push(reader);
            services.push(Arc::from(service.as_str()));
            streams.push(stream);
            paths.push(path);
            pre_read_positions.push(seek_pos);
        }

        let mut iter = Self {
            readers,
            services,
            streams,
            heap,
            line_buf: String::with_capacity(256),
            paths,
            pre_read_positions,
        };

        for i in 0..iter.readers.len() {
            iter.read_next_into_heap(i);
        }

        iter
    }

    fn read_next_into_heap(&mut self, source_idx: usize) {
        loop {
            // Save position before reading
            if let Ok(pos) = self.readers[source_idx].stream_position() {
                self.pre_read_positions[source_idx] = pos;
            }

            self.line_buf.clear();
            match self.readers[source_idx].read_line(&mut self.line_buf) {
                Ok(0) => return,  // EOF
                Ok(_) => {}
                Err(_) => return, // I/O error
            }

            if let Some(log_line) = LogReader::parse_log_line_arc(
                &self.line_buf,
                &self.services[source_idx],
                self.streams[source_idx],
            ) {
                self.heap.push(HeapEntry {
                    timestamp: log_line.timestamp.timestamp_millis(),
                    log_line,
                    source_idx,
                });
                return;
            }
            // Line failed to parse — skip it and try the next one
        }
    }

    /// Returns true if there are un-yielded entries remaining in the heap.
    pub fn has_more(&self) -> bool {
        !self.heap.is_empty()
    }

    /// Export current positions for each source file.
    ///
    /// For sources with an entry still in the heap (un-yielded): returns the
    /// position before that entry was read (so it will be re-read next time).
    /// For exhausted sources: returns the current stream position.
    pub fn export_positions(&mut self) -> HashMap<PathBuf, u64> {
        // Collect which source indices still have entries in the heap
        let mut in_heap = vec![false; self.readers.len()];
        for entry in self.heap.iter() {
            in_heap[entry.source_idx] = true;
        }

        let mut positions = HashMap::new();
        for i in 0..self.readers.len() {
            let pos = if in_heap[i] {
                // This source has an un-yielded entry — return position before it
                self.pre_read_positions[i]
            } else {
                // Source exhausted — return current position (EOF)
                self.readers[i].stream_position().unwrap_or(self.pre_read_positions[i])
            };
            positions.insert(self.paths[i].clone(), pos);
        }

        positions
    }
}

impl Iterator for MergedLogIterator {
    type Item = LogLine;

    fn next(&mut self) -> Option<Self::Item> {
        let entry = self.heap.pop()?;
        let source_idx = entry.source_idx;

        // Read next line from this source
        self.read_next_into_heap(source_idx);

        Some(entry.log_line)
    }
}
