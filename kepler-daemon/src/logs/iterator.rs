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
    log_line: LogLine,
    source_idx: usize,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.log_line.timestamp == other.log_line.timestamp
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
        other.log_line.timestamp.cmp(&self.log_line.timestamp)
    }
}

/// Iterator that merges multiple log sources chronologically (oldest first)
pub struct MergedLogIterator {
    readers: Vec<BufReader<File>>,
    /// Service names as Arc<str> for cheap cloning
    services: Vec<Arc<str>>,
    streams: Vec<LogStream>,
    heap: BinaryHeap<HeapEntry>,
}

impl MergedLogIterator {
    /// Create a new merged iterator from a list of log files
    pub fn new(files: Vec<(PathBuf, String, LogStream)>) -> Self {
        let capacity = files.len();
        let mut readers = Vec::with_capacity(capacity);
        let mut services: Vec<Arc<str>> = Vec::with_capacity(capacity);
        let mut streams = Vec::with_capacity(capacity);
        let heap = BinaryHeap::new();

        for (path, service, stream) in files.into_iter() {
            if let Ok(file) = File::open(&path) {
                let reader = BufReader::new(file);
                readers.push(reader);
                services.push(Arc::from(service.as_str()));
                streams.push(stream);
            }
        }

        // Initialize heap with first entry from each reader
        let mut iter = Self {
            readers,
            services,
            streams,
            heap,
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
        let heap = BinaryHeap::new();

        for (path, service, stream) in files.into_iter() {
            let file = match File::open(&path) {
                Ok(f) => f,
                Err(_) => continue,
            };

            let mut seek_pos = positions.get(&path).copied().unwrap_or(0);

            // Handle truncation: if saved position > current file size, reset to 0
            if let Ok(metadata) = file.metadata()
                && seek_pos > metadata.len()
            {
                seek_pos = 0;
            }

            let mut reader = BufReader::new(file);
            if seek_pos > 0
                && reader.seek(SeekFrom::Start(seek_pos)).is_err()
            {
                continue;
            }

            readers.push(reader);
            services.push(Arc::from(service.as_str()));
            streams.push(stream);
        }

        let mut iter = Self {
            readers,
            services,
            streams,
            heap,
        };

        for i in 0..iter.readers.len() {
            iter.read_next_into_heap(i);
        }

        iter
    }

    fn read_next_into_heap(&mut self, source_idx: usize) {
        loop {
            let mut line = String::new();
            match self.readers[source_idx].read_line(&mut line) {
                Ok(0) => return,  // EOF
                Ok(_) => {}
                Err(_) => return, // I/O error
            }

            if let Some(log_line) = LogReader::parse_log_line_inplace(
                line,
                &self.services[source_idx],
                self.streams[source_idx],
            ) {
                self.heap.push(HeapEntry {
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

    /// Retry reading from all sources that previously hit EOF.
    ///
    /// Useful for follow mode where files may grow after the iterator
    /// was created. Sources that already have an entry in the heap are skipped.
    pub fn retry_eof_sources(&mut self) {
        let mut in_heap = vec![false; self.readers.len()];
        for entry in self.heap.iter() {
            in_heap[entry.source_idx] = true;
        }
        for (i, is_in_heap) in in_heap.iter().enumerate() {
            if !is_in_heap {
                self.read_next_into_heap(i);
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
