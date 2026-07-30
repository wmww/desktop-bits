//! One prompt at a time. A concurrent request fails closed; prompts are never queued, there is no
//! alternate lock path, and the gate does not continue without the lock.
//!
//! The lock lives on the open file description, so the kernel releases it on any exit including
//! SIGKILL — no PID file or stale-lock recovery is needed. (Unlike the session lock, which is
//! exactly the opposite.) The fd stays CLOEXEC so an approved long-running command does not hold it.

use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::sys::errno_message;

#[derive(Debug)]
pub struct LockFile {
    fd: RawFd,
}

impl Drop for LockFile {
    fn drop(&mut self) {
        // SAFETY: our fd, not used again.
        unsafe { libc::close(self.fd) };
    }
}

/// Creating the file is safe without an installer or a tmpfiles.d entry because /run itself is
/// root-owned and only root-writable, so no other uid can win the race or plant a symlink.
pub fn acquire(path: &Path, expect_uid: u32) -> Result<LockFile, String> {
    let mut c_path = path.as_os_str().as_bytes().to_vec();
    c_path.push(0);

    // SAFETY: c_path is NUL terminated; the flags and mode are constants.
    let fd = unsafe {
        libc::open(
            c_path.as_ptr() as *const libc::c_char,
            libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(errno_message(&format!("cannot open lock file {}", path.display())));
    }
    let lock = LockFile { fd };

    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: fd is open, st is the right type.
    if unsafe { libc::fstat(fd, &mut st) } < 0 {
        return Err(errno_message("fstat on the lock file"));
    }
    if st.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(format!("lock file {} is not a regular file", path.display()));
    }
    if st.st_uid != expect_uid {
        return Err(format!(
            "lock file {} is owned by uid {}, not {expect_uid}",
            path.display(),
            st.st_uid
        ));
    }
    if st.st_mode & (libc::S_IWGRP | libc::S_IWOTH) != 0 {
        return Err(format!("lock file {} is group or other writable", path.display()));
    }

    // SAFETY: fd is open.
    if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } < 0 {
        return Err(format!(
            "another sudo-prompt already holds {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }

    Ok(lock)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uid() -> u32 {
        // SAFETY: always safe.
        unsafe { libc::geteuid() }
    }

    #[test]
    fn creates_and_locks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        let held = acquire(&path, uid()).unwrap();
        assert!(path.exists());
        drop(held);
    }

    #[test]
    fn a_concurrent_request_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        let _held = acquire(&path, uid()).unwrap();
        let err = acquire(&path, uid()).unwrap_err();
        assert!(err.contains("already holds"), "{err}");
    }

    #[test]
    fn releasing_lets_the_next_request_in() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        drop(acquire(&path, uid()).unwrap());
        acquire(&path, uid()).expect("second acquire");
    }

    #[test]
    fn rejects_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, b"").unwrap();
        let path = dir.path().join("lock");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        let err = acquire(&path, uid()).unwrap_err();
        assert!(err.contains("cannot open lock file"), "{err}");
    }

    #[test]
    fn rejects_a_wrong_owner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        std::fs::write(&path, b"").unwrap();
        let err = acquire(&path, uid() + 1).unwrap_err();
        assert!(err.contains("owned by uid"), "{err}");
    }

    #[test]
    fn rejects_group_writable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660)).unwrap();
        let err = acquire(&path, uid()).unwrap_err();
        assert!(err.contains("group or other writable"), "{err}");
    }

    #[test]
    fn rejects_a_non_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        std::fs::create_dir(&path).unwrap();
        let err = acquire(&path, uid()).unwrap_err();
        // Opening a directory O_RDWR fails with EISDIR before the fstat check.
        assert!(err.contains("cannot open lock file") || err.contains("not a regular file"), "{err}");
    }
}
