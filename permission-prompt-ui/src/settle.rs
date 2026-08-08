//! The quiet period that has to pass before the prompt will accept an answer.
//!
//! Settling is a quiet period, not a fixed window from mapping: a fast typist mid-sentence would
//! otherwise hit Enter well inside a second, and a DPMS-blanked output would let a fixed window
//! elapse with nothing on screen.

use std::time::{Duration, Instant};

/// Quiet period the prompt needs before it will accept an answer.
pub const SETTLE: Duration = Duration::from_millis(400);

/// Time from the last non-input restart after which we give up and deny. Bounds a stuck or spammed
/// key. Absolute rather than a multiple of [`SETTLE`]: what makes 5s the right ceiling is how long a
/// person will stare at a prompt that refuses to unlock, which has nothing to do with the length of
/// the quiet period.
pub const SETTLE_CAP: Duration = Duration::from_millis(5000);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettleState {
    /// Nothing presented yet, or the quiet period is still running.
    Waiting,
    /// Controls live; the prompt now waits indefinitely for an answer.
    Settled,
    /// Input kept arriving past the cap. Fail closed: deny.
    CapExceeded,
}

pub struct Settle {
    duration: Duration,
    cap: Duration,
    /// Start of the current quiet period. `None` until the first surface is presented.
    quiet_start: Option<Instant>,
    /// Start of the current settling attempt. Only a non-input restart moves this.
    attempt_start: Option<Instant>,
    settled: bool,
}

impl Settle {
    pub fn new(duration: Duration, cap: Duration) -> Self {
        Settle {
            duration,
            cap,
            quiet_start: None,
            attempt_start: None,
            settled: false,
        }
    }

    /// A surface presented for the first time, or keyboard focus came back. Restarts both the
    /// quiet period and the cap, and un-settles an already live prompt: a hotplugged output must
    /// not show a prompt that is already answerable, and neither must a refocused one.
    pub fn restart(&mut self, now: Instant) {
        self.quiet_start = Some(now);
        self.attempt_start = Some(now);
        self.settled = false;
    }

    /// A key press, key release or pointer button. Restarts the quiet period but not the cap —
    /// that is what keeps a stuck key bounded while letting a late-waking output still settle.
    /// Once settled, input is the answer rather than a disturbance.
    pub fn input(&mut self, now: Instant) {
        if self.settled || self.quiet_start.is_none() {
            return;
        }
        self.quiet_start = Some(now);
    }

    pub fn poll(&mut self, now: Instant) -> SettleState {
        if self.settled {
            return SettleState::Settled;
        }
        let (Some(quiet), Some(attempt)) = (self.quiet_start, self.attempt_start) else {
            return SettleState::Waiting;
        };
        if now.saturating_duration_since(quiet) >= self.duration {
            self.settled = true;
            return SettleState::Settled;
        }
        if now.saturating_duration_since(attempt) >= self.cap {
            return SettleState::CapExceeded;
        }
        SettleState::Waiting
    }

    pub fn is_settled(&self) -> bool {
        self.settled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: Duration = Duration::from_millis(1000);
    const CAP: Duration = Duration::from_millis(5000);

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn waits_until_a_surface_is_presented() {
        let t0 = Instant::now();
        let mut s = Settle::new(D, CAP);
        assert_eq!(s.poll(at(t0, 10_000)), SettleState::Waiting);
    }

    #[test]
    fn settles_after_a_quiet_period() {
        let t0 = Instant::now();
        let mut s = Settle::new(D, CAP);
        s.restart(t0);
        assert_eq!(s.poll(at(t0, 999)), SettleState::Waiting);
        assert_eq!(s.poll(at(t0, 1000)), SettleState::Settled);
    }

    #[test]
    fn input_restarts_the_quiet_period() {
        let t0 = Instant::now();
        let mut s = Settle::new(D, CAP);
        s.restart(t0);
        s.input(at(t0, 900));
        assert_eq!(s.poll(at(t0, 1500)), SettleState::Waiting);
        assert_eq!(s.poll(at(t0, 1900)), SettleState::Settled);
    }

    #[test]
    fn continuous_input_hits_the_cap_and_denies() {
        let t0 = Instant::now();
        let mut s = Settle::new(D, CAP);
        s.restart(t0);
        let mut ms = 0;
        loop {
            ms += 100;
            s.input(at(t0, ms));
            match s.poll(at(t0, ms)) {
                SettleState::Waiting => {}
                other => {
                    assert_eq!(other, SettleState::CapExceeded);
                    assert_eq!(ms, 5000);
                    break;
                }
            }
            assert!(ms < 10_000, "cap never reached");
        }
    }

    #[test]
    fn input_does_not_extend_the_cap() {
        let t0 = Instant::now();
        let mut s = Settle::new(D, CAP);
        s.restart(t0);
        s.input(at(t0, 4900));
        assert_eq!(s.poll(at(t0, 5000)), SettleState::CapExceeded);
    }

    #[test]
    fn a_non_input_restart_also_restarts_the_cap() {
        let t0 = Instant::now();
        let mut s = Settle::new(D, CAP);
        s.restart(t0);
        // A late-waking output presents at 4.9s: the prompt gets a fresh chance to settle.
        s.restart(at(t0, 4900));
        assert_eq!(s.poll(at(t0, 5000)), SettleState::Waiting);
        assert_eq!(s.poll(at(t0, 5900)), SettleState::Settled);
    }

    #[test]
    fn a_restart_unsettles_a_live_prompt() {
        let t0 = Instant::now();
        let mut s = Settle::new(D, CAP);
        s.restart(t0);
        assert_eq!(s.poll(at(t0, 1000)), SettleState::Settled);
        s.restart(at(t0, 2000));
        assert!(!s.is_settled());
        assert_eq!(s.poll(at(t0, 2500)), SettleState::Waiting);
    }

    #[test]
    fn input_after_settling_is_not_a_disturbance() {
        let t0 = Instant::now();
        let mut s = Settle::new(D, CAP);
        s.restart(t0);
        assert_eq!(s.poll(at(t0, 1000)), SettleState::Settled);
        s.input(at(t0, 1100));
        assert_eq!(s.poll(at(t0, 1100)), SettleState::Settled);
    }
}
