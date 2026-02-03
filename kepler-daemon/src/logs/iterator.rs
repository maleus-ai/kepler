//! Forward log iterator using min-heap (oldest entries first)

use std::collections::BinaryHeap;
use std::fs::File;
use std::io::{BufRead, BufReader};
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
}

impl MergedLogIterator {
    /// Create a new merged iterator from a list of log files
    pub fn new(files: Vec<(PathBuf, String, LogStream)>) -> Self {
        let mut readers = Vec::with_capacity(files.len());
        let mut services: Vec<Arc<str>> = Vec::with_capacity(files.len());
        let mut streams = Vec::with_capacity(files.len());
        let heap = BinaryHeap::new();

        for (path, service, stream) in files.into_iter() {
            if let Ok(file) = File::open(&path) {
                let reader = BufReader::new(file);
                readers.push(reader);
                services.push(Arc::from(service.as_str()));
                streams.push(stream);
            } else {
                continue;
            }
        }

        // Initialize heap with first entry from each reader
        let mut iter = Self {
            readers,
            services,
            streams,
            heap,
            line_buf: String::with_capacity(256),
        };

        for i in 0..iter.readers.len() {
            iter.read_next_into_heap(i);
        }

        iter
    }

    fn read_next_into_heap(&mut self, source_idx: usize) {
        self.line_buf.clear();
        if self.readers[source_idx].read_line(&mut self.line_buf).is_ok() && !self.line_buf.is_empty() {
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
            }
        }
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
