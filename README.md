# Wallslæd

A wallpaper rotator for GNOME Shell 50 on Wayland, built to stay light. It cycles through a folder of thousands of wallpapers without repeats, and adds:
- desktop right-click items, including **Next Wallpaper** and **Show File Location**
- optional GPU transition effects
- animated GIF and video wallpapers

## How it's put together

| Piece | What it does | Why it's separate |
|---|---|---|
| `wallslaed` daemon (Rust) | Scans the folder, picks the next wallpaper, shrinks big images to screen size, sets the background, serves a D-Bus API | Heavy work stays out of `gnome-shell`. Big image decodes there cause stutter, and GJS frees memory late, which looks like a leak. |
| GNOME Shell extension (GJS) | Desktop menu items, transition effects, hosting animated wallpapers | Only code inside the Shell can touch the desktop background and its menu |
| `wallslaed-player` (Rust, GTK 4) | Plays an animated wallpaper | Runs **only** while an animated wallpaper is showing. It exits when a still one takes over, so all its memory goes back to the system. |

### Random without repeats
Wallpapers are shuffled into a "bag":
- Every file is shown once before anything repeats.
- New files join the current round at random spots.
- The last few of one round can't come straight back at the start of the next.
- **Favorites** show up more often (*Favorite Weight*), and **Never Show This Again** removes a file for good.
- Position and history survive restarts (`~/.local/state/wallslaed/state.json`).

### Staying light
- The daemon runs one event loop with no polling. It wakes for the timer, a D-Bus call, or a file change in the folder, and is idle otherwise.
- Images larger than the screen are scaled once to screen size and cached in `~/.cache/wallslaed`, which keeps the newest 40 files. An 8K image would otherwise sit in the compositor as a ~130 MB texture.
- The next wallpaper is prepared ahead of time, so switching is instant.
- Transition effects are attached for the length of the transition and then removed.
- The timer pauses while the screen is locked, and changes once on unlock if it missed a tick.

### Animated wallpapers
- GIFs are converted once to VP9 video in the background (needs `ffmpeg`), because GIF decoding runs on the CPU every frame.
- Video is decoded in hardware on the GPU the compositor runs on, and passed to GTK without copies where the drivers allow it.
- Playback pauses when maximized or fullscreen windows cover every screen, and optionally on battery.
- The overview and lock screen show a still frame.

## Install

```sh
./install.sh
```

The script:
- builds the binaries and puts them in `~/.local/bin`
- installs the extension and the GSettings schema
- enables the `wallslaed.service` systemd user unit

On Wayland, GNOME Shell only notices a newly installed extension after you log out and back in. After that, run `gnome-extensions enable wallslaed@hebbihebb`.

Pick your folder and timer in the extension's settings: `gnome-extensions prefs wallslaed@hebbihebb`. The default folder is `~/Pictures`.

Remove it with `./uninstall.sh`. Add `--purge` to also forget settings, favorites and history.

## Use

- **Right-click the desktop:**
  - Next Wallpaper
  - Previous Wallpaper
  - Show File Location (opens Files with the file selected)
  - Add to Favorites
  - Never Show This Again
  - Pause Rotation
- **Command line:** `wallslaed next | prev | show | favorite | ban | pause | resume | toggle | rescan | status`
- **Keyboard shortcut:** in *Settings → Keyboard → Custom Shortcuts*, add one that runs `wallslaed next`.

## Notes for this laptop

- Another wallpaper changer (such as `azwallpaper`) will fight Wallslæd over the background. `install.sh` warns if one is enabled.
- `LIBVA_DRIVER_NAME=nvidia` is set in `/etc/environment` and in `~/.config/environment.d/va-api.conf`. That forces every VA-API app onto the NVIDIA driver, even on the AMD GPU. The player works around this for itself.
- Here the built-in panel and HDMI port are wired to the NVIDIA GPU, so GNOME composites on it. The player detects that and decodes with NVDEC on the same GPU. On an AMD/Intel-primary setup it uses VA-API on that GPU.

## Measured

These numbers come from a headless GNOME Shell 50 test session on this machine, with a 1920×1080 virtual monitor:

| | |
|---|---|
| Daemon, idle | ~10 MB RSS, 0 wakeups in 20 s |
| Daemon, after 300 rapid changes | ~10 MB RSS; peaks at ~180 MB only while first scaling an uncached 8K image, then returns it |
| Player, converted GIF playing | ~3% of one CPU core |
| Player, 1080p30 VP9 video playing | ~11% of one CPU core |
| Player, paused (covered or on battery) | ~0% |
| Player, static wallpaper | not running |

## Layout

```
daemon/     wallslaed: shuffle bag, scanning, pre-scaling, D-Bus service, CLI
player/     wallslaed-player: animated wallpaper renderer
extension/  wallslaed@hebbihebb: GNOME Shell extension and preferences
data/       GSettings schema, systemd user unit
```

`cargo test` runs the shuffle-bag, scanning and pre-scaling tests.
