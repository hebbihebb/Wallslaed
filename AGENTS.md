# Wallslæd Agent Instructions

Project-specific instructions for Wallslæd (shared workflow, Single-Pick Validation, git hygiene, and shell rules live in global `~/.claude/CLAUDE.md`). Tool stubs (`CLAUDE.md`) defer here.

## Read First

1. `CURRENT_TASKS.md`: active work and state (local, git-ignored).
2. `AGENTS.local.md`, if present: machine-specific facts (local, git-ignored).
3. `ROADMAP.md`: unstarted ideas; ask before starting an item.
4. `docs/how-it-works.md`: how daemon, extension, and player fit together. `README.md` is the user guide.

## The Golden Rule: the Live Desktop Is Off-Limits

This project runs inside the user's real GNOME session. Without explicit go-ahead **for that action, in this session**, never:
- run `./install.sh` / `./uninstall.sh` or copy into `~/.local/bin`, `~/.local/share/gnome-shell/extensions/`, or `~/.local/share/glib-2.0/schemas/`;
- `gnome-extensions enable|disable`, or restart/stop/kill `wallslaed.service`, `wallslaed-player`, `gnome-shell`, Nautilus, or other user processes;
- run mutating `wallslaed` commands (`next`, `prev`, `pause`, `play`, `stop`, `ban`, `favorite`, `download`, `rescan`);
- write real settings (`gsettings set|reset`, `dconf write|reset`);
- download large files or call external APIs (`yt-dlp`, Pexels, Pixabay) with real network traffic.

Read-only probes (`wallslaed status`, `gsettings get`, `journalctl`, `systemctl --user status`, `git`) are fine.

## User Data

Irreplaceable; never modify, move, delete, or point tests at:
- `~/.local/state/wallslaed/state.json` (favorites, bans, history, shuffle position) and `media.json` (active music video, saved positions);
- configured wallpaper and music-video folders (paths in `AGENTS.local.md`; daemon only reads sources, generated files go to `~/.cache/wallslaed`);
- user GNOME settings (`dconf`).

## Isolated Testing

Run anything touching GSettings, D-Bus, daemon, player, or GNOME Shell through **`tools/isolated-session.sh`**:

```bash
cargo build --release
tools/isolated-session.sh --install <scratch-dir> <test-script>
# inside script:
#   gnome-shell --headless --virtual-monitor 1920x1080 --wayland --no-x11 &
#   gdbus wait --session --timeout 40 org.gnome.Shell
#   gnome-extensions enable wallslaed@hebbihebb
```

- **Never** hand-roll `dbus-run-session -- script` with `export HOME=...` inside the script (D-Bus-activated `dconf-service` inherits the bus env and writes to real settings).
- Stop and alert the user if the script prints `WARNING: real settings now refer to the scratch folder` (`NOTE` for `org/gnome/desktop/background` alone is just the real daemon rotating).
- Generate test media with `ffmpeg -f lavfi` (testsrc2 + sine); never copy user files. Keep lasting scratch dirs in `.scratch/` (git-ignored). Wayland socket paths cap at 108 bytes (`tools/isolated-session.sh` handles short `XDG_RUNTIME_DIR`).

## Code Rules

- **GNOME Shell runs in the compositor:** no blocking I/O or synchronous subprocess waits in `extension/`. Clean up everything created in `enable()` inside `disable()` (runs on every screen lock). Write to subprocess stdin one command at a time.
- **Daemon borrows:** never hold a `RefCell` borrow of `Daemon` across a `gio::Settings` setter, signal emission, or synchronous callback (clone, drop borrow, call).
- **D-Bus & schema:** keep `INTROSPECTION` (`daemon/src/daemon.rs`) and `IFACE_XML` (`extension/wallslaed@hebbihebb/iface.js`) in sync. Schema changes are additive; `glib-compile-schemas --strict --dry-run data` must pass.
- Player must exit within 2 s of `quit` or stdin EOF. Prefer extending existing code; justify every new dependency.

## Verification & Git

| Change | Minimum check |
| --- | --- |
| Docs only | `git diff --check`; referenced paths/commands exist |
| Rust (daemon, player) | `cargo build --workspace` and `cargo test --workspace`, no warnings |
| Settings schema | `glib-compile-schemas --strict --dry-run data` |
| Extension JS | Syntax check + headless `tools/isolated-session.sh` run with 0 Wallslæd `JS ERROR`s; probe visible result directly (headless screenshots hide blur) |
| Daemon/player behaviour | Scripted `tools/isolated-session.sh` run exercising changed path |
| Live desktop | Ask first (Golden Rule) |

- **State vocabulary:** *planned*, *implemented*, *verified*, *accepted*, *committed*, *merged*, *installed*.
- **Public repo (`github.com/hebbihebb/Wallslaed`):** never commit personal or machine-specific details (home paths, hardware, mounts, folders, habits); put those in git-ignored `CURRENT_TASKS.md` or `AGENTS.local.md`. Keep worktrees under `.worktrees/<branch>` (never `/tmp`, which reboots clear).

## Lessons Learned

- **2026-10-25, settings leak:** `HOME` exported inside `dbus-run-session` leaked test values into real `dconf`. Always use `tools/isolated-session.sh`.
- **2026-10-26, wrong process:** `pgrep -xo gnome-shell` / `wallslaed` matches the live user process, not the nested test process (`eu-stack` attached to live GNOME Shell). Track `$!` directly and verify `--headless` before attaching/killing.
- **2026-10-26, GNOME 51 wallpaper deadlock:** GJS 1.90 hangs under rapid wallpaper changes (`docs/gnome-gjs-deadlock.md`). Keep stress-test intervals ≥1.5 s and abort on hang (1.5 s only narrows risk; a 1.5 s run hung at change 275 on 2026-11-06).
- **2026-11-07, invisible blur regression:** headless screenshots hid opaque popup tint; always probe visible alpha/style directly.
