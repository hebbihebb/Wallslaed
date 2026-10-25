# Wallslæd Agent Instructions

The authoritative instructions for every coding agent working on this repository. Tool-specific files (`CLAUDE.md`, skills, memories) defer to this file.

## Read First

1. `CURRENT_TASKS.md`: what is in progress, its state, and what comes next.
2. `README.md`: how the daemon, extension and player fit together, and why they are separate.
3. `git status`: if there is uncommitted work you did not create, find out what it is before touching anything.

## The Golden Rule: the Live Desktop Is Off-Limits

This project runs inside the user's real GNOME session, on the machine they are using right now. Mistakes here have already changed real settings once (see Lessons Learned). Without the user's explicit go-ahead **for that action, in this session**, do not:

- run `./install.sh` or `./uninstall.sh`, or copy files into `~/.local/bin`, `~/.local/share/gnome-shell/extensions/` or `~/.local/share/glib-2.0/schemas/`;
- `gnome-extensions enable|disable`, or restart, stop or kill `wallslaed.service`, `wallslaed-player`, `gnome-shell`, Nautilus or any other user process;
- run `wallslaed` commands that change things on the real daemon (`next`, `prev`, `pause`, `play`, `stop`, `ban`, `favorite`, `download`, `rescan`);
- write real settings: `gsettings set|reset`, `dconf write|reset`, or anything under `/io/github/hebbihebb/Wallslaed/` or `org.gnome.*`;
- download anything large, or call external APIs (yt-dlp, Pexels, Pixabay) with real network traffic.

Reading is fine: `wallslaed status`, `gsettings get`, `journalctl`, `systemctl --user status`, `git` inspection.

## User Data

Irreplaceable; never modify, move or delete, and never point a test at it:

- `~/.local/state/wallslaed/state.json`: favorites, bans, history and shuffle position. `media.json` next to it: the active music video and saved positions.
- The wallpaper folder (currently `~/Pictures/Wallpapers`, 1500+ files) and the music video folder (`~/Videos/Music`). The daemon only ever reads source files; generated files go to `~/.cache/wallslaed`.
- The user's GNOME settings (dconf).

`~/Network/Gigabyte/*` are **hard NFS mounts**: if the network drops, anything that touches them hangs forever (this froze Nautilus once). Never scan, `stat`, `ls` or `find` them, and never use them as test folders. If you must, wrap the command in `timeout -s KILL 5`.

## Testing Without Touching the Desktop

Anything that uses GSettings, D-Bus, the daemon, the player or a GNOME Shell runs through **`tools/isolated-session.sh`**, which gives it its own home, settings, runtime folder and session bus:

```bash
cargo build --release
tools/isolated-session.sh --install <scratch-dir> <test-script>
# inside the script, a headless shell:
#   gnome-shell --headless --virtual-monitor 1920x1080 --wayland --no-x11 &
#   gdbus wait --session --timeout 40 org.gnome.Shell
#   gnome-extensions enable wallslaed@hebbihebb   # starts INITIALIZED in headless mode
```

- **Never** hand-roll `dbus-run-session -- script` with `export HOME=...` inside the script. Services the bus starts on demand (dconf-service above all) inherit the bus's environment, so their writes land in the real home.
- If the script prints `WARNING: real settings now refer to the scratch folder`, stop and tell the user. A `NOTE` listing only `org/gnome/desktop/background` is the real daemon rotating, not a leak.
- Make test media with `ffmpeg -f lavfi` (testsrc2 plus sine); never copy the user's files.
- Keep scratch folders in the session scratchpad; the script refuses folders inside the real home's config.

## Code Rules

- **GNOME Shell code runs inside the compositor.** No blocking I/O or synchronous subprocess waits in `extension/`. Everything created in `enable()` (signals, GLib sources, actors, subprocesses) is cleaned up in `disable()`, which also runs on every screen lock. Write to a subprocess's stdin one command at a time (GIO allows one pending write per stream).
- **Daemon borrows:** never hold a `RefCell` borrow of `Daemon` across a `gio::Settings` setter, signal emission or anything else that can run our handlers synchronously. Clone what you need, drop the borrow, then call (audit finding F1).
- **D-Bus interface:** `INTROSPECTION` in `daemon/src/daemon.rs` and `IFACE_XML` in `extension/wallslaed@hebbihebb/iface.js` change together.
- **Settings schema** changes are additive (new keys only), and `glib-compile-schemas --strict --dry-run data` must pass.
- The player must exit on its own within two seconds of `quit` or end of stdin; the extension relies on that instead of kill timers.
- Prefer extending existing code to rewriting it. Justify every new dependency.

## Verification

| Change | Minimum check |
| --- | --- |
| Docs only | `git diff --check`; referenced paths and commands exist |
| Rust (daemon, player) | `cargo build --workspace` and `cargo test --workspace`, no warnings |
| Settings schema | `glib-compile-schemas --strict --dry-run data` |
| Extension JS | Syntax check, then a headless run through `tools/isolated-session.sh`, with no `JS ERROR` mentioning Wallslæd in the shell log |
| Daemon/player behaviour | A scripted run through `tools/isolated-session.sh` exercising the changed path |
| Anything for the live desktop | Ask first (Golden Rule), then follow what the user approves |

Hand back evidence, not homework: what passed, what failed, what was **not** tested, and one sentence on what the user should try by hand. Report state with these words: *planned*, *implemented* (in the working tree), *verified* (checks passed), *accepted* (the user tried it), *committed*, *merged*, *installed* (in the user's live session).

## Git

- `main` holds accepted work only. Work happens on `feature/<name>` or `fix/<name>` branches (Codex: `codex/<name>`).
- Commit each testable unit on its branch; never end a session with large uncommitted work without recording its state in `CURRENT_TASKS.md`.
- Never push, force-push, delete branches or rewrite published history without explicit approval. The remote is `github.com/hebbihebb/Wallslaed` (private).
- The user prefers plain-language explanations of Git decisions.

## Audits

Only when the user asks, on its own branch:

1. **Audit:** write `AUDIT.md` (ID, severity, `file:line`, scenario, confirmed or suspected, proposed fix). Change no code.
2. **Review:** go through the findings with the user; they accept, reject or defer each one.
3. **Fix:** only the accepted findings, then delete `AUDIT.md` so it never reaches `main`.

## Shell on This Machine

- `cp`, `mv` and `rm` are interactive aliases and hang waiting for a prompt. Write `command cp -f`, `command mv -f`, `command rm -f` (or `\cp`).
- Never `pgrep -f` or `pkill -f` a pattern that appears in your own command line; it matches your own shell. Use `pgrep -x <name>`, or bracket a letter: `pgrep -f '[w]allslaed-player'`.
- Wayland socket paths are limited to 108 bytes, so nested sessions need a short `XDG_RUNTIME_DIR` (`tools/isolated-session.sh` handles it).
- Terminals opened from Nautilus live inside Nautilus's systemd unit; killing that unit kills those terminals (and any agent running in them).
- `yt-dlp` lives in `~/.local/bin` and needs `--js-runtimes node` for YouTube (no deno installed); `daemon/src/media.rs` adds it.
- End optional probes (`ls`, `which`) with `|| true`.

## Lessons Learned

- **2026-10-25, settings leak:** a headless test exported a scratch `HOME` *inside* `dbus-run-session`. dconf-service ran with the real home, and test values were written into the user's real settings: the wallpaper folder became `/x`, the interval 7 seconds, and the enabled-extension list was replaced. The originals were reconstructed from intact state; the true original interval is unknown. Hence `tools/isolated-session.sh` and the Golden Rule.
- **2026-10-25, `cp -i` hang:** a test copied files with plain `cp`, which waited invisibly at an overwrite prompt.
