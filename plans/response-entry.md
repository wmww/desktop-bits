# Plan: an optional response entry above the buttons

A single-line text box just above the button row, placeholder "Response". If the human types
anything, the answer carries it back to the caller on stderr: `User response: ...` printed before
the command's output on approval, or after the exact denial message on denial. Empty box → nothing
printed, byte-identical to today.

The point is agents: "next time do it this other way" plus an accept or a deny is a channel the
exit status alone cannot carry. The gate has it always (the gate has no options, and a
caller-visible flag would be a caller-controlled one); `permission-prompt` gets it behind a flag,
off by default.

## Why this is sound (record in notes when done)

- **The gate's "no IM context" invariant changes shape.** A `GtkEntry` creates a `GtkIMContext`,
  which the threat-model notes currently cite as absent. Still sound: the entry's IM context lives
  on the *gate's own* root-authenticated connection, wlbouncer denies input-method and
  virtual-keyboard globals to every non-root uid, and IM commits *text*, never key events —
  approval still requires a fresh physical key press or pointer press delivered by the compositor,
  read in the capture phase before any widget. Nothing an attacker holds can reach the entry, and
  nothing the entry does can answer the prompt. Update the note's prose from "creates no IM
  context" to this argument.
- **The settle cap cannot be tripped by typing**, because the entry is insensitive until settled
  (wired into `Dialog::set_settled` with the buttons). An insensitive entry cannot take focus, so
  during settling the capture-phase key controller still swallows everything and the
  claimed-click gesture still eats pointer presses. Typing only becomes possible once the prompt
  is live, and `Settle::input` is a no-op after settling.
- **Enter and Escape keep their global meaning while typing.** They are handled (and stopped) in
  the capture phase before the entry can see them, so Enter approves-with-response and Escape
  denies-with-response. The entry never emits `activate`. This is deliberate: a response is an
  annotation on the answer, not a third answer.
- The response is human-typed on the trusted surface; the caller it is printed to is the
  requester. No new trust edge. It is *disclosure to the requester*, though: the human should not
  type secrets there, which "Response" as a placeholder does not invite.
- The denial line stays exactly `User denied sudo :(` on its own line — callers grep for it — with
  the response on the following line.

## Shared UI (`permission-prompt-ui`)

- `DialogSpec` gains `response: bool`. `dialog::build` appends, between the fields and the button
  row, a `gtk::Entry` with placeholder text `"Response"`, full width. Built by a new
  `text::entry()` — the source lint already reserves `gtk::Entry` to `text.rs`; extend the lint's
  token list with the entry APIs used (`set_placeholder_text`, `EntryBuffer`) if they aren't
  covered. No markup concerns (entries render literally), but keep construction in `text.rs`
  anyway: one audited module makes text widgets.
- **One `gtk::EntryBuffer` shared by every window's entry** (one dialog per output), created once
  per `run` and stored where the phase loop of `plans/minimize-chip.md` won't recreate it, so the
  text stays in sync across outputs and survives a future minimize/expand cycle.
- `Dialog` gains `response: Option<gtk::Entry>`; `set_settled` toggles its sensitivity with the
  buttons. `log_geometry` logs a `geometry: response X Y W H` line so the GUI tests can click it.
- `run` returns `Answer { verdict: Verdict, response: Option<String> }` instead of a bare
  `Verdict`. `Verdict` itself is unchanged (no churn at match sites). The response is captured
  from the buffer in `finish`, trimmed of nothing, mapped to `None` when empty. Only `Approved`
  and `Denied` carry it — a settle-cap, signal or error exit was not a human answer, and printing
  a half-typed sentence under an error would misattribute it.

### Input state machine (`wire_input`)

The one real change. Current rule: stop every key except Ctrl+C/Ctrl+Insert. New rule, still in
the capture phase:

1. Track `held` and call `settle.input` exactly as today, for every key.
2. Enter/Escape: handle globally (approve/deny when settled and fresh), always `Stop`.
3. Copy chord: `Proceed`, as today.
4. Anything else: `Proceed` **iff the window's focused widget is the response entry** (it can only
   hold focus once sensitive, i.e. settled); otherwise `Stop`.

Consequences to verify rather than assume: Tab while the entry is focused moves focus to a label
(harmless — labels select-on-focus is off, and Enter/Escape stay global); Ctrl+V pastes into the
entry (GtkEntry strips newlines from pasted text — add a test asserting the buffer stays
single-line); the entry's context menu is reachable by right-click, which is fine.

Focus handling is untouched: `drop_focus` still leaves nothing focused on map and on refocus, so
the user clicks the entry to type. A refocused window un-settles (existing behaviour), which
briefly disables the entry; the buffer keeps the text.

## Gate side (`sudo-prompt`)

- `present::build` sets `response: true` unconditionally.
- `gate::run` threads the response out. `Fail::Denied` becomes `Fail::Denied { response }` (or the
  response returns beside `Fail`); `main.rs` prints `DENIED_MESSAGE`, then
  `User response: ...` on the next stderr line, then exits 125.
- On approval, print `User response: ...` to stderr immediately before `exec::tighten_fds`/
  `execve` — being pre-exec is what guarantees it precedes any command output.
- Defensive sanitisation at the print site: the entry cannot hold newlines, but the printed line
  is a protocol read by agents, so replace any control character with a space (or escape via the
  existing `Escaped` machinery) before printing. One line in, one line out, always.
- The decision log/journal record gains the response: append it to the `log::info!` line and add
  `SUDO_PROMPT_RESPONSE` to the journal fields, escaped through the same path as the command.
  Omit the field when empty. "Why was this denied" belongs in the audit trail too.

## Generic presenter (`permission-prompt`)

- New clap flag `--response` (bool, default off). Off → `response: false`, no entry, no geometry
  line, zero change for existing callers.
- On → entry shown; on exit (approved *or* denied — there is no denial message here, callers read
  the exit status) print `User response: ...` to stderr when non-empty. Exit codes unchanged.
  Cap/signal/error exits print nothing, as at the gate.

## Order of work

1. `permission-prompt-ui`: `text::entry`, `DialogSpec.response`, shared buffer, `set_settled`,
   `Answer`, input-machine change, geometry line. Update both binaries mechanically
   (`response: false` / discard the response) so the workspace compiles green with no behaviour
   change. Unit tests + source lint.
2. Gate wiring: `response: true`, both print sites, sanitiser, journal field. Unit test the
   sanitiser and the Rendered spec.
3. `permission-prompt --response` flag and print site.
4. GUI tests.
5. Doc updates.

## Tests

`tests/gui-test.sh`, marker/geometry driven as ever (click target from `geometry: response`):

- Type a response, click approve → stderr has the `User response:` line *before* the approved
  command's own output; command exit status passed through.
- Type a response, press Escape while the entry is focused → exit 125, stderr is exactly the
  denial line then the response line.
- Enter while the entry is focused approves (no double-activation, one verdict).
- Empty entry, approve and deny → stderr byte-identical to today.
- Click the entry before `controls live` → nothing focuses, prompt still settles normally and
  does not hit the cap; then type after `controls live` and answer.
- `permission-prompt` without `--response` → no `geometry: response` line; with it → entry works
  and the line prints on both outcomes.

Unit tests: entry present/absent per spec flag; insensitive until settled; `Answer` carries the
text only for Approved/Denied; sanitiser replaces control bytes; multi-line paste ends up
single-line in the buffer; CSS colour test still green if any new class is added; source lint
green with the extended token list.

## Doc updates when done

- `notes/permission-prompt.md`: the IM-context paragraph in the threat model (see "why this is
  sound"), the Input section (keys reach the focused entry only, Enter/Escape stay global, entry
  disabled until settled), "What the dialog says" (the entry row), the gate's exit-status/stderr
  contract (response line format, its place relative to the denial message and the exec), the
  journal field, and `permission-prompt`'s flag.
- Delete this plan.
