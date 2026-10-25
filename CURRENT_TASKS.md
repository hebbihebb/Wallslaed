# Current Tasks

Last updated: 2026-10-25 (deployed for testing).

## Git state

- `main` (`ad30b95`): the build the user accepted on 2026-10-25.
- `fix/audit-findings`: `main` plus only the audit fixes. Verified in an isolated headless shell. This is the fallback if the music video work is not wanted.
- `feature/music-videos` (active): built on `fix/audit-findings`; adds music videos, media controls and the wallpaper browser.
- Backup of the accepted installed build: `~/.local/share/wallslaed-backups/installed-2026-10-25/` (binaries, extension, schema, service, state); `restore.sh` there puts it back.
- Nothing is pushed.

## In progress: music videos, media controls, wallpaper browser

Plan: music videos downloaded with yt-dlp play as the wallpaper with sound, outside the shuffle; the player is an MPRIS media player; the preferences gain Music Videos and Browse (Pexels/Pixabay) pages.

| Part | State |
| --- | --- |
| Player: audio, `--start/--title/--art`, stdin controls, MPRIS, position reports, 2 s self-exit | implemented, verified in an isolated headless shell |
| Daemon: music mode, `PlayMedia/StopMedia/ReportPosition/ListMedia`, downloads queue, CLI `music/play/stop/download` | implemented, verified (download only against a bad link, never a real one) |
| Extension: audio playback, no auto-pause for music, menu controls and Music Videos submenu | implemented; playback verified headless, **menu not tested** (needs clicks) |
| Preferences: Music Videos and Browse pages, interval fix | implemented; window opens without errors; **pages not exercised** |
| Accurate seeking, `--js-runtimes node`, systemd-scope fallback | implemented, verified except a real YouTube download |
| README | **not updated** for the new features |

User testing after a full restart (2026-10-25): working; Pixabay browsing works (Pexels currently refuses new API keys, on their side). A YouTube download from the Music Videos page showed no progress: yt-dlp hides progress when its output isn't a terminal. Fixed with `--progress` (verified against a local server); not yet installed. User: "accepted so far if there are no performance bugs".

## Audit (Codex, 2026-10-25)

All seven findings (F1–F7) and guards for U3–U5 are fixed on `fix/audit-findings`, one commit per group; the report was committed there and then removed, so it never reaches `main`. The user accepted the installed build and asked for it "with patches" as the fallback.

## Next

1. Deployed `feature/music-videos` for testing on 2026-10-25 (`./install.sh`); the user logs out and in to load the new extension, then tries the desktop menu, both preference pages, and one real download.
2. If accepted: update `README.md`, merge `fix/audit-findings` and then `feature/music-videos` into `main`.
3. If not: `git switch fix/audit-findings && ./install.sh` for the patched accepted build, or `restore.sh` in the backup for the exact old one.
