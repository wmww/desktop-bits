//! Compiled-in values. There is no production configuration file, environment override, test
//! display or alternate lock path.

use std::path::PathBuf;

/// The only place the gate looks for a compositor socket.
pub const DISPLAY_ROOT: &str = "/run/user/0";

/// /run is tmpfs, so this will not exist after a boot; the gate creates it safely.
pub const LOCK_PATH: &str = "/run/sudo-prompt.lock";

#[cfg(not(feature = "test-seams"))]
mod inner {
    use super::*;

    pub fn display_root() -> PathBuf {
        DISPLAY_ROOT.into()
    }

    pub fn lock_path() -> PathBuf {
        LOCK_PATH.into()
    }

    /// Everything the gate validates is expected to be owned by root.
    pub fn owner_uid() -> u32 {
        0
    }

    pub fn check_privilege() -> Result<(), String> {
        // SAFETY: geteuid is always safe.
        let euid = unsafe { libc::geteuid() };
        if euid != 0 {
            return Err(format!("must run as root (euid {euid}); invoke it through sudo"));
        }
        Ok(())
    }
}

/// Test seams. Never enabled in the installed binary: they let the UI be driven in a nested
/// compositor as an ordinary user, which would otherwise be impossible to do at all.
#[cfg(feature = "test-seams")]
mod inner {
    use super::*;

    fn env_path(name: &str, default: &str) -> PathBuf {
        std::env::var_os(name).map(PathBuf::from).unwrap_or_else(|| default.into())
    }

    pub fn display_root() -> PathBuf {
        env_path("SUDO_PROMPT_TEST_DISPLAY_ROOT", DISPLAY_ROOT)
    }

    pub fn lock_path() -> PathBuf {
        env_path("SUDO_PROMPT_TEST_LOCK_PATH", LOCK_PATH)
    }

    pub fn owner_uid() -> u32 {
        // SAFETY: geteuid is always safe.
        unsafe { libc::geteuid() }
    }

    pub fn check_privilege() -> Result<(), String> {
        Ok(())
    }
}

pub use inner::{check_privilege, display_root, lock_path, owner_uid};
