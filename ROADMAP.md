# Roadmap

Nothing is scheduled. Items here are ideas to look into; each needs the user's go-ahead before work starts, and moves to `CURRENT_TASKS.md` when it does.

## After the Fedora 45 / GNOME 51 port

The port itself moved to `CURRENT_TASKS.md` (`fix/gnome-51`). Still open:

- **GJS wallpaper-change deadlock:** see `docs/gnome-gjs-deadlock.md` for the workarounds (biggest: stop writing GNOME's background settings on every change) and what to do when GJS !1134 is released, or if it never is. Each workaround needs the user's go-ahead.
- **Glycin wallpaper loading** (GNOME 51 loads backgrounds through `BackgroundTextureCache`): check by eye that AVIF and HEIC wallpapers show (JXL ones rotate in), and how long very large images take.

## Research

### Stop drawing a music video while the desktop is fully covered

**Why:** a music video keeps playing behind windows so the sound continues. Its picture is still redrawn 30 times a second even when maximized or fullscreen windows hide the whole desktop. Measured on 2026-10-25 with a 1080p30 VP9 music video during normal desktop use: GNOME Shell used about 13% of one core, against about 4% with a still wallpaper. The player itself used about 11% (decoding on NVDEC, decoder at 1%).

**Questions to answer:**

- How much of the extra GNOME Shell load comes from the background clones being repainted, compared with the user's own windows? Measure with the desktop covered and uncovered, and with the clones detached.
- Does detaching the clones, or hiding the background actors, while the desktop is covered stop the repaints? Does Mutter already skip occluded repaints?
- Can the player stop handing video frames to GTK while the audio continues: a playbin video-sink swap, a `valve`/`fakesink` branch, or dropping frames in `gtk4paintablesink`? Would that also stop the hardware decoding?
- What does it save in GPU power (`nvidia-smi --query-gpu=power.draw`) and battery life, if anything, when a discrete GPU drives the display and stays awake anyway?
- Does showing the picture again on uncover look right (no stale frame or flash)?

**Done when:** there are before/after measurements and a recommendation, either to build it, and how, or to leave it.

### Keep music playing on the lock screen

Locking the screen disables the extension (no `unlock-dialog` session mode), which stops the player; it resumes from the saved position on unlock. Find out what `unlock-dialog` support would take, and whether the extensions.gnome.org review rules allow it for this kind of extension.

### Performance ideas left over from the 2026 rendering investigation

Measured or prototyped on `fix/perf-deep-dive`; none was good enough, or safe enough, to ship as it stands.

- **A popup blur that keeps its buffers.** GNOME's blur effect makes three new framebuffers for every repaint whose source changed. A prototype (a JS `Clutter.Effect` drawing through `Clutter.LayerNode.new_to_framebuffer` into buffers it keeps) cut that to almost none: hover p99 halved, shell CPU while hovering about 17% lower, and the look matched in screenshots. It is not built in: it replaces a GNOME-maintained effect with our own shader. Better fixed upstream, in `ShellBlurEffect` / `ClutterBlurNode`.
- **Heavy transitions' first use.** Burn, Freeze and Shatter still cost one 50-100 ms frame the first time their shader is used in a session: now a hidden warm-up frame a few seconds after a change, no longer the visible one. Bounding the shader loops by a uniform (no unrolling) made no measurable difference. Left to try: replacing their noise and shard searches with small precomputed textures (needs a second texture on both GNOME 50's and 51's effect classes).
- **Video frames straight to GL.** Wrapping the GTK sink in `glsinkbin` makes NVDEC frames arrive as GL textures instead of system memory, but cost more CPU on NVIDIA (player 13.5% -> 16%, shell 7% -> 9%, 1080p30). Worth measuring again on VA-API and on newer GStreamer.
- **Pictures changed outside Wallslæd.** A wallpaper file rewritten in place isn't reloaded, and a picture set from outside keeps Wallslæd's old baked blur. A fix must not race the daemon's own background writes.
- **Colour fidelity of prepared pictures:** ICC profiles, CMYK, HDR and alpha are not carried into the prepared JPEG; decide what is supported.
- **A persistent wallpaper presenter** (one background actor per monitor, no GNOME `Background` per change): fewer actors and settings writes per change, but a large change with its own risks.

## Not yet exercised

- The desktop menu under rapid real clicks (exercised only programmatically in a headless shell, 2026-10-25).
- The Browse page with Pexels (Pexels was refusing new API keys on 2026-10-25; Pixabay works).
