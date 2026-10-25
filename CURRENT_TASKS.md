# Current Tasks

Last updated: 2026-10-25 (accepted, merged, pushed).

## Git state

- `main`: accepted work, including the audit fixes and music videos (merged 2026-10-25), pushed to `origin`.
- `fix/audit-findings` and `feature/music-videos`: merged into `main` and deleted (never pushed). `main` is the only branch.
- Installed in the live session: the `ccb294b` code, the same as `main`.
- Backup of the build accepted before this work: `~/.local/share/wallslaed-backups/installed-2026-10-25/` (`restore.sh` puts it back).

## Done: music videos, media controls, wallpaper browser

Plan: music videos downloaded with yt-dlp play as the wallpaper with sound, outside the shuffle; the player is an MPRIS media player; the preferences gain Music Videos and Browse (Pexels/Pixabay) pages.

| Part | State |
| --- | --- |
| Player: audio, `--start/--title/--art`, stdin controls, MPRIS, position reports, 2 s self-exit | implemented, verified in an isolated headless shell |
| Daemon: music mode, `PlayMedia/StopMedia/ReportPosition/ListMedia`, downloads queue, CLI `music/play/stop/download` | implemented, verified (download only against a bad link, never a real one) |
| Extension: audio playback, no auto-pause for music, menu controls and Music Videos submenu | implemented; playback verified headless, **menu not tested** (needs clicks) |
| Preferences: Music Videos and Browse pages, interval fix | implemented; window opens without errors; **pages not exercised** |
| Accurate seeking, `--js-runtimes node`, systemd-scope fallback | implemented, verified except a real YouTube download |
| README | updated |

User testing after a full restart (2026-10-25): working; Pixabay browsing works (Pexels currently refuses new API keys, on their side). A YouTube download from the Music Videos page showed no progress: yt-dlp hides progress when its output isn't a terminal. Fixed with `--progress` (verified against a local server) and installed. The user tested music video playback by hand and accepted the work; no background performance problems were found (daemon ~13 MB and idle, no player running when nothing animates).

## Audit (Codex, 2026-10-25)

All seven findings (F1–F7) and guards for U3–U5 are fixed on `fix/audit-findings`, one commit per group; the report was committed there and then removed, so it never reaches `main`. The user accepted the installed build and asked for it "with patches" as the fallback.

## Next

Nothing in progress. Open ideas, not planned:

- Keep music playing on the lock screen (needs `unlock-dialog` session mode support).
- Not yet exercised: the desktop menu's music items under rapid use, and Pexels (refusing new API keys at the time).
