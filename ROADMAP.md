# Roadmap

Nothing is scheduled. Items here are ideas to look into; each needs the user's go-ahead before work starts, and moves to `CURRENT_TASKS.md` when it does.

## Fedora 45 / GNOME Shell 51 port

Researched 2026-10-26 while the user prepared to install the Fedora 45 beta (GNOME 51, Linux 7.2), against the GNOME Shell and Mutter `51.rc` sources and the [GNOME 51 porting guide](https://gjs.guide/extensions/upgrading/gnome-shell-51.html). Nothing here can be tested before the upgrade: the isolated headless session runs whatever GNOME Shell is installed. The first two items mean the current build does **not** work on GNOME 51.

### Needed, or the extension does not load

1. **`metadata.json` lists only `"50"`.** GNOME Shell 51 refuses the extension as out of date. Add `"51"` (keep `"50"` only if the port below still works on 50).
2. **`Shell.GLSLEffect` is gone** (replaced by `Clutter.ShaderEffect` with a `Cogl.Snippet`). `transitions.js:269` subclasses it at module load, so the import throws and the *whole* extension fails: no desktop menu, transitions or animated wallpapers, not just the shader effects. Port the effect class:
   - `vfunc_build_pipeline()` + `add_glsl_snippet(..., false)` becomes `vfunc_get_static_snippet()` returning `Cogl.Snippet.new(Cogl.SnippetHook.FRAGMENT, declarations, null)` with `snippet.set_post(code)`. Use `set_post`, not `set_replace` as in the guide's example: our code runs after the default texture lookup and edits `cogl_color_out`. GNOME 51's `js/ui/lightbox.js` (`RadialShaderEffect`) is a working example.
   - Uniforms are set by name: `set_uniform_float('progress', 1, [p])`; drop `get_uniform_location`. Check that setting `seed` on a shader that doesn't use it is harmless.
   - Keep one class per shader: `Clutter.ShaderEffect` also caches the program per type.
   - Supporting 50 and 51 in one build would need both code paths (50's `Clutter.ShaderEffect` uses `get_static_shader_source`). Simpler: target 51 only, and keep the backups of the 50 builds.

### Needed, but nothing visibly breaks

3. **`PopupMenu.close()` takes `{animate: false}`** instead of `BoxPointer.PopupAnimation.NONE`. `transitions.js:356` passes `0`, which GNOME 51 reads as "animate", so the outgoing wallpaper's menu would animate closed while that wallpaper is destroyed: the case behind the 2026-10-25 `boxpointer.js` disposed-object errors. `{animate: false}` also works on 50 (an object masks to no animation).

### Worth doing

4. **Reduced motion:** GNOME 51 adds `St.Settings.get().reducedMotion` (`St.ReducedMotion.REDUCE`). `transitions.js` only checks `enable_animations`; with reduced motion on, play the crossfade instead of the moving effects.
5. **Wallpapers now load through Glycin** (`BackgroundTextureCache`, async `Background._loadImage()`). Our `_swapBackgroundActor` override matches 51's unchanged method, so the transitions hook needs no change. After the upgrade, check that AVIF, HEIC and JXL wallpapers still show, and how long very large images take (the `prescale` setting may matter more).
6. **`feature/shell-blur`:** apart from the shared items above, its hooks still exist in 51 (`Shell.BlurEffect`, `UnlockDialog._updateBackgroundEffects`, `PopupMenuManager.addMenu`). Recheck `liveBlur.js`'s other overrides when that branch resumes.

### After the upgrade, not code

- Rebuild and reinstall (`cargo build --release`, then the install, with the user's go-ahead): the daemon and player link against the new GLib, GTK 4 and GStreamer.
- Freeze debugging: Fedora 45 planned to restrict `ptrace` to child processes unless a debugger package pulls in `yama-ptrace-enable` (Change "Restrict ptrace by default"; approval not confirmed). If so, `eu-stack -p $(pgrep -x gnome-shell)` from `CURRENT_TASKS.md` needs `sudo`.
- Unaffected: the kernel is already 7.2 on Fedora 44; `~/.local/bin/yt-dlp` is a standalone binary, so Python 3.15 doesn't break it; RPM signature enforcement doesn't touch our files. The NVIDIA akmod rebuilds as usual.

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
