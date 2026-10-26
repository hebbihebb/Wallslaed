# Roadmap

Nothing is scheduled. Items here are ideas to look into; each needs the user's go-ahead before work starts, and moves to `CURRENT_TASKS.md` when it does.

## After the Fedora 45 / GNOME 51 port

The port itself moved to `CURRENT_TASKS.md` (`fix/gnome-51`). Still open:

- **GJS wallpaper-change deadlock:** see `docs/gnome-gjs-deadlock.md` for the workarounds (biggest: stop writing GNOME's background settings on every change) and what to do when GJS !1134 is released, or if it never is. Each workaround needs the user's go-ahead.
- **Glycin wallpaper loading** (GNOME 51 loads backgrounds through `BackgroundTextureCache`): check by eye that AVIF and HEIC wallpapers show (JXL ones rotate in), and how long very large images take.

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
