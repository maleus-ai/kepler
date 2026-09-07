use super::*;

#[test]
fn test_service_cgroup_path() {
    let root = PathBuf::from("/sys/fs/cgroup/kepler");
    let path = service_cgroup_path(&root, "abc123", "my-service");
    assert_eq!(
        path,
        PathBuf::from("/sys/fs/cgroup/kepler/abc123/my-service")
    );
}

#[test]
fn test_detect_cgroupv2() {
    let result = detect_cgroupv2();
    match &result {
        Some(root) => eprintln!("cgroup v2 detected: {:?}", root),
        None => eprintln!("cgroup v2 not available (expected in unprivileged containers)"),
    }
    // Don't assert — depends on environment (privileged vs unprivileged)
}

/// Helper: get cgroup root, skipping if REQUIRE_CGROUPV2 is not set.
fn require_cgroupv2() -> Option<PathBuf> {
    if std::env::var("REQUIRE_CGROUPV2").as_deref() != Ok("1") {
        eprintln!("Skipping: REQUIRE_CGROUPV2 not set");
        return None;
    }
    Some(detect_cgroupv2().expect("cgroup v2 should be available in privileged mode"))
}

/// Helper: unique config hash per test to avoid collisions.
fn test_hash(label: &str) -> String {
    format!("test_{}_{}", label, std::process::id())
}

/// Helper: move a PID out of a service cgroup so the directory can be rmdir'd.
///
/// Once controllers are enabled for a cgroup's children, that cgroup refuses to
/// hold processes itself (cgroup v2 "no internal processes" rule). Since the
/// monitor now enables the `memory` controller, the namespace root can no longer
/// be used as a parking spot — use the `init` sub-cgroup the test entrypoint
/// creates, falling back to the root when it is absent.
fn move_pid_out_of_service_cgroup(pid: u32) -> io::Result<()> {
    let init_procs = PathBuf::from("/sys/fs/cgroup/init/cgroup.procs");
    let target = if init_procs.exists() {
        init_procs
    } else {
        PathBuf::from("/sys/fs/cgroup/cgroup.procs")
    };
    std::fs::write(&target, pid.to_string())
}

/// Helper: forcibly clean up a service cgroup (kill processes first if needed).
fn force_cleanup_service_cgroup(cgroup: &Path) {
    let _ = kill_cgroup(cgroup);
    std::thread::sleep(std::time::Duration::from_millis(100));
    let _ = std::fs::remove_dir(cgroup);
}

/// This test only runs when REQUIRE_CGROUPV2=1 is set (e.g. in test-cgroup).
/// It asserts that cgroup v2 is actually available and functional.
#[test]
fn test_cgroupv2_required_when_env_set() {
    let root = match require_cgroupv2() {
        Some(r) => r,
        None => return,
    };

    // Test full lifecycle: create, add (self), enumerate, remove
    let hash = test_hash("lifecycle");
    let cgroup = create_service_cgroup(&root, &hash, "test-svc").unwrap();
    assert!(cgroup.exists());

    // Add our own PID
    add_pid_to_cgroup(&cgroup, std::process::id()).unwrap();
    let pids = enumerate_cgroup_pids(&cgroup);
    assert!(
        pids.contains(&std::process::id()),
        "our PID should be in cgroup"
    );

    // Can't rmdir a cgroup with processes in it — move ourselves out first
    move_pid_out_of_service_cgroup(std::process::id()).unwrap();

    // Give the kernel time to finish migrating all threads out of the cgroup
    std::thread::sleep(std::time::Duration::from_millis(100));

    remove_service_cgroup(&cgroup).unwrap();
    remove_config_cgroup(&root, &hash).unwrap();
    assert!(!cgroup.exists());
}

/// Spawn a sleep process, add it to a cgroup, kill the cgroup, verify it's dead and empty.
#[test]
fn test_kill_cgroup() {
    let root = match require_cgroupv2() {
        Some(r) => r,
        None => return,
    };

    let hash = test_hash("kill");
    let cgroup = create_service_cgroup(&root, &hash, "kill-svc").unwrap();

    // Spawn a sleep process
    let mut child = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("failed to spawn sleep");

    add_pid_to_cgroup(&cgroup, child.id()).unwrap();
    assert!(!enumerate_cgroup_pids(&cgroup).is_empty());

    kill_cgroup(&cgroup).unwrap();
    // Reap the zombie so the kernel fully removes the process
    let _ = child.wait();

    let pids = enumerate_cgroup_pids(&cgroup);
    assert!(pids.is_empty(), "cgroup should be empty after kill, got {:?}", pids);

    remove_service_cgroup(&cgroup).unwrap();
    remove_config_cgroup(&root, &hash).unwrap();
}

/// Spawn a shell that forks children inside the cgroup, kill, verify all dead.
///
/// The parent must be added to the cgroup BEFORE it forks, because children
/// inherit the parent's cgroup at fork time. We use a delayed-fork script:
/// the shell sleeps first (giving us time to move it), then forks children.
#[test]
fn test_kill_cgroup_with_children() {
    let root = match require_cgroupv2() {
        Some(r) => r,
        None => return,
    };

    let hash = test_hash("kill_children");
    let cgroup = create_service_cgroup(&root, &hash, "children-svc").unwrap();

    // Spawn sh that sleeps before forking — gives us time to add it to the cgroup
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg("sleep 0.5; sleep 60 & sleep 60 & wait")
        .spawn()
        .expect("failed to spawn sh");

    // Add parent to cgroup while it's in the initial sleep
    add_pid_to_cgroup(&cgroup, child.id()).unwrap();

    // Wait for children to be forked (after the 0.5s initial sleep)
    std::thread::sleep(std::time::Duration::from_millis(1500));

    let pids_before = enumerate_cgroup_pids(&cgroup);
    assert!(pids_before.len() >= 2, "expected at least parent + children, got {}", pids_before.len());

    kill_cgroup(&cgroup).unwrap();
    let _ = child.wait();
    // Give kernel time to reap all children
    std::thread::sleep(std::time::Duration::from_millis(100));

    let pids_after = enumerate_cgroup_pids(&cgroup);
    assert!(pids_after.is_empty(), "all processes should be dead, got {:?}", pids_after);

    force_cleanup_service_cgroup(&cgroup);
    let _ = remove_config_cgroup(&root, &hash);
}

/// Sum the RSS of a set of PIDs by reading `/proc/<pid>/status`, in bytes.
///
/// Not cfg-gated: its only caller is gated at runtime by `require_cgroupv2()`,
/// and gating the function too would break compilation of that caller elsewhere.
fn sum_process_rss(pids: &[u32]) -> u64 {
    pids.iter()
        .filter_map(|pid| {
            let status = std::fs::read_to_string(format!("/proc/{}/status", pid)).ok()?;
            let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
            let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
            Some(kb * 1024)
        })
        .sum()
}

/// The memory controller must be delegated all the way down to the service
/// leaf, otherwise `memory.current` does not exist and the monitor silently
/// falls back to summing per-process RSS.
#[test]
fn test_memory_current_readable() {
    let root = match require_cgroupv2() {
        Some(r) => r,
        None => return,
    };

    let hash = test_hash("memcurrent");
    let cgroup = create_service_cgroup(&root, &hash, "mem-svc").unwrap();

    let usage = read_memory_current(&cgroup);
    assert!(
        usage.is_some(),
        "memory.current should be readable at {:?}. \
         leaf cgroup.controllers={:?}, config-level cgroup.subtree_control={:?}, \
         root cgroup.subtree_control={:?}",
        cgroup,
        std::fs::read_to_string(cgroup.join("cgroup.controllers")),
        std::fs::read_to_string(root.join(&hash).join("cgroup.subtree_control")),
        std::fs::read_to_string(root.join("cgroup.subtree_control")),
    );

    force_cleanup_service_cgroup(&cgroup);
    let _ = remove_config_cgroup(&root, &hash);
}

/// A process that allocates and then forks **without exec'ing** shares its
/// pages copy-on-write. Summing per-process RSS counts those shared pages once
/// per process; `memory.current` counts them once, which is the whole reason
/// the monitor prefers it.
///
/// This models a pre-fork server (gunicorn, unicorn, php-fpm): load data, then
/// fork N workers. The RSS sum reports roughly (N+1)x the real footprint.
#[test]
fn test_memory_current_does_not_double_count_cow() {
    let root = match require_cgroupv2() {
        Some(r) => r,
        None => return,
    };

    let hash = test_hash("cow");
    let cgroup = create_service_cgroup(&root, &hash, "cow-svc").unwrap();

    // Allocate ~60 MB, then fork two children that block opening a FIFO nobody
    // writes to. Opening a FIFO for reading is a blocking syscall, not an exec,
    // so the children keep sharing the parent's pages copy-on-write — which is
    // exactly the situation being measured. (A background `read` without the
    // redirect would not work: POSIX gives background commands in a
    // non-interactive shell /dev/null on stdin, so they would hit EOF and exit.)
    //
    // The leading sleep gives us time to move the parent into the cgroup before
    // it forks, since children inherit the cgroup at fork time.
    let fifo = format!("/tmp/kepler_cow_fifo_{}", std::process::id());
    let marker = format!("/tmp/kepler_cow_ready_{}", std::process::id());
    let script = format!(
        "sleep 0.5; mkfifo {f}; \
         x=$(head -c 60000000 /dev/zero | tr '\\0' 'a'); \
         ( read l < {f} ) & ( read l < {f} ) & \
         touch {m}; read l < {f}",
        f = fifo,
        m = marker,
    );

    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(&script)
        .spawn()
        .expect("failed to spawn sh");

    add_pid_to_cgroup(&cgroup, child.id()).unwrap();

    // Wait on a marker file the script touches once the allocation is done and
    // both children are forked. Counting PIDs is not a usable readiness signal
    // here: the `head | tr` pipeline of the command substitution transiently
    // puts three processes in the cgroup while the variable is still filling.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !std::path::Path::new(&marker).exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "workload never signalled readiness"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    let pids = enumerate_cgroup_pids(&cgroup);
    assert!(
        pids.len() >= 3,
        "expected parent + 2 forked children in the cgroup, got {:?}",
        pids
    );

    let rss_sum = sum_process_rss(&pids);
    let cgroup_usage = read_memory_current(&cgroup).expect("memory.current should be readable");

    eprintln!(
        "{} processes: RSS sum = {} MB, memory.current = {} MB (~60 MB really allocated)",
        pids.len(),
        rss_sum / 1024 / 1024,
        cgroup_usage / 1024 / 1024,
    );

    // The kernel's accounting must be close to what was really allocated...
    assert!(
        cgroup_usage > 40 * 1024 * 1024 && cgroup_usage < 100 * 1024 * 1024,
        "memory.current should be near the 60 MB actually allocated, got {} MB",
        cgroup_usage / 1024 / 1024,
    );

    // ...while the RSS sum inflates it by roughly the number of processes.
    assert!(
        rss_sum > cgroup_usage + 40 * 1024 * 1024,
        "RSS sum ({} MB) should visibly over-count vs memory.current ({} MB) — \
         if this fails, the children exec'd instead of staying COW-shared and \
         the test no longer proves anything",
        rss_sum / 1024 / 1024,
        cgroup_usage / 1024 / 1024,
    );

    let _ = kill_cgroup(&cgroup);
    let _ = child.wait();
    let _ = std::fs::remove_file(&fifo);
    let _ = std::fs::remove_file(&marker);
    force_cleanup_service_cgroup(&cgroup);
    let _ = remove_config_cgroup(&root, &hash);
}

/// Create 2 service cgroups, verify list_service_cgroups returns both.
#[test]
fn test_list_service_cgroups() {
    let root = match require_cgroupv2() {
        Some(r) => r,
        None => return,
    };

    let hash = test_hash("list");
    create_service_cgroup(&root, &hash, "svc-a").unwrap();
    create_service_cgroup(&root, &hash, "svc-b").unwrap();

    let mut services = list_service_cgroups(&root, &hash);
    services.sort();
    assert_eq!(services, vec!["svc-a", "svc-b"]);

    // Cleanup
    force_cleanup_service_cgroup(&service_cgroup_path(&root, &hash, "svc-a"));
    force_cleanup_service_cgroup(&service_cgroup_path(&root, &hash, "svc-b"));
    let _ = remove_config_cgroup(&root, &hash);
}

/// Live process in cgroup → remove_service_cgroup returns EBUSY.
/// EBUSY handling (kill + retry) is done at the ContainmentManager level.
#[test]
fn test_remove_service_cgroup_ebusy() {
    let root = match require_cgroupv2() {
        Some(r) => r,
        None => return,
    };

    let hash = test_hash("ebusy");
    let cgroup = create_service_cgroup(&root, &hash, "ebusy-svc").unwrap();

    let mut child = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("failed to spawn sleep");

    add_pid_to_cgroup(&cgroup, child.id()).unwrap();

    // remove_service_cgroup should return an error (EBUSY) when processes are present
    let result = remove_service_cgroup(&cgroup);
    assert!(result.is_err(), "should fail with EBUSY when processes are in cgroup");

    // Clean up: kill the process, reap it, then remove
    kill_cgroup(&cgroup).unwrap();
    let _ = child.wait();

    remove_service_cgroup(&cgroup).unwrap();
    let _ = remove_config_cgroup(&root, &hash);
}

/// Removing a nonexistent cgroup path should succeed (ENOENT suppressed).
#[test]
fn test_remove_service_cgroup_nonexistent() {
    let root = match require_cgroupv2() {
        Some(r) => r,
        None => return,
    };

    let path = service_cgroup_path(&root, "nonexistent_hash", "nonexistent_svc");
    // Should return Ok even though path doesn't exist
    remove_service_cgroup(&path).unwrap();
}

/// Config cgroup with a child service cgroup → remove_config_cgroup should return Ok (ENOTEMPTY suppressed).
#[test]
fn test_remove_config_cgroup_notempty() {
    let root = match require_cgroupv2() {
        Some(r) => r,
        None => return,
    };

    let hash = test_hash("notempty");
    create_service_cgroup(&root, &hash, "child-svc").unwrap();

    // Try to remove config cgroup while child exists — should be Ok (ENOTEMPTY suppressed)
    remove_config_cgroup(&root, &hash).unwrap();

    // The config dir should still exist (because child is there)
    assert!(root.join(&hash).exists());

    // Cleanup
    force_cleanup_service_cgroup(&service_cgroup_path(&root, &hash, "child-svc"));
    let _ = remove_config_cgroup(&root, &hash);
}
