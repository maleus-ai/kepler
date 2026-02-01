use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use kepler_daemon::logs::{LogStream, SharedLogBuffer};
use tempfile::TempDir;

/// Benchmark log buffer write performance with different buffer sizes
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
                        let buffer = SharedLogBuffer::with_options(
                            temp_dir.path().to_path_buf(),
                            10 * 1024 * 1024, // 10MB max log size
                            5,                 // 5 rotated files
                            buffer_size,
                        );
                        (temp_dir, buffer)
                    },
                    |(_temp_dir, buffer)| {
                        for i in 0..num_lines {
                            buffer.push(
                                black_box("test-service"),
                                black_box(format!("Log line number {} with some content to simulate realistic log output", i)),
                                black_box(LogStream::Stdout),
                            );
                        }
                        // Flush to ensure all data is written
                        buffer.flush_all();
                    },
                );
            },
        );
    }

    group.finish();
}

/// Benchmark caller latency (time to return from push call)
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
            BenchmarkId::new("single_push", &label),
            &buffer_size,
            |b, &buffer_size| {
                let temp_dir = TempDir::new().unwrap();
                let buffer = SharedLogBuffer::with_options(
                    temp_dir.path().to_path_buf(),
                    10 * 1024 * 1024,
                    5,
                    buffer_size,
                );

                let mut i = 0u64;
                b.iter(|| {
                    buffer.push(
                        black_box("test-service"),
                        black_box(format!("Log line {}", i)),
                        black_box(LogStream::Stdout),
                    );
                    i += 1;
                });
            },
        );
    }

    group.finish();
}

/// Benchmark multiple services writing high volume logs
/// Tests realistic scenarios with many services each producing thousands of logs
fn bench_multi_service_high_volume(c: &mut Criterion) {
    let mut group = c.benchmark_group("multi_service_high_volume");

    // Increase sample size timeout for longer benchmarks
    group.sample_size(50);

    // Test configurations: (num_services, lines_per_service)
    let configs = [
        (10, 2000),   // 10 services, 2k logs each = 20k total
        (20, 2000),   // 20 services, 2k logs each = 40k total
        (50, 2000),   // 50 services, 2k logs each = 100k total
        (20, 4000),   // 20 services, 4k logs each = 80k total
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
                            let buffer = SharedLogBuffer::with_options(
                                temp_dir.path().to_path_buf(),
                                50 * 1024 * 1024, // 50MB max log size
                                5,
                                buffer_size,
                            );
                            // Pre-generate service names
                            let service_names: Vec<String> = (0..num_services)
                                .map(|i| format!("service-{:03}", i))
                                .collect();
                            (temp_dir, buffer, service_names)
                        },
                        |(_temp_dir, buffer, service_names)| {
                            for service_name in &service_names {
                                for line_id in 0..lines_per_service {
                                    buffer.push(
                                        black_box(service_name.as_str()),
                                        black_box(format!(
                                            "[{}] Log entry {} - Processing request with data payload",
                                            service_name, line_id
                                        )),
                                        black_box(if line_id % 10 == 0 {
                                            LogStream::Stderr
                                        } else {
                                            LogStream::Stdout
                                        }),
                                    );
                                }
                            }
                            buffer.flush_all();
                        },
                    );
                },
            );
        }
    }

    group.finish();
}

/// Benchmark interleaved writes from multiple services
/// Simulates realistic concurrent logging where services write in round-robin fashion
fn bench_interleaved_writes(c: &mut Criterion) {
    let mut group = c.benchmark_group("interleaved_writes");
    group.sample_size(50);

    let num_services = 20;
    let total_lines = 40000; // 2000 lines per service
    let lines_per_service = total_lines / num_services;

    group.throughput(Throughput::Elements(total_lines as u64));

    let buffer_sizes = [0, 16 * 1024, 64 * 1024];

    for buffer_size in buffer_sizes {
        let label = if buffer_size == 0 {
            "sync".to_string()
        } else {
            format!("{}KB", buffer_size / 1024)
        };

        group.bench_with_input(
            BenchmarkId::new("round_robin", &label),
            &buffer_size,
            |b, &buffer_size| {
                b.iter_with_setup(
                    || {
                        let temp_dir = TempDir::new().unwrap();
                        let buffer = SharedLogBuffer::with_options(
                            temp_dir.path().to_path_buf(),
                            50 * 1024 * 1024,
                            5,
                            buffer_size,
                        );
                        let service_names: Vec<String> = (0..num_services)
                            .map(|i| format!("service-{:03}", i))
                            .collect();
                        (temp_dir, buffer, service_names)
                    },
                    |(_temp_dir, buffer, service_names)| {
                        // Interleaved writes - each iteration writes one line per service
                        for line_id in 0..lines_per_service {
                            for service_name in &service_names {
                                buffer.push(
                                    black_box(service_name.as_str()),
                                    black_box(format!(
                                        "Interleaved log {} from {}",
                                        line_id, service_name
                                    )),
                                    black_box(LogStream::Stdout),
                                );
                            }
                        }
                        buffer.flush_all();
                    },
                );
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_log_writes,
    bench_caller_latency,
    bench_multi_service_high_volume,
    bench_interleaved_writes
);
criterion_main!(benches);
