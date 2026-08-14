# The first click after the controls go live is lost if the pointer never moved

Pre-existing, found while adding the response entry. Affects both buttons and the entry.

**Symptom.** If the pointer is resting where a control will be *before* that control becomes
sensitive — the settle period disables all of them — then the first click after `controls live` does
nothing. A second click, or any pointer motion in between, works. So a human whose mouse happens to
sit over "Run as root" when the prompt appears has to click it twice.

**Cause.** Not our claimed-press gesture: hovering with no click at all reproduces it, and a
pre-settle click somewhere *else* on the surface does not. GTK picks the pointer's target widget on
crossing and motion, and does not re-pick when a widget under a motionless pointer changes
sensitivity — so the press is still delivered to whatever was picked while the control was
insensitive (the dialog box), which does nothing with it.

**Repro** (nested sway, `tests/gui-test.sh` helpers):

~~~
launch /bin/echo x; presented
wdotool mousemove $(center approve)   # no click needed
settled
wdotool mousemove $(center approve) click 1   # same coordinates: no motion event
# nothing happens; a second identical click approves
~~~

**Candidate fixes**, none tried:

- Stop using sensitivity for the settling look. Keep the controls sensitive and grey them with a
  CSS class instead; safety would then rest entirely on the capture-phase gesture that already
  swallows every press until settled, and on the key controller that swallows every key. Weakens
  the "an insensitive entry cannot take focus" argument in `notes/permission-prompt.md`.
- Find a public GTK4 call that re-picks the pointer target, and make `Dialog::set_settled` do it.
  Nothing obvious exists; `gtk_widget_set_sensitive` is not enough and hiding/showing a control
  would move the buttons at the moment they go live, which the layout deliberately avoids.
- Report it upstream as a GTK bug: a sensitivity change under a stationary pointer arguably should
  re-pick.

`tests/gui-test.sh` moves the pointer off and back before the post-settle click, so the suite
tests the intended behaviour rather than this wart. Fixing this should let that motion go away.
