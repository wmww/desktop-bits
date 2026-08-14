# A click swallowed during the quiet period leaves that pointer position dead

Found while adding the minimize button. It affects the two controls whose presses are still
claimed — approve and deny — so it is not new; presses over the response box and minimize are let
through during the quiet period and are unaffected.

The window's capture-phase `GestureClick` claims a pointer press that arrives before the prompt has
settled (`app::wire_input`), which is what stops a press from turning into an activation once the
release lands after settling. The cost: after such a claim, a *later* click at the **same pointer
position** never reaches the button under it — the button visibly takes hover, and nothing happens.
Moving the pointer by a few pixels first re-arms it, and then the click works.

Reproduced in a nested sway on GTK 4.22, identically for approve and deny (and, when it was still
claimed, minimize): click once while `controls settling`, wait for `controls live`, click again
without moving — no verdict. The
`tests/gui-test.sh` checks all move the pointer before clicking, which is why the suite never saw
it.

Impact is small but real: a human who clicks the prompt the moment it appears, then clicks the same
button again without moving their hand (a trackpad tap, a still mouse), gets nothing and has to
jiggle the pointer. Failing closed, at least.

Not diagnosed further. Likely GTK's crossing/implicit-grab bookkeeping after a claimed sequence
rather than anything in this repo; the fix might be as small as not claiming the sequence and
instead dropping the press some other way, but that must not weaken the guarantee the claim buys.
