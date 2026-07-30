//! `/usr/local/bin/sudo` — an unprivileged dispatcher in front of the real sudo.
//!
//! This is convenience, not security. Calling /usr/bin/sudo directly grants nothing, because no
//! sudoers rule permits anything but the gate. The shim gets no sudoers entry, links no UI crate,
//! and stays a separate binary rather than a second personality of `sudo-prompt`: it runs as the
//! untrusted caller, while the gate runs as root under a sudoers rule.

pub mod classify;
