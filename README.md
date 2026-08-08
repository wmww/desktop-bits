Wayland desktop environment bits and bobs.

- `sudo-prompt` — the sole sudo authorization gate: presents a fixed prompt on an
  `ext-session-lock-v1` surface and execs the approved command as root.
- `sudo-shim` — `/usr/local/bin/sudo`, an unprivileged dispatcher that rewrites requests into gate
  invocations and passes everything else through to the real sudo.
- `permission-prompt` — a generic yes/no presenter. Not an authorization boundary.
- `permission-prompt-ui` — the shared GTK4 code: escaping, surface modes, the input state machine.

See `notes/permission-prompt.md` for the design and `notes/sudo-prompt-operations.md` for the
rollout order, the manual verification items and the abandoned-lock recovery drill.

## Set up sudo prompt
1. Make sure you don't lose access to root, setting a root password is highly recommended
2. Build and install this program somewhere globally accessible and in your PATH
  - `cargo build --release --workspace`
  - `install -m 755 target/release/sudo-prompt /usr/local/bin/sudo-prompt`
  - `install -m 755 target/release/sudo-shim /usr/local/bin/sudo-shim`
3. Symlink `sudo` to `sudo-shim`
  - `ln -s /usr/local/bin/sudo-shim /usr/local/bin/sudo`
4. Add `%wheel ALL=(root) NOPASSWD: /usr/local/bin/sudo-prompt -- *` to `/etc/sudoers` (assuming `wheel` is your sudo group)
5. Get rid of anything else in `/etc/sudoers` that allows sudo from untrusted users
6. Check it with `sudo-prompt/verify.sh` as root (`--group NAME` if your sudo group isn't `wheel`,
   `--trusted NAME` for any uid that is meant to hold unrestricted root). It reads only, and exits
   non-zero if anything is wrong.

### Theming

The prompt takes its colours from root's GTK theme. It runs with a scrubbed environment — no caller
gets to choose how a root prompt looks — so `GTK_THEME` is out and the theme has to be set somewhere
root owns. As root:

```sh
gsettings set org.gnome.desktop.interface gtk-theme Adwaita-dark
```

Or, where there is no settings portal, `/root/.config/gtk-4.0/settings.ini`:

```ini
[Settings]
gtk-theme-name=Adwaita
gtk-application-prefer-dark-theme=1
```

With neither set it is GTK's built-in light theme. `RUST_LOG=debug` logs which one it resolved.
