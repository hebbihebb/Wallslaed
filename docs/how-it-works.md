# How Wallslæd works

Technical notes for working on Wallslæd. For using it, see the [README](../README.md); for the rules coding agents follow, see [AGENTS.md](../AGENTS.md).

## How it's put together

| Piece | What it does | Why it's separate |
|---|---|---|
| `wallslaed` daemon (Rust) | Scans the folder, picks the next wallpaper, shrinks big images to screen size, bakes the blur, sets the background, serves a D-Bus API | Heavy work stays out of `gnome-shell`. Big image decodes there cause stutter, and GJS frees memory late, which looks like a leak. |
| GNOME Shell extension (GJS) | Desktop menu items, transition effects, hosting animated wallpapers, blur, top-bar autohide | Only code inside the Shell can touch the desktop background, its menu and the Shell's own surfaces |
| `wallslaed-player` (Rust, GTK 4) | Plays an animated wallpaper or music video | Runs **only** while one is showing. It exits when a still one takes over, so all its memory goes back to the system. |

## Random without repeats

Wallpapers are shuffled into a "bag":
- Every file is shown once before anything repeats.
- New files join the current round at random spots.
- The last few of one round can't come straight back at the start of the next.
- **Favorites** show up more often (*Favorite Weight*), and **Never Show This Again** removes a file for good.
- Position and history survive restarts (`~/.local/state/wallslaed/state.json`).

## Staying light

- The daemon runs one event loop with no polling. It wakes for the timer, a D-Bus call, or a file change in the folder, and is idle otherwise.
- Images larger than the screen are scaled once to screen size and cached in `~/.cache/wallslaed`. An 8K image would otherwise sit in the compositor as a ~130 MB texture. Scaled and converted pictures are kept up to the `cache-size` setting (2 GB by default, Preferences > Cache Size), newest first, so a JPEG XL conversion (about half a second) isn't redone every round; the small baked blurs keep the newest 40. When a picture is decoded to be scaled or converted, its blur is baked from those pixels, not decoded again.
- JPEG XL, AVIF and HEIC wallpapers always become a cached JPEG, at screen size or smaller, decoded with glycin (GNOME's own sandboxed image loaders). GNOME Shell decoding them itself stalled the desktop for up to a second on 5K JPEG XL files, and the blur bake couldn't read them.
- The next and previous wallpapers are prepared ahead of time, so switching is instant. The daemon reports them as `next-picture` and `previous-picture`, and the extension (`preload.js`) decodes them through glycin's async loaders and uploads them to the GPU a few rows at a time while the shell is idle. GNOME 51 would otherwise decode and upload each one on the compositor's thread at the moment of the change, a 125-155 ms freeze at 4K even with transitions off (550 ms for a 16-bit PNG). When GNOME loads one of them, it gets the finished texture.
- Any other wallpaper GNOME asks for (the end of a shuffle round, a picture set from outside Wallslæd) is loaded the same way on demand: it shows up 0.2-0.5 s later instead of freezing the screen. Only the first wallpaper after login, loaded before the extension starts, and a picture this fails to load go through GNOME's own loader.
- None of these textures enter GNOME's texture cache, which keeps every wallpaper it has loaded on the GPU for the life of the session (about 2 GB after 64 4K wallpapers in a test). Only the current, next and previous ones are kept.
- Transition effects are attached for the length of the transition and then removed. Blur fades through two fixed blurs (radius 24 and 64), each computed once, instead of animating the radius, which redid the whole blur every frame. A shader effect is compiled the first time it is drawn, a 40-130 ms freeze; so a few seconds after each change the effect the next one will play (Random picks it then) is drawn once on a tiny hidden actor.
- The timer pauses while the screen is locked, and changes once on unlock if it missed a tick.

## Animated wallpapers

- GIFs are converted once to VP9 video in the background (needs `ffmpeg`), because GIF decoding runs on the CPU every frame.
- Video is decoded in hardware on the GPU the compositor runs on, and passed to GTK without copies where the drivers allow it.
- Playback pauses when maximized or fullscreen windows cover every screen, and optionally on battery. On a screen covered by one, the video is taken off the desktop even if it keeps playing (a music video, or other screens still visible), so the shell stops redrawing it: about 5% of a core for a 1080p30 music video. A translucent maximized window then shows the still frame behind it.
- The overview and lock screen show a still frame.

## Music videos

- They live in their own folder (default `~/Videos/Music`) and never come up in the rotation; you pick one, and the rotation waits until you stop it or change the wallpaper.
- Downloads run `yt-dlp` (in `~/.local/bin`) one at a time, in their own systemd scope, so a multi-gigabyte file doesn't count against the daemon's memory limit. Unfinished files stay out of the music folder.
- Sound plays even when windows cover the desktop. The player shows up in GNOME's media controls (top bar, lock screen, media keys).
- Where each video was left off is saved every 15 seconds (`~/.local/state/wallslaed/media.json`), so locking the screen or logging out resumes it.

## Blur and the top bar

- The overview, top bar and dash don't run a blur at all. While preparing a wallpaper, the daemon also writes a small blurred, darkened copy (about an eighth of the screen size, `{hash}-blur-{strength}-{brightness}.png` in the cache) and reports it as `blurred` in its state. The extension (`blur.js`) shows it scaled up: one tiny texture per monitor, shared by every surface. Without a baked copy (a format the daemon can't read, or no daemon), a GPU blur of the wallpaper stands in.
- Without a baked copy, the GPU blur runs once per monitor: one blurred actor in a hidden holder, which the overview and top bar show as clones.
- Menus, quick settings, notifications, dialogs, OSDs, app folders and the screenshot panel float over windows, so `liveBlur.js` uses GNOME's own background blur in a sibling actor just below each popup, attached on open and removed on close. Menus draw offscreen, so a blur inside them would see nothing. One rectangle per popup; the popup's own tint does the darkening (its alpha follows the brightness setting), so the square blur doesn't show past rounded corners.
- That blur copies exactly its own rectangle of the screen. When GNOME redraws only part of a popup (a hovered item), the rest of that rectangle holds last frame's popup and blur, and blurring it left lasting boxes. So while a popup is in use (pointer or key events inside it, and 300 ms after), every frame redraws its whole rectangle: a redraw queued at the stage's `before-update` joins the same frame. Any other frame that redraws only part of it (seen in `paint-view`'s redraw region) has the whole rectangle redrawn on the next frame.
- Over bare desktop the popup shows its piece of the baked wallpaper blur instead (the same one the overview uses), with no blur shader and no whole-rectangle redraws: about half the shell CPU while hovering a menu. Every frame it checks that nothing else is under the popup (a window, another popup, the top bar, a transition, a video wallpaper, something like Fedora's logo) and switches back to the live blur if anything is. The baked copy has the brightness in it, so the tint leaves that part out.
- **App windows** (GNOME 51): apps can ask Mutter to blur behind their windows (`ext-background-effect-v1`; GTK 4.24's `backdrop-filter`). `appBlur.js` sets the compositor-wide radius with `global.compositor.set_background_blur_params()` and restores GNOME's values when off, on disable and on lock.
- The lock screen keeps GNOME's own blur; `lockBlur.js` sets its radius and brightness from the settings. The extension runs in the `unlock-dialog` session mode for this alone: everything else is torn down on lock and rebuilt on unlock, as if the shell had disabled the extension.
- `topbar.js` gives the top bar's strut up and slides the bar out with `translation_y` while a window of the focused app overlaps it. It shows in the overview, while a panel menu is open, and when the pointer pushes against a pressure barrier at the top edge.

## GNOME's wallpaper-change deadlock

GNOME Shell (GJS 1.90 and earlier) can freeze for good around wallpaper changes; the daemon writes both picture keys in one transaction to make it rarer. Cause, measurements, workarounds and what to do whether or not upstream fixes it: [gnome-gjs-deadlock.md](gnome-gjs-deadlock.md).

## Measured

These numbers come from a headless GNOME Shell 50 test session with a 1920×1080 virtual monitor:

| | |
|---|---|
| Daemon, idle | ~10 MB RSS, 0 wakeups in 20 s |
| Daemon, after 300 rapid changes | ~10 MB RSS; peaks at ~180 MB only while first scaling an uncached 8K image, then returns it |
| Player, converted GIF playing | ~3% of one CPU core |
| Player, 1080p30 VP9 video playing | ~11% of one CPU core |
| Player, paused (covered or on battery) | ~0% |
| Player, static wallpaper | not running |
| GNOME Shell, 200 wallpaper changes, 50 menus, 30 disable/enable cycles | no blur: +2.6 MB; Wallslæd blur: +2.8 MB; Blur my Shell: +26 MB (and 15 MB more to start with) |

## Hybrid-GPU laptops

- A system-wide `LIBVA_DRIVER_NAME=nvidia` (in `/etc/environment` or `~/.config/environment.d/`) forces every VA-API app onto the NVIDIA driver, even on the integrated GPU. The player works around this for itself.
- When the display is wired to the NVIDIA GPU, GNOME composites on it; the player detects that and decodes with NVDEC on the same GPU. On an AMD/Intel-primary setup it uses VA-API on that GPU.

## Install script

`install.sh` builds the binaries and puts them in `~/.local/bin`, installs the extension (`*.js`, `*.css`, `metadata.json`) and the GSettings schema, enables and restarts the `wallslaed.service` systemd user unit, and warns if another wallpaper changer is enabled.

## Layout

```
daemon/     wallslaed: shuffle bag, scanning, pre-scaling, blur baking, D-Bus service, CLI
player/     wallslaed-player: animated wallpaper renderer
extension/  wallslaed@hebbihebb: GNOME Shell extension and preferences
data/       GSettings schema, systemd user unit
tools/      isolated-session.sh: test without touching the real desktop
docs/       this file
```

## Testing

`cargo test` runs the shuffle-bag, scanning, pre-scaling, blur-baking and download-parsing tests. Anything that needs GSettings, D-Bus or a GNOME Shell runs in a throwaway session: `tools/isolated-session.sh --install <scratch-dir> <script>` (see `AGENTS.md`).
