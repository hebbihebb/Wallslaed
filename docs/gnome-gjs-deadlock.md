# GNOME's wallpaper-change deadlock

Found 2026-10-26 while stress-testing `feature/shell-blur`. Not a Wallslæd bug, but changing wallpapers is what triggers it, so it matters more to Wallslæd than to most GNOME users.

## What happens

GNOME Shell freezes for good: the screen stops updating, input is ignored, nothing crashes and nothing is logged. One CPU core sits at 100%, in a thread called `dconf worker`.

It is an ABBA deadlock between two locks in GJS, GNOME Shell's JavaScript engine glue, and GLib's GSettings:

| Thread | Holds | Waits for |
| --- | --- | --- |
| Main (JS garbage collector sweeping) | GJS toggle queue lock | GSettings backend lock, to run the weak notify of a `GSettings` being freed |
| dconf worker (delivering a settings change) | GSettings backend lock | GJS toggle queue lock (`g_weak_ref_get` -> toggle notify), spinning |

Stacks: `.scratch/blur51/churn-deadlock-stack.txt`, `.scratch/blur51/churn2-stack.txt`, `.scratch/blur51/baseline/run*/home/hang-stack.txt`.

## Why wallpaper changes trigger it

On every change of `org.gnome.desktop.background picture-uri`, GNOME's `ui/background.js` (51.0) drops its `Background` object and creates a new one. Each `Background` creates a `Gio.Settings` for the interface schema and a `GnomeDesktop.WallClock` (which holds another `GSettings`). The old ones become garbage. So each change produces exactly the objects the collector is freeing in the deadlock, and the change itself is a settings write that makes the dconf worker deliver signals. The two only have to overlap once.

## Status upstream

- GJS merge request [!1134](https://gitlab.gnome.org/GNOME/gjs/-/merge_requests/1134) "object: fix GC sweep deadlock with toggle notifies and late toggle notify use-after-free": open, not merged (checked 2026-11-06; its branch, two commits on master from 2026-10-17, isn't in master). It releases native objects after dropping the toggle queue lock.
- Fedora 45 ships GJS 1.90.1 without it. 1.90.1 has other toggle and GC fixes (!1121, !1123, !1126) but still deadlocks (below). GNOME 50 / Fedora 44 had the same code; the 2026-10-25 freeze while cycling wallpapers from the desktop menu fits it exactly.
- Ubuntu 26.04's GJS 1.88.0 (GNOME 50.1) did not hang in the same test (below), on different hardware and a different GNOME: not enough to say 1.88 is unaffected (the 2026-10-25 freeze was on GNOME 50).
- Same bug elsewhere: Cinnamon's GJS fork, [linuxmint/cjs#140](https://github.com/linuxmint/cjs/pull/140); an extension that changes pictures, [gnome-air-raid-map#10](https://github.com/p5ych0/gnome-air-raid-map/issues/10).

## Measurements (headless GNOME Shell 51, isolated session)

| Setup | Changes every 0.35 s | Result |
| --- | --- | --- |
| GNOME alone, `picture-uri` and `picture-uri-dark` written separately (as Wallslæd did) | up to 200 | hung in 4 of 4 runs, after 20-100 changes |
| GNOME alone, both keys in one dconf transaction | 200 | 0 of 3 runs hung |
| Wallslæd (daemon one write, extension with transitions and blur) | 200 | hung once |
| Wallslæd, changes every 1.5 s | 60 | no hang |

Rechecked 2026-11-06, GNOME alone, two writes, 0.35 s apart, 200 changes (`baseline/test.sh`):

| GJS | Result |
| --- | --- |
| 1.90.1 (Fedora 45, GNOME 51.1) | hung in 3 of 3 runs, after at most 20, 180-200 and at most 20 changes; one stack is the exact ABBA above (main thread in `g_settings_backend_watch_weak_notify` from `g_object_remove_toggle_ref`, dconf worker in `ToggleQueue::lock()` from `g_settings_backend_dispatch_signal`) |
| 1.90.1 with !1134 backported, loaded into the test shell only | 0 of 8 runs hung |
| 1.88.0 (Ubuntu 26.04, GNOME 50.1, other hardware) | 0 of 3 runs hung; doesn't isolate GJS (other machine, distro and GNOME, whose `ui/background.js` may churn settings differently) |

The backport is a 3-way merge with a trivial `#include` conflict in `toggle.cpp`, and `gjs_error()` replaced by `g_error()` (the former only exists in master); it builds as C++17 and passes all 30 toggle-queue unit tests, the MR's new ones included. Patch and logs: `.scratch/gjs-1134/`.

Scripts: `.scratch/blur51/baseline/test.sh` (two writes), `single.sh` (one), `.scratch/blur51/churn.sh` (Wallslæd). Run them with `tools/isolated-session.sh`; never against the real session.

## Can we work around it?

Not completely from outside GJS: any GNOME code that frees a JS-wrapped `GSettings` while any program writes a dconf key can hit it. But Wallslæd causes most of the risk on this machine, and can remove most of it.

**Done (`e08eacf`):** the daemon writes both picture keys in one transaction, so a change sends one signal instead of two. The second signal used to arrive just as GNOME dropped the old `Background`.

**Possible next steps, biggest effect first:**

1. **Stop changing GNOME's background settings on every wallpaper change.** The extension already hooks GNOME's background code for transitions. It could give the new picture to the existing background directly (`Meta.Background.set_texture()`, which GNOME 51 uses itself), so GNOME never creates a new `Background`, `Gio.Settings` or `WallClock`, and no dconf write happens. The daemon would write `picture-uri` only now and then (say every few minutes, and when the extension is disabled or the session ends) so GNOME Settings, the login screen and other apps still see a recent wallpaper. This removes both triggers for Wallslæd's own changes. Costs: real work in `transitions.js`/`blur.js` and the daemon, GNOME Settings shows a slightly old wallpaper, and anything else reading `picture-uri` lags.
2. **Collect garbage at a quiet moment.** After a change has settled, the extension could run the collector itself (`System.gc()`), so the old background is freed while nothing is writing settings, instead of whenever GJS decides. Cheap to try and measure with the scripts above; it narrows the window, it doesn't close it.
3. **Share one interface `Gio.Settings` and `WallClock` between GNOME's backgrounds** by overriding `Background._init`. Removes the garbage itself, but overriding a GObject class's `_init` from an extension is fragile across GNOME versions. Only if 1 is rejected.
4. **Recommend a calmer pace.** A minimum interval in the preferences, and the desktop menu's Next/Previous ignoring clicks closer than about a second apart.

## If it gets fixed

- Watch !1134 and the Fedora `gjs` package. Once a fixed version is installed, rerun `.scratch/blur51/baseline/test.sh` (two writes, 0.35 s) a few times: it should survive 200 changes every time.
- Keep the one-write change; it is correct regardless.
- Remove the README's freeze note, the ROADMAP item and the AGENTS.md stress-test limit (keep the lesson).
- Workaround 1, if built, could stay for its other benefit (no settings traffic per change), or go to keep the code simpler.

## If it never gets fixed

- Build workaround 1. It's the only change that takes Wallslæd out of the trigger for its own wallpaper changes.
- Keep stress tests at 1.5 s or slower, and make them stop on a hang, as `.scratch/blur51/churn.sh` does.
- Help it land: comment on !1134 with the headless reproduction (GNOME Shell alone, no extensions, `baseline/test.sh` hangs within 100 changes). A minimal reproduction is often what gets a fix merged.
- Last resort, for one machine only: rebuild Fedora's `gjs` package with the !1134 backport (`.scratch/gjs-1134/`; the raw MR doesn't apply to 1.90.1). Use `Release: 1.wallslaed1%{?dist}` rather than `%autorelease`: it sorts above the official `-1` and below any official `-2` or 1.90.2, so a Fedora update replaces it; don't versionlock it. It replaces a system library, so that's the user's call, never an agent's.
- If it happens anyway: Ctrl+Alt+F3, log in, `pkill -KILL -x gnome-shell`, Ctrl+Alt+F2 (ends the session like a logout). Before killing, `eu-stack -p $(pgrep -x gnome-shell)` > a file confirms it's this bug (look for `ToggleQueue::lock` and `g_settings_backend_watch_weak_notify`).
