# Wallslæd

Wallslæd changes your GNOME desktop wallpaper for you. Point it at a folder of pictures, and it shows them one by one on a timer, in random order, without repeats until it has gone through them all. It stays out of your way and uses very little memory, even with thousands of wallpapers.

It also does a few more things you might otherwise need several extensions for:

- **Right-click menu on the desktop:** skip to the next wallpaper, go back, mark favorites, never see one again.
- **Transitions:** wallpapers can change with an effect, from a simple crossfade to melting, shattering or burning away.
- **Animated wallpapers:** GIFs and videos play as your wallpaper, and pause when windows cover the desktop.
- **Music videos:** download a music video once, then play it as your wallpaper with sound, with the usual media controls.
- **Wallpaper browser:** search free animated wallpapers on Pixabay and Pexels and download them in one click.
- **Blur:** a frosted-glass look behind the overview, top bar, menus, notifications and lock screen.
- **Auto-hiding top bar:** the top bar slides away when a window needs the space.

Wallslæd is made for **GNOME 51 on Wayland** (Fedora 45).

## Getting started

### What you need

- GNOME 51, on Wayland (the default on most distributions)
- Rust, to build it (`cargo`), and on Fedora `libseccomp-devel` and `fontconfig-devel` for its image decoding; `glycin-loaders` and `bubblewrap` come with GNOME
- GStreamer with its GTK 4 video sink (on Fedora: `gstreamer1-plugin-gtk4`) and the usual codec plugins, for animated wallpapers
- `ffmpeg`, for GIFs and video wallpapers
- `yt-dlp`, only if you want music videos

### Install

From this folder, run:

```sh
./install.sh
```

Then **log out and back in**; GNOME only notices new extensions after that. If Wallslæd isn't switched on afterwards, turn it on in the Extensions app, or run `gnome-extensions enable wallslaed@hebbihebb`.

### Pick your wallpapers

Open the settings, either from the Extensions app or with:

```sh
gnome-extensions prefs wallslaed@hebbihebb
```

Choose your wallpaper folder and how often the wallpaper should change. Until you choose, Wallslæd uses your Pictures folder. Subfolders are included, and new pictures you add are picked up on their own.

If you were using another wallpaper changer, turn it off: two of them will keep overriding each other. The same goes for Blur my Shell and Hide Top Bar, if you'd like Wallslæd to do those jobs.

## Everyday use

### The desktop menu

Right-click an empty spot on the desktop:

| Item | What it does |
|---|---|
| Next Wallpaper / Previous Wallpaper | Skip ahead, or go back to the one before |
| Show File Location | Opens Files with the current wallpaper selected |
| Add to Favorites | Favorites come up more often (how often is a setting) |
| Never Show This Again | Removes it from the rotation for good; the file itself stays where it is |
| Pause Rotation | Keeps the current wallpaper until you resume |
| Music Videos | Pick a saved music video to play (see below) |
| Wallslæd Settings | Opens Wallslæd's settings (GNOME's own *Settings* below it opens GNOME Settings) |

### A keyboard shortcut for the next wallpaper

In GNOME Settings, go to *Keyboard → View and Customize Shortcuts → Custom Shortcuts*, and add one with the command `wallslaed next`.

### How the order works

Every wallpaper is shown once before any of them repeats, and one that just ended a round won't come straight back at the start of the next. Favorites show up more often. Wallslæd remembers where it was, so a restart or a log-out doesn't reset the order.

## Transitions

In the settings, under *Transitions*, pick how one wallpaper changes into the next, and how long the change takes:

Crossfade, Zoom, Pixelate, Wipe, Blur, Tilt, Burn, Freeze, Shatter, Ripple, Melt and Light Leak. Or pick *Random* for a different one each time, or *None* for an instant change.

The effects only run while the wallpaper is changing; they cost nothing the rest of the time.

## Animated wallpapers

Put GIFs or videos (MP4, WebM, MKV and the like) in your wallpaper folder, and they join the rotation like any picture.

- They pause on their own while maximized or fullscreen windows cover the screen, and, if you like, while your laptop is on battery.
- The overview and lock screen show a still frame.
- The first time a GIF comes up, Wallslæd quietly converts it to a video in the background, which is much lighter to play.
- Don't want them? Switch off *Include GIFs and Videos* in the settings.

## Music videos

A music video plays as your wallpaper with sound, and keeps playing while you work. It shows up in GNOME's media controls in the top bar and on the lock screen, and the media keys work.

1. In the settings, open the **Music Videos** page and paste a video link. It downloads in the background; you can close the settings meanwhile.
2. Play it from that page, or from the desktop menu under *Music Videos*.
3. While it plays, the desktop menu has *Pause Music*, *Mute* and *Stop Music Video*.

The wallpaper rotation waits while a music video plays. Wallslæd remembers where you left off in each video, so locking the screen or logging out picks up from there. Videos are saved in `~/Videos/Music` and never come up in the normal rotation.

Only download videos you have the right to keep.

## Finding new animated wallpapers

The **Browse** page in the settings searches Pixabay and Pexels for free animated wallpapers. Preview one, then download it straight into an `Animated` folder inside your wallpaper folder.

Each site needs a free API key, which you get by signing up on their website and paste into the Browse page once.

## Blur and the top bar

On the **Appearance** page in the settings:

- **Blur** gives the overview, the top bar, the dash, menus, quick settings, notifications, dialogs, app folders, the screenshot tool and the lock screen a blurred, frosted look. Set how strong and how dark it is, and switch any of those parts off.
- **Hide Over Windows** makes the top bar slide away while a window touches it, so maximized windows get the whole screen. It comes back in the overview, and when you push the mouse against the top edge of the screen (also over a fullscreen window).
- **App Windows** sets how strongly GNOME blurs behind apps that ask for it, such as GTK 4 apps with see-through backgrounds and some terminals. It's a GNOME 51 feature; off leaves GNOME's own strength.

The blur is built to be light. Instead of blurring your wallpaper over and over, Wallslæd makes one small blurred copy when the wallpaper changes and reuses it everywhere. Menus only blur what's behind them while they're open.

## Command line

Everything in the desktop menu also works from a terminal, which is handy for scripts and shortcuts:

```
wallslaed next          wallslaed favorite      wallslaed status
wallslaed prev          wallslaed ban           wallslaed rescan
wallslaed show          wallslaed pause         wallslaed resume
wallslaed toggle

wallslaed music             list saved music videos
wallslaed play <file>       play one
wallslaed stop              stop it
wallslaed download <link>   download one
```

## Troubleshooting

**The wallpaper keeps flipping back, or two wallpapers fight.**
Another wallpaper changer is running. Turn it off in the Extensions app. The installer warns you about the common ones.

**Nothing happened after installing.**
Log out and back in, then make sure Wallslæd is switched on in the Extensions app.

**The wallpaper doesn't change.**
Check that rotation isn't paused (desktop menu), and that your folder has pictures in it. `wallslaed status` shows what Wallslæd sees.

**Animated wallpapers show a still picture.**
They pause while windows cover the screen, and on battery if that setting is on. Otherwise, check that GStreamer and its GTK 4 video sink are installed.

**Blur or the top bar acts strangely.**
Make sure Blur my Shell and Hide Top Bar are switched off; they try to do the same things.

**The whole desktop freezes, usually while changing wallpapers quickly.**
That's a bug in GNOME itself (in GJS, its JavaScript engine; [GJS merge request !1134](https://gitlab.gnome.org/GNOME/gjs/-/merge_requests/1134) fixes it, not released yet). It can happen whenever the wallpaper changes, with or without Wallslæd, and is likelier the faster wallpapers change ([details](docs/gnome-gjs-deadlock.md)). Wallslæd changes the wallpaper in one step to make it rarer. If it happens, switch to a text console (Ctrl+Alt+F3), log in and run `pkill -KILL -x gnome-shell`, then go back with Ctrl+Alt+F2. That ends your session like a logout, so unsaved work in open apps is lost; restarting the computer does the same.

## Uninstall

```sh
./uninstall.sh
```

This removes the program and the extension but keeps your settings, favorites and history, in case you come back. To remove those too, run `./uninstall.sh --purge`. Your wallpapers and music videos are never touched.

## For developers

How the pieces fit together, measurements, and how to test without touching your own desktop: [docs/how-it-works.md](docs/how-it-works.md). Rules for coding agents are in [AGENTS.md](AGENTS.md).
