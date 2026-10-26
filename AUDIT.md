# Shell blur and top-bar audit

Audit date: 2026-10-26. Revision: `a62229a`. Branch: `codex/audit-shell-blur`.

Scope: `git log 480b01e..a62229a` (seven commits), including the committed lock-screen implementation. The original request named `7005255`; the user expanded it during the audit. The daemon preparation code, `blur.js`, `liveBlur.js`, and `topbar.js` are identical at those two revisions. No implementation files were changed. Claude's checkout and its uncommitted changes were left alone.

There are **two high**, **nine medium**, and **three low** findings below. No critical finding was established. This is an audit for review, not approval to implement the proposed fixes. Findings describe the audited revision, not changes Claude might make afterward.

## Findings, ranked by severity

### SB-01 — High — An asynchronous screenshot open can resurrect blur after teardown

**Location:** `extension/wallslaed@hebbihebb/liveBlur.js:43`, `:85`, `:125`; `extension/wallslaed@hebbihebb/extension.js:59`.

**Scenario:** Screenshot UI opening awaits `screenshot_stage_to_content()` in GNOME 50. Disable the extension, or tear down the desktop on lock, during that await. Clearing injections does not cancel an already registered `result.then(run)`. Its callback subsequently calls `_attach()` on the destroyed `LiveBlur` instance. That instance has no destroyed flag, and `_attach()` creates actors and signal handlers after `_clear()` has finished. The screenshot blur's root is an internal child, so hiding the outer screenshot UI does not automatically remove this orphan. Its close-method override has already been removed. Persistent Shell actors/signals keep the old instance reachable; repeated races can accumulate instances and blur resources.

**Status: confirmed.** In the isolated Shell at `a62229a`, run, in one Eval:

```js
const ex = Main.extensionManager.lookup('wallslaed@hebbihebb').stateObj;
globalThis.oldLive = ex._liveBlur;
Main.screenshotUI.open();
ex.disable();
```

After two seconds, `oldLive._blurs.size` was **1**. After `Main.screenshotUI.close(true)` it was still **1**, with the UI invisible. Normal teardown should leave zero. Test: `/tmp/wallslaed-audit-evidence/runtime.sh`; log: `runtime-a62229a.log`. The test explicitly cleared the orphan afterward.

**Proposed fix:** Mark `LiveBlur` destroyed before teardown; check that flag in every deferred completion and `_attach()`. Also use an open-generation or visibility token so an old open cannot attach after a later close/reopen. Track screenshot UI close/visibility independently of injection restoration.

### SB-02 — High — Low blur strengths can exceed the daemon's memory limit

**Location:** `daemon/src/prepare.rs:246`, `:249`, `:254`, `:266`; service limit: `data/wallslaed.service:16`.

**Scenario:** With prescaling disabled, prepare a large supported image at strength 1–4. The reduction factor clamps to **1**, so the supposedly small blur is full source resolution. The resized RGB image plus the two full-size temporary buffers in `image::imageops::fast_blur()` coexist. A 9000×9000 source therefore exceeds the installed service's `MemoryMax=600M`; an installed daemon can be OOM-killed instead of changing wallpaper. A very wide image that does not need shrinking to cover the screen can also enter this path with prescaling enabled. Strength 0 skips the blur but still allocates and writes a full-resolution copy. `JOB_LOCK` prevents parallel decodes, not these simultaneous buffers within one decode.

**Status: confirmed memory overrun; service OOM consequence inferred.** A generated 9000×9000 PNG passed to the unchanged `prepare::blur_only()` at strength 1 peaked at **718,768 KiB RSS (701.9 MiB)**. It was run outside the service, so no real daemon was OOM-killed. Command: `/usr/bin/time -v /tmp/wallslaed-audit-evidence/memory-probe-bin 1`; source: `memory-probe.rs`; log: `memory-1.log`. The input was generated with `ffmpeg -f lavfi`. The debug build took 64.94 seconds; that duration is not a release-performance claim.

**Proposed fix:** Bound decoded/output dimensions and total pixel count independently of blur strength. Scale to the intended monitor size before computing sigma and output dimensions, and handle strength 0 without a second full-size bitmap. For images beyond a safe decode budget, use a decoder reduction where available or decline baking and use the fallback.

### SB-03 — Medium — Cache hits change the blur's cache key

**Location:** `daemon/src/prepare.rs:109`, `:179`, `:231`, `:324`.

**Scenario:** Revisit a prescaled wallpaper or a cached video poster. Preparation calls `touch()` on its cached JPEG, updating its mtime. `bake_blur()` hashes that same mtime as image identity. Identical pixels and identical settings consequently produce a different PNG on each preparation, repeating decoding, resizing, blurring, PNG encoding, Shell image loading and fades. Obsolete variants consume the shared 40-file budget and evict useful wallpapers/posters sooner.

**Status: confirmed.** A standalone probe importing the unchanged `library.rs` and `prepare.rs` prepared the same prescaled image twice: `same picture=true`, `same blur=false`. After 47 preparations of that one unchanged image, the cache held **40 files, 39 of them blur PNGs**. Test: `cache-probe.rs` / `cache-probe-bin`; log: `cache-probe.log` under the evidence directory.

**Proposed fix:** Separate content identity from eviction recency. Derive the blur key from the immutable source/preparation key, with settings, rather than the touched generated file's mtime. Keep access timestamps in separate metadata or another field.

### SB-04 — Medium — Rebaking can evict the picture it still needs

**Location:** `daemon/src/prepare.rs:213`, `:216`, `:330`; `daemon/src/daemon.rs:578`.

**Scenario:** Leave a prescaled picture on screen and adjust strength through roughly 40 distinct values. `blur_only()` refreshes/writes the blur PNG but does not refresh or pin its input JPEG. Trimming therefore deletes the still-current JPEG while `Daemon.current.picture` continues to name it. The next rebake fails metadata lookup and clears the blur; a later monitor rebuild or background reload can also encounter a missing picture. Current media, the saved rotation picture during music playback, and preloaded dependencies share the same unpinned budget.

**Status: confirmed.** `eviction-probe-bin` prepared one small prescaled image, then called `blur_only()` at strengths 31–73. At strength **69**, `picture_exists=false` while the just-created blur still existed. At strength **70** onward baking returned `None`. Log: `eviction.log`; probe imports the unmodified preparation module.

**Proposed fix:** Pin files referenced by current/media/preloaded state, and trim preparation bundles with their dependent blur/poster files. At minimum, refresh the input on rebake, but explicit dependency protection is needed to cover a long-running music video and background work too.

### SB-05 — Medium — A music video prepared during a settings change keeps stale blur

**Location:** `daemon/src/daemon.rs:573`, `:593`, `:721`; compare the guard at `:558`.

**Scenario:** Start a music video whose poster is not cached, then change blur settings while its preparation is running. At that moment `media_now` is `None`, so `rebake()` has no media picture to update. The original media job completes with the old settings and `play_media()` publishes it without an equivalent of `apply()`'s `baked_with` check. The stale blur remains until another setting change or media replacement.

**Status: confirmed.** An isolated daemon test generated a one-second local video and delayed only its poster-producing ffmpeg subprocess by three seconds. Changing strength from 30 to **71** during that delay left `GetState.blurred` naming **`-blur-30-60.png`** after playback preparation completed, while GSettings reported 71. Script/log: `daemon-race.sh` / `daemon-race.log`. No player, network download or real daemon was used.

**Proposed fix:** Validate `Prepared.baked_with` when accepting a media preparation, just as for the rotation. Schedule a current-generation rebake when it is stale, preserving the media/source check.

### SB-06 — Medium — Rebakes are neither coalesced nor checked against the latest settings

**Location:** `daemon/src/daemon.rs:375`, `:575`, `:589`, `:593`, `:599`.

**Scenario:** Drag the strength/brightness control while a large picture is being baked. Every change schedules another worker that retains the daemon and waits on `JOB_LOCK`; superseded work still decodes/encodes and calls `preload()` when finished. Completions compare only picture paths, not a request generation or `s.blur()`. An older completion can therefore replace newer blur state. For example, disabling both baked surfaces schedules a `blur=None` completion that takes no decode lock; an older running bake can finish afterward and restore its obsolete `blurred`/`baked_with`. With both current and media pictures, the two separately locked bakes also permit interleaving. A normal preload's `baked_with` guard does not protect these assignments.

**Status: suspected from source; completion inversion and sustained queue growth were not reproduced under load.** The existing stale-preload guard was inspected and does correctly reject mismatched finished preloads. It does not cancel the work or validate rebake results.

**Proposed fix:** Debounce/coalesce changes into one desired request; use a monotonic rebake generation and compare both source identity and desired settings before publishing. Skip superseded requests before expensive work and avoid duplicate in-flight preloads.

### SB-07 — Medium — Fullscreen reveal moves an invisible panel

**Location:** `extension/wallslaed@hebbihebb/topbar.js:148`, `:246`, `:299`.

**Scenario:** A window on the primary monitor is fullscreen. GNOME's chrome tracking hides `panelBox` because `trackFullscreen` remains true. Edge pressure calls `_reveal()`, which resets translation and disables unredirect but never changes that visibility decision. The bar is considered revealed while remaining invisible; the user cannot reach its controls by the advertised edge action.

**Status: confirmed.** In a headless two-monitor session, a generated GTK window was fullscreened, then the same `_reveal()` path used by the pressure trigger was invoked. Result: `visible=false`, `hidden=false`, `revealed=true`, `unredirect=true`. Test/log: `topbar-extra.sh` / `topbar-extra.log`. Physical pointer pressure on NVIDIA was not tested.

**Proposed fix:** Explicitly coordinate reveal visibility with fullscreen chrome tracking, and restore normal tracking afterward. Keep unredirect disabled for the full duration the visible panel/menu needs compositing; do not rely on translation alone.

### SB-08 — Medium — A pointer on another monitor keeps the bar revealed

**Location:** `extension/wallslaed@hebbihebb/topbar.js:312`.

**Scenario:** Reveal the bar, then move horizontally to another monitor near the top, or into a monitor above the primary. Polling checks only `y < primary.y + panelHeight`. It checks neither x nor the lower y bound. The pointer can be nowhere near the panel while `_revealed`, the polling source and the unredirect-disable state persist. The bar continues to cover windows on the primary.

**Status: confirmed.** With a 1920×1080 primary and a portrait monitor starting at x=1920, the recorded pointer at **[1920,10]** was outside the primary's half-open rectangle, but after multiple poll intervals the state remained `revealed=true`, `poll=true`, `hidden=false`. Test/log: `topbar-extra.sh` / `topbar-extra.log`; also reproduced in `runtime-a62229a.log`.

**Proposed fix:** Test the complete panel rectangle in stage coordinates, including both x bounds and both y bounds. Reset reveal/poll state when monitors change or the edge-reveal feature is disabled.

### SB-09 — Medium — Geometry changes of other windows in the focused app are missed

**Location:** `extension/wallslaed@hebbihebb/topbar.js:153`, `:219`.

**Scenario:** An application has two windows. A remains focused below the panel; B moves under the panel without being focused or restacked (an application can reposition its other window). `_overlapped()` considers B, but geometry notifications are connected only to A. No check runs, so the visible bar covers B until an unrelated watched event. Moving B away can likewise leave the bar hidden.

**Status: confirmed.** Two GTK windows from the same app were created. With A focused, B was moved from y=400 to y=5. After settling: `focus=audit-A`, `hidden=false`, `_overlapped()=true`. Calling `_check()` immediately corrected `hidden` to true. Test/log: `topbar-extra.sh` / `topbar-extra.log`.

**Proposed fix:** Watch position/size changes for all eligible windows of the focused app on the active workspace, updating those subscriptions as windows/apps/workspaces change. Preserve the existing once-per-frame debounce.

### SB-10 — Medium — Changing blur settings removes blur from an already open popup

**Location:** `extension/wallslaed@hebbihebb/liveBlur.js:95`, `:168`; `extension/wallslaed@hebbihebb/stylesheet.css:47`.

**Scenario:** Open Quick Settings and adjust brightness. The handler clears every popup blur and its CSS class, without recreating blurs for surfaces still open. Even changing the overview, panel or lock-screen switch clears unrelated open popups. Reopening is required to restore them. Brightness itself is not applied to popup blur: effects always use brightness 1 and the tint's alpha is constant, so reopening still does not make popups follow that setting.

**Status: confirmed.** With Quick Settings visibly open, setting brightness to 0.2 changed the blur map from one entry to **zero**, while `menu.isOpen` remained true (`runtime-a62229a.log`). The fixed effect brightness and CSS tint were checked in source.

**Proposed fix:** Reconcile existing open surfaces by their own category rather than clearing on every `blur-*` change. Update their radius/tint/brightness in place, with consistent overlap handling, and attach/detach immediately when their category toggle changes.

### SB-11 — Medium — Three largely overlapping background blurs multiply popup rendering work

**Location:** `extension/wallslaed@hebbihebb/liveBlur.js:30`, `:159`, `:244`.

**Scenario:** Open a large Quick Settings/calendar/folder/screenshot surface over changing window content. Each surface allocates three independent `BACKGROUND` effects covering mostly the same pixels. These are independent offscreen/blur operations, not three clips of one cached result. Later parts are also painted after earlier parts, so overlap needs validation for compounded blur and partial-redraw feedback. Large surfaces, scaling and multiple open popups increase the bandwidth cost on the compositor thread/GPU.

**Status: suspected performance/rendering risk, with confirmed redundant coverage.** The runtime probe recorded Quick Settings rectangles of **330×256**, **402×184**, and **380×234**: 247,368 processed rectangle pixels for a 402×256 bounding rectangle (about **2.40×** coverage before padding/passes). Three separate effects were present. No NVIDIA stall or ghosting was reproduced; this is not evidence that such a crash exists. The sibling placement does correctly avoid BoxPointer's own `OffscreenRedirect.ALWAYS` buffer.

**Proposed fix:** Blur once into a shared result and apply a rounded mask or nonoverlapping clips, ensuring the sample source excludes that result. Benchmark moving/high-contrast content with partial redraws at the real scales on NVIDIA before accepting the current three-effect design.

### SB-12 — Low — GPU fallback is cached per surface, not once per monitor

**Location:** `extension/wallslaed@hebbihebb/blur.js:149`, `:173`, `:242`.

**Scenario:** The daemon is absent or cannot bake a format. The primary monitor's overview and panel each construct their own actor-mode `Shell.BlurEffect`. Sharing `Meta.BackgroundContent` shares wallpaper input, not the effects' offscreen output. With both surfaces mapped, this duplicates full-monitor blur buffers/work, contrary to the intended one-GPU-blur-per-monitor fallback. Usually the hidden overview limits simultaneous painting, so this is lower severity than popup rendering.

**Status: suspected performance impact; duplicate effect construction confirmed by source.** The daemon-absent fallback was exercised headless without errors, but GPU buffer sharing or NVIDIA memory usage was not measured.

**Proposed fix:** Put the fallback effect on one per-monitor source actor and let the surfaces display clones of that rendered result. Verify caching and actor destruction across monitor changes.

### SB-13 — Low — Distinct supported brightness settings alias the same cached image

**Location:** `daemon/src/prepare.rs:237`; `data/io.github.hebbihebb.Wallslaed.gschema.xml:102`.

**Scenario:** Store brightness 0.601, then 0.604 through the supported double setting. Both keys round to `60`, although multiplication uses the unrounded double and `baked_with` compares it exactly. The second request reuses pixels baked for the first value. The preferences' coarse steps make this less likely through its normal UI.

**Status: confirmed.** `cache-probe-bin` returned `brightness 0.601/0.604 aliases=true` for calls to the unchanged `blur_only()` implementation. Log: `cache-probe.log`.

**Proposed fix:** Either use the exact normalized floating-point representation in the key, or quantize once and use that same value for pixels, equality checks and the key.

### SB-14 — Low — Rename failure leaves a permanently excluded `.part` file

**Location:** `daemon/src/prepare.rs:281`, `:339`.

**Scenario:** PNG encoding succeeds but the final rename fails, for example because cache-directory permissions or its mount change between those operations. The early `.ok()?` returns without removing the temporary file. `trim_cache()` excludes every name containing `.part`, so repeated failures across different settings/images can accumulate abandoned files outside the 40-file limit. Encoding failures do clean up correctly.

**Status: suspected from the error path; rename failure was not injected.** No user/cache mount or permissions were changed for this audit.

**Proposed fix:** Clean up the temporary file on rename failure as well, preferably with an ownership guard. Reap old abandoned partial files separately from active transcodes.

## Checks that passed, and paths found sound

- **Required checks at `a62229a`:** `cargo build --workspace` passed without compiler warnings; `cargo test --workspace` passed all 19 daemon tests (player has zero tests); `glib-compile-schemas --strict --dry-run data` passed. Every extension `.js` file passed `node --input-type=module --check` through stdin. `git diff --check` was run on this report before committing.
- **Normal churn, independently repeated on `a62229a`:** the supplied `mem-test.sh` ran 200 wallpaper changes, 50 menu open/close cycles and 30 extension disable/enable cycles. Stage actor count stayed **2120** at all four measurements. RSS was **464028 → 467524 → 465152 → 471164 KiB** (net +6.97 MiB), measured after two forced GCs at each stage. No Wallslæd JS error was logged. This finite test does not prove absence of native-resource retention, and forced GC does not measure natural-GC peak memory. Claude's earlier three-way comparison was read as context, not substituted for this repeat.
- **Normal popup lifecycle:** a rapid close/reopen before the deferred update left one current blur; closing removed it. Destroying the target before its root left zero entries. Deferred redraw IDs are removed by `Blur.destroy()`, and signals/actors are explicitly torn down in the ordinary path. A strong `Map` here is not by itself an unbounded leak; SB-01 bypasses its ordinary lifetime.
- **Static surfaces:** forced rebuild and disable during a mapped panel fade produced no Wallslæd errors. Ordinary teardown removed the named actors and restored panel translation to zero. This tests the same rebuild path as monitor changes; it is not a physical hotplug test. Pending image-load signals are disconnected on replacement/teardown. Fade callbacks remove older children rather than growing an unlimited actor list.
- **Lock screen:** corrected isolated test `lock-test.sh`, final log `lock-final.log`, created a real GNOME unlock dialog and simulated ten mode cycles. Desktop components were absent in `unlock-dialog` and present again in `user`; destroying each dialog left `_dialogs.size=0`. Strength 71/brightness 0.2 produced radius 142/brightness approximately 0.2 at scale 1. Turning lock matching off restored GNOME's radius 90/brightness approximately 0.65. Disabling while locked emptied the tracker and restored stock blur. No Wallslæd JS errors or disposed-object warnings occurred in the final run. Initial test scripts incorrectly destroyed modal dialogs without releasing grabs, and one had a variable typo; those runs are not attributed to Wallslæd.
- **Top-bar ownership:** ordinary disable restored one tracked panel entry with `affectsStruts=true`; translation, polling and the owned unredirect-disable flag have explicit teardown paths. Source review found matching barrier removal/destruction, and matching compositor enable/disable calls guarded by `_unredirectDisabled`. GNOME 50's `PressureBarrier.destroy()` disconnects barriers; its trigger code has no timer that Wallslæd forgot to cancel. These checks do not resolve SB-07/SB-08.
- **Compositor safety:** no new blocking file read, synchronous subprocess wait or network operation was found in the scoped extension modules. Popup allocation notifications queue one `BEFORE_REDRAW` update rather than directly mutating child geometry during their allocation callback. Panel placement only updates position/clip, and the exercised runs did not report allocation loops. `BoxPointer.ALWAYS` is present in the installed GNOME source and is addressed by sibling placement.
- **Corner inset:** `1 - Math.SQRT1_2` is approximately 0.292893. Its diagonal inset lies on the circular corner boundary; the three rectangles fit inside equal-radius rounded corners. It is a stepped approximation, not an exact circular mask. That geometry does not remove the repeated-blur cost in SB-11; asymmetric theme corner radii and scaled screenshots were not verified.
- **Daemon bookkeeping:** regular `apply()` detects stale baked settings, finished preloads require both matching generation and `baked_with`, and state export switches the `blurred` entry to media alongside `picture`. Rebake's new result mutations drop the mutable borrow before calling `emit_changed()`/`preload()`. Preparation and rebake share one decode lock. Brightness is clamped internally and the schema restricts it to 0.2–1; strength accepts 0–100. The normal bake test validates size, darkening, softened edges and reuse for an unchanged original source.
- **Borrow-rule boundary:** no new own-settings setter under a daemon borrow was introduced by these commits. Existing `apply()`/media/resume paths still call `set_background()` under a borrow, and existing `emit_changed()` borrows while emitting the D-Bus signal. Those are literal exceptions to the broad AGENTS.md wording, present before `480b01e`. No synchronous callback back into `Daemon` was found on its separate background-settings object or that D-Bus emission, and no RefCell panic was reproduced; they are not reported as a newly confirmed F1 regression.
- **Schema/install integration:** new keys are additive, defaults and preferences bindings match, and Appearance is registered in preferences. Both install paths include `*.css`; isolated installation at the audited revision loaded the stylesheet. No new dependency was introduced. The `a{sv}` state field addition requires no D-Bus XML signature change.

## Evidence and reproduction environment

All audit-created scripts, probes, extracted GNOME source and logs are in `/tmp/wallslaed-audit-evidence/`. They are temporary local evidence, not additional committed files. The separate worktree is `/tmp/wallslaed-audit-7005255`, whose tip was advanced to `a62229a` after the scope change. Only this report is committed.

Shell/daemon tests were run exclusively through the repository harness, for example:

```sh
tools/isolated-session.sh --install /tmp/wallslaed-audit-evidence/runtime-a62229a \
  env MONITOR='1920x1080 --virtual-monitor 1080x1920' \
  bash /tmp/wallslaed-audit-evidence/runtime.sh
tools/isolated-session.sh --install /tmp/wallslaed-audit-evidence/daemon-race \
  bash /tmp/wallslaed-audit-evidence/daemon-race.sh
tools/isolated-session.sh --install /tmp/wallslaed-audit-evidence/lock-final \
  bash /tmp/wallslaed-audit-evidence/lock-test.sh
```

The supplied `lib.sh`, helper, GTK test-window script and synthetic `media2/` images were reused read-only. A private helper copy added unlock-dialog support for the lock test. Debug binaries built from the audit worktree were installed into scratch homes; a local `target/release -> debug` symlink supplied the paths expected by `--install`. No release-performance conclusion is drawn from those runs. Standalone Rust probes used the compiled dependency artifacts and imported the unchanged source modules with `#[path]`.

The initial top-bar/daemon reproductions used `7005255`; the relevant modules are byte-identical at `a62229a`. The async leak and open-popup settings issue were additionally rerun on `a62229a`. Installed GNOME 50 JS was extracted from `/usr/lib64/gnome-shell/libshell-18.so`, including `layout`, `boxpointer`, `screenshot`, `popupMenu`, `background`, `unlockDialog`, `screenShield` and `sessionMode`.

No settings-leak WARNING occurred. The live desktop, its extensions/services/processes, wallpaper/media folders, saved state and NFS mounts were not modified. No remote API or download was used. Tests stopped only their own nested Shell/daemon processes.

## Limits and follow-up for review

Not established headless: NVIDIA driver stability, GPU bandwidth/buffer residency, partial-redraw ghosting, real pointer pressure, physical monitor hotplug, fractional/HiDPI scaling, a non-origin primary monitor, real authentication/lock/unlock, or visual correctness against all GNOME 50 theme states. The two-monitor test included a portrait secondary, but did not establish those other geometry cases. The preference page was checked structurally, not exercised visually. App-folder, switcher, notification and OSD hookups were source-reviewed, not independently exercised in this audit. The full original popup screenshot suite was not rerun. Always-on-top/foreign-dialog/fullscreen stacking beyond the reproduced fullscreen case, hidden player windows, and daemon restart with a surface already open were not exhaustively tested.

Replacing contents drops JavaScript references; native `Meta.Background`, content and image release was not instrumented, so the finite actor/RSS measurements cannot prove prompt native disposal or bound natural-GC peaks. No monotonic native leak was established in ordinary churn. SB-01 is a separate reproduced lifetime violation.

State: audit **implemented**, required checks and the listed reproductions **verified**; report **committed** on the audit branch only. No fix is implemented, accepted, merged or installed. Review each finding for acceptance, rejection or deferral before fixes. After accepted fixes and explicit live-install approval, the most useful manual check is edge reveal over fullscreen on both monitors, followed by real lock/unlock and popup use on NVIDIA.
