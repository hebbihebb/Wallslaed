# Current Tasks

Last updated: 2026-10-26 (shell blur and top-bar autohide on `feature/shell-blur`, replacing Blur my Shell and Hide Top Bar).

## Git state

- `main`: accepted work, including the menu race fix and the new transitions (merged 2026-10-25), pushed to `origin`.
- `fix/menu-race-dissolve` and `feature/more-transitions`: merged into `main` and deleted (never pushed).
- `feature/shell-blur` (2026-10-26): blur and top-bar autohide, see below. Not merged, not installed.
- Installed in the live session: the new-transitions build (2026-10-25 23:29), which still offers Slide and Iris. Reinstalling `main` drops them; a stored `slide` or `iris` then plays as Crossfade.
- Backups: `~/.local/share/wallslaed-backups/installed-main-761c5ae/` (the build before the new transitions) and `installed-2026-10-25/` (before the menu race fix); each has a `restore.sh`.

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

## Done: `fix/menu-race-dissolve` (2026-10-25)

Follows a full desktop freeze on 2026-10-25 06:15 while cycling wallpapers from the desktop menu: GNOME Shell stopped answering (at-spi logged it unresponsive) with no crash, JS error or GPU message. Rapid cycling and right-clicking during transitions did **not** hang a headless shell, so the freeze itself is unexplained. Suspected: the NVIDIA driver (both screens run on it) during a shader transition. If it recurs, get `eu-stack -p $(pgrep -x gnome-shell)` from a TTY or SSH before powering off.

| Change | State |
| --- | --- |
| `menu.js`: the music list no longer touches a menu destroyed while `ListMedia` was pending (the 05:50 errors at `menu.js:115-122`) | verified: forced race in an isolated shell, 6/12/6 errors before, 0 after |
| `transitions.js`: the outgoing wallpaper stops taking clicks and closes its menu, so a right-click mid-transition opens the new wallpaper's menu (GNOME's `boxpointer.js` disposed-object errors) | verified: 30 rounds, 0 errors, every click landed on the new wallpaper |
| Dissolve removed: not in `EFFECTS` or the preferences list; a stored `dissolve` plays and displays as Crossfade; schema value kept | verified headless with `dissolve` and `random` stored |
| Wipe: removed the per-row edge jitter that smeared the soft edge into horizontal streaks (user report: "a horizontal swipe left pixelated effect in its wake") | verified by frame captures mid-transition (software rendering) |

Not tested: the real NVIDIA rendering, and real mouse clicks. Accepted by the user and merged.

## Done: more transitions (2026-10-25)

Tilt, Burn, Freeze, Shatter, Ripple, Melt and Light Leak, plus: a wallpaper arriving mid-transition skips its own effect, and monitors sharing a picture play the same one. The source was never committed; it was recovered from the installed build the user tested and accepted (commit `456d260`). The new effects play only when picked by name; they are not in the Random pool yet.

Retired at the user's request: Slide ("looks bad") and Iris (Ripple does the same, better). A stored `slide` or `iris` plays and displays as Crossfade; the schema values stay. Verified in an isolated headless shell (`slide`, `iris`, `ripple`, `melt`, `fade`, `random` stored, three wallpaper changes each: no JS errors, extension stays active). Not installed yet.

## In progress: shell blur and top-bar autohide (`feature/shell-blur`, 2026-10-26)

Goal: drop Blur my Shell (it breaks, stops working or grows until GNOME Shell crashes) and Hide Top Bar, and have Wallslæd do what the user uses of them, lighter. Plan approved 2026-10-26: `~/.claude/plans/there-is-another-extension-woolly-hejlsberg.md`. User's surfaces: overview, top bar, dash, menus and popups, app folders, screenshot UI, lock screen; top bar hides when the focused app's window overlaps it.

How it is lighter than Blur my Shell: the daemon bakes a small blurred copy of each wallpaper (about 1/8 screen size), and the overview and top bar share that one small texture per monitor, with no blur shader running. Popups use GNOME's own background blur in a sibling actor, only while they are open.

| Part | State |
| --- | --- |
| Daemon bakes `{hash}-blur-{strength}-{brightness}.png`, reported as `blurred`; re-bakes on setting changes | implemented, verified (unit test; headless run) |
| `blur.js`: overview background, top bar, see-through dash; GPU fallback without a baked copy | implemented, verified headless (screenshots, 10 enable/disable cycles, settings toggles, daemon stopped) |
| `liveBlur.js`: menus, quick settings, calendar, desktop menu, dialogs, OSD, banners, app folders, screenshot panel | implemented, verified headless (screenshots, blur removed after each close, no ghosting under a changing window); window switcher not exercised |
| `topbar.js`: overlap autohide, strut removal, overview/menus show it, top-edge pressure reveal | implemented, verified headless (window moved under/away, maximized, overview, menu, virtual-pointer reveal and leave, 5 enable/disable cycles, struts restored) |
| Preferences: Appearance page | implemented; window opens without errors; page not looked at |
| Lock screen blur follows the settings (`unlock-dialog` session mode); everything else torn down on lock, rebuilt on unlock | implemented, verified headless (lock/unlock cycles, settings while locked). An early version missed the dialog in 2 runs; adopting an existing dialog fixed it (3/3 since), cause not pinned down |
| Memory, same workload in three isolated sessions (200 wallpapers, 50 menus, 30 disable/enable cycles) | no blur +2.6 MB, Wallslæd +2.8 MB and 6 actors, Blur my Shell +26 MB and 261 actors |

Not testable headless: real NVIDIA rendering, real mouse pressure at the edge, a real lock and unlock.

Next: a Codex audit (prompt given to the user 2026-10-26), then the user disables Blur my Shell and Hide Top Bar, a backup is made, and this build is installed for testing. Nothing installed yet.
