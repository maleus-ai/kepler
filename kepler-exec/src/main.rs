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

    let mut user_spec: Option<String> = None;
    let mut groups_spec: Option<String> = None;
    let mut rlimit_as: Option<u64> = None;
    let mut rlimit_cpu: Option<u64> = None;
    let mut rlimit_nofile: Option<u64> = None;

    let mut i = 0;
    let mut cmd_start = None;

    while i < args.len() {
        match args[i].as_str() {
            "--user" => {
                i += 1;
                user_spec = Some(parse_string_arg(&args, i, "--user"));
            }
            "--groups" => {
                i += 1;
                groups_spec = Some(parse_string_arg(&args, i, "--groups"));
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

    // Parse explicit groups if provided
    let explicit_groups: Option<Vec<u32>> = groups_spec.as_deref().map(|spec| {
        spec.split(',')
            .map(|g| resolve_group(g.trim()))
            .collect()
    });

    // Drop privileges: handle supplementary groups, then setgid, then setuid
    drop_privileges(user_spec.as_deref(), explicit_groups);

    // Close inherited file descriptors above stderr before exec
    kepler_unix::process::close_inherited_fds();

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

/// Resolve a user specification string to (uid, gid, Option<username>).
///
/// Supported formats:
/// - `"name"` — lookup user by name, get uid/gid from passwd
/// - `"name:group"` — lookup user by name, resolve group separately
/// - `"uid"` — numeric uid, gid=uid, reverse-lookup for username
/// - `"uid:gid"` — numeric uid and gid, reverse-lookup for username
#[cfg(unix)]
fn resolve_user(spec: &str) -> (u32, u32, Option<String>) {
    use nix::unistd::User;

    if let Some((left, right)) = spec.split_once(':') {
        // Format: "left:right" — left is user, right is group
        let (uid, username) = resolve_user_part(left);
        let gid = resolve_group(right);
        (uid, gid, username)
    } else if let Ok(uid) = spec.parse::<u32>() {
        // Numeric uid — reverse-lookup for username (for initgroups)
        let username = User::from_uid(nix::unistd::Uid::from_raw(uid))
            .ok()
            .flatten()
            .map(|u| u.name);
        (uid, uid, username)
    } else {
        // Username — lookup by name
        let user = User::from_name(spec).unwrap_or_else(|e| {
            eprintln!("kepler-exec: failed to look up user '{}': {}", spec, e);
            process::exit(127);
        });
        let user = user.unwrap_or_else(|| {
            eprintln!("kepler-exec: user '{}' not found", spec);
            process::exit(127);
        });
        (user.uid.as_raw(), user.gid.as_raw(), Some(user.name))
    }
}

/// Resolve the user part of a "user:group" spec to (uid, Option<username>).
#[cfg(unix)]
fn resolve_user_part(spec: &str) -> (u32, Option<String>) {
    use nix::unistd::User;

    if let Ok(uid) = spec.parse::<u32>() {
        let username = User::from_uid(nix::unistd::Uid::from_raw(uid))
            .ok()
            .flatten()
            .map(|u| u.name);
        (uid, username)
    } else {
        let user = User::from_name(spec).unwrap_or_else(|e| {
            eprintln!("kepler-exec: failed to look up user '{}': {}", spec, e);
            process::exit(127);
        });
        let user = user.unwrap_or_else(|| {
            eprintln!("kepler-exec: user '{}' not found", spec);
            process::exit(127);
        });
        (user.uid.as_raw(), Some(user.name))
    }
}

/// Resolve a group specification (name or numeric) to gid.
#[cfg(unix)]
fn resolve_group(spec: &str) -> u32 {
    use nix::unistd::Group;

    if let Ok(gid) = spec.parse::<u32>() {
        return gid;
    }

    let grp = Group::from_name(spec).unwrap_or_else(|e| {
        eprintln!("kepler-exec: failed to look up group '{}': {}", spec, e);
        process::exit(127);
    });
    let grp = grp.unwrap_or_else(|| {
        eprintln!("kepler-exec: group '{}' not found", spec);
        process::exit(127);
    });
    grp.gid.as_raw()
}

/// Drop privileges: handle supplementary groups, setgid, then setuid.
/// Order matters — group operations must happen before dropping to non-root uid.
///
/// - `user_spec` present + no `explicit_groups` → initgroups(username, gid) or setgroups([gid])
/// - `user_spec` present + `explicit_groups` → setgroups(explicit_gids)
/// - no `user_spec` → nothing to do
#[cfg(unix)]
fn drop_privileges(user_spec: Option<&str>, explicit_groups: Option<Vec<u32>>) {
    let user_spec = match user_spec {
        Some(s) => s,
        None => return,
    };

    let (uid, gid, username) = resolve_user(user_spec);

    // Set supplementary groups
    match explicit_groups {
        Some(gids) => {
            // Explicit lockdown: use exactly these groups
            if let Err(e) = kepler_unix::groups::setgroups(&gids) {
                eprintln!("kepler-exec: setgroups({:?}) failed: {}", gids, e);
                process::exit(127);
            }
        }
        None => {
            // Default: load all supplementary groups if we have a username
            match username {
                Some(ref name) => {
                    if let Err(e) = kepler_unix::groups::initgroups(name, gid) {
                        eprintln!(
                            "kepler-exec: initgroups('{}', {}) failed: {}",
                            name, gid, e
                        );
                        process::exit(127);
                    }
                }
                None => {
                    // No username (numeric uid with no passwd entry) — fallback to [gid]
                    if let Err(e) = kepler_unix::groups::setgroups(&[gid]) {
                        eprintln!("kepler-exec: setgroups([{}]) failed: {}", gid, e);
                        process::exit(127);
                    }
                }
            }
        }
    }

    // setgid before setuid (must still be root to change gid)
    {
        use nix::unistd::{Gid, setgid};
        if let Err(e) = setgid(Gid::from_raw(gid)) {
            eprintln!("kepler-exec: setgid({}) failed: {}", gid, e);
            process::exit(127);
        }
    }

    // setuid last
    {
        use nix::unistd::{Uid, setuid};
        if let Err(e) = setuid(Uid::from_raw(uid)) {
            eprintln!("kepler-exec: setuid({}) failed: {}", uid, e);
            process::exit(127);
        }
    }
}

#[cfg(unix)]
fn parse_string_arg(args: &[String], i: usize, flag: &str) -> String {
    if i >= args.len() {
        eprintln!("kepler-exec: {} requires a value", flag);
        process::exit(127);
    }
    args[i].clone()
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

    if let Some(bytes) = rlimit_as {
        #[cfg(target_os = "macos")]
        {
            let _ = bytes;
            eprintln!(
                "kepler-exec: warning: RLIMIT_AS is not supported on macOS \
                 (incompatible with dyld shared cache); ignoring memory limit"
            );
        }
        #[cfg(not(target_os = "macos"))]
        if let Err(e) = setrlimit(Resource::RLIMIT_AS, bytes, bytes) {
            eprintln!("kepler-exec: setrlimit(RLIMIT_AS, {}) failed: {}", bytes, e);
            process::exit(127);
        }
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
