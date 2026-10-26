# Current Tasks

Last updated: 2026-10-26 (shell blur installed for a live test; Codex performance audit next).

## Git state

- `main`: accepted work, including the Random/settings fix, the GNOME 51 port and the glycin conversion (merged 2026-10-26), pushed to `origin`.
- `fix/random-and-settings`, `fix/gnome-51`, `feature/glycin-prepare`: merged into `main` (fast-forward) and pushed.
- `feature/shell-blur`: resumed 2026-10-26 for GNOME 51, `main` merged in, all planned steps implemented and verified headless; see below. Pushed; installed 2026-10-26 for a live test. Not merged.
- `feature/fast-switch`: P-01 from the Codex performance audit, branched from `feature/shell-blur`; see below. P-01 installed and accepted 2026-10-26; P-02 change (`ddc9958`) installed and accepted 2026-10-26. Pushed to `origin/feature/fast-switch` (includes the `feature/shell-blur` commits); not merged. Backups: `installed-fast-switch-603caca/` (before P-02), `installed-shell-blur-ebfca40/` (before preloading), both in `~/.local/share/wallslaed-backups/`.
- `codex/perf-audit`: the Codex performance audit report (`bdb984d`, `AUDIT.md`), branched from `7f533a9`; keep until P-01..P-06 are decided.
- `codex/audit-shell-blur`: deleted 2026-10-26 (all 14 findings fixed). Next: a Codex performance audit on `codex/perf-audit`; prompt in `.scratch/codex-perf-audit-prompt.md`.
- Installed in the live session: `feature/shell-blur` (`9be4b8b`), installed 2026-10-26; extension takes effect after logging out and back in. Hide Top Bar was still enabled in the user's session at install time (conflicts with `topbar-autohide`), and the user's `blur-popups`, `blur-app-folders`, `blur-screenshot` were off.
- Backups: `~/.local/share/wallslaed-backups/installed-main-d77bfef/` (before the shell blur test), `installed-gnome51-bf0d962/` (before the glycin conversion), `installed-random-settings-5a7d4ac/` (the last GNOME 50 build), `installed-main-480b01e/` (the build before this fix), `installed-transitions-2026-10-25/`, `installed-main-761c5ae/` (the build before the new transitions) and `installed-2026-10-25/` (before the menu race fix); each has a `restore.sh`.

## Implemented: audit P-03..P-06 (`feature/fast-switch`, 2026-10-26)

The rest of the Codex performance audit (`AUDIT.md` on `codex/perf-audit`), one commit each. Verified headless; not installed yet.

| Finding | Change | Headless result (4K scale 2, `.scratch/fast-switch/`) |
| --- | --- | --- |
| P-03 music video repaints behind windows (`34c8a92`) | `animated.js`: a monitor covered by a maximized or fullscreen window loses its clone; player and sound keep running | covered: shell 10-30 ms CPU per 6 s and 0 frames (visible: 260-310 ms, 181 frames); player 410-430 ms (visible 860-990); music still playing; clone back on uncover, picture not black. A translucent maximized window shows the still frame |
| P-04 blur transition, first-use stalls (`6c46d1f`) | `transitions.js`: Blur fades through two fixed blurs (24, 64) computed once, exact pairwise opacities, no offscreen buffer; the next effect's shader drawn once on a tiny hidden actor 3 s after each change (Random picks ahead) | Blur 290-430 ms CPU per change (audit: 660-810; fade 280-310); first-use Shatter/Freeze/Burn paint 29/5/10 ms (audit: 86-128). Blur's first frame still 18-70 ms (allocating its buffers) |
| P-05 popup blur redraws (`f07b93c`) | `liveBlur.js`: over bare desktop the popup shows its piece of the baked blur (`blur.js bakedContent`); checked every frame against windows, other popups, top bar, transitions, video clones, background children (Fedora logo); tint drops the baked brightness | hover sweep 360-460 ms CPU (live: 830-850); a window under the menu switches to live, moving it away back to baked. The live blur sampled the wrong area in the headless scale-2 session (red where the wallpaper is green), so baked vs live screenshots differ (PSNR 17.8); the baked one matches what is behind |
| P-06 cache too small, double decode (`f98cee8`) | `prepare.rs`: generated pictures kept newest-first up to `cache-size` (new key, 2048 MB, Preferences > Cache Size), blurs keep 40; blur baked from the pixels just decoded | 28 tests (2 new); 5.5K JXL with blur 448-450 ms vs 479-481 ms, 8K JPEG 347-350 vs 375-382 ms; preferences open, setting writes |

Regression (runs 11, 12): earlier suites unchanged, no JS errors or CRITICALs. Not tested: the real desktop (NVIDIA, two monitors), real menus and hover by hand, how the baked popup looks next to the live one on the real screen, audio continuity by ear.

## Accepted: GNOME's texture cache bypassed, Previous prepared (`feature/fast-switch`, 2026-10-26)

Audit P-02 remainder: GNOME's `BackgroundTextureCache` never evicts, so everything still loading through GNOME (Previous, end of a round, 16-bit images) stayed on the GPU for the session. The user chose option 3: every desktop wallpaper load goes through `preload.js`, plus the daemon preparing Previous.

| Change | State |
| --- | --- |
| Daemon: keeps the next and previous wallpapers prepared (`preloaded` is a list; the old current one is kept as previous); `previous-picture` in the state; `Bag::peek_previous` | verified: build, 26 tests (new `peek_previous_matches_previous`), no warnings |
| `preload.js`: a picture GNOME asks for that isn't prepared is loaded on demand the same way, never through GNOME's cache; 16-bit and float formats; sliced texture for oversized pictures, as GNOME does | verified headless (below) |

Headless, 4K scale 2 (`.scratch/fast-switch/`, run 5 mode `p02`, run 6 full suite):

- Previous: prepared, 10-36 ms longest main-loop gap (was 114-188 ms through GNOME).
- Unprepared pictures: shown 0.19-0.49 s after the change, 9-46 ms gaps; the same through GNOME 128-205 ms.
- 16-bit PNG: pixel-identical to GNOME's rendering, 13 ms gap (GNOME: 554 ms).
- 60 changes mixing Next, Previous and unprepared pictures: zero loads through GNOME, at most 3 textures kept, GPU memory 0.76-1.1 GB, flat.
- Run 6 (full suite): all earlier checks still pass, shell log free of CRITICAL/JS errors.

GNOME bug report: not filed (needs the user's GNOME GitLab login; Claude in Chrome wasn't connected). Draft in `.scratch/fast-switch/gnome-issue.md`.

Installed 2026-10-26; the user tested it after logging back in: "seems to work just fine". Not tested: the real desktop; an image too large for one texture (sliced path).

## Accepted: wallpaper switch without the freeze (`feature/fast-switch`, 2026-10-26)

Audit finding P-01 (`AUDIT.md` on `codex/perf-audit`): GNOME 51 decodes a new wallpaper synchronously and uploads it in one call on the compositor thread, a 125-155 ms freeze at 4K even with transitions off. The audit proposed a persistent presenter replacing GNOME's background handling; this does less: it keeps GNOME's pipeline and only supplies the texture.

| Change | State |
| --- | --- |
| Daemon: `next-picture` in the state once the next wallpaper is prepared, `Changed` emitted when it is (empty while music plays) | verified: build, 25 tests, no warnings |
| `preload.js`: decodes `next-picture` with glycin's async loaders, uploads 16 rows at a time (2 ms bursts every 8 ms, paused during transitions); hooks GNOME's `Background._loadImage` so a prepared file gets the finished texture; keeps only current and next, outside GNOME's texture cache; 16-bit/float images, misses and errors go to GNOME | verified headless (below) |
| `transitions.js`: `busy` getter; `docs/how-it-works.md` | done |

Headless, 3840x2160 at scale 2, through `tools/isolated-session.sh` (`.scratch/fast-switch/`, runs 2-4; run 4 is the full suite on `1cf3489`, shell log free of CRITICAL/JS errors. Run 1 had a CRITICAL from the test calling `destroy()` by hand before a disable, not from the code):

- Longest main-loop gap on a switch with no transition: 11-22 ms prepared, 56-158 ms through GNOME in the same session (audit: 125-156 ms). Fade 12-39 ms. Blur and Shatter still show 40-90 ms first-frame paints (audit P-04, the effects themselves).
- Upload slices 0.1-0.2 ms each, a few 10-14 ms ones (likely GC); 4K picture uploaded in 0.1-0.4 s.
- Screenshots of a prepared JPEG, an RGBA PNG and a 5.5K JXL are pixel-identical to GNOME's own rendering of the same file (PSNR inf).
- GPU memory over 40 changes stays flat: 0.5-0.8 GB with the final build (run 3), 0.8-1.1 GB in run 2 after many fallback loads (the audit saw +2.2 GB over 64 with GNOME's cache).
- Rapid Next, Previous (falls back), disable/enable mid-upload, lock/unlock: no JS errors, no settings leak.
- Known miss: at the end of a shuffle round the daemon can't predict the next wallpaper, so that one change loads through GNOME, as before.

Installed 2026-10-26 (`603caca`). After logging back in, the user tested Next by hand: "it already feels way more responsive". Extension ACTIVE, no Wallslæd errors in the journal. Still not tested: HDR or 16-bit images on screen, multi-hour GPU memory on the real desktop.

## Accepted: `feature/glycin-prepare` (2026-10-26)

User report on the installed GNOME 51 build: the pointer "sticks to the menu for a second" on Next Wallpaper. Watching 34 real changes, GNOME Shell's CPU jumped to 50-78% for up to 1.5 s on JPEG XL wallpapers (401 of 1536 files, 4000-5500 px wide), which the daemon couldn't read and passed to GNOME untouched; most JPEG/WebP changes stayed under 40%. The screen is 3840x2160, so prescale rarely shrinks anything else.

| Change | State |
| --- | --- |
| `prepare.rs`: formats the `image` crate can't read (JXL, AVIF, HEIC...) are decoded with the `glycin` crate (GNOME's sandboxed loaders, its own main context) and always written as a cached JPEG, scaled to cover the screen if larger | verified: unit test (5504 and 1000 px JXL); isolated session with three 5504x3072 JXL files: GNOME got only cached 3870x2160 JPEGs, conversion ~0.5 s off the main loop, the next one prepared ahead (0.02 s switch) |
| `wallslaed.service`: `MemoryHigh` 256M to 400M (`MemoryMax` stays 600M) | measured: daemon + glycin-jxl loader peak ~320 MB per 5.5K JXL, back to ~34 MB after |
| README: formats, build requirements, the Wallslæd Settings menu item | done |

Cost: 136 more crates (zbus, moxcms, gufo...), links libseccomp and fontconfig; release binary 4.9 to 7.7 MB. Loader processes idle out after 30 s (glycin's pool).

Installed and watched on the real desktop: 37 changes, no JXL reached GNOME, the 1.5 s stalls at 52-78% shell CPU are gone (changes now cost 30-45% for about half a second, as plain JPEGs did); the user reports it feels smooth. Not tested: AVIF and HEIC (none in the library).

## Accepted: `fix/gnome-51` (2026-10-26)

The user upgraded to Fedora 45 (GNOME Shell 51.0, GTK 4.24, GStreamer 1.28). Researched beforehand in `ROADMAP.md`; the port:

| Change | State |
| --- | --- |
| `metadata.json`: `shell-version` `["51"]` (the shader port doesn't run on 50) | verified: extension ACTIVE headless |
| `transitions.js`: `Shell.GLSLEffect` (removed in 51) replaced by `Clutter.ShaderEffect` + `vfunc_get_static_snippet` (`set_post`), uniforms set by name | verified: a mid-transition screenshot of all twelve effects shows each one drawing; no JS, shader or Cogl errors |
| `transitions.js`: outgoing menu closed with `{animate: false}` (51 read the old `PopupAnimation.NONE` as "animate") | verified: a menu open during a wallpaper change closes |
| `transitions.js`: GNOME 51's reduced-motion setting plays Crossfade | verified: 100/100 picks were Crossfade with `reduced-motion` set to `reduce` |
| Rust rebuilt against Fedora 45's libraries | verified: `cargo build`/`cargo test` (18 passed), no warnings |

Also verified headless: Random draws all twelve effects, the desktop menu's "Wallslæd Settings" opens the preferences window (GTK 4.24, no errors), 5 disable/enable cycles. Test script: `.scratch/gnome-51/test.sh`.

A headless GNOME Shell 51 segfaults on shutdown if a client window is still open (`gnome_shell_plugin_kill_window_effects`); it happens without Wallslæd too (`.scratch/gnome-51/baseline/`), so test scripts close windows before stopping the shell.

User tested the installed build 2026-10-26: transitions look fine on the NVIDIA desktop, the menu works (also during a transition), music videos play. `ptrace_scope` is 0 on this install, so `eu-stack` still works without `sudo`.

## Accepted: `fix/random-and-settings` (2026-10-26)

User reports: Random never played the newer effects, and both "settings" items in the desktop menu open GNOME Settings (they are GNOME's own: "Display Settings" and "Settings", which reopens its last page).

| Change | State |
| --- | --- |
| `transitions.js`: the Random pool holds all twelve effects | verified headless: 600 draws hit all twelve (43-69 each); 12 real wallpaper changes played 11 different effects, no JS errors |
| `menu.js`: "Wallslæd Settings" at the end of our desktop-menu section opens the preferences | verified headless: activating it closed the menu and opened the "Wallslæd" preferences window; 3 disable/enable cycles, extension stays active |

Not tested: real mouse clicks, the new effects on NVIDIA in the Random rotation. Test script: `.scratch/random-settings/test.sh`.

## In progress: shell blur and top-bar autohide (`feature/shell-blur`, 2026-10-26)

Resumed 2026-10-26 on GNOME 51. Plan approved: `~/.claude/plans/squishy-hugging-cloud.md` (merge `main`, popup blur rework for the hover boxes, all 14 audit findings SB-01..SB-14 from `AUDIT.md`, GNOME 51 app-window blur setting). The original plan: `~/.claude/plans/there-is-another-extension-woolly-hejlsberg.md`.

### 2026-10-26 work on GNOME 51 (`e2fb252`..)

| Step | State |
| --- | --- |
| Merge `main` (GNOME 51 port, glycin, Random/settings) | committed `e2fb252`; build, 25 tests, schema |
| `blur.js` on GNOME 51: `Meta.BackgroundImageCache` is gone (extension went to ERROR); baked blur loaded with glycin async, GPU fallback blurs GNOME's own background | verified headless (screenshots of overview, top bar, menu, quick settings; JXL wallpaper baked) |
| Popup boxes on hover (user report): one blur per popup, whole rectangle redrawn while in use, partial redraws followed up; tint follows brightness (SB-11, SB-10, SB-01) | verified headless: pointer sweep 169 frames, 0 partially redrawn blurs; 0 frames when idle; corners clean; settings in place; screenshot/disable race clean |
| Top bar: fullscreen reveal, leave on any monitor, all the app's windows (SB-07..09) | verified headless, 2 monitors, two-window GTK app; hot-corner gap 100 px |
| Daemon blur bake (SB-02..06, SB-13, SB-14) | 5 unit tests; SB-05/06 against the running daemon with a slowed ffmpeg |
| GPU fallback once per monitor (SB-12) | verified headless: 2 sources, 3 clones, old ones gone after changes |
| App-window blur (GNOME 51 `set_background_blur_params`), `blur-apps`/`blur-apps-strength`, Appearance page | verified headless with a GTK 4 `backdrop-filter` window: 24/60/4/24 on disable |
| Both picture keys in one write (GNOME deadlock mitigation, below) | verified: baseline 4/4 hangs with two writes, 0/3 with one |
| Regression: 60 changes 1.5 s apart, 50 menus with pointer motion, 30 disable/enable, 10 lock/unlock, preferences | actors 2109 -> 2044, shell +31 MB then flat, no JS errors |

**GNOME deadlock (not ours):** GNOME Shell 51 / GJS 1.90 can hang for good when a wallpaper change's settings signal meets the garbage collector freeing the replaced background (GJS MR !1134, open). Plain GNOME without Wallslæd hung within 20-100 changes at 0.35 s in 4 of 4 runs. Very likely the unexplained freeze of 2026-10-25. Details in `docs/how-it-works.md`; README has user advice. During this, a test script attached `eu-stack` to the user's real GNOME Shell by mistake (see AGENTS.md lessons); it kept running.

Not tested: real NVIDIA rendering of everything above, real mouse pressure at the edge, a real lock/unlock with a password, physical monitor hotplug, HiDPI scale 2 (the user's), AVIF/HEIC.

User test of `a62229a` on GNOME 50 (2026-10-26), before it was shelved:

- The pointer stuck in the top-left corner: fixed afterwards by `5a25d8a` (barrier stops short of the hot corner), not yet tried on the real desktop.
- Menus and popups rendered wrong: square boxes appeared inside right-click and top-bar menus on mouse-over and stayed, and vanished when the screenshot tool opened. Diagnosis: partial redraws re-blur only the hovered item's rectangle from stale pixels (see the plan).
- 401 of 1536 wallpapers are JPEG XL; the blur bake couldn't read them, so they used the GPU fallback. The merge of `main` (glycin) fixes that.


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

History: shelved 2026-10-26 after that test (Blur my Shell and Hide Top Bar were disabled for it); audited by Codex on `codex/audit-shell-blur` (`0402ef0`, 14 findings).

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
