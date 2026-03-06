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

    // Can't rmdir a cgroup with processes in it — move ourselves back
    // to the root cgroup first
    let root_procs = PathBuf::from("/sys/fs/cgroup/cgroup.procs");
    std::fs::write(&root_procs, std::process::id().to_string()).unwrap();

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
