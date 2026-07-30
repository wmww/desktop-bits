# Non-root uids can draw over the sudo prompt

`plans/permission-prompt.md` assumes an overlay-layer surface is a trusted display. On this host it
isn't: wlbouncer grants `zwlr_layer_shell_v1` to `ff`, `comms` and `code` (portal needs it), and
stacking between two overlay-layer clients is compositor-defined.

So a compromised browser uid can place its own overlay surface over a live root prompt: cover the
command text with something benign, leave the approve button exposed, and the user approves the real
request. It cannot forge input (virtual keyboard, input method and virtual pointer are denied to
non-root) but it does not need to — the human clicks.

The attack needs the covering uid and the sudo-group uid to both be compromised, or to cooperate.
Note the plan now keeps `ai` in the sudo-prompt group (agent sudo is an intended flow), and `ai` is
the uid most exposed to untrusted input, so "a sudo-group uid does something the human didn't
intend" is cheaper than it looks. `ai` has no layer shell, so it can raise the request but not do
the covering.

Options:

- Teach wlbouncer to filter per layer, so only root gets `overlay`. Cleanest; needs a wlbouncer
  change, since it currently filters globals only.
- Drop layer shell for `ff`/`comms`/`code` and find another answer for the portal.
- Use `ext-session-lock-v1` for the gate instead: the compositor guarantees exclusivity and routes
  all input to the lock client. Downside is a hard failure mode — if the gate crashes, sway leaves
  the session locked.
- Accept it and rely on a root-only visual marker (a phrase or colour readable only from /root) so
  a covered or faked prompt is distinguishable.

Deferred deliberately for the first implementation pass.
