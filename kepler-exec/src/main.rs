//! kepler-exec: lightweight wrapper that applies uid/gid and resource limits
//! before exec'ing the real command.
//!
//! This avoids fork() overhead in the daemon by keeping the daemon's Command
//! free of pre_exec/uid()/gid() calls, which lets Rust use posix_spawnp().

use std::env;
use std::process;

#[cfg(unix)]
fn main() {
    // Guard: refuse to run if the setuid/setgid bit is set on this binary.
    // If ruid != euid, someone has chmod u+s'd us — abort immediately.
    reject_if_setuid();

    let args: Vec<String> = env::args().skip(1).collect();

    let mut uid: Option<u32> = None;
    let mut gid: Option<u32> = None;
    let mut rlimit_as: Option<u64> = None;
    let mut rlimit_cpu: Option<u64> = None;
    let mut rlimit_nofile: Option<u64> = None;

    let mut i = 0;
    let mut cmd_start = None;

    while i < args.len() {
        match args[i].as_str() {
            "--uid" => {
                i += 1;
                uid = Some(parse_u32_arg(&args, i, "--uid"));
            }
            "--gid" => {
                i += 1;
                gid = Some(parse_u32_arg(&args, i, "--gid"));
            }
            "--rlimit-as" => {
                i += 1;
                rlimit_as = Some(parse_u64_arg(&args, i, "--rlimit-as"));
            }
            "--rlimit-cpu" => {
                i += 1;
                rlimit_cpu = Some(parse_u64_arg(&args, i, "--rlimit-cpu"));
            }
            "--rlimit-nofile" => {
                i += 1;
                rlimit_nofile = Some(parse_u64_arg(&args, i, "--rlimit-nofile"));
            }
            "--" => {
                cmd_start = Some(i + 1);
                break;
            }
            other => {
                eprintln!("kepler-exec: unknown argument: {}", other);
                process::exit(127);
            }
        }
        i += 1;
    }

    let cmd_start = match cmd_start {
        Some(idx) if idx < args.len() => idx,
        _ => {
            eprintln!("kepler-exec: missing command after --");
            process::exit(127);
        }
    };

    let program = &args[cmd_start];
    let cmd_args = &args[cmd_start + 1..];

    // Apply resource limits before dropping privileges
    apply_rlimits(rlimit_as, rlimit_cpu, rlimit_nofile);

    // Drop privileges: clear supplementary groups, then setgid, then setuid
    drop_privileges(uid, gid);

    // Close inherited file descriptors above stderr before exec
    close_inherited_fds();

    // exec the real command (replaces this process)
    use std::ffi::CString;

    let c_program = CString::new(program.as_bytes()).unwrap_or_else(|_| {
        eprintln!("kepler-exec: invalid program name");
        process::exit(127);
    });

    let mut c_args: Vec<CString> = Vec::with_capacity(1 + cmd_args.len());
    c_args.push(c_program.clone());
    for arg in cmd_args {
        c_args.push(CString::new(arg.as_bytes()).unwrap_or_else(|_| {
            eprintln!("kepler-exec: invalid argument");
            process::exit(127);
        }));
    }

    // execvp searches PATH — only returns on error
    match nix::unistd::execvp(&c_program, &c_args) {
        Ok(infallible) => match infallible {},
        Err(e) => {
            eprintln!("kepler-exec: exec failed: {}", e);
            process::exit(127);
        }
    }
}

/// Abort if running as a setuid/setgid binary.
/// Prevents privilege escalation if someone accidentally sets the setuid bit.
#[cfg(unix)]
fn reject_if_setuid() {
    use nix::unistd::{getuid, geteuid, getgid, getegid};

    let ruid = getuid();
    let euid = geteuid();
    let rgid = getgid();
    let egid = getegid();

    if ruid != euid || rgid != egid {
        eprintln!(
            "kepler-exec: refusing to run as setuid/setgid binary \
             (uid={}, euid={}, gid={}, egid={})",
            ruid, euid, rgid, egid
        );
        process::exit(127);
    }
}

/// Drop privileges: clear supplementary groups, setgid, then setuid.
/// Order matters — group operations must happen before dropping to non-root uid.
#[cfg(unix)]
fn drop_privileges(uid: Option<u32>, gid: Option<u32>) {
    // Clear supplementary groups before changing primary gid/uid.
    // Without this, the child inherits the daemon's supplementary groups
    // (e.g. docker, sudo, wheel), defeating privilege separation.
    if uid.is_some() || gid.is_some() {
        use nix::unistd::Gid;

        let target_groups: Vec<Gid> = match gid {
            Some(g) => vec![Gid::from_raw(g)],
            None => vec![],
        };
        if let Err(e) = setgroups_portable(&target_groups) {
            eprintln!("kepler-exec: setgroups({:?}) failed: {}", target_groups, e);
            process::exit(127);
        }
    }

    if let Some(gid) = gid {
        use nix::unistd::{Gid, setgid};
        if let Err(e) = setgid(Gid::from_raw(gid)) {
            eprintln!("kepler-exec: setgid({}) failed: {}", gid, e);
            process::exit(127);
        }
    }

    if let Some(uid) = uid {
        use nix::unistd::{Uid, setuid};
        if let Err(e) = setuid(Uid::from_raw(uid)) {
            eprintln!("kepler-exec: setuid({}) failed: {}", uid, e);
            process::exit(127);
        }
    }
}

/// Portable setgroups: nix::unistd::setgroups is not available on Apple targets,
/// so we call libc::setgroups directly.
#[cfg(unix)]
fn setgroups_portable(groups: &[nix::unistd::Gid]) -> Result<(), nix::errno::Errno> {
    let gids: Vec<libc::gid_t> = groups.iter().map(|g| g.as_raw()).collect();
    let ret = unsafe { libc::setgroups(gids.len() as libc::size_t, gids.as_ptr()) };
    if ret == 0 {
        Ok(())
    } else {
        Err(nix::errno::Errno::last())
    }
}

/// Close all file descriptors above stderr (fd > 2) to prevent leaking
/// daemon resources (sockets, log files, etc.) to the child process.
#[cfg(unix)]
fn close_inherited_fds() {
    // Try close_range syscall first (Linux 5.9+)
    #[cfg(target_os = "linux")]
    {
        let ret = unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) };
        if ret == 0 {
            return;
        }
    }

    // Fallback: iterate fd directory (/proc/self/fd on Linux, /dev/fd on macOS)
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
            unsafe { libc::close(fd); }
        }
    }
}

#[cfg(unix)]
fn parse_u32_arg(args: &[String], i: usize, flag: &str) -> u32 {
    if i >= args.len() {
        eprintln!("kepler-exec: {} requires a value", flag);
        process::exit(127);
    }
    args[i].parse::<u32>().unwrap_or_else(|_| {
        eprintln!("kepler-exec: invalid value for {}: {} (must be 0..={})", flag, args[i], u32::MAX);
        process::exit(127);
    })
}

#[cfg(unix)]
fn parse_u64_arg(args: &[String], i: usize, flag: &str) -> u64 {
    if i >= args.len() {
        eprintln!("kepler-exec: {} requires a value", flag);
        process::exit(127);
    }
    args[i].parse::<u64>().unwrap_or_else(|_| {
        eprintln!("kepler-exec: invalid value for {}: {}", flag, args[i]);
        process::exit(127);
    })
}

#[cfg(unix)]
fn apply_rlimits(rlimit_as: Option<u64>, rlimit_cpu: Option<u64>, rlimit_nofile: Option<u64>) {
    use nix::sys::resource::{setrlimit, Resource};

    if let Some(bytes) = rlimit_as
        && let Err(e) = setrlimit(Resource::RLIMIT_AS, bytes, bytes)
    {
        eprintln!("kepler-exec: setrlimit(RLIMIT_AS, {}) failed: {}", bytes, e);
        process::exit(127);
    }

    if let Some(secs) = rlimit_cpu
        && let Err(e) = setrlimit(Resource::RLIMIT_CPU, secs, secs)
    {
        eprintln!("kepler-exec: setrlimit(RLIMIT_CPU, {}) failed: {}", secs, e);
        process::exit(127);
    }

    if let Some(fds) = rlimit_nofile
        && let Err(e) = setrlimit(Resource::RLIMIT_NOFILE, fds, fds)
    {
        eprintln!("kepler-exec: setrlimit(RLIMIT_NOFILE, {}) failed: {}", fds, e);
        process::exit(127);
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!("kepler-exec: only supported on Unix platforms");
    process::exit(127);
}
