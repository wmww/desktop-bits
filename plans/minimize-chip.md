# Plan: minimize the gate prompt to a corner chip

Let the human shrink an active sudo prompt to a small layer surface pinned to the bottom-right of
each output, so they can use the desktop to investigate before deciding. The chip shows the command
(one line, cut off if long) and the requesting user in smaller text, plus an X that denies. Clicking
anywhere else on the chip expands back to the full session-lock prompt with a fresh settling period.

## Why this is sound (record in notes when done)

The chip is deliberately **powerless**: its only actions are deny and escalate back to the
exclusive surface. Approval never leaves the session-lock surface, always behind a fresh settle.

- The chip lives on the overlay layer, which non-root uids can also draw on
  (`issues/overlay-layer-spoofing.md`), so it is coverable and spoofable. Accepted: covering it
  hides a pending prompt (the accepted DoS class), misleading text on a fake is corrected the
  moment the real lock surface shows the real argv, and tricking a click yields either a denial
  (safe) or an expand. Add the chip to that issue's "what is left" list as accepted-by-design.
- Fresh settle on expand is load-bearing, not cosmetic: it defeats bait-a-double-click, because the
  second click is input and restarts the quiet period. This falls out of existing machinery — new
  lock surfaces restart settling on presentation.
- The prompt always **starts** full/locked; only a human interaction on the lock surface can
  minimize it. The gate has no options, so a caller cannot request starting minimized. Keep it so.
- Relock failure on expand (someone else took the only lock while we were minimized) is a denial,
  consistent with "`failed` is a denial".
- The flock is held throughout, so other requests stay blocked on this one
  (see `plans/blocking-flock.md`; the two plans are independent).

## Refactor structure

The core problem: today `permission_prompt_ui::run` is single-shot — one main loop, one verdict,
one unlock in straight-line code at the end, because unlocking inside a callback deadlocks on the
state `RefCell`. Minimize needs unlock-without-exiting and a later relock. Preserve the invariant
instead of fighting it: **lock transitions only ever happen between main loops, in straight-line
code in `run`**. Each lock epoch and each chip period is its own *phase* with its own fresh `Inner`
and its own `glib::MainLoop`.

~~~
pub fn run(cfg) -> Verdict {
    install_css(); install_signal_handlers();          // once
    loop {
        // today's run(), minus the loop: lock, surfaces, settle, input machine
        match run_full_phase(&cfg) {
            Full::Decided(v) => { unlock_and_sync(); clear ACTIVE_LOCK; return v }
            Full::Minimize   => { unlock_and_sync(); clear ACTIVE_LOCK; }
        }
        // chip surfaces, no settle machinery, no lock
        match run_chip_phase(&cfg) {
            Chip::Expand     => continue,              // next iteration relocks from scratch
            Chip::Decided(v) => return v,              // Denied / DeniedSignal / Error
        }
    }
}
~~~

Consequences of that shape:

- A fresh `gtk4_session_lock::Instance` per lock epoch. Relocking in one process is untested
  library territory — if it misbehaves, that is an upstream finding for
  `issues/gtk4-session-lock-warts.md`, not something to work around silently.
- Everything per-phase resets for free: `Settle` (fresh quiet period and fresh `SETTLE_CAP`),
  `held` keys, `shown_settled`, `windows`, `pending`, `locked`, and the 2s zero-output timer
  reruns each lock epoch.
- The signal poll moves out of the settle timer into its own 50ms timer that runs in *both* phases
  (the chip phase has no settle timer). A signal while minimized is a denial, as everywhere.
- The panic hook needs no change: `unlock_and_sync` no-ops when not locked.

Module layout: `app.rs` keeps `run`, the phase loop, signals, CSS and the shared input machinery;
the chip phase goes in a new `permission-prompt-ui/src/chip.rs`. Split more out of `app.rs` if it
helps, none of it is sacred.

### API between gate and UI

Add to `DialogSpec`: `chip: Option<ChipSpec>` where `ChipSpec { command: Escaped, user: Escaped }`.
`present.rs` fills it (the one-line shell-quoted command, already built, and the requesting
username). `chip.is_some()` is what enables the whole feature — the generic presenter passes `None`
and gets today's behaviour, and no separate config flag exists. Minimize is only honoured in
`SurfaceMode::SessionLock`; the other modes don't seize the screen, so it has no meaning there.

## The full prompt's buttons

Bottom row becomes `[spacer] [minimize] [X] [Run as root]` — deny stays where it is, turned into an
X; minimize sits to its left; approve keeps its text label. The pair should evoke window
decorations while remaining ordinary GTK-styled buttons in the ordinary place.

- Both icons are cairo-drawn (`gtk::DrawingArea` inside the button, `set_draw_func`), not font
  glyphs: the minimize icon has no reliable glyph, and drawing both keeps stroke weight and colour
  identical. Colour comes from `widget.color()` at draw time so the theme and the `:disabled`
  opacity apply, same as text.
- The minimize icon is a small rectangle outline sitting in the lower-right of the icon box —
  it depicts what the button does (the prompt becomes a small rect at the lower-right of the
  screen). The X is two strokes.
- Constructor beside `text::button` (there or in `dialog.rs`; a DrawingArea is not a text widget,
  so the source lint is indifferent — what matters is `can-focus` off, `focus_on_click` off, and an
  accessible label: "Minimize" / "Cancel").
- Padding: keep the row height; icon buttons likely want narrower horizontal padding than
  `8px 20px` so they read as square-ish decoration buttons. New CSS classes stay within the legacy
  colour set (the unit test enforces it).
- `Dialog` gains `minimize: Option<gtk::Button>`; `set_settled` disables it with the others — no
  minimize during settling (consistency beats the de-escalation argument, and it keeps the input
  state machine untouched: no new keys, Escape still denies, Enter still approves).
- `log_geometry` logs a `minimize` rect so the GUI tests can click it.
- Clicking minimize finishes the phase with `Full::Minimize` — same `finish`-style path as
  verdicts, loop quits, unlock happens outside callbacks in `run`.

## The chip

- One chip per output (consistent with the full prompt's per-output surfaces; revisit if the user
  prefers a single chip). Layer shell: `Layer::Overlay`, anchored Bottom+Right with ~16px margins,
  `KeyboardMode::None` — no keyboard, so the chip's input surface is exactly two click targets.
  Exclusive zone 0 (floats over content). Namespace `sudo-prompt-chip`.
- Content, all via `text.rs` with the existing `Escaped` values: the command on one line in small
  monospace, end-ellipsized, and the requesting user smaller and dimmer below. Ellipsis is fine
  *here* — the chip is not the approval surface, and the full argv is one click away; note the
  deliberate contrast with `mono_block`'s never-ellipsize rule where approval is at stake. Wrap
  the chip in the gate's identity: `@error_color` border/accent, so it cannot be mistaken for a
  random notification.
- The X button denies immediately (`Verdict::Denied`, the normal denial exit and message). No
  settle on the chip: denial is the safe direction.
- A click anywhere else on the chip window expands: window-level `GestureClick` in the bubble
  phase so the X's own gesture wins first; verify that in the GUI test.
- Monitors: new output → new chip (reuse the `monitors.connect_items_changed` pattern from
  `start_layer`); last output gone → `Verdict::Error`, same as the full prompt. Removal detection
  can use `connect_destroy`/`invalidate` as `start_layer` does.
- Log markers for the tests: `minimized`, `chip presented`, `expanded`, and `geometry: chip-cancel`.

## Gate side

`gate.rs` barely changes: `present::build` fills `ChipSpec`; verdict handling is untouched (a
chip-deny is an ordinary `Denied`). The decision log line stays one line per decision.

## Order of work

1. Pure refactor, no behaviour change: extract the phase structure (`run_full_phase` returning
   `Full::Decided` only), move the signal poll to its own timer. Full test suite green.
2. Dialog: third button, icon buttons, CSS, geometry line — still inert (no `ChipSpec` yet means
   the minimize button isn't built).
3. `chip.rs`, the phase loop, relock-on-expand with failure-as-denial.
4. `present.rs` `ChipSpec` + gate wiring; log markers.
5. GUI tests, then doc updates.

## Tests

`tests/gui-test.sh` additions, all marker/geometry driven:

- Minimize → `session unlocked` and `chip presented` appear; a plain layer client can now render
  (desktop usable).
- Click chip body → `expanded`, `session locked`, controls go `settling` then `live` (fresh settle
  proven), then deny.
- Click chip X → exit 125 with the exact denial message.
- While minimized, hold the session lock with another client, click chip → gate exits with the
  could-not-lock error (fail closed).
- Output unplug while minimized → error exit (no outputs left).
- Minimize button disabled during settle: click it before `controls live`, prompt stays up.

Unit tests: `ChipSpec` derivation in `present.rs`; icon buttons carry accessible labels; the CSS
colour test covers the new classes; source lint stays green (chip text goes through `text.rs`).

## Doc updates when done

- `notes/permission-prompt.md`: new section (chip is powerless, coverable-and-accepted, starts
  full, fresh settle on expand, relock failure denies, phase/lock-epoch structure).
- `issues/overlay-layer-spoofing.md`: add the chip to "what is left" as accepted-by-design.
- `issues/gtk4-session-lock-warts.md`: anything the relock cycle surfaces.
