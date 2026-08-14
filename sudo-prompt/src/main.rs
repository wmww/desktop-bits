//! `sudo-prompt` — the sole sudo gate.
//!
//! No command runs as root unless a human saw its argv on a root-owned surface and approved it.
//! The gate, not sudoers, decides which command runs; sudoers only decides who may raise a prompt.
//!
//! It has a fixed security presentation and accepts only environment assignments and an argv after
//! `--`. There are no UI, surface, display, timing, lock, theme or config options: a sudoers
//! approved binary with prompt options would let a requester disable the settle delay, pick a
//! weaker surface, or narrate a privileged action as something harmless.

use sudo_prompt::gate::{self, Fail, DENIED_MESSAGE, EXIT_FAILURE};

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .format_timestamp_millis()
        .init();

    match gate::run() {
        Fail::Denied => {
            eprintln!("{DENIED_MESSAGE}");
            std::process::exit(EXIT_FAILURE);
        }
        Fail::Error(msg) => {
            // A command that itself exits 125 is indistinguishable from this; stderr disambiguates.
            eprintln!("sudo-prompt: {msg}");
            std::process::exit(EXIT_FAILURE);
        }
    }
}
