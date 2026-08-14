# gtk4-layer-shell's session lock API: rough edges

Found while building `sudo-prompt` against gtk4-layer-shell 1.3.0 (via the `gtk4-session-lock` 0.4
crate). The ones that need one are worked around in `permission-prompt-ui/src/app.rs`; this repo's
author maintains the library, so they are worth fixing upstream.

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

## `Gtk-CRITICAL` from a stale lock surface after every unlock

Diagnosed on labwc (`WAYLAND_DISPLAY=wayland-0`) with the gate built `--features test-seams`. It is
not the first lock — that one is clean. It is the minimize chip's lock transitions, one line each:

~~~
minimized / session unlocked
Gtk-CRITICAL: gtk_native_get_surface: assertion 'GTK_IS_NATIVE (self)' failed
chip presented
...
expanded / lock surface for monitor Some("WL-1")
Gtk-CRITICAL: gtk_native_get_surface: assertion 'GTK_IS_NATIVE (self)' failed
session locked
~~~

The second one lands between `monitor` and `locked`, which is what made this look like the plain
lock path. It is not: locking without ever minimizing never logs it.

Backtrace (`gdb`, breakpoint `gtk_native_get_surface if self == 0`, debuginfod for the library):

~~~
gtk_native_get_surface (self=0x0)                      gtknative.c:245
find_lock_surface_with_wl_surface                      gtk4-session-lock.c:230
g_list_find_custom (list=all_lock_surfaces)
lock_surface_hook_callback_impl                        gtk4-session-lock.c:237
xdg_wm_base_get_xdg_surface_hook                       lock-surface.c:121
... gdk_wayland_toplevel_present / gtk_window_show / gtk_window_present ...
~~~

Cause, in gtk4-layer-shell 1.3.0 (still the same on `main`):

- `gtk_lock_surface_unmap_window` sets `self->gtk_window = NULL` and destroys the window, but leaves
  the entry in the global `all_lock_surfaces`. Its comment says this is deliberate — the client may
  still hold a reference and reuse the window.
- The entry is removed from that list only in `gtk_lock_surface_destroy`, which is the
  `GDestroyNotify` of the window's object data, i.e. when the `GtkWindow` is *finalized* — not when
  the lock ends. `clear_lock_state` clears `self->lock_surfaces` but not the global list.
- Presenting *any* GTK toplevel goes through the library's `xdg_wm_base.get_xdg_surface` shim, which
  walks `all_lock_surfaces` to ask "is this wl_surface one of mine?". The predicate dereferences
  `self->gtk_window` unguarded: `gtk_native_get_surface(GTK_NATIVE(NULL))` → the CRITICAL.

So: after any unlock, the next toplevel presented logs one CRITICAL per unmapped-but-still-listed
lock surface. On the minimize path that toplevel is the chip; on the expand path it is the next
lock window. Nothing misbehaves — the predicate returns "no match", which is the right answer.

Upstream fix is one line, `if (!self->gtk_window) return 1;` before line 230; better, drop the
entry from `all_lock_surfaces` in `gtk_lock_surface_unmap_window`, since a lock surface with no
window can never match. Worth a bug or PR either way.

Nothing to do on this side: the gate holds its own reference to the window on purpose, which is the
usage the library's own comment describes.

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
