//! Cross-platform pipe utilities.
//!
//! Provides `pipe_cloexec()` which creates a pipe with both ends marked
//! close-on-exec. On Linux this uses the atomic `pipe2(O_CLOEXEC)`;
//! on macOS it falls back to `pipe()` + `fcntl(F_SETFD, FD_CLOEXEC)`.

use std::os::fd::OwnedFd;

/// Create a pipe with both ends marked `CLOEXEC`.
///
/// Uses `pipe2(O_CLOEXEC)` where available (Linux, FreeBSD, etc.),
/// falls back to `pipe()` + `fcntl` on platforms without `pipe2` (macOS).
pub fn pipe_cloexec() -> Result<(OwnedFd, OwnedFd), nix::Error> {
    pipe_cloexec_impl()
}

#[cfg(not(target_os = "macos"))]
fn pipe_cloexec_impl() -> Result<(OwnedFd, OwnedFd), nix::Error> {
    nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC)
}

#[cfg(target_os = "macos")]
fn pipe_cloexec_impl() -> Result<(OwnedFd, OwnedFd), nix::Error> {
    let (read_fd, write_fd) = nix::unistd::pipe()?;
    set_cloexec(&read_fd)?;
    set_cloexec(&write_fd)?;
    Ok((read_fd, write_fd))
}

/// Remove the `CLOEXEC` flag from a file descriptor so it survives `exec`.
pub fn remove_cloexec(fd: &OwnedFd) -> Result<(), nix::Error> {
    nix::fcntl::fcntl(
        fd,
        nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::empty()),
    )?;
    Ok(())
}

/// Set the `CLOEXEC` flag on a file descriptor.
#[cfg(target_os = "macos")]
fn set_cloexec(fd: &OwnedFd) -> Result<(), nix::Error> {
    nix::fcntl::fcntl(
        fd,
        nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC),
    )?;
    Ok(())
}
