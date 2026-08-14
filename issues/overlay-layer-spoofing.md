# Non-root uids can draw over an overlay-layer surface

wlbouncer grants `zwlr_layer_shell_v1` to `ff`, `comms` and `code` (portal needs it), and stacking
between two overlay-layer clients is compositor-defined. So a compromised browser uid can place its
own overlay surface over another client's: cover the important text with something benign, leave the
approve button exposed, and the human clicks. It cannot forge input (virtual keyboard, input method
and virtual pointer are denied to non-root) but it does not need to.

**No longer applies to the sudo gate.** `sudo-prompt` presents on an `ext-session-lock-v1` surface,
which the compositor renders above the overlay layer and gives all input to, so a layer-shell client
cannot cover it. The gate has no layer-shell fallback. Cost: if
the gate is SIGKILLed the session stays locked until recovered from a TTY.

What is left:

- The generic `permission-prompt` presenter in layer mode is still coverable. It is not an
  authorization boundary, so this is minor.
- The gate's **minimize chip** is an overlay-layer surface, so it is coverable and spoofable by the
  same uids. Accepted by design, because the chip is powerless: covering it hides a pending prompt,
  which is the already-accepted DoS class; a fake chip's text is corrected the moment the real lock
  surface shows the real argv; and tricking a click on either target yields a denial (safe) or an
  expand back to the exclusive surface, behind a fresh quiet period. Approval is never reachable
  from the chip.
- The equivalent hazard moved up a protocol: a non-root uid that could bind
  `ext_session_lock_manager_v1` would get the same exclusivity, could lock the session before the
  gate does (blocking all sudo, since the gate fails closed) and could show an uncoverable fake
  prompt. wlbouncer denies unknown globals by default; confirm that includes this one and that it is
  never granted alongside layer shell.
- Teaching wlbouncer to filter per layer (only root gets `overlay`) is still the clean general fix
  for the layer itself, and would matter again for anything else that trusts an overlay surface.
