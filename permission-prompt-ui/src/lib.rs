//! Shared UI primitives for `sudo-prompt` and `permission-prompt`.
//!
//! This is an internal workspace API, not a stable one. It exists so the two binaries can share
//! rendering code without sharing a security contract: `sudo-prompt` is an authorization boundary,
//! `permission-prompt` is not.
//!
//! The load-bearing property of this crate is that it owns every widget that can display text (see
//! [`text`]), and that caller data reaches those widgets only as an [`untrusted::Escaped`] built
//! from an [`untrusted::Untrusted`].

pub mod app;
pub mod dialog;
pub mod settle;
mod text;
pub mod untrusted;

pub use app::{
    init, install_panic_hook, run, unlock_and_sync, Answer, PromptConfig, SurfaceMode, Verdict,
};
pub use dialog::{DialogSpec, Field, Style};
pub use settle::{SETTLE, SETTLE_CAP};
pub use untrusted::{Escaped, Untrusted};
