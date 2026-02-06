use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use kepler_daemon::logs::{BufferedLogWriter, LogReader, LogStream};
use std::sync::Arc;
use std::thread;
use tempfile::TempDir;

// ============================================================================
// BufferedLogWriter Benchmarks (New Architecture - No Shared State)
// ============================================================================

/// Benchmark single-threaded log write performance with different buffer sizes
fn bench_log_writes(c: &mut Criterion) {
    let mut group = c.benchmark_group("log_writes");

    // Test with 4000 log lines (realistic workload)
    let num_lines = 4000;
    group.throughput(Throughput::Elements(num_lines as u64));

    // Buffer sizes to test: 0 (sync), 4KB, 16KB, 64KB
    let buffer_sizes = [0, 4 * 1024, 16 * 1024, 64 * 1024];

    for buffer_size in buffer_sizes {
        let label = if buffer_size == 0 {
            "sync".to_string()
        } else {
            format!("{}KB", buffer_size / 1024)
        };

        group.bench_with_input(
            BenchmarkId::new("write_lines", &label),
            &buffer_size,
            |b, &buffer_size| {
                b.iter_with_setup(
                    || {
                        let temp_dir = TempDir::new().unwrap();
                        let logs_dir = temp_dir.path().to_path_buf();
                        std::fs::create_dir_all(&logs_dir).unwrap();
                        (temp_dir, logs_dir)
                    },
                    |(temp_dir, logs_dir)| {
                        let mut writer = BufferedLogWriter::new(
                            &logs_dir,
                            "test-service",
                            LogStream::Stdout,
                            Some(10 * 1024 * 1024), // 10MB max log size (truncates when exceeded)
                            buffer_size,
                        );
                        for i in 0..num_lines {
                            writer.write(black_box(&format!(
                                "Log line number {} with some content to simulate realistic log output",
                                i
                            )));
                        }
                        // Flush via drop
                        drop(writer);
                        temp_dir
                    },
                );
            },
        );
    }

    group.finish();
}

/// Benchmark caller latency (time to return from write call)
fn bench_caller_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("caller_latency");

    // Buffer sizes to test
    let buffer_sizes = [0, 16 * 1024, 64 * 1024];

    for buffer_size in buffer_sizes {
        let label = if buffer_size == 0 {
            "sync".to_string()
        } else {
            format!("{}KB", buffer_size / 1024)
        };

        group.bench_with_input(
            BenchmarkId::new("single_write", &label),
            &buffer_size,
            |b, &buffer_size| {
                let temp_dir = TempDir::new().unwrap();
                let logs_dir = temp_dir.path().to_path_buf();
                std::fs::create_dir_all(&logs_dir).unwrap();
                let mut writer = BufferedLogWriter::new(
                    &logs_dir,
                    "test-service",
                    LogStream::Stdout,
                    Some(10 * 1024 * 1024), // 10MB max (truncates when exceeded)
                    buffer_size,
                );

                let mut i = 0u64;
                b.iter(|| {
                    writer.write(black_box(&format!("Log line {}", i)));
                    i += 1;
                });
            },
        );
    }

    group.finish();
}

/// Benchmark multiple independent writers (simulates per-task writers - no contention)
fn bench_independent_writers(c: &mut Criterion) {
    let mut group = c.benchmark_group("independent_writers");
    group.sample_size(50);

    // Test configurations: (num_services, lines_per_service)
    let configs = [
        (10, 2000),   // 10 services, 2k logs each = 20k total
        (20, 2000),   // 20 services, 2k logs each = 40k total
        (50, 2000),   // 50 services, 2k logs each = 100k total
    ];

    let buffer_sizes = [0, 16 * 1024];

    for (num_services, lines_per_service) in configs {
        let total_lines = num_services * lines_per_service;

        for buffer_size in buffer_sizes {
            let buffer_label = if buffer_size == 0 {
                "sync".to_string()
            } else {
                format!("{}KB", buffer_size / 1024)
            };

            let label = format!("{}svc_{}logs_{}", num_services, lines_per_service, buffer_label);

            group.throughput(Throughput::Elements(total_lines as u64));

            group.bench_with_input(
                BenchmarkId::new("services", &label),
                &(num_services, lines_per_service, buffer_size),
                |b, &(num_services, lines_per_service, buffer_size)| {
                    b.iter_with_setup(
                        || {
                            let temp_dir = TempDir::new().unwrap();
                            let logs_dir = temp_dir.path().to_path_buf();
                            std::fs::create_dir_all(&logs_dir).unwrap();
                            let service_names: Vec<String> = (0..num_services)
                                .map(|i| format!("service-{:03}", i))
                                .collect();
                            (temp_dir, logs_dir, service_names, buffer_size)
                        },
                        |(temp_dir, logs_dir, service_names, buffer_size)| {
                            // Each service gets its own writer - no shared state
                            for service_name in &service_names {
                                let mut writer = BufferedLogWriter::new(
                                    &logs_dir,
                                    service_name,
                                    LogStream::Stdout,
                                    Some(50 * 1024 * 1024),
                                    buffer_size,
                                );
                                for line_id in 0..lines_per_service {
                                    writer.write(black_box(&format!(
                                        "[{}] Log entry {} - Processing request with data payload",
                                        service_name, line_id
                                    )));
                                }
                            }
                            temp_dir
                        },
                    );
                },
            );
        }
    }

    group.finish();
}

/// Benchmark concurrent writes from multiple threads (each thread has its own writer)
fn bench_concurrent_writers(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_writers");
    group.sample_size(30);

    let thread_counts = [2, 4, 8, 16];
    let lines_per_thread = 2000;

    for num_threads in thread_counts {
        let total_lines = num_threads * lines_per_thread;
        group.throughput(Throughput::Elements(total_lines as u64));

        for buffer_size in [0, 16 * 1024] {
            let buffer_label = if buffer_size == 0 {
                "sync".to_string()
            } else {
                format!("{}KB", buffer_size / 1024)
            };

            let label = format!("{}threads_{}", num_threads, buffer_label);

            group.bench_with_input(
                BenchmarkId::new("parallel", &label),
                &(num_threads, buffer_size),
                |b, &(num_threads, buffer_size)| {
                    b.iter_with_setup(
                        || {
                            let temp_dir = TempDir::new().unwrap();
                            let logs_dir = Arc::new(temp_dir.path().to_path_buf());
                            std::fs::create_dir_all(&*logs_dir).unwrap();
                            (temp_dir, logs_dir)
                        },
                        |(temp_dir, logs_dir)| {
                            let handles: Vec<_> = (0..num_threads)
                                .map(|thread_id| {
                                    let logs_dir = Arc::clone(&logs_dir);
                                    thread::spawn(move || {
                                        // Each thread gets its own BufferedLogWriter
                                        // No shared state = no lock contention!
                                        let mut writer = BufferedLogWriter::new(
                                            &logs_dir,
                                            &format!("thread-{:02}", thread_id),
                                            LogStream::Stdout,
                                            Some(50 * 1024 * 1024),
                                            buffer_size,
                                        );
                                        for line_id in 0..lines_per_thread {
                                            writer.write(black_box(&format!(
                                                "Thread {} log line {} with payload data",
                                                thread_id, line_id
                                            )));
                                        }
                                    })
                                })
                                .collect();

                            for handle in handles {
                                handle.join().unwrap();
                            }
                            temp_dir
                        },
                    );
                },
            );
        }
    }

    group.finish();
}

/// Generate a log line of specific size
fn make_log_line(size: usize, id: usize) -> String {
    let prefix = format!("[{}] ", id);
    let padding_needed = size.saturating_sub(prefix.len());
    format!("{}{}", prefix, "x".repeat(padding_needed))
}

/// Benchmark different log line sizes
fn bench_log_line_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("log_line_sizes");
    group.sample_size(20);

    let num_threads = 4;
    let lines_per_thread = 2000;
    let total_lines = num_threads * lines_per_thread;

    // Test different log line sizes: 50B, 200B, 1KB, 4KB
    let line_sizes = [50, 200, 1024, 4096];

    for line_size in line_sizes {
        group.throughput(Throughput::Bytes((total_lines * line_size) as u64));

        for buffer_size in [0, 16 * 1024] {
            let buffer_label = if buffer_size == 0 { "sync" } else { "16KB" };
            let label = format!("{}_{}B", buffer_label, line_size);

            group.bench_function(&label, |b| {
                b.iter_with_setup(
                    || {
                        let temp_dir = TempDir::new().unwrap();
                        let logs_dir = Arc::new(temp_dir.path().to_path_buf());
                        std::fs::create_dir_all(&*logs_dir).unwrap();
                        (temp_dir, logs_dir)
                    },
                    |(temp_dir, logs_dir)| {
                        let handles: Vec<_> = (0..num_threads)
                            .map(|thread_id| {
                                let logs_dir = Arc::clone(&logs_dir);
                                thread::spawn(move || {
                                    let mut writer = BufferedLogWriter::new(
                                        &logs_dir,
                                        &format!("svc-{}", thread_id),
                                        LogStream::Stdout,
                                        Some(100 * 1024 * 1024),
                                        buffer_size,
                                    );
                                    for i in 0..lines_per_thread {
                                        writer.write(black_box(&make_log_line(line_size, i)));
                                    }
                                })
                            })
                            .collect();

                        for handle in handles {
                            handle.join().unwrap();
                        }
                        temp_dir
                    },
                );
            });
        }
    }

    group.finish();
}

/// Benchmark many services (simulates microservices environment)
fn bench_many_services(c: &mut Criterion) {
    let mut group = c.benchmark_group("many_services");
    group.sample_size(20);

    let configs = [
        (10, 500),   // 10 services, 500 logs each
        (50, 100),   // 50 services, 100 logs each
        (100, 50),   // 100 services, 50 logs each
        (200, 25),   // 200 services, 25 logs each
    ];

    for (num_services, logs_per_service) in configs {
        let total_logs = num_services * logs_per_service;
        group.throughput(Throughput::Elements(total_logs as u64));

        let label = format!("{}svc_{}logs", num_services, logs_per_service);

        group.bench_function(&label, |b| {
            b.iter_with_setup(
                || {
                    let temp_dir = TempDir::new().unwrap();
                    let logs_dir = temp_dir.path().to_path_buf();
                    std::fs::create_dir_all(&logs_dir).unwrap();
                    let services: Vec<String> = (0..num_services)
                        .map(|i| format!("service-{:03}", i))
                        .collect();
                    (temp_dir, logs_dir, services)
                },
                |(temp_dir, logs_dir, services)| {
                    // Each service gets its own writer
                    for service in &services {
                        let mut writer = BufferedLogWriter::new(
                            &logs_dir,
                            service,
                            LogStream::Stdout,
                            Some(100 * 1024 * 1024),
                            16 * 1024,
                        );
                        for log_id in 0..logs_per_service {
                            writer.write(black_box(&format!("Log {} from {}", log_id, service)));
                        }
                    }
                    temp_dir
                },
            );
        });
    }

    group.finish();
}

/// Benchmark burst writes (simulates error spikes)
fn bench_burst_writes(c: &mut Criterion) {
    let mut group = c.benchmark_group("burst_writes");
    group.sample_size(20);

    let num_threads = 8;
    let burst_sizes = [1000, 5000, 10000, 20000];

    for burst_size in burst_sizes {
        let logs_per_thread = burst_size / num_threads;
        group.throughput(Throughput::Elements(burst_size as u64));

        group.bench_function(format!("burst_{}", burst_size), |b| {
            b.iter_with_setup(
                || {
                    let temp_dir = TempDir::new().unwrap();
                    let logs_dir = Arc::new(temp_dir.path().to_path_buf());
                    std::fs::create_dir_all(&*logs_dir).unwrap();
                    (temp_dir, logs_dir)
                },
                |(temp_dir, logs_dir)| {
                    let handles: Vec<_> = (0..num_threads)
                        .map(|thread_id| {
                            let logs_dir = Arc::clone(&logs_dir);
                            thread::spawn(move || {
                                let mut writer = BufferedLogWriter::new(
                                    &logs_dir,
                                    &format!("burst-svc-{}", thread_id),
                                    LogStream::Stderr,
                                    Some(100 * 1024 * 1024),
                                    16 * 1024,
                                );
                                for i in 0..logs_per_thread {
                                    writer.write(black_box(&format!(
                                        "BURST LOG {} - error trace data here",
                                        i
                                    )));
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                    temp_dir
                },
            );
        });
    }

    group.finish();
}

/// Benchmark log reading performance - single service
fn bench_log_reading(c: &mut Criterion) {
    let mut group = c.benchmark_group("log_reading");
    group.sample_size(20);

    // Number of lines to write before reading
    let num_lines = 10000;

    // Prepare test data
    let temp_dir = TempDir::new().unwrap();
    let logs_dir = temp_dir.path().to_path_buf();
    std::fs::create_dir_all(&logs_dir).unwrap();

    {
        let mut writer = BufferedLogWriter::new(
            &logs_dir,
            "read-test",
            LogStream::Stdout,
            Some(100 * 1024 * 1024),
            0, // Unbuffered for setup
        );
        for i in 0..num_lines {
            writer.write(&format!("Log line {} with some content", i));
        }
    }

    // Benchmark tail with different sizes
    for tail_size in [100, 1000, 5000, 10000] {
        group.throughput(Throughput::Elements(tail_size as u64));
        group.bench_function(format!("tail_{}", tail_size), |b| {
            let reader = LogReader::new(logs_dir.clone());
            b.iter(|| {
                let entries = black_box(reader.tail(tail_size, Some("read-test")));
                entries
            });
        });
    }

    // Benchmark pagination
    for page_size in [100, 500, 1000] {
        // 5 pages * page_size = total elements read
        group.throughput(Throughput::Elements((5 * page_size) as u64));
        group.bench_function(format!("paginate_page{}", page_size), |b| {
            let reader = LogReader::new(logs_dir.clone());
            b.iter(|| {
                // Read 5 pages
                for page in 0..5 {
                    let offset = page * page_size;
                    let (entries, has_more) = black_box(
                        reader.get_paginated(Some("read-test"), offset, page_size)
                    );
                    let _ = (entries, has_more);
                }
            });
        });
    }

    group.finish();
}

/// Benchmark reading from multiple services (merge-sort performance)
fn bench_multi_service_reading(c: &mut Criterion) {
    let mut group = c.benchmark_group("multi_service_reading");
    group.sample_size(20);

    // Test configurations: (num_services, lines_per_service)
    let configs = [
        (5, 2000),    // 5 services, 2k logs each = 10k total
        (10, 2000),   // 10 services, 2k logs each = 20k total
        (20, 2000),   // 20 services, 2k logs each = 40k total
        (50, 1000),   // 50 services, 1k logs each = 50k total
    ];

    for (num_services, lines_per_service) in configs {
        // Setup: write logs for all services
        let temp_dir = TempDir::new().unwrap();
        let logs_dir = temp_dir.path().to_path_buf();
        std::fs::create_dir_all(&logs_dir).unwrap();

        for svc_id in 0..num_services {
            let mut writer = BufferedLogWriter::new(
                &logs_dir,
                &format!("service-{:03}", svc_id),
                if svc_id % 2 == 0 { LogStream::Stdout } else { LogStream::Stderr },
                Some(100 * 1024 * 1024),
                0, // Unbuffered
            );
            for line_id in 0..lines_per_service {
                writer.write(&format!(
                    "Service {} log {} with content data",
                    svc_id, line_id
                ));
            }
        }

        let total_lines = num_services * lines_per_service;
        let label = format!("{}svc_{}logs", num_services, lines_per_service);

        // Benchmark reading all services (merge-sort)
        group.throughput(Throughput::Elements(1000));
        group.bench_function(format!("{}_tail_1000", label), |b| {
            let reader = LogReader::new(logs_dir.clone());
            b.iter(|| {
                let entries = black_box(reader.tail(1000, None));
                entries
            });
        });

        // Benchmark paginated read (full scan)
        group.throughput(Throughput::Elements(total_lines as u64));
        group.bench_function(format!("{}_paginate_all", label), |b| {
            let reader = LogReader::new(logs_dir.clone());
            b.iter(|| {
                let mut offset = 0;
                let page_size = 1000;
                loop {
                    let (entries, has_more) = reader.get_paginated(None, offset, page_size);
                    black_box(&entries);
                    offset += entries.len();
                    if !has_more {
                        break;
                    }
                }
            });
        });

        // Keep temp_dir alive by storing it
        std::mem::forget(temp_dir);
    }

    group.finish();
}

/// Benchmark reading with truncation (logs truncated when max size reached)
fn bench_truncated_log_reading(c: &mut Criterion) {
    let mut group = c.benchmark_group("truncated_log_reading");
    group.sample_size(20);

    // Setup: create logs with small max size to trigger truncation
    let temp_dir = TempDir::new().unwrap();
    let logs_dir = temp_dir.path().to_path_buf();
    std::fs::create_dir_all(&logs_dir).unwrap();

    let num_services = 5;
    let max_log_size = 50 * 1024; // 50KB - small to trigger truncation

    for svc_id in 0..num_services {
        let mut writer = BufferedLogWriter::new(
            &logs_dir,
            &format!("truncated-svc-{}", svc_id),
            LogStream::Stdout,
            Some(max_log_size as u64),
            0, // Unbuffered for immediate writes
        );
        // Write enough to cause truncation
        // ~100 bytes per line, 50KB max = ~500 lines before truncation
        // Writing 1000 lines will trigger truncation
        for line_id in 0..1000 {
            writer.write(&format!(
                "Truncated log {} - service {} - padding to fill up faster xxxxxxxxxxxxx",
                line_id, svc_id
            ));
        }
    }

    // Benchmark reading after truncation
    for tail_size in [100, 500, 1000, 2000] {
        group.bench_function(format!("tail_{}_truncated", tail_size), |b| {
            let reader = LogReader::new(logs_dir.clone());
            b.iter(|| {
                let entries = black_box(reader.tail(tail_size, None));
                entries
            });
        });
    }

    // Benchmark single service with truncated logs
    group.bench_function("tail_1000_single_truncated", |b| {
        let reader = LogReader::new(logs_dir.clone());
        b.iter(|| {
            let entries = black_box(reader.tail(1000, Some("truncated-svc-0")));
            entries
        });
    });

    // Benchmark paginated read
    group.bench_function("paginate_truncated", |b| {
        let reader = LogReader::new(logs_dir.clone());
        b.iter(|| {
            let mut offset = 0;
            let page_size = 500;
            for _ in 0..10 {
                let (entries, _has_more) = reader.get_paginated(None, offset, page_size);
                black_box(&entries);
                offset += page_size;
            }
        });
    });

    std::mem::forget(temp_dir);
    group.finish();
}

/// Benchmark bounded reading (memory-limited reads)
fn bench_bounded_reading(c: &mut Criterion) {
    let mut group = c.benchmark_group("bounded_reading");
    group.sample_size(20);

    // Setup: create large logs
    let temp_dir = TempDir::new().unwrap();
    let logs_dir = temp_dir.path().to_path_buf();
    std::fs::create_dir_all(&logs_dir).unwrap();

    // Create service with large log lines
    let mut writer = BufferedLogWriter::new(
        &logs_dir,
        "large-logs",
        LogStream::Stdout,
        Some(100 * 1024 * 1024),
        0, // Unbuffered
    );
    // ~1KB per line
    let large_line = "x".repeat(1000);
    for i in 0..10000 {
        writer.write(&format!("[{}] {}", i, large_line));
    }
    drop(writer);

    // Benchmark bounded reads with different byte limits
    let byte_limits = [
        10 * 1024,      // 10KB
        100 * 1024,     // 100KB
        1024 * 1024,    // 1MB
        10 * 1024 * 1024, // 10MB
    ];

    for byte_limit in byte_limits {
        let label = if byte_limit >= 1024 * 1024 {
            format!("{}MB", byte_limit / (1024 * 1024))
        } else {
            format!("{}KB", byte_limit / 1024)
        };

        group.throughput(Throughput::Bytes(byte_limit as u64));
        group.bench_function(format!("bounded_{}", label), |b| {
            let reader = LogReader::new(logs_dir.clone());
            b.iter(|| {
                let entries = black_box(reader.tail_bounded(
                    10000,
                    Some("large-logs"),
                    Some(byte_limit),
                ));
                entries
            });
        });
    }

    std::mem::forget(temp_dir);
    group.finish();
}

/// Benchmark concurrent read and write operations
fn bench_concurrent_read_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_read_write");
    group.sample_size(15);

    let num_writers = 4;
    let num_readers = 2;
    let writes_per_writer = 1000;
    let reads_per_reader = 100;

    group.throughput(Throughput::Elements(
        (num_writers * writes_per_writer + num_readers * reads_per_reader * 100) as u64
    ));

    group.bench_function("mixed_workload", |b| {
        b.iter_with_setup(
            || {
                let temp_dir = TempDir::new().unwrap();
                let logs_dir = Arc::new(temp_dir.path().to_path_buf());
                std::fs::create_dir_all(&*logs_dir).unwrap();

                // Pre-populate with some data for readers
                for svc_id in 0..num_writers {
                    let mut writer = BufferedLogWriter::new(
                        &logs_dir,
                        &format!("concurrent-svc-{}", svc_id),
                        LogStream::Stdout,
                        Some(100 * 1024 * 1024),
                        0, // Unbuffered
                    );
                    for i in 0..500 {
                        writer.write(&format!("Initial log {} from service {}", i, svc_id));
                    }
                }
                (temp_dir, logs_dir)
            },
            |(temp_dir, logs_dir)| {
                // Spawn writer threads
                let writer_handles: Vec<_> = (0..num_writers)
                    .map(|writer_id| {
                        let logs_dir = Arc::clone(&logs_dir);
                        thread::spawn(move || {
                            let mut writer = BufferedLogWriter::new(
                                &logs_dir,
                                &format!("concurrent-svc-{}", writer_id),
                                LogStream::Stdout,
                                Some(100 * 1024 * 1024),
                                16 * 1024,
                            );
                            for i in 0..writes_per_writer {
                                writer.write(black_box(&format!(
                                    "Concurrent write {} from writer {}",
                                    i, writer_id
                                )));
                            }
                        })
                    })
                    .collect();

                // Spawn reader threads
                let reader_handles: Vec<_> = (0..num_readers)
                    .map(|reader_id| {
                        let logs_dir = Arc::clone(&logs_dir);
                        thread::spawn(move || {
                            let reader = LogReader::new((*logs_dir).clone());
                            for _ in 0..reads_per_reader {
                                // Mix of operations
                                if reader_id % 2 == 0 {
                                    let _ = black_box(reader.tail(100, None));
                                } else {
                                    let _ = black_box(reader.get_paginated(None, 0, 100));
                                }
                            }
                        })
                    })
                    .collect();

                // Wait for all threads
                for handle in writer_handles {
                    handle.join().unwrap();
                }
                for handle in reader_handles {
                    handle.join().unwrap();
                }
                temp_dir
            },
        );
    });

    group.finish();
}

/// Benchmark realistic mixed workload
fn bench_realistic_workload(c: &mut Criterion) {
    let mut group = c.benchmark_group("realistic_workload");
    group.sample_size(15);

    let num_services = 20;
    let writer_threads = 8;
    let logs_per_writer = 1000;

    group.throughput(Throughput::Elements((writer_threads * logs_per_writer) as u64));

    group.bench_function("realistic", |b| {
        b.iter_with_setup(
            || {
                let temp_dir = TempDir::new().unwrap();
                let logs_dir = Arc::new(temp_dir.path().to_path_buf());
                std::fs::create_dir_all(&*logs_dir).unwrap();
                let services: Vec<String> = (0..num_services)
                    .map(|i| format!("app-{:02}", i))
                    .collect();
                (temp_dir, logs_dir, services)
            },
            |(temp_dir, logs_dir, services)| {
                let handles: Vec<_> = (0..writer_threads)
                    .map(|thread_id| {
                        let logs_dir = Arc::clone(&logs_dir);
                        let services = services.clone();
                        thread::spawn(move || {
                            // Each thread writes to a different service
                            let service = &services[thread_id % services.len()];
                            let mut writer = BufferedLogWriter::new(
                                &logs_dir,
                                service,
                                if thread_id % 2 == 0 { LogStream::Stdout } else { LogStream::Stderr },
                                Some(100 * 1024 * 1024),
                                16 * 1024,
                            );
                            for i in 0..logs_per_writer {
                                let line_size = 50 + (i % 200);
                                writer.write(black_box(&make_log_line(line_size, i)));
                            }
                        })
                    })
                    .collect();

                for handle in handles {
                    handle.join().unwrap();
                }
                temp_dir
            },
        );
    });

    group.finish();
}

/// Benchmark showing benefit of no lock contention
///
/// In the old architecture, all threads would contend for the same RwLock.
/// In the new architecture, each thread has its own BufferedLogWriter with
/// no shared state, so there's zero contention.
fn bench_no_contention_benefit(c: &mut Criterion) {
    let mut group = c.benchmark_group("no_contention_benefit");
    group.sample_size(20);

    // Test scaling with thread count
    let thread_counts = [1, 2, 4, 8, 16, 32];
    let lines_per_thread = 2000;

    for num_threads in thread_counts {
        let total_lines = num_threads * lines_per_thread;
        group.throughput(Throughput::Elements(total_lines as u64));

        group.bench_function(format!("{}threads", num_threads), |b| {
            b.iter_with_setup(
                || {
                    let temp_dir = TempDir::new().unwrap();
                    let logs_dir = Arc::new(temp_dir.path().to_path_buf());
                    std::fs::create_dir_all(&*logs_dir).unwrap();
                    (temp_dir, logs_dir)
                },
                |(temp_dir, logs_dir)| {
                    let handles: Vec<_> = (0..num_threads)
                        .map(|thread_id| {
                            let logs_dir = Arc::clone(&logs_dir);
                            thread::spawn(move || {
                                // Each thread gets its own writer - no shared state
                                let mut writer = BufferedLogWriter::new(
                                    &logs_dir,
                                    &format!("service-{:02}", thread_id),
                                    LogStream::Stdout,
                                    Some(100 * 1024 * 1024),
                                    16 * 1024,
                                );
                                for i in 0..lines_per_thread {
                                    writer.write(black_box(&format!(
                                        "Thread {} line {} - No lock contention!",
                                        thread_id, i
                                    )));
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                    temp_dir
                },
            );
        });
    }

    group.finish();
}

criterion_group!(
    write_benches,
    bench_log_writes,
    bench_caller_latency,
    bench_independent_writers,
    bench_concurrent_writers,
    bench_log_line_sizes,
    bench_many_services,
    bench_burst_writes,
    bench_no_contention_benefit,
);

/// Benchmark comparing iterator-based reading vs read-all-and-sort
///
/// The MergedLogIterator uses a min-heap to merge multiple files chronologically,
/// reading one line at a time from each file. This is efficient for:
/// - Reading first N entries (head)
/// - Streaming logs in order
/// - Memory-constrained environments
fn bench_iterator_vs_sort(c: &mut Criterion) {
    let mut group = c.benchmark_group("iterator_vs_sort");
    group.sample_size(20);

    // Setup: create multiple services with logs
    let temp_dir = TempDir::new().unwrap();
    let logs_dir = temp_dir.path().to_path_buf();
    std::fs::create_dir_all(&logs_dir).unwrap();

    let num_services = 10;
    let lines_per_service = 2000;
    let total_lines = num_services * lines_per_service;

    for svc_id in 0..num_services {
        let mut writer = BufferedLogWriter::new(
            &logs_dir,
            &format!("iter-svc-{:02}", svc_id),
            if svc_id % 2 == 0 { LogStream::Stdout } else { LogStream::Stderr },
            Some(100 * 1024 * 1024),
            0, // Unbuffered
        );
        for line_id in 0..lines_per_service {
            writer.write(&format!(
                "Service {} log line {} with content",
                svc_id, line_id
            ));
        }
    }

    // Benchmark: Read first N using iterator (head) vs read all and sort (simulated tail)
    for read_count in [100, 1000, 5000] {
        group.throughput(Throughput::Elements(read_count as u64));

        // Iterator approach: stops after N entries
        group.bench_function(format!("iterator_head_{}", read_count), |b| {
            let reader = LogReader::new(logs_dir.clone());
            b.iter(|| {
                let entries: Vec<_> = black_box(reader.head(read_count, None));
                entries
            });
        });

        // Sort approach: reads ALL entries, sorts, takes N
        group.bench_function(format!("sort_all_take_{}", read_count), |b| {
            let reader = LogReader::new(logs_dir.clone());
            b.iter(|| {
                let entries = black_box(reader.tail(read_count, None));
                entries
            });
        });
    }

    // Benchmark: Read ALL entries - both should be similar
    group.throughput(Throughput::Elements(total_lines as u64));

    group.bench_function("iterator_all", |b| {
        let reader = LogReader::new(logs_dir.clone());
        b.iter(|| {
            let entries: Vec<_> = black_box(reader.iter(None).collect());
            entries
        });
    });

    group.bench_function("sort_all", |b| {
        let reader = LogReader::new(logs_dir.clone());
        b.iter(|| {
            let entries = black_box(reader.tail(total_lines, None));
            entries
        });
    });

    std::mem::forget(temp_dir);
    group.finish();
}

/// Benchmark comparing different approaches to read and merge logs from multiple services
/// All approaches return N entries from multiple interleaved log files
fn bench_merge_strategies(c: &mut Criterion) {
    let mut group = c.benchmark_group("merge_strategies");
    group.sample_size(20);

    // Setup: create multiple services with interleaved timestamps
    let temp_dir = TempDir::new().unwrap();
    let logs_dir = temp_dir.path().to_path_buf();
    std::fs::create_dir_all(&logs_dir).unwrap();

    let num_services = 10;
    let lines_per_service = 5000;
    let total_lines = num_services * lines_per_service;

    for svc_id in 0..num_services {
        let mut writer = BufferedLogWriter::new(
            &logs_dir,
            &format!("merge-svc-{:02}", svc_id),
            LogStream::Stdout,
            Some(100 * 1024 * 1024),
            0, // Unbuffered
        );
        for line_id in 0..lines_per_service {
            writer.write(&format!(
                "Service {} log line {} - some content padding here",
                svc_id, line_id
            ));
        }
    }

    // Compare strategies for reading N entries
    for read_count in [100, 1000, 5000, 10000] {
        group.throughput(Throughput::Elements(read_count as u64));

        // Strategy 1: MergedLogIterator (heap-based merge, oldest first)
        // Reads one line at a time from each file, stops after N
        group.bench_function(format!("iterator_{}", read_count), |b| {
            let reader = LogReader::new(logs_dir.clone());
            b.iter(|| {
                let entries: Vec<_> = black_box(reader.iter(None).take(read_count).collect());
                entries
            });
        });

        // Strategy 2: get_paginated (uses merged iterator with skip/take)
        // Memory-efficient single-pass pagination
        group.bench_function(format!("paginated_{}", read_count), |b| {
            let reader = LogReader::new(logs_dir.clone());
            b.iter(|| {
                let (entries, _has_more) = black_box(reader.get_paginated(None, 0, read_count));
                entries
            });
        });

        // Strategy 3: tail_bounded (read all, sort, take last N)
        group.bench_function(format!("tail_{}", read_count), |b| {
            let reader = LogReader::new(logs_dir.clone());
            b.iter(|| {
                let entries = black_box(reader.tail(read_count, None));
                entries
            });
        });
    }

    // Full scan comparison
    group.throughput(Throughput::Elements(total_lines as u64));

    group.bench_function("iterator_full", |b| {
        let reader = LogReader::new(logs_dir.clone());
        b.iter(|| {
            let entries: Vec<_> = black_box(reader.iter(None).collect());
            entries
        });
    });

    group.bench_function("paginated_full", |b| {
        let reader = LogReader::new(logs_dir.clone());
        b.iter(|| {
            let (entries, _has_more) = black_box(reader.get_paginated(None, 0, total_lines));
            entries
        });
    });

    std::mem::forget(temp_dir);
    group.finish();
}

/// Benchmark comparing new reverse-reading tail() vs legacy read-all-sort tail()
fn bench_reverse_tail(c: &mut Criterion) {
    let mut group = c.benchmark_group("tail_comparison");
    group.sample_size(20);

    // Setup: create multiple services with lots of logs
    let temp_dir = TempDir::new().unwrap();
    let logs_dir = temp_dir.path().to_path_buf();
    std::fs::create_dir_all(&logs_dir).unwrap();

    let num_services = 5;
    let lines_per_service = 10000; // 10k lines per service = 50k total
    let total_lines = num_services * lines_per_service;

    for svc_id in 0..num_services {
        let mut writer = BufferedLogWriter::new(
            &logs_dir,
            &format!("tail-svc-{:02}", svc_id),
            LogStream::Stdout,
            Some(100 * 1024 * 1024),
            0, // Unbuffered
        );
        for line_id in 0..lines_per_service {
            writer.write(&format!(
                "Service {} log line {} - padding content here for realistic size",
                svc_id, line_id
            ));
        }
    }

    // Compare new vs legacy tail with different counts
    for tail_count in [100, 500, 1000, 5000] {
        group.throughput(Throughput::Elements(tail_count as u64));

        // New reverse-reading tail
        group.bench_function(format!("new_tail_{}", tail_count), |b| {
            let reader = LogReader::new(logs_dir.clone());
            b.iter(|| {
                let entries = black_box(reader.tail(tail_count, None));
                entries
            });
        });

    }

    // Single service comparison
    group.throughput(Throughput::Elements(100));

    group.bench_function("new_tail_100_single_svc", |b| {
        let reader = LogReader::new(logs_dir.clone());
        b.iter(|| {
            let entries = black_box(reader.tail(100, Some("tail-svc-00")));
            entries
        });
    });

    std::mem::forget(temp_dir);
    group.finish();
}

criterion_group!(
    read_benches,
    bench_log_reading,
    bench_multi_service_reading,
    bench_truncated_log_reading,
    bench_bounded_reading,
    bench_concurrent_read_write,
    bench_iterator_vs_sort,
    bench_merge_strategies,
    bench_reverse_tail,
);

criterion_group!(
    mixed_benches,
    bench_realistic_workload,
);

criterion_main!(write_benches, read_benches, mixed_benches);
