use super::*;

/// Get the current process's user info for testing.
/// Returns (username, uid, gid).
fn current_user() -> (String, u32, u32) {
    let uid = nix::unistd::getuid().as_raw();
    let user = nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
        .expect("should resolve current UID")
        .expect("current UID should exist in passwd");
    (user.name, uid, user.gid.as_raw())
}

#[test]
fn getgrouplist_for_current_user() {
    let (name, _uid, gid) = current_user();
    let c_name = std::ffi::CString::new(name.as_str()).unwrap();
    let groups = getgrouplist(&c_name, gid);
    assert!(groups.is_ok(), "getgrouplist for current user should succeed");
    let groups = groups.unwrap();
    assert!(
        groups.contains(&gid),
        "current user should have their own primary GID {}",
        gid
    );
}

#[test]
fn uid_has_gid_current_user() {
    let (_name, uid, gid) = current_user();
    assert!(
        uid_has_gid(uid, gid),
        "current user (UID {}) should have their primary GID {}",
        uid, gid
    );
}

#[test]
fn uid_has_gid_nonexistent_uid() {
    // Use a very high UID that is unlikely to exist
    assert!(
        !uid_has_gid(u32::MAX - 1, 0),
        "nonexistent UID should return false"
    );
}

#[test]
fn getgrouplist_nonexistent_user_does_not_panic() {
    let c_name = std::ffi::CString::new("__kepler_nonexistent_user_99999__").unwrap();
    // Should return an error, not panic
    let _ = getgrouplist(&c_name, 99999);
}
