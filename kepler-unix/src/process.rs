/// Set the effective group ID.
#[cfg(unix)]
pub fn setgid(gid: u32) -> std::io::Result<()> {
    let ret = unsafe { libc::setgid(gid) };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Set the effective user ID.
#[cfg(unix)]
pub fn setuid(uid: u32) -> std::io::Result<()> {
    let ret = unsafe { libc::setuid(uid) };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Close all file descriptors above stderr (fd > 2).
///
/// Platform strategies:
/// - Linux: `close_range` syscall (5.9+) → `/proc/self/fd` enumeration → brute-force
/// - FreeBSD, OpenBSD, NetBSD: `closefrom(3)` (always available)
/// - macOS and others: `/dev/fd` enumeration → brute-force
#[cfg(unix)]
pub fn close_inherited_fds() {
    // BSDs: closefrom(3) closes all fds >= lowfd in a single call
    #[cfg(any(target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
    {
        unsafe { libc::closefrom(3) };
        return;
    }

    // Linux: try close_range syscall first (5.9+), returns -1/ENOSYS on older kernels
    #[cfg(target_os = "linux")]
    {
        let ret = unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) };
        if ret == 0 {
            return;
        }
    }

    // Enumerate fd directory: /proc/self/fd (Linux) or /dev/fd (macOS)
    #[cfg(not(any(target_os = "freebsd", target_os = "openbsd", target_os = "netbsd")))]
    {
        let fd_dir = if cfg!(target_os = "linux") {
            "/proc/self/fd"
        } else {
            "/dev/fd"
        };

        if let Ok(entries) = std::fs::read_dir(fd_dir) {
            let fds_to_close: Vec<i32> = entries
                .filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().to_str().and_then(|s| s.parse::<i32>().ok()))
                .filter(|&fd| fd > 2)
                .collect();

            for fd in fds_to_close {
                unsafe {
                    libc::close(fd);
                }
            }
            return;
        }

        // Brute-force fallback: iterate 3..OPEN_MAX
        let max_fd = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) } as i32;
        let max_fd = if max_fd > 0 { max_fd } else { 1024 };
        for fd in 3..max_fd {
            unsafe {
                libc::close(fd);
            }
        }
    }
}
