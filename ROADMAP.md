# Roadmap

Nothing is scheduled. Items here are ideas to look into; each needs the user's go-ahead before work starts, and moves to `CURRENT_TASKS.md` when it does.

## After the Fedora 45 / GNOME 51 port

The port itself moved to `CURRENT_TASKS.md` (`fix/gnome-51`). Still open:

- **GJS wallpaper-change deadlock:** see `docs/gnome-gjs-deadlock.md` for the workarounds (biggest: stop writing GNOME's background settings on every change) and what to do when GJS !1134 is released, or if it never is. Each workaround needs the user's go-ahead.
- **Glycin wallpaper loading** (GNOME 51 loads backgrounds through `BackgroundTextureCache`): check by eye that AVIF and HEIC wallpapers show (JXL ones rotate in), and how long very large images take.

## Data-safety audit (Codex, 2026-10-26)

`AUDIT.md` on `codex/data-safety-audit` (`4adaaa2`, audited `7c6c9d3`): 15 findings, DS-01..DS-15, with repros and probes in `.scratch/data-safety-audit/`. Keep the branch until these are decided. Suggested order:

### 1. Never lose favorites, bans or positions (DS-01..DS-06)

The most important group: these can silently erase `state.json` or `media.json`.

- **DS-01:** a corrupt, truncated or unreadable `state.json`/`media.json` loads as empty, and the next save overwrites the original (`daemon.rs:138`, `media.rs:25`). Treat only `NotFound` as first run; otherwise keep the file, don't save over it, and say so. Add a format version and keep unknown fields.
- **DS-02:** `Bag::sync()` (`shuffle.rs:38`) drops favorites and history for any file missing from a scan. So a missing or unmounted wallpaper folder at startup, a failed subtree, **or switching to another folder** erases them for good. The folder coming back isn't noticed until a rescan. Keep favorites regardless of availability, keep the last good inventory when a scan fails, and watch for the folder's return.
- **DS-03:** state is loaded and saved in the bus-*acquired* callback, before the name is owned, so a second daemon that loses the name can still write a stale snapshot (reproduced: position 77 → 42). Load state only once the name is acquired, and use unique temp file names.
- **DS-04:** save errors are ignored. A favorite or ban looks accepted but is gone after a restart. Return a `Result`, log it, keep the state dirty and retry.
- **DS-05:** `Ban` only saves if a next wallpaper is applied, so banning the last eligible one (or a failed prepare) isn't saved. Save straight after `ban_current()`.
- **DS-06:** no `sync_all()` on the temp file or its folder, so a power cut can leave an old or empty file (which DS-01 then overwrites).

### 2. Nothing hangs the daemon (DS-07..DS-09, DS-15)

- **DS-07:** music listing, thumbnail lookups, state writes and download setup run on the main loop, so a stuck mount freezes every D-Bus method. Also `animated.js` finds the player executable synchronously inside the Shell. Move these to worker threads; resolve the player path asynchronously.
- **DS-08:** one hung ffmpeg holds `JOB_LOCK` in `prepare.rs` forever, so no uncached wallpaper can be shown again (reproduced). Add deadlines and kill superseded jobs; don't hold the lock across subprocess waits.
- **DS-09:** Cancel only sends SIGINT. A downloader that ignores it blocks the queue forever. Escalate after a grace period, and clean up leftovers at startup.
- **DS-15:** downloader output lines are read with no length limit. Cap line length and stored title/error text.

### 3. Media and cache robustness (DS-11..DS-14)

- **DS-11:** the JPEG writer's final flush isn't checked, so a full disk can rename a truncated JPEG into the cache, and it's reused from then on (reproduced). Flush explicitly; check cached files before reusing them.
- **DS-12:** a broken video (e.g. zero bytes) leaves music mode on with no player, and it's saved for the next login. Report player failure to the daemon and fall back to rotation.
- **DS-13:** the player's two-second exit watchdog starts only after GTK/GPU setup, so a player stuck during startup never exits. Start the stdin reader and watchdog first.
- **DS-14:** cache trimming can delete the file `prepare()` is about to return when it's bigger than the cache budget (e.g. a large converted animation). Protect the result before trimming.

### 4. Lower priority

- **DS-10:** `is_music_video()` uses a lexical `starts_with`, so `PlayMedia("<music>/../x.mp4")` or a symlink escapes the music folder. This isn't a privilege issue (the caller is already the user), but it pollutes `media.json`. Canonicalize the path on a worker before checking it.
- Race candidates from the audit's "not verified" list: `start_media()`'s error branch can stop newer music (`daemon.rs:799–823`), and overlapping rescans have no generation check. Probe these before fixing.
- `prefs.js:56`: the wallpaper-folder subtitle isn't markup-escaped (a folder name with `<` or `&` misrenders).

## Research

### Stop drawing a music video while the desktop is fully covered

**Why:** a music video keeps playing behind windows so the sound continues. Its picture is still redrawn 30 times a second even when maximized or fullscreen windows hide the whole desktop. Measured on 2026-10-25 with a 1080p30 VP9 music video while the user worked normally: GNOME Shell used about 13% of one core, against about 4% with a still wallpaper. The player itself used about 11% (decoding on NVDEC, decoder at 1%).

**Questions to answer:**

- How much of the extra GNOME Shell load comes from the background clones being repainted, compared with the user's own windows? Measure with the desktop covered and uncovered, and with the clones detached.
- Does detaching the clones, or hiding the background actors, while the desktop is covered stop the repaints? Does Mutter already skip occluded repaints?
- Can the player stop handing video frames to GTK while the audio continues: a playbin video-sink swap, a `valve`/`fakesink` branch, or dropping frames in `gtk4paintablesink`? Would that also stop the hardware decoding?
- What does it save in GPU power (`nvidia-smi --query-gpu=power.draw`) and battery life, if anything, given the NVIDIA GPU stays awake for the display anyway?
- Does showing the picture again on uncover look right (no stale frame or flash)?

**Done when:** there are before/after measurements and a recommendation, either to build it, and how, or to leave it.

### Keep music playing on the lock screen

Locking the screen disables the extension (no `unlock-dialog` session mode), which stops the player; it resumes from the saved position on unlock. Find out what `unlock-dialog` support would take, and whether the extensions.gnome.org review rules allow it for this kind of extension.

## Not yet exercised

- The desktop menu under rapid real clicks (exercised only programmatically in a headless shell, 2026-10-25).
- The Browse page with Pexels (Pexels was refusing new API keys on 2026-10-25; Pixabay works).
