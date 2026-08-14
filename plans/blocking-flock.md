# Plan: queue concurrent requests instead of failing them

Today a second `sudo` request fails immediately: `lockfile::acquire` takes the flock with
`LOCK_NB` and any contention is an operational error (exit 125). Change it to wait. Fail-fast was
never a security control — a caller can retry in a loop — and it punishes exactly the legitimate
cases: two scripts racing, or the human running sudo themselves while a prompt is pending
(which the minimize feature makes routine).

## Mechanism

Plain blocking `flock(2)`, not a sleep loop: the kernel queues the waiter and wakes it instantly on
release. In `lockfile::acquire`, after the existing open/fstat checks:

1. Try `LOCK_EX | LOCK_NB` first. Success → done, silently, as today.
2. On `EWOULDBLOCK`, emit one `log::warn!` line — "another sudo-prompt is active; waiting for it" —
   so a request that hangs in a terminal explains itself. (env_logger writes to stderr, which under
   sudo is the caller's terminal; the prompts default to warn so it is visible.)
3. Then block in `flock(fd, LOCK_EX)`, retrying on `EINTR`. Log the acquisition at debug.

No timeout: once acquired the prompt already waits indefinitely, and the wait is interruptible.

Signals need no new handling. At the point the flock is taken (early in `gate::run`, before
capture/scrub/display/GTK), the UI's signal handlers are not yet installed, so SIGINT/SIGTERM/SIGHUP
have default disposition: the waiting process dies, the kernel closes the fd, and no lock state
leaks — the same behaviour every pre-prompt phase has today. Ctrl+C on a queued request therefore
works immediately. The `EINTR` retry is defensive only.

Nothing else moves. The flock stays where it is in the order of operations, so a request that
queued for minutes still does display selection and environment capture fresh when its turn comes.
The fd stays CLOEXEC, so the lock is still released at exec and nested sudo under an approved
command still works. `sudo -n` still fails fast (passes through to real sudo, denied there).

## Accepted consequences (document, don't fix)

- A spam storm becomes a queue of prompts: each denial advances the queue rather than ending it,
  costing ~a second of settle each. Within the already-accepted DoS scope — a retrying attacker
  could produce this today — but the UX changes and the notes' DoS paragraph should say so.
- An orphaned waiter: SIGHUP covers the requesting terminal dying, but a SIGKILLed caller leaves a
  queued gate that eventually prompts for a command nobody is waiting on. The human denies it.
  No clean detection exists; harmless.
- Wake order under contention is unspecified, not FIFO. Irrelevant at human scale.

## Tests

- `lockfile.rs` unit tests: replace `a_concurrent_request_fails_closed` with the new contract —
  a non-blocking probe (however the two-step is structured) fails while held, and a thread blocked
  in `acquire` completes promptly once the holder drops (thread + channel, join with a generous
  timeout; assert completion, never assert on how long the block lasted).
- `tests/gui-test.sh`: start a gate, start a second request, wait for its "another sudo-prompt is
  active" log marker, deny the first, then see the second present and deny it too. Flip any
  existing check that asserts a concurrent request exits 125 immediately.

## Doc updates

- `lockfile.rs` module comment ("prompts are never queued" is no longer true).
- `notes/permission-prompt.md`: the flock paragraph, and the accepted-DoS wording.
