# gtk4-layer-shell's session lock API: two rough edges

Found while building `sudo-prompt` against gtk4-layer-shell 1.3.0 (via the `gtk4-session-lock` 0.4
crate). Both are worked around in `permission-prompt-ui/src/app.rs`; this repo's author maintains the
library, so they are worth fixing upstream.

## `monitor` and `locked` fire in either order

The header documents `::monitor` as firing "once for each monitor that exists when a lock is
started", but not whether that is before or after `::locked`. In practice both orders occur on the
same sway with the same code.

That matters because the documented usage (create a window in the `monitor` handler and assign it) is
also where you would present it, and presenting before `locked` is not legal — a lock surface only
exists inside the lock. It also means neither signal can answer "were there any outputs at all?",
which a client that must fail rather than hold a lock with nothing on screen needs to know.

Workaround: create and assign in `monitor`, but queue the `present()` and flush the queue in
`locked`; answer the zero-output question from a 2s timer instead of a signal.

Suggested fix: guarantee `locked` first, or document the order and add a point clients can rely on
for "all initially present monitors have been reported".

## Gdk-CRITICALs when a lock fails after `monitor` fired

When `lock()` fails because another client already holds the lock, and `monitor` had already fired,
tearing down the assigned-but-unmapped window logs:

~~~
Gdk-CRITICAL: gdk_toplevel_focus: assertion 'GDK_IS_TOPLEVEL (toplevel)' failed
Gdk-CRITICAL: gdk_toplevel_set_startup_id: assertion 'GDK_IS_TOPLEVEL (toplevel)' failed
~~~

Harmless — the client is on its way to exiting 125 — but it is noise on exactly the path an operator
is most likely to be reading, and it suggests the failure teardown runs GTK window code against a
surface that is no longer a toplevel.

## Gtk-CRITICALs on the *successful* lock path too, on a real compositor

Locking on the author's own sway logs four of these between "lock surface for monitor WL-1" and
"session locked", i.e. while the window created in `monitor` is assigned but the lock is not up yet:

~~~
Gtk-CRITICAL: gtk_native_get_surface: assertion 'GTK_IS_NATIVE (self)' failed
~~~

Different assertion and different path from the failure-case CRITICALs above, and it does **not**
reproduce in the nested headless sway the tests use — so it is invisible to `tests/gui-test.sh` and
was only seen by running the gate against a live session. Nothing misbehaves: the lock comes up and
the prompt is answerable. Not yet narrowed to which call in `assign`/`present` makes it.

## Relocking in one process works, but is undocumented

The minimize chip unlocks the session and later locks it again from the same process, with a fresh
`GtkSessionLockInstance` per lock epoch (the old one is dropped after `unlock()` and a display
roundtrip). That works — repeatedly, in a nested sway — but nothing in the header says whether an
instance is single-use or whether a second lock from the same client is legal, so a client that
needs it is relying on observed behaviour. Worth a sentence in the docs either way.

## Also worth documenting: monitor removal does not destroy the window

The header says an assigned window "will be automatically unmapped and dereferenced when its monitor
is removed". A client holding its own strong reference therefore gets no `destroy` signal and has no
obvious way to notice the output went away. `GdkMonitor::invalidate` works, but the docs point at
"GTK APIs can be used for that" without saying which.

## And: an oversized dialog is a protocol error, not a clip

Not a library bug, but worth a line in the session lock docs. `ext-session-lock-v1` requires the
surface to commit exactly the configured size, so a GTK window whose *minimum* size exceeds a small
output is disconnected with a protocol error — which for a lock client means the session stays locked
with nothing on it. Layer-shell surfaces merely clip in the same situation, so the habit does not
carry over.
