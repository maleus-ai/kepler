//! Test helper: allocate N MB of resident memory and hold it until killed.
//!
//! Used by the monitoring E2E tests to check that a service's child processes
//! are accounted for. The allocation must be *resident*, not just reserved, so
//! every page is touched — otherwise it would never show up in RSS or in the
//! cgroup's `memory.current`.
//!
//! Usage: `memhog [MB]` (default 200)

fn main() {
    let mb: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(200);

    let mut buf = vec![0u8; mb * 1024 * 1024];

    // Keep re-touching every page rather than touching once and sleeping.
    // Pages written once and then left alone are cold anonymous memory: under
    // any memory pressure the kernel reclaims them and RSS silently drops,
    // which made the measurement flaky. Re-touching keeps them resident and
    // active. `black_box` stops the optimizer from deciding the buffer is dead.
    loop {
        for i in (0..buf.len()).step_by(4096) {
            buf[i] = buf[i].wrapping_add(1);
        }
        std::hint::black_box(&buf);
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}
