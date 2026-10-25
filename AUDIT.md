# Wallslæd extension bug audit

Date: 2026-10-25. Reviewed snapshot: `ad30b95b300be0c107a979f92162647c234e1f28` (`main` at audit start). Target: GNOME Shell 50 / Wayland; installed Shell: **50.6**.

**Result: 7 confirmed findings — 2 High, 3 Medium, 2 Low. No confirmed Critical shell crash or freeze.** The daemon abort in F1 is not a compositor crash.

All locations below refer to that snapshot. Another agent began changing the daemon, player and schema during this audit; the user instructed this audit to work around that activity. This report deliberately preserves a stable review target. Recheck findings against the eventual implementation before applying fixes.

## Scope and evidence

Read all six extension JavaScript modules, `metadata.json`, the schema and README, plus `daemon/src/daemon.rs` and `player/src/main.rs` for their contracts. Checked the daemon entrypoint's bus constants. Compared the background override against both upstream Shell 50.0 and the installed 50.6 JavaScript resource, and consulted matching upstream 50.6 sources for native behavior.

Verification performed:

- `cargo build --workspace --offline --locked`: passed.
- `cargo test --workspace --offline --locked`: passed, 12 daemon tests; player has no tests. These commands completed before the concurrent player changes were detected; they do not validate the other agent's eventual changes or Shell integration.
- `glib-compile-schemas --strict --dry-run data`: passed against the original schema.
- Parsed and structurally compared the two D-Bus XML declarations: identical.
- Ran disposable GJS processes with real GLib/GIO and in-memory harnesses that evaluated the relevant extension methods. Shell menu objects were substituted where necessary; this is component-level reproduction, not a claim that the whole extension ran in a compositor.
- Tested the built daemon's pause call on a **separate private D-Bus session**, with `GSETTINGS_BACKEND=memory`, core dumps disabled and state/cache destinations under nonexistent `/proc` paths. The live daemon and live background settings were not used.
- Read existing Shell/daemon journal entries. No Wallslæd-specific Shell exception was found in the filtered output. Two disposed BoxPointer messages had insufficient attribution to count as findings. The daemon log showed startup/library counts, not evidence explaining the historical player crashes.

No install/uninstall scripts, live extension enable/disable operations, live wallpaper commands, commits, branches or formatting operations were run. No headless compositor integration test was performed. Audit scripts and fetched upstream sources were kept in memory; `AUDIT.md` is the only authored file.

## Confirmed findings, most severe first

### F1 — High — The menu's pause call can abort the daemon

**Location:** `extension/wallslaed@hebbihebb/menu.js:39`; contract implementation at `daemon/src/daemon.rs:198`, `daemon/src/daemon.rs:203`, `daemon/src/daemon.rs:235`, `daemon/src/daemon.rs:439`.

**Defect:** `SetPaused` and `TogglePause` retain an immutable `RefCell<Daemon>` borrow while changing GSettings, whose synchronous change callback needs a mutable borrow and panics.

**Failure scenario:** Start with rotation unpaused → open the desktop menu → select **Pause Rotation** → `TogglePause` calls `settings.set_boolean()` while `s = d.borrow()` remains alive → `changed::paused` calls `restart_timer()` → `borrow_mut()` fails and the daemon aborts before replying. The extension loses its D-Bus owner, and animated playback is stopped by its null-state handling. With the supplied service unit, systemd attempts recovery after its restart delay. Calling `SetPaused(true)` also has the same outstanding-borrow pattern when it actually changes the value.

**Reproduction evidence:** On the private bus, `GetState` returned `paused=false`; `TogglePause` returned `org.freedesktop.DBus.Error.NoReply` with “Message recipient disconnected from message bus without replying”; the daemon exited **-6 / SIGABRT**, and stderr ended with `thread caused non-unwinding panic. aborting.` A separate memory-backend GSettings probe confirmed that the `changed` handler executes inside `set_boolean()`. This is a runtime contract failure even though the XML signatures match. The private-bus result establishes the synchronous-backend failure; the live user's dconf backend was not modified to test its timing.

**Proposed fix:** Clone the settings object and release the daemon borrow before either setter. For example, in `TogglePause`:

```rust
let settings = d.borrow().settings.clone();
let paused = settings.boolean("paused");
let _ = settings.set_boolean("paused", !paused);
```

Use the same borrow-release pattern for `SetPaused`. Verify both methods on a private bus and check that each returns normally and emits the updated state. Included despite its Rust location because the extension directly invokes this broken contract.

### F2 — High — Menu destroy handlers accumulate across lock/unlock cycles

**Location:** `extension/wallslaed@hebbihebb/menu.js:47`, `extension/wallslaed@hebbihebb/menu.js:62`.

**Defect:** Each decoration connects a menu `destroy` callback that captures its `DesktopMenu` instance, but `DesktopMenu.destroy()` never disconnects it.

**Failure scenario:** Keep a static wallpaper unchanged → open its desktop menu → lock/unlock, causing disable/enable → open the same background's menu again → repeat. Disable removes the injected items and marker, but the underlying Shell menu survives. Every subsequent decoration adds another callback. That menu retains all previous `DesktopMenu` instances and their daemon references until its background actor is eventually replaced. Paused rotation makes the retention arbitrarily long.

**Reproduction evidence:** Evaluating the original decoration/cleanup methods with a persistent menu emitter for 100 cycles left **100 connected destroy callbacks** after cleanup. This is accumulation, not merely a temporary async reference. Shell 50.6 ties the menu's destruction to destruction of its background actor; disabling this extension does not destroy that actor. See [backgroundMenu.js, lines 29–66](https://github.com/GNOME/gnome-shell/blob/50.6/js/ui/backgroundMenu.js#L29-L66).

**Proposed fix:** Store each menu's destroy signal ID, e.g. in a `Map<menu, id>`. During disable, disconnect those handlers as well as destroying the section/separator and deleting `_wallslaed`. On actual menu destruction, remove its map entry. Verify repeated decoration/cleanup against one surviving menu leaves zero extension handlers after every disable.

### F3 — Medium — Concurrent stdin writes silently discard control commands

**Location:** `extension/wallslaed@hebbihebb/animated.js:148`, `extension/wallslaed@hebbihebb/animated.js:153`, `extension/wallslaed@hebbihebb/animated.js:253`.

**Defect:** `_send()` starts another asynchronous write without serializing against an outstanding write, then treats every write failure as an already-dead player.

**Failure scenario:** A control write remains outstanding → another playback update or `_stop()` submits `play`, `pause` or `quit` to the same stream → GIO rejects the second operation as pending → the catch silently drops it. A lost final `play` can leave a recovered player paused while `_paused` already says false; a lost `quit` leaves it alive until the kill timeout. To reproduce the playback case deterministically, use a test child whose stdin is blocked/backpressured, request pause, request play while that write remains pending, then allow the child to read. It receives pause without the final play. Normal idle coalescing reduces this race; it does not provide stream serialization under backpressure.

**Reproduction evidence:** The original `_send()` method, a real `Gio.Subprocess` running `cat`, and two immediate calls (`pause`, `play`) produced **`Stream has outstanding operation`** from the second `write_bytes_finish()` and delivered only **`["pause"]`**. The diagnostic was captured by the harness before the extension's catch swallowed it. This confirms the helper/API failure; rapid UI clicks under ordinary writable-pipe conditions were not demonstrated to produce a permanently paused player.

**Proposed fix:** Give each player a serialized command writer. Await each write before starting the next, preserve a final desired playback state, and send/close stdin in order during shutdown. Use `write_all_async()` or handle short writes. Distinguish cancellation/broken pipe from pending or other unexpected failures; only update the recorded delivered state after successful delivery, or retry toward the desired state.

### F4 — Medium — Shutdown sources remain scheduled after disable

**Location:** `extension/wallslaed@hebbihebb/animated.js:141`, `extension/wallslaed@hebbihebb/animated.js:271`.

**Defect:** `_stop()` creates a two-second GLib source without retaining its ID, so neither normal child exit nor `destroy()` can remove it.

**Failure scenario:** Show an animated wallpaper → disable/lock → `destroy()` calls `_stop()`, which creates a new source during teardown → disable returns while extension-owned code is still scheduled. The same happens for each rapid wallpaper replacement; even players that quit immediately leave their fallback source until its deadline. Repeated fast cycles accumulate outstanding callbacks/process references temporarily.

**Reproduction evidence:** Calling the original `destroy()` with a disposable `sleep` subprocess left a source discoverable by `GLib.MainContext.default().find_source_by_id(timer)` **after destroy returned**. It then ran and killed the child. The retention is bounded by the timeout under a progressing main loop; this is not evidence of an indefinite leak or a null-settings crash.

**Proposed fix:** Track retiring player records and their timeout IDs, including players no longer in `_player`. Remove each source on child exit. During final destruction, remove all remaining sources and terminate/close the corresponding subprocess resources without a blocking wait in Shell. Merely removing the timeout while leaving an unresponsive child alive would be incomplete. The [extension review rules](https://gjs.guide/extensions/review-guidelines/review-guidelines.html#remove-main-loop-sources) require cleanup even for self-removing sources.

### F5 — Medium — An open menu never follows daemon availability or state

**Location:** `extension/wallslaed@hebbihebb/menu.js:15`, `extension/wallslaed@hebbihebb/menu.js:50`.

**Defect:** Item sensitivity and labels are refreshed only by the `open` injection; `DesktopMenu` does not subscribe to the daemon's `changed` event.

**Failure scenario:** Open a static wallpaper's menu while the daemon is absent → start the daemon while leaving the menu open → Next/Previous/Pause stay disabled until reopening. Conversely, open while the daemon is available → it dies without a background replacement → controls remain enabled and issue failing calls. An external pause/favorite change likewise leaves the open menu's action label incorrect.

**Reproduction evidence:** Running the original `_decorate()` against an existing menu gave `Next sensitive=false` initially; changing the daemon state to available left it false. Calling `_decorate()` again made it true; changing state back to null then left it true. There is no daemon signal connection in this class to perform the missing refresh.

**Proposed fix:** Subscribe to `daemon.changed`, refresh existing decorated menus when state changes, and disconnect that subscription on destroy. Keep lazy decoration for newly created BackgroundMenus. Test daemon appearance, disappearance and external pause changes with one menu held open.

### F6 — Low — The kill fallback uses exit-status getters as liveness probes

**Location:** `extension/wallslaed@hebbihebb/animated.js:142`.

**Defect:** `get_if_exited()` and `get_if_signaled()` require an already-terminated subprocess, so querying a still-running child in the fallback emits GLib criticals.

**Failure scenario:** The player hangs or does not finish quitting within two seconds → the fallback calls both getters while its PID is still live → two critical messages are emitted before `force_exit()` runs. This is precisely the case the fallback is supposed to handle normally.

**Reproduction evidence:** The disposable `sleep` child in F4 produced both `g_subprocess_get_if_exited: assertion 'pid == 0' failed` and `g_subprocess_get_if_signaled: assertion 'pid == 0' failed`, then was killed. An immediate getter probe on another live child reproduced the same API violations.

**Proposed fix:** Track completion from `wait_async`/`wait_finish` and consult that flag, removing the fallback upon completion. Alternatively the Unix `force_exit()` implementation safely handles a process that has already exited, so a liveness getter is unnecessary. See [GLib subprocess status and signal implementation](https://github.com/GNOME/glib/blob/2.88.0/gio/gsubprocess.c#L1014-L1215). This does **not** establish that `force_exit()` can kill a reused PID; GLib explicitly synchronizes signal delivery with reaping.

### F7 — Low — Preferences can rewrite a valid short interval to 30 seconds

**Location:** `extension/wallslaed@hebbihebb/prefs.js:78`, `extension/wallslaed@hebbihebb/prefs.js:81`, `extension/wallslaed@hebbihebb/prefs.js:86`; `data/io.github.hebbihebb.Wallslaed.gschema.xml:26`.

**Defect:** The schema accepts intervals down to five seconds, but the preferences adjustment clamps values below half a minute and its notification writes that clamped value back.

**Failure scenario:** Leave preferences open with an interval of at least 30 seconds → another settings client sets a schema-valid value of 5 seconds → `changed::interval` assigns `5 / 60` minutes to the row → the adjustment clamps it to `0.5` → `notify::value` writes **30 seconds** into GSettings. The user's valid setting is silently replaced. The UI also cannot select schema-valid values from 5 through 29 seconds. Initial window loading alone does not prove a write, because the value handler is connected afterwards.

**Reproduction evidence:** A real `Gtk.Adjustment` with the declared limits, assigned `5 / 60`, returned `0.5`; the exact notification-handler arithmetic yields 30. The original callback connections complete the write-back path.

**Proposed fix:** Align the adjustment with the schema minimum and prevent settings-to-widget synchronization from feeding clamped/rounded values back to settings. A seconds-based control is the clearest way to preserve the full integer-second range; otherwise block the value handler during synchronization and choose suitable precision/increments.

## D-Bus contract check

The XML in `extension/wallslaed@hebbihebb/daemon.js:11–25` and `daemon/src/daemon.rs:16–31` is structurally identical. The actual registration/dispatcher is at daemon lines 130–139 and 194–229. Bus name and interface are `io.github.hebbihebb.Wallslaed`; object path is `/io/github/hebbihebb/Wallslaed`.

| Member | Input | Output / signal payload | Result |
|---|---|---|---|
| `Next`, `Previous` | none | none | Match |
| `SetPaused` | `b` | none | Signature matches; runtime failure F1 |
| `TogglePause` | none | none | Signature matches; runtime failure F1 |
| `ToggleFavorite`, `Ban` | none | none | Match |
| `Rescan`, `ShowInFiles` | none | none | Match |
| `GetState` | none | `a{sv}` | Match |
| `Changed` | — | `a{sv}` | Match |

`state_variant()` at daemon lines 473–485 supplies:

| Keys | Variant value type | Extension interpretation |
|---|---|---|
| `current`, `picture`, `playable` | `s` | Original source path, still-picture path, playable path respectively; empty string when unavailable |
| `animated`, `paused`, `favorite` | `b` | Correct boolean uses; `paused` controls rotation, separately from visibility/battery playback pausing |
| `count`, `remaining` | `u` | Present, not currently used by these menus/host |

`unpack()` correctly recursively unwraps the per-key variants of `a{sv}`. A GJS native-variant check confirmed the expected strings/booleans. The generated `GetStateAsync()` result is destructured as one output, and `Changed` as one dictionary argument, consistently with the interface.

`DO_NOT_AUTO_START` is intentional: the proxy observes a missing daemon without activating it. Name-owner notification triggers `_refresh()` when it subsequently appears or restarts. Preferences independently use `NO_AUTO_START`. There is no spelling/signature mismatch or demonstrated late-start failure. A stale-refresh concern remains in Unverified U1.

## Player contract check

At the reviewed snapshot, `player/src/main.rs:65` accepts the first argument as a literal filename, lines 91–108 create `wallslaed-player-<index>` windows, lines 115–124 accept newline-delimited `play`, `pause`, `quit`, and EOF quits. The extension uses an argv array `[path, file]`, exact matching commands plus newline, and `Meta.WaylandClient.owns_window()` before parsing titles. Spaces or shell metacharacters in file paths are not shell input. No monitor index is passed to the player through argv or stdin.

No extension-originating malformed command or path was demonstrated to explain the historical player SIGABRTs. F3 concerns delivery, not invalid command syntax. Rust EOF/quit behavior matches the expected shutdown contract.

The numerical suffix is a **GDK monitor enumeration index** in the player but is interpreted as a **Mutter logical monitor index** in the extension. That cross-process mapping is not established by the protocol; see U2 rather than treating every hotplug as a confirmed failure.

## Line-by-line comparison: `_swapBackgroundActor`

Sources: [Shell 50.6 background.js, lines 728–745](https://github.com/GNOME/gnome-shell/blob/50.6/js/ui/background.js#L728-L745), cross-checked against the installed resource in `/usr/lib64/gnome-shell/libshell-18.so`; [Shell 50.0 counterpart](https://github.com/GNOME/gnome-shell/blob/50.0/js/ui/background.js#L679-L695) has the same operations. Local override: `extension/wallslaed@hebbihebb/transitions.js:72–86`.

| Upstream 50.6 line | Local line | Comparison |
|---|---|---|
| 728 | 73 | Same entrypoint |
| — | 74–75 | Added desktop-container guard; other managers call original |
| 729 | 77 | Saves outgoing actor |
| 730 | 78 | Installs incoming actor |
| 731 | 79 | Clears pending actor |
| 732 | 80 | Same `changed` emission position |
| 733 | 81 | Whitespace |
| 734 | 82 | Same screen-transition test, plus invisible-actor shortcut |
| 735 | 83 | Destroys outgoing actor |
| 736 | 84–85 | Upstream returns; local `else` prevents animation |
| 737 | 84 | Equivalent branch boundary |
| 738 | — | Whitespace |
| 739 | 85, 96 | Delegates outgoing animation to `_run` |
| 740 | 140 | Default fade keeps opacity target |
| 741 | 98 | Fixed duration becomes configured duration |
| 742 | 141 | Default fade keeps easing mode |
| 743 | 106, 141 | Default fade keeps destroy-on-completion |
| 744 | 143 | Animation-call closure |
| 745 | 86 | Method closure |

No missing swap operation was found. Loading gates and vignette/brightness copying remain in upstream `_updateBackgroundActor()` (50.6 lines 747–778), outside this override. Actor construction and initial loaded notification also remain upstream. Overview/lock-screen managers use other containers and therefore take the original path. Desktop managers are all eligible; each outgoing actor/effect/timeline has its own instance, so multi-monitor animation is not inherently a shared-instance conflict.

Additional transition checks:

- `Transitions.destroy()` destroys tracked outgoing actors; their destroy handlers remove set entries. Custom timelines stop on actor destruction (`transitions.js:152`). No demonstrated timeline outliving its destroyed actor was found.
- Shell's ease callback invokes `onComplete` only for a finished animation, so cancelling by destroying an actor does not itself establish a second destroy through that callback. See [50.6 environment.js, lines 57–100](https://github.com/GNOME/gnome-shell/blob/50.6/js/ui/environment.js#L57-L100).
- The three fixed-name shader classes are registered when the ES module is imported, not on each `enable()`. Shell imports the extension once and retains its state object; ordinary unlock cycles do not re-register these GTypes. See [50.6 extensionSystem.js, lines 482–521](https://github.com/GNOME/gnome-shell/blob/50.6/js/ui/extensionSystem.js#L482-L521).
- Per-GType shader pipeline caching is consistent with [Shell 50.6 shell-glsl-effect.c, lines 104–140](https://github.com/GNOME/gnome-shell/blob/50.6/src/shell-glsl-effect.c#L104-L140). Separate effect instances receive their own uniforms. Shader initialization/zero-size concerns needing rendering evidence are U3.

## Other lifecycle, API and review checks

- Enable constructs settings, menu, transition and animated-host instances; the extension class has no resource-creating constructor. Disable restores injections before destroying owned helpers and dropping references.
- `AnimatedHost.destroy()` removes `_checkId` and disconnects its recorded settings/global/UPower/background/window signals. A pending UPower initialization callback calls `_queueCheck()`, which returns because `_player` is null. The idle is not a confirmed surviving source; the untracked shutdown timers are F4.
- The proxy initialization and awaited `GetStateAsync()` callbacks check `_proxy` before applying state after client destruction. The player wait callback compares the exact player object, not its filename, so an old A process exiting after A→B→A does not stop the new A process. Async write completion uses its supplied stream, not nulled host fields. No callback-after-destroy null dereference was reproduced along these paths.
- `wait_async()` omits `wait_finish()`, but that alone does not establish a zombie/leak: GLib reaps separately and its finish method propagates a boolean result. Calling finish remains the proper completion pattern in a proposed shutdown fix; it is not counted as a separate unproven leak.
- A clone is attached to the manager's newly assigned actor after `changed`; the outgoing clone is destroyed during replacement. The local override emits in the correct order. A minimized/zero-opacity source is not by itself proof that a clone is invisible: [Mutter 50.6 clutter-clone.c, lines 123–183](https://github.com/GNOME/mutter/blob/50.6/clutter/clutter/clutter-clone.c#L123-L183) overrides source paint opacity and permits unmapped-source painting.
- The minimize notification guard checks `window.minimized`; Mutter only notifies on an actual state change. `lower()` changes stack position and does not itself emit `raised`. No immediate recursive loop was established. See [Mutter 50.6 window.c, minimize/unminimize](https://github.com/GNOME/mutter/blob/50.6/src/core/window.c#L2734-L2789) and [lower](https://github.com/GNOME/mutter/blob/50.6/src/core/window.c#L5477-L5485).
- Shell rebuilds desktop managers before emitting its layout-manager `monitors-changed` signal, so `_restart()` reconnects against the new manager array; the array is populated in monitor-index order. That specific hotplug ordering is not a demonstrated bug. See [Shell 50.6 layout.js, background creation](https://github.com/GNOME/gnome-shell/blob/50.6/js/ui/layout.js#L537-L573) and [notification order](https://github.com/GNOME/gnome-shell/blob/50.6/js/ui/layout.js#L627-L637).
- Lazy menu decoration handles newly rebuilt BackgroundMenus. Inserting a section at 0 and a separator at 1 preserves the stock items; no stock-item index collision was found. Existing-menu signal retention and live state are F2/F5.
- Every JS settings key exists with the expected type; enum-backed `transition` is valid through `get_string()`. Favorite weight, transition duration and maximum interval limits agree. The lower interval mismatch is F7. The GTK4 asynchronous folder dialog is suitable for Wayland; a nonlocal file's null path being silently ignored is a limitation to test if remote folders are intended, not a proven local-folder failure.
- Runtime API checks against installed Mutter/Shell 50 typelibs confirmed `Meta.Window.show_in_window_list`, `hide_from_window_list`, `is_maximized`, `minimize`, `lower`, and `Shell.BlurEffect.radius`. No removed API was identified. Shell-only and preferences-only GTK imports are separated correctly.
- Under the [GNOME extension review guidelines](https://gjs.guide/extensions/review-guidelines/review-guidelines.html), F2 violates signal cleanup and F4 violates main-loop cleanup. Module-level class/proxy-wrapper definitions do not create running actors or settings instances. Separately, if submitting to extensions.gnome.org, the shared app schema's `io.github...` ID/path conflicts with the documented `org.gnome.shell.extensions` schema namespace requirement. That is a distribution-review constraint, not an additional runtime defect; resolving it needs a deliberate shared-daemon/settings migration plan.

## Unverified

These are not counted or severity-ranked as confirmed defects.

### U1 — A stale `GetState` reply may overwrite newer state

**Location:** `extension/wallslaed@hebbihebb/daemon.js:62–70`.

`_refresh()` checks only that a proxy still exists, not that its name owner or state generation matches the request. A delayed old-owner response or response dispatched after a newer `Changed` could theoretically restore obsolete `playable` data. **Confirmation needed:** capture this ordering with the actual synchronous daemon handler and GDBus/GJS dispatch on a private bus; arbitrary manually reordered promises would not establish that it occurs. A fix, if confirmed, would bind requests to owner identity and invalidate snapshots on newer signals/refreshes. Destroy itself is guarded.

### U2 — GDK and Mutter monitor indices may name different outputs

**Location:** `extension/wallslaed@hebbihebb/animated.js:165`, `animated.js:172`, `animated.js:209`; snapshot `player/src/main.rs:91–103`.

GDK appends Wayland monitors in output-announcement order, whereas Shell uses logical-monitor numbers. Mutter exposes physical monitor outputs and can retain existing globals across layout changes. See [GTK Wayland monitor enumeration](https://github.com/GNOME/gtk/blob/4.22.0/gdk/wayland/gdkmonitor-wayland.c#L348-L399) and [Mutter 50.6 output updates](https://github.com/GNOME/mutter/blob/50.6/src/wayland/meta-wayland-outputs.c#L570-L615). **Confirmation needed:** compare connector/geometry mappings on an isolated multi-monitor session before/after rearrangement, primary changes, hotplug and mirroring. Different aspect ratios would expose a mismatched source as a stretched/cropped wallpaper. Prefer an explicit connector/geometry identity handshake if confirmed; a restarted player alone does not prove equal enumeration order.

### U3 — Shader initialization order and zero dimensions

**Location:** `extension/wallslaed@hebbihebb/transitions.js:134–135`, `transitions.js:153`.

`_animate()` invokes `setProgress(0)` before `setup()` assigns the uniform location. Also, pixelation divides by the sampled actor size. GJS converts undefined integer arguments to zero in a native integer probe; that fact alone does not establish a shader failure. The outgoing actor normally already has an allocation. **Confirmation needed:** exercise all three shaders in an isolated Shell, inspect GL/compiler diagnostics and uniform locations, and record actor sizes during startup/hotplug. Initialize uniforms before the initial step and defer or skip effects with zero dimensions if a failure is observed. No NVIDIA-specific freeze is inferred from this source pattern.

### U4 — A retiring player's late window can bypass tucking

**Location:** `extension/wallslaed@hebbihebb/animated.js:131`, `animated.js:159–163`.

After `_stop()`, only the current player is considered by `_owns()`. A late map from the still-exiting old client is ignored; disabling also disconnects the global map hook before every child is necessarily gone. **Confirmation needed:** delay player startup/stdin handling in an isolated compositor, switch wallpaper or disable before map, and observe whether an old real window becomes visible/focused before quit or kill. Retain retiring client ownership until exit, or close its Wayland client connection as part of shutdown, if confirmed. The object-identity wait guard does protect the replacement player's lifetime.

### U5 — Coverage checks and visibility edge cases

**Location:** `extension/wallslaed@hebbihebb/animated.js:54–59`, `animated.js:258–268`.

There is no bounds check around `get_monitor()`. An out-of-range positive index would extend the array and could make `.every()` false because of holes; `-1` merely adds a non-index property. Neither establishes a shell crash. **Confirmation needed:** record such an index on a relevant fullscreen/maximized window during actual monitor reconfiguration and show a persistent wrong pause result. A cheap guard is `0 <= index && index < n`.

Also test moving maximized windows between equally sized monitors without a restack/resize notification, and leaving the overview visible with an otherwise uncovered workspace. The host does not explicitly subscribe to overview visibility or per-window monitor moves. Establish a missed playback update or sustained unnecessary decoding before treating these as separate bugs; a still overview background alone is already consistent with the separate-manager design.

## Out of scope and exclusions

- F1 is the only Rust implementation defect reported: it directly breaks a menu-invoked D-Bus contract. No broader Rust/media/GStreamer audit was performed, including the concurrent new media features.
- The supplied deleted-source/cached-image/history behavior is excluded as requested. No extension-specific aggravation was demonstrated.
- Historical player SIGABRTs are not attributed to this extension without a trace or reproducible input path.

## Working-tree accounting

The repository was clean at the first `git status --short`. During the audit, another agent modified the player, daemon and schema and added media/MPRIS files; the user explicitly instructed this audit to preserve and work around those changes. Consequently, a global status containing only `?? AUDIT.md` cannot truthfully be promised during that concurrent work. This audit authored only `AUDIT.md`; it did not revert, edit, stage or commit any other file. The report's snapshot remains the original commit regardless of later working-tree changes.
