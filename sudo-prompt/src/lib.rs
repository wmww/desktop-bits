//! `sudo-prompt` internals, split out so tests — including the shim's — can drive the real
//! parsers rather than a copy of them.

pub mod cli;
pub mod cmdenv;
pub mod config;
pub mod display;
pub mod envsetup;
pub mod exec;
pub mod gate;
pub mod interp;
pub mod journal;
pub mod lockfile;
pub mod present;
pub mod sys;
