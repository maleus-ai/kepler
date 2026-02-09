//! Reverse log reading - reads files backwards for efficient tail operations

use std::collections::BinaryHeap;
use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;

use super::{LogLine, LogReader, LogStream};

// ============================================================================
// Reverse Line Reader - reads lines from end of file backwards
// ============================================================================

/// Reads lines from a file in reverse order (last line first).
/// Uses chunked reading from the end of the file for efficiency.
pub struct ReverseLineReader {
    file: File,
    /// Current position in file (we read backwards from here)
    pos: u64,
    /// Buffer for reading chunks
    chunk_buf: Vec<u8>,
    /// Lines extracted from current chunk (in reverse order)
    pending_lines: Vec<String>,
    /// Leftover bytes from previous chunk (partial line at chunk boundary)
    leftover: Vec<u8>,
    /// Chunk size for reading
    chunk_size: usize,
}

impl ReverseLineReader {
    const DEFAULT_CHUNK_SIZE: usize = 64 * 1024; // 64KB chunks

    pub fn new(file: File) -> std::io::Result<Self> {
        let file_size = file.metadata()?.len();
        Ok(Self {
            file,
            pos: file_size,
            chunk_buf: Vec::with_capacity(Self::DEFAULT_CHUNK_SIZE),
            pending_lines: Vec::new(),
            leftover: Vec::new(),
            chunk_size: Self::DEFAULT_CHUNK_SIZE,
        })
    }

    /// Read the next line (going backwards through the file)
    pub fn next_line(&mut self) -> std::io::Result<Option<String>> {
        loop {
            // Return pending lines first
            if let Some(line) = self.pending_lines.pop() {
                return Ok(Some(line));
            }

            // No more pending lines, try to read another chunk
            if self.pos == 0 && self.leftover.is_empty() {
                return Ok(None); // End of file (beginning reached)
            }

            self.read_chunk_backwards()?;
        }
    }

    fn read_chunk_backwards(&mut self) -> std::io::Result<()> {
        use std::io::{Read, Seek, SeekFrom};

        // Calculate how much to read
        let read_size = std::cmp::min(self.pos as usize, self.chunk_size);
        if read_size == 0 && self.leftover.is_empty() {
            return Ok(());
        }

        // Seek and read
        let new_pos = self.pos - read_size as u64;
        self.file.seek(SeekFrom::Start(new_pos))?;

        self.chunk_buf.clear();
        self.chunk_buf.resize(read_size, 0);
        self.file.read_exact(&mut self.chunk_buf)?;

        self.pos = new_pos;

        // Combine with leftover from previous chunk
        if !self.leftover.is_empty() {
            self.chunk_buf.append(&mut self.leftover);
        }

        // Split into lines
        self.extract_lines_from_chunk();

        Ok(())
    }

    fn extract_lines_from_chunk(&mut self) {
        // Find all newlines and extract lines in reverse order
        // Uses memchr for SIMD-optimized newline scanning (like uutils/coreutils)
        let mut lines: Vec<String> = Vec::new();
        let mut end = self.chunk_buf.len();

        // Handle trailing newline
        if end > 0 && self.chunk_buf[end - 1] == b'\n' {
            end -= 1;
        }
        // Handle trailing \r\n
        if end > 0 && self.chunk_buf[end - 1] == b'\r' {
            end -= 1;
        }

        // Use memrchr to find newlines from the end (SIMD-optimized)
        let mut search_end = end;
        while let Some(newline_pos) = memchr::memrchr(b'\n', &self.chunk_buf[..search_end]) {
            // Found a complete line: content is from newline_pos+1 to end
            if newline_pos + 1 < end
                && let Ok(line) = std::str::from_utf8(&self.chunk_buf[newline_pos + 1..end]) {
                    let line = line.trim_end_matches('\r');
                    if !line.is_empty() {
                        lines.push(line.to_string());
                    }
                }
            end = newline_pos;
            search_end = newline_pos;
        }

        // Whatever remains at the start is either:
        // - A complete line (if pos == 0, i.e., beginning of file)
        // - A partial line (leftover for next chunk)
        if self.pos == 0 {
            // Beginning of file - this is a complete line
            if end > 0
                && let Ok(line) = std::str::from_utf8(&self.chunk_buf[0..end]) {
                    let line = line.trim_end_matches('\r');
                    if !line.is_empty() {
                        lines.push(line.to_string());
                    }
                }
        } else {
            // Partial line - save for next chunk
            self.leftover = self.chunk_buf[0..end].to_vec();
        }

        // Lines were extracted in reverse order (last line in chunk first).
        // Reverse so that pop() returns the last line first.
        lines.reverse();
        self.pending_lines = lines;
    }
}

// ============================================================================
// Reverse Merged Log Iterator - max-heap for newest entries first
// ============================================================================

/// Entry in the max-heap (newest timestamp first)
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
        // Normal ordering for max-heap (largest timestamp first)
        self.log_line.timestamp.cmp(&other.log_line.timestamp)
    }
}

/// Source for reverse reading - handles both current file and rotated files
struct ReverseLogSource {
    reader: ReverseLineReader,
    service: Arc<str>,
    stream: LogStream,
    /// Remaining rotated files to read (sorted newest to oldest)
    remaining_files: Vec<PathBuf>,
}

/// Iterator that merges multiple log sources in reverse chronological order (newest first).
/// Efficiently reads from the end of files, stopping after N entries.
pub struct ReverseMergedLogIterator {
    sources: Vec<Option<ReverseLogSource>>,
    heap: BinaryHeap<HeapEntry>,
}

impl ReverseMergedLogIterator {
    /// Create a new reverse iterator from log files.
    /// Files should be ordered from newest to oldest for each service.
    pub fn new(files_by_service: Vec<(Vec<PathBuf>, String, LogStream)>) -> Self {
        let mut sources: Vec<Option<ReverseLogSource>> = Vec::new();
        let mut heap = BinaryHeap::new();

        for (mut files, service, stream) in files_by_service {
            // files are ordered oldest to newest, reverse for newest first
            files.reverse();

            if files.is_empty() {
                continue;
            }

            // Open the first (newest) file
            let first_file = files.remove(0);
            if let Ok(file) = File::open(&first_file) {
                if let Ok(reader) = ReverseLineReader::new(file) {
                    let source_idx = sources.len();
                    let service_arc: Arc<str> = Arc::from(service.as_str());

                    let mut source = ReverseLogSource {
                        reader,
                        service: service_arc,
                        stream,
                        remaining_files: files,
                    };

                    // Read first line from this source
                    if let Some(entry) = Self::read_next_entry(&mut source, source_idx) {
                        heap.push(entry);
                        sources.push(Some(source));
                    } else {
                        sources.push(None);
                    }
                } else {
                    sources.push(None);
                }
            } else {
                sources.push(None);
            }
        }

        Self { sources, heap }
    }

    fn read_next_entry(source: &mut ReverseLogSource, source_idx: usize) -> Option<HeapEntry> {
        loop {
            // Try to read from current file
            match source.reader.next_line() {
                Ok(Some(line)) => {
                    if let Some(log_line) = LogReader::parse_log_line_arc(
                        &line,
                        &source.service,
                        source.stream,
                    ) {
                        return Some(HeapEntry {
                            log_line,
                            source_idx,
                        });
                    }
                    // Invalid line, try next
                    continue;
                }
                Ok(None) => {
                    // Current file exhausted, try next rotated file
                    if let Some(next_file) = source.remaining_files.first().cloned() {
                        source.remaining_files.remove(0);
                        if let Ok(file) = File::open(&next_file)
                            && let Ok(reader) = ReverseLineReader::new(file) {
                                source.reader = reader;
                                continue; // Try reading from new file
                            }
                    }
                    return None; // No more files
                }
                Err(_) => return None,
            }
        }
    }
}

impl Iterator for ReverseMergedLogIterator {
    type Item = LogLine;

    fn next(&mut self) -> Option<Self::Item> {
        let entry = self.heap.pop()?;
        let source_idx = entry.source_idx;

        // Read next line from this source
        if let Some(ref mut source) = self.sources[source_idx]
            && let Some(next_entry) = Self::read_next_entry(source, source_idx) {
                self.heap.push(next_entry);
            }

        Some(entry.log_line)
    }
}
