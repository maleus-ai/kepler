#[cfg(unix)]
use std::ffi::CStr;

/// Get all group IDs for a given user (cross-platform).
///
/// Handles the macOS `c_int` vs Linux `gid_t` type difference internally.
/// Uses a retry loop to handle buffer growth when the initial buffer is too small.
#[cfg(unix)]
pub fn getgrouplist(user: &CStr, base_gid: u32) -> Result<Vec<u32>, nix::errno::Errno> {
    #[cfg(target_os = "macos")]
    type GidType = libc::c_int;
    #[cfg(not(target_os = "macos"))]
    type GidType = libc::gid_t;

    let mut ngroups: libc::c_int = 16;
    let mut groups: Vec<GidType> = vec![0; ngroups as usize];

    loop {
        let prev_len = groups.len();
        let ret = unsafe {
            libc::getgrouplist(
                user.as_ptr(),
                base_gid as GidType,
                groups.as_mut_ptr(),
                &mut ngroups,
            )
        };

        if ret == -1 {
            let new_len = if (ngroups as usize) > prev_len {
                // OS told us the required size
                ngroups as usize
            } else {
                // macOS may not update ngroups on failure; double the buffer
                prev_len.saturating_mul(2)
            };
            // Cap at a reasonable maximum to prevent infinite growth
            if new_len > 1024 {
                return Err(nix::errno::Errno::EINVAL);
            }
            ngroups = new_len as libc::c_int;
            groups.resize(new_len, 0);
        } else {
            groups.truncate(ngroups as usize);
            return Ok(groups.into_iter().map(|g| g as u32).collect());
        }
    }
}

/// Check if a user (by UID) has a given GID in their group list.
///
/// Resolves UID to username, then queries via `getgrouplist()`.
/// Returns `false` if the UID cannot be resolved or the lookup fails.
#[cfg(unix)]
pub fn uid_has_gid(uid: u32, target_gid: u32) -> bool {
    let user = match nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid)) {
        Ok(Some(u)) => u,
        _ => return false,
    };

    let c_name = match std::ffi::CString::new(user.name.as_str()) {
        Ok(n) => n,
        Err(_) => return false,
    };

    match getgrouplist(&c_name, user.gid.as_raw()) {
        Ok(gids) => gids.contains(&target_gid),
        Err(_) => false,
    }
}

/// Query the OS limit on supplementary groups (NGROUPS_MAX).
/// Falls back to 65536 if the sysconf call fails.
#[cfg(unix)]
pub fn ngroups_max() -> usize {
    let val = unsafe { libc::sysconf(libc::_SC_NGROUPS_MAX) };
    if val < 0 { 65536 } else { val as usize }
}

/// Set the supplementary group list for the current process.
#[cfg(unix)]
pub fn setgroups(gids: &[u32]) -> std::io::Result<()> {
    let gids: Vec<libc::gid_t> = gids.to_vec();
    let ret = unsafe { libc::setgroups(gids.len() as _, gids.as_ptr()) };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Load all supplementary groups for a user and set them.
#[cfg(unix)]
pub fn initgroups(username: &str, gid: u32) -> std::io::Result<()> {
    let c_name = std::ffi::CString::new(username)
        .map_err(|_| std::io::Error::other("invalid username for initgroups"))?;
    let ret = unsafe { libc::initgroups(c_name.as_ptr(), gid as _) };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
