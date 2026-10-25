# Current Tasks

Last updated: 2026-10-25.

## Git state

Branch `main`, last commit `ad30b95`. Everything below is **uncommitted** in the working tree and not yet on a feature branch. Nothing is installed in the live session: the running daemon, player and extension are still the `ad30b95` build.

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

## Audit (`AUDIT.md`, Codex, 2026-10-25)

All seven findings (F1–F7) are fixed in the working tree, plus cheap guards for U3–U5. F1 is verified by test; F7 was not verified, because that test run was spoiled by the settings leak. The user did not review the findings one by one before the fixes (the audit process in `AGENTS.md` came later). Delete `AUDIT.md` before this work reaches `main`.

## Next

1. Ask the user whether to commit this work to a `feature/music-videos` branch.
2. With approval: `./install.sh`, then the user logs out and in, and tries the desktop menu, both preference pages, and one real download.
3. Update `README.md`, then merge once the user accepts.
