# Design the pkexec authorization path

`pkexec` is installed and provides a separate route to root with a normal toplevel prompt. It is
not covered by the sudo gate and should not be described as protected by it.

Decide whether to remove `pkexec`, configure an appropriate authentication agent, or replace the
callers that rely on it. Any replacement needs its own threat model and integration plan.
