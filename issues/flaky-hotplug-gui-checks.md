# The two hotplug-removal checks in gui-test.sh are flaky

Seen while changing the command environment (an unrelated area). One run of `tests/gui-test.sh`:

~~~
PASS  a hotplugged output gets its own lock surface
FAIL  hotplug remove: status=125
FAIL  last output lost: status=125
~~~

An immediate re-run passed all 21, as did four later runs — once in six. Nothing in between changed,
and the failing checks are the two that `swaymsg output HEADLESS-2 unplug` drives.

125 is the gate's denial exit, so the prompt went away rather than hanging — it failed closed, which
is the safe direction. The likely cause is the fixed `sleep 2` after each `swaymsg` call in those
two checks racing sway's output teardown: if the gate observes the last output leaving before the
harness reads `status`, the run is already over and the checks read a finished gate as a failure.

Suspect the harness, not the gate, until shown otherwise — but confirm that, because "the prompt
denied because an output went away" and "the prompt denied because it lost its surfaces early" are
the same exit code and only the second is a bug. Worth replacing the sleeps with a wait on a
`gate.log` marker, the way `settled()` already does.
