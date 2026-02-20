/// Count open file descriptors for the current process (Linux only).
/// Requires the `fd-tracking` feature flag.
/// Returns None on non-Linux platforms or when the feature is disabled.
#[cfg(feature = "fd-tracking")]
pub fn count_open_fds() -> Option<usize> {
    std::fs::read_dir("/proc/self/fd").ok().map(|d| d.count())
}

#[cfg(not(feature = "fd-tracking"))]
pub fn count_open_fds() -> Option<usize> {
    None
}
