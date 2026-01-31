//! Utility functions that don't really belong in any other module.

/// Sets the umask value for this process to a value that makes files and directories only
/// accessible by this user. This applies to all new files and directories created by the process,
/// and is inherited by subprocesses as well.
///
/// This is used by the `translate` binary to make its output and diagnostics directories
/// not-world-accessible.
pub fn set_user_only_umask() {
    // Safety: This is only unsafe because libc marks all its bindings as unsafe by default. The
    // `umask` function cannot induce UB.
    unsafe {
        libc::umask(0o077);
    }
}
