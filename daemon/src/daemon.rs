//! The long-running service: owns the timer, the shuffle bag, and the D-Bus API.
//! Everything runs on one GLib main loop; image work goes to a worker thread.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gio::prelude::*;
use glib::{ControlFlow, SourceId, Variant, VariantDict};

use crate::library::{self, Kind};
use crate::media::{self, MediaState};
use crate::prepare::{self, Blur, Job, Prepared};
use crate::shuffle::{self, Bag};
use crate::store::{self, Store};
use crate::{BUS_NAME, IFACE, OBJECT_PATH, SCHEMA};

const INTROSPECTION: &str = r#"
<node>
  <interface name="io.github.hebbihebb.Wallslaed">
    <method name="Next"/>
    <method name="Previous"/>
    <method name="SetPaused"><arg type="b" name="paused" direction="in"/></method>
    <method name="TogglePause"/>
    <method name="ToggleFavorite"/>
    <method name="Ban"/>
    <method name="Rescan"/>
    <method name="ShowInFiles"/>
    <method name="GetState"><arg type="a{sv}" name="state" direction="out"/></method>
    <method name="PlayMedia"><arg type="s" name="path" direction="in"/></method>
    <method name="StopMedia"/>
    <method name="ReportPosition">
      <arg type="s" name="path" direction="in"/>
      <arg type="d" name="seconds" direction="in"/>
    </method>
    <method name="ListMedia"><arg type="aa{sv}" name="items" direction="out"/></method>
    <method name="Download">
      <arg type="s" name="url" direction="in"/>
      <arg type="s" name="id" direction="out"/>
    </method>
    <method name="CancelDownload"><arg type="s" name="id" direction="in"/></method>
    <method name="ListDownloads"><arg type="a(ssds)" name="downloads" direction="out"/></method>
    <signal name="Changed"><arg type="a{sv}" name="state"/></signal>
    <signal name="DownloadProgress">
      <arg type="s" name="id"/>
      <arg type="s" name="title"/>
      <arg type="d" name="fraction"/>
      <arg type="s" name="status"/>
    </signal>
  </interface>
</node>
"#;

/// Consecutive files that may fail to load before we stop trying.
const MAX_FAILURES: u32 = 8;
const RESCAN_DELAY_SECS: u32 = 3;
/// How often to look for a wallpaper folder that couldn't be read.
const SCAN_RETRY_SECS: u32 = 60;
/// Blur settings changes closer together than this bake once.
const REBAKE_DELAY_MS: u64 = 150;
const FALLBACK_SCREEN: (u32, u32) = (3840, 2160);
/// Download progress is announced in steps of this fraction.
const PROGRESS_STEP: f64 = 0.01;
/// A cancelled download that hasn't stopped after this is killed.
const CANCEL_GRACE_SECS: u32 = 10;
/// Downloader output is read in pieces this big; longer lines are cut.
const READ_CHUNK: usize = 8192;
const LINE_MAX: usize = 64 * 1024;
/// How long the downloader's output may stay open after it exited (a child
/// process holding it).
const OUTPUT_GRACE: Duration = Duration::from_secs(2);
/// How long shutting down waits for state to reach the disk.
const EXIT_FLUSH: Duration = Duration::from_secs(3);

#[derive(Clone, Copy)]
enum Step {
    Next,
    Previous,
    Reload,
}

struct Daemon {
    /// State is loaded and saved only once we own the bus name: a second
    /// daemon that loses the race must not write anything.
    ready: bool,
    store: Store,
    settings: gio::Settings,
    background: gio::Settings,
    library: Vec<String>,
    bag: Bag,
    rng: fastrand::Rng,
    current: Option<Prepared>,
    /// The next and previous wallpapers, prepared ahead of time.
    preloaded: Vec<Prepared>,
    /// Sources being preloaded right now, so none is started twice.
    preloading: Vec<String>,
    /// Bumped on every user-visible change so stale worker results are dropped.
    generation: u64,
    /// Bumped on every rebake, so only the newest one's result is used.
    blur_generation: u64,
    /// A rebake waiting for the blur settings to stop changing.
    rebake_timer: Option<SourceId>,
    failures: u32,
    timer: Option<SourceId>,
    rescan_timer: Option<SourceId>,
    /// A scan is running; another one asked for meanwhile runs after it.
    scanning: bool,
    rescan_again: bool,
    monitors: Vec<gio::FileMonitor>,
    locked: bool,
    pending_tick: bool,
    screen: (u32, u32),
    conn: Option<gio::DBusConnection>,
    state_path: PathBuf,
    cache_dir: PathBuf,
    /// Music video mode: while `media.active` is set the rotation is on hold.
    media: MediaState,
    media_path: PathBuf,
    /// The active music video once its poster is ready, and its thumbnail.
    media_now: Option<Prepared>,
    media_art: Option<PathBuf>,
    downloads: Downloads,
}

#[derive(Default)]
struct Downloads {
    next_id: u64,
    queue: VecDeque<(String, String)>,
    running: Option<Running>,
}

struct Running {
    id: String,
    title: String,
    fraction: f64,
    status: String,
    /// None while its folders are being set up.
    proc: Option<gio::Subprocess>,
    /// Its own folder for unfinished files, removed unless it succeeds.
    temp: PathBuf,
    cancelled: bool,
}

type Shared = Rc<RefCell<Daemon>>;

pub fn run() -> Result<(), String> {
    if gio::SettingsSchemaSource::default()
        .and_then(|s| s.lookup(SCHEMA, true))
        .is_none()
    {
        return Err(format!("GSettings schema {SCHEMA} is not installed (run install.sh)"));
    }

    let state_dir = glib::user_state_dir().join("wallslaed");
    let cache_dir = glib::user_cache_dir().join("wallslaed");

    let d: Shared = Rc::new(RefCell::new(Daemon {
        ready: false,
        store: Store::new(),
        settings: gio::Settings::new(SCHEMA),
        background: gio::Settings::new("org.gnome.desktop.background"),
        library: Vec::new(),
        bag: Bag::default(),
        rng: fastrand::Rng::new(),
        current: None,
        preloaded: Vec::new(),
        preloading: Vec::new(),
        generation: 0,
        blur_generation: 0,
        rebake_timer: None,
        failures: 0,
        timer: None,
        rescan_timer: None,
        scanning: false,
        rescan_again: false,
        monitors: Vec::new(),
        locked: false,
        pending_tick: false,
        screen: FALLBACK_SCREEN,
        conn: None,
        state_path: state_dir.join("state.json"),
        cache_dir,
        media: MediaState::default(),
        media_path: state_dir.join("media.json"),
        media_now: None,
        media_art: None,
        downloads: Downloads::default(),
    }));

    let main_loop = glib::MainLoop::new(None, false);
    quit_on_signals(&main_loop);
    {
        let (d1, d2) = (d.clone(), d.clone());
        let ml = main_loop.clone();
        let _owner = gio::bus_own_name(
            gio::BusType::Session,
            BUS_NAME,
            gio::BusNameOwnerFlags::NONE,
            move |conn, _| on_bus(&d1, conn),
            move |_, _| on_name(&d2),
            move |_, _| {
                eprintln!("wallslaed: lost bus name {BUS_NAME} (already running?)");
                ml.quit();
            },
        );
        main_loop.run();
    }
    // State is saved on every change; give the last one a moment to land.
    let s = d.borrow();
    if s.ready && !s.store.flush(EXIT_FLUSH) {
        eprintln!("wallslaed: state may not be saved: {}", s.store.error().unwrap_or_else(|| "writing took too long".into()));
    }
    Ok(())
}

/// Write end of the pipe the signal handler pokes.
static SIGNAL_PIPE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

extern "C" fn on_quit_signal(_: libc::c_int) {
    let fd = SIGNAL_PIPE.load(std::sync::atomic::Ordering::Relaxed);
    // write() is safe in a signal handler; nothing else here would be.
    unsafe { libc::write(fd, [1u8].as_ptr().cast(), 1) };
}

/// End the main loop on SIGTERM (systemd stopping us) or SIGINT, so the last
/// state is written. A handler, not a blocked mask: children (yt-dlp, the image
/// loaders) would inherit the mask and stop hearing these signals themselves.
fn quit_on_signals(main_loop: &glib::MainLoop) {
    let mut fds = [0; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return;
    }
    SIGNAL_PIPE.store(fds[1], std::sync::atomic::Ordering::Relaxed);
    let handler = on_quit_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGINT, handler);
    }
    let ml = main_loop.clone();
    std::thread::spawn(move || {
        let mut b = 0u8;
        while unsafe { libc::read(fds[0], (&mut b as *mut u8).cast(), 1) } != 1 {}
        ml.quit();
    });
}

/// The bus is ours to talk on, but the name may still go to another daemon:
/// set up the object, touch no state.
fn on_bus(d: &Shared, conn: gio::DBusConnection) {
    let info = gio::DBusNodeInfo::for_xml(INTROSPECTION).expect("valid introspection XML");
    let iface = info.lookup_interface(IFACE).expect("interface in XML");
    let d1 = d.clone();
    conn.register_object(OBJECT_PATH, &iface)
        .method_call(move |_, _, _, _, method, params, invocation| {
            handle_method(&d1, method, &params, invocation)
        })
        .build()
        .expect("register D-Bus object");

    // Pause while the screen is locked; change once on unlock if a tick was missed.
    let d1 = d.clone();
    let _sub = conn
        .subscribe_to_signal(
            None,
            Some("org.gnome.ScreenSaver"),
            Some("ActiveChanged"),
            Some("/org/gnome/ScreenSaver"),
            None,
            gio::DBusSignalFlags::NONE,
            move |sig| {
                let locked = sig.parameters.child_value(0).get::<bool>().unwrap_or(false);
                let tick = {
                    let mut s = d1.borrow_mut();
                    s.locked = locked;
                    !locked && std::mem::take(&mut s.pending_tick)
                };
                if tick {
                    advance(&d1, Step::Next);
                }
            },
        );
    // Lives as long as the process.
    std::mem::forget(_sub);

    let d1 = d.clone();
    let _sub = conn
        .subscribe_to_signal(
            None,
            Some("org.gnome.Mutter.DisplayConfig"),
            Some("MonitorsChanged"),
            Some("/org/gnome/Mutter/DisplayConfig"),
            None,
            gio::DBusSignalFlags::NONE,
            move |sig| {
                let screen = query_screen(sig.connection);
                let mut s = d1.borrow_mut();
                if s.screen != screen {
                    s.screen = screen;
                    s.preloaded.clear();
                }
            },
        );
    std::mem::forget(_sub);

    d.borrow_mut().conn = Some(conn);
}

/// We own the name: this is the one daemon. Load the state and start.
fn on_name(d: &Shared) {
    {
        let mut s = d.borrow_mut();
        let s = &mut *s;
        if let Some(dir) = s.state_path.parent() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                eprintln!("wallslaed: can't create {}: {e}", dir.display());
            }
        }
        s.bag = store::load(&s.state_path);
        if s.bag.version > shuffle::VERSION {
            eprintln!("wallslaed: state.json is from a newer version; keeping what this one doesn't know");
        }
        s.media = store::load(&s.media_path);
        s.store.back_up(&s.state_path);
        s.store.back_up(&s.media_path);
        s.ready = true;
        if let Some(conn) = &s.conn {
            s.screen = query_screen(conn);
        }
    }
    connect_settings(d);
    clean_downloads(d);
    rescan(d);
    // Pick a music video back up after a restart or a new login.
    let active = d.borrow().media.active.clone();
    let Some(path) = active else { return };
    let folder = media::folder(&d.borrow().settings);
    let d = d.clone();
    glib::spawn_future_local(async move {
        let real = gio::spawn_blocking(move || media::music_video(&folder, Path::new(&path)))
            .await
            .ok()
            .flatten();
        if d.borrow().media.active.is_none() {
            return; // stopped meanwhile
        }
        match real {
            Some(path) => start_media(&d, path),
            // Gone, or on a folder that isn't there right now: play the rotation
            // this time, but leave the choice saved for when it's back.
            None => {
                {
                    let mut s = d.borrow_mut();
                    s.media.active = None;
                    s.media_now = None;
                }
                resume_rotation(&d);
            }
        }
    });
}

fn handle_method(d: &Shared, method: &str, params: &Variant, inv: gio::DBusMethodInvocation) {
    if !d.borrow().ready {
        inv.return_error(gio::DBusError::Failed, "starting up");
        return;
    }
    match method {
        // Changing the wallpaper ends a music video.
        "Next" | "Previous" => {
            let was_media = end_media(d);
            let moved = advance(d, if method == "Next" { Step::Next } else { Step::Previous });
            // Nothing to go back to: show the rotation's wallpaper again.
            if was_media && !moved {
                resume_rotation(d);
            }
        }
        // Setting a key runs our changed handler right away, and that handler
        // needs a mutable borrow, so don't hold one across the set.
        "SetPaused" => {
            let p = params.child_value(0).get::<bool>().unwrap_or(false);
            let settings = d.borrow().settings.clone();
            let _ = settings.set_boolean("paused", p);
        }
        "TogglePause" => {
            let settings = d.borrow().settings.clone();
            let _ = settings.set_boolean("paused", !settings.boolean("paused"));
        }
        "ToggleFavorite" => {
            d.borrow_mut().bag.toggle_favorite();
            save_state(&d.borrow());
            emit_changed(d);
        }
        "Ban" => {
            let banned = d.borrow_mut().bag.ban_current();
            if let Some(p) = banned {
                eprintln!("wallslaed: never showing {p} again");
                // Saved now, whether or not something else can be shown.
                save_state(&d.borrow());
            }
            advance(d, Step::Next);
        }
        "Rescan" => rescan(d),
        "ShowInFiles" => show_in_files(d),
        "GetState" => {
            let state = state_variant(&d.borrow());
            inv.return_value(Some(&Variant::tuple_from_iter([state])));
            return;
        }
        // Checking a path reads the disk, which may hang (a network folder):
        // done on a worker, answered when it's back.
        "PlayMedia" => {
            let path = params.child_value(0).get::<String>().unwrap_or_default();
            let folder = media::folder(&d.borrow().settings);
            let d = d.clone();
            glib::spawn_future_local(async move {
                let real = gio::spawn_blocking(move || media::music_video(&folder, Path::new(&path)))
                    .await
                    .ok()
                    .flatten();
                match real {
                    Some(path) => {
                        start_media(&d, path);
                        inv.return_value(None);
                    }
                    None => inv.return_error(gio::DBusError::InvalidArgs, "not a video in the music folder"),
                }
            });
            return;
        }
        "StopMedia" => stop_media(d),
        "ReportPosition" => {
            let (path, secs) = params.get::<(String, f64)>().unwrap_or_default();
            if !secs.is_finite() {
                inv.return_error(gio::DBusError::InvalidArgs, "not a position");
                return;
            }
            let secs = secs.clamp(0.0, media::POSITION_MAX);
            // The playing video, or one already saved, is known to be good.
            let known = {
                let s = d.borrow();
                s.media.active.as_deref() == Some(path.as_str()) || s.media.positions.contains_key(&path)
            };
            if known {
                save_position(d, path, secs);
            } else {
                let folder = media::folder(&d.borrow().settings);
                let d = d.clone();
                glib::spawn_future_local(async move {
                    let real = gio::spawn_blocking(move || media::music_video(&folder, Path::new(&path)))
                        .await
                        .ok()
                        .flatten();
                    if let Some(path) = real {
                        save_position(&d, path, secs);
                    }
                });
            }
        }
        "ListMedia" => {
            let folder = media::folder(&d.borrow().settings);
            glib::spawn_future_local(async move {
                let list = gio::spawn_blocking(move || media::list(&folder)).await.unwrap_or_default();
                let items: Vec<Variant> = list
                    .into_iter()
                    .map(|item| {
                        let dict = VariantDict::new(None);
                        let thumb = item.thumbnail.as_deref().and_then(Path::to_str).unwrap_or("");
                        dict.insert("path", item.path.as_str());
                        dict.insert("title", item.title.as_str());
                        dict.insert("thumbnail", thumb);
                        dict.end()
                    })
                    .collect();
                let array = Variant::array_from_iter_with_type(glib::VariantTy::VARDICT, items);
                inv.return_value(Some(&Variant::tuple_from_iter([array])));
            });
            return;
        }
        "Download" => {
            let url = params.child_value(0).get::<String>().unwrap_or_default();
            if !media::valid_url(&url) {
                inv.return_error(gio::DBusError::InvalidArgs, "not an http(s) link");
                return;
            }
            let id = queue_download(d, url);
            inv.return_value(Some(&(id,).to_variant()));
            return;
        }
        "CancelDownload" => {
            let id = params.child_value(0).get::<String>().unwrap_or_default();
            cancel_download(d, &id);
        }
        "ListDownloads" => {
            let s = d.borrow();
            let mut list: Vec<(String, String, f64, String)> = Vec::new();
            if let Some(r) = &s.downloads.running {
                list.push((r.id.clone(), r.title.clone(), r.fraction, r.status.clone()));
            }
            for (id, url) in &s.downloads.queue {
                list.push((id.clone(), url.clone(), 0.0, "queued".into()));
            }
            inv.return_value(Some(&(list,).to_variant()));
            return;
        }
        _ => {
            inv.return_error(gio::DBusError::UnknownMethod, &format!("no method {method}"));
            return;
        }
    }
    inv.return_value(None);
}

fn connect_settings(d: &Shared) {
    let settings = d.borrow().settings.clone();
    let d1 = d.clone();
    settings.connect_changed(None, move |_, key| match key {
        "folder" | "recursive" | "animated-enabled" | "music-folder" => rescan(&d1),
        "interval" => restart_timer(&d1),
        "paused" => {
            restart_timer(&d1);
            emit_changed(&d1);
        }
        "prescale" => {
            d1.borrow_mut().preloaded.clear();
            advance(&d1, Step::Reload);
        }
        "blur-strength" | "blur-brightness" | "blur-overview" | "blur-panel" => queue_rebake(&d1),
        _ => {}
    });
}

fn folder(settings: &gio::Settings) -> PathBuf {
    let f = settings.string("folder");
    if f.is_empty() {
        glib::user_special_dir(glib::UserDirectory::Pictures).unwrap_or_else(glib::home_dir)
    } else if let Some(rest) = f.strip_prefix("~/") {
        glib::home_dir().join(rest)
    } else {
        PathBuf::from(f.as_str())
    }
}

/// What a scan reads and filters by: a scan for other settings is out of date.
#[derive(PartialEq)]
struct ScanParams {
    root: PathBuf,
    recursive: bool,
    animated: bool,
    music: PathBuf,
}

fn scan_params(settings: &gio::Settings) -> ScanParams {
    ScanParams {
        root: folder(settings),
        recursive: settings.boolean("recursive"),
        animated: settings.boolean("animated-enabled"),
        music: media::folder(settings),
    }
}

/// Rescan the folder on a worker thread, then reconcile the bag and watchers.
/// One scan at a time: a folder that hangs ties up one thread, not one per try.
fn rescan(d: &Shared) {
    let params = {
        let mut s = d.borrow_mut();
        if s.scanning {
            s.rescan_again = true;
            return;
        }
        s.scanning = true;
        s.rescan_again = false;
        scan_params(&s.settings)
    };
    let d = d.clone();
    glib::spawn_future_local(async move {
        let (root, recursive) = (params.root.clone(), params.recursive);
        let scan = gio::spawn_blocking(move || library::scan(&root, recursive)).await;
        let again = {
            let mut s = d.borrow_mut();
            s.scanning = false;
            // Settings changed while scanning: this result is for the old ones.
            std::mem::take(&mut s.rescan_again) || scan_params(&s.settings) != params
        };
        if again {
            rescan(&d);
            return;
        }
        let Ok(mut scan) = scan else { return };
        if !scan.found {
            // Unplugged, unmounted, renamed: not emptied. Keep the library and
            // everything saved about it, and look again later.
            eprintln!(
                "wallslaed: can't read {}; keeping the last list, trying again in {SCAN_RETRY_SECS} s",
                params.root.display()
            );
            schedule_rescan(&d, SCAN_RETRY_SECS);
            return;
        }
        if !scan.complete {
            eprintln!("wallslaed: some of {} couldn't be read", params.root.display());
        }
        if !params.animated {
            scan.files
                .retain(|p| library::kind_of(Path::new(p)) == Some(Kind::Static));
        }
        // Music videos play with sound; never pick them at random. If the music
        // folder holds the whole wallpaper folder, that would leave nothing, so
        // the exclusion only applies to a music folder inside it.
        if !params.root.starts_with(&params.music) {
            scan.files.retain(|p| !Path::new(p).starts_with(&params.music));
        }
        let (had_current, shown) = {
            let mut s = d.borrow_mut();
            let s = &mut *s;
            s.bag.sync(&scan.files, scan.complete, &mut s.rng);
            s.library = scan.files;
            s.monitors = watch(&d, &scan.dirs);
            s.preloaded.clear();
            eprintln!(
                "wallslaed: {} wallpapers, {} left in this cycle",
                s.library.len(),
                s.bag.remaining()
            );
            save_state(s);
            (s.bag.current().is_some(), s.current.is_some())
        };
        if had_current && !shown {
            // Starting up: show where we left off.
            advance(&d, Step::Reload);
        } else if !had_current {
            advance(&d, Step::Next);
        } else {
            emit_changed(&d);
            preload(&d);
        }
    });
}

/// Rescan after `secs`, replacing any rescan already waiting.
fn schedule_rescan(d: &Shared, secs: u32) {
    let mut s = d.borrow_mut();
    if let Some(id) = s.rescan_timer.take() {
        id.remove();
    }
    let d2 = d.clone();
    s.rescan_timer = Some(glib::timeout_add_seconds_local(secs, move || {
        d2.borrow_mut().rescan_timer = None;
        rescan(&d2);
        ControlFlow::Break
    }));
}

fn watch(d: &Shared, dirs: &[PathBuf]) -> Vec<gio::FileMonitor> {
    dirs.iter()
        .filter_map(|dir| {
            let m = gio::File::for_path(dir)
                .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
                .ok()?;
            let d = Rc::downgrade(d);
            m.connect_changed(move |_, _, _, event| {
                use gio::FileMonitorEvent as E;
                if !matches!(
                    event,
                    E::Created | E::Deleted | E::MovedIn | E::MovedOut | E::Renamed
                ) {
                    return;
                }
                let Some(d) = d.upgrade() else { return };
                // Copying a folder of wallpapers fires many events; rescan once.
                schedule_rescan(&d, RESCAN_DELAY_SECS);
            });
            Some(m)
        })
        .collect()
}

fn eligible(library: &[String]) -> impl Fn(&str) -> bool + '_ {
    move |p: &str| library.binary_search_by(|x| x.as_str().cmp(p)).is_ok()
}

/// Returns whether a wallpaper was picked.
fn advance(d: &Shared, step: Step) -> bool {
    let pick = {
        let mut s = d.borrow_mut();
        let s = &mut *s;
        let weight = s.settings.uint("favorite-weight");
        let ok = eligible(&s.library);
        let pick = match step {
            Step::Next => s.bag.next(&s.library, &ok, weight, &mut s.rng),
            Step::Previous => s.bag.previous(&ok),
            Step::Reload => s.bag.current().map(str::to_owned),
        };
        // Where the rotation is now, saved whether or not the picture loads.
        if pick.is_some() && !matches!(step, Step::Reload) {
            save_state(s);
        }
        pick
    };
    match pick {
        Some(path) => {
            show(d, path);
            true
        }
        None if matches!(step, Step::Previous) => false,
        None => {
            eprintln!("wallslaed: no wallpapers found in {}", folder(&d.borrow().settings).display());
            false
        }
    }
}

fn show(d: &Shared, path: String) {
    let (generation, job, ready) = {
        let mut s = d.borrow_mut();
        s.generation += 1;
        let ready = s.preloaded.iter().position(|p| p.source == path).map(|i| s.preloaded.swap_remove(i));
        // The cache can lose files (cleared, full disk): prepare it again.
        let ready = ready.filter(|p| [&p.picture, &p.playable, &p.blurred].into_iter().flatten().all(|f| f.exists()));
        (s.generation, s.job(path), ready)
    };
    if let Some(p) = ready {
        apply(d, p);
        return;
    }
    let d = d.clone();
    glib::spawn_future_local(async move {
        let result = gio::spawn_blocking(move || prepare::prepare(&job)).await;
        if d.borrow().generation != generation {
            return;
        }
        match result {
            Ok(Ok(p)) => apply(&d, p),
            Ok(Err(e)) => {
                let failures = {
                    let mut s = d.borrow_mut();
                    s.failures += 1;
                    eprintln!("wallslaed: skipping {}: {e}", s.bag.current().unwrap_or("?"));
                    s.failures
                };
                if failures < MAX_FAILURES {
                    advance(&d, Step::Next);
                }
            }
            Err(_) => eprintln!("wallslaed: worker thread panicked"),
        }
    });
}

fn apply(d: &Shared, p: Prepared) {
    {
        let mut s = d.borrow_mut();
        s.failures = 0;
        // A music video keeps the screen; the rotation catches up when it ends.
        if s.media.active.is_none() {
            set_background(&s, p.picture.as_deref());
        }
        // Usually what Previous shows: keep it ready.
        if let Some(old) = s.current.replace(p) {
            if old.baked_with == s.blur() {
                s.preloaded.push(old);
            }
        }
    }
    emit_changed(d);
    restart_timer(d);
    let stale = {
        let s = d.borrow();
        s.current.as_ref().is_some_and(|c| c.baked_with != s.blur())
    };
    if stale {
        rebake(d);
    } else {
        preload(d);
    }
}

/// Rebake once the blur settings stop changing (a slider being dragged).
fn queue_rebake(d: &Shared) {
    if let Some(id) = d.borrow_mut().rebake_timer.take() {
        id.remove();
    }
    let d1 = d.clone();
    let id = glib::timeout_add_local_once(Duration::from_millis(REBAKE_DELAY_MS), move || {
        d1.borrow_mut().rebake_timer = None;
        rebake(&d1);
    });
    d.borrow_mut().rebake_timer = Some(id);
}

/// Bake the blur again for what's on screen, after its settings changed.
fn rebake(d: &Shared) {
    let (generation, blur, cache_dir, keep, cache_bytes, current, media) = {
        let mut s = d.borrow_mut();
        s.preloaded.clear();
        s.blur_generation += 1;
        let pic = |p: &Option<Prepared>| p.as_ref().and_then(|p| Some((p.picture.clone()?, p.key.clone())));
        (s.blur_generation, s.blur(), s.cache_dir.clone(), s.in_use(), s.cache_bytes(), pic(&s.current), pic(&s.media_now))
    };
    let d = d.clone();
    glib::spawn_future_local(async move {
        let bake = move |pic: &Option<(PathBuf, String)>| {
            let (picture, key) = pic.as_ref()?;
            prepare::blur_only(picture, key, &cache_dir, blur?, &keep, cache_bytes)
        };
        let (cur, med) = (current.clone(), media.clone());
        let Ok((current_blur, media_blur)) =
            gio::spawn_blocking(move || (bake(&cur), bake(&med))).await
        else {
            return;
        };
        {
            let mut s = d.borrow_mut();
            // A newer rebake, or newer settings, will bring their own.
            if s.blur_generation != generation || s.blur() != blur {
                return;
            }
            // Only if the same pictures are still up; a newer one brings its own blur.
            let same = |p: &Prepared, pic: &Option<(PathBuf, String)>| {
                p.picture.as_ref() == pic.as_ref().map(|(picture, _)| picture)
            };
            if let Some(c) = s.current.as_mut().filter(|c| same(c, &current)) {
                c.blurred = current_blur;
                c.baked_with = blur;
            }
            if let Some(m) = s.media_now.as_mut().filter(|m| same(m, &media)) {
                m.blurred = media_blur;
                m.baked_with = blur;
            }
        }
        emit_changed(&d);
        preload(&d);
    });
}

/// Prepare the next and previous wallpapers now so switching later is instant.
fn preload(d: &Shared) {
    let (generation, jobs) = {
        let mut s = d.borrow_mut();
        let s = &mut *s;
        let wanted: Vec<String> = {
            let ok = eligible(&s.library);
            [s.bag.peek(&ok), s.bag.peek_previous(&ok)].into_iter().flatten().map(str::to_owned).collect()
        };
        s.preloaded.retain(|p| wanted.contains(&p.source));
        let missing: Vec<String> = wanted
            .into_iter()
            .filter(|w| !s.preloaded.iter().any(|p| &p.source == w) && !s.preloading.contains(w))
            .collect();
        s.preloading.extend(missing.iter().cloned());
        (s.generation, missing.into_iter().map(|w| s.job(w)).collect::<Vec<_>>())
    };
    for job in jobs {
        let d = d.clone();
        glib::spawn_future_local(async move {
            let source = job.source.clone();
            let result = gio::spawn_blocking(move || prepare::prepare(&job)).await;
            let ready = {
                let mut s = d.borrow_mut();
                s.preloading.retain(|p| *p != source);
                match result {
                    // Dropped if the blur settings changed while it was being prepared.
                    Ok(Ok(p)) if s.generation == generation && p.baked_with == s.blur() => {
                        s.preloaded.retain(|q| q.source != p.source);
                        s.preloaded.push(p);
                        true
                    }
                    _ => false,
                }
            };
            // The extension uploads `next-picture` and `previous-picture` ahead of the switch.
            if ready {
                emit_changed(&d);
            }
        });
    }
}

fn set_background(s: &Daemon, picture: Option<&Path>) {
    let Some(pic) = picture else { return };
    let uri = gio::File::for_path(pic).uri();
    // Set both so dark mode doesn't keep showing the old one, in one write.
    // GNOME Shell (GJS 1.90 and earlier) can deadlock when a settings change
    // signal arrives while its garbage collector frees the background a
    // change replaced (GJS MR !1134, not merged yet). In a headless shell
    // without Wallslæd, changes every 0.35 s hung it within 20-100 changes
    // in 4 of 4 runs with two writes, and 0 of 3 runs with one. Fewer, not
    // none: with the extension's own garbage, it still hung once.
    s.background.delay();
    let _ = s.background.set_string("picture-uri", &uri);
    let _ = s.background.set_string("picture-uri-dark", &uri);
    s.background.apply();
}

fn restart_timer(d: &Shared) {
    let mut s = d.borrow_mut();
    if let Some(id) = s.timer.take() {
        id.remove();
    }
    if s.settings.boolean("paused") || s.media.active.is_some() {
        return;
    }
    let secs = s.settings.uint("interval").max(5);
    let d1 = d.clone();
    // Second-granularity timers are batched by GLib, which keeps wakeups rare.
    s.timer = Some(glib::timeout_add_seconds_local(secs, move || {
        let locked = {
            let mut s = d1.borrow_mut();
            s.timer = None;
            if s.locked {
                s.pending_tick = true;
            }
            s.locked
        };
        if !locked {
            advance(&d1, Step::Next);
        }
        ControlFlow::Break
    }));
}

fn emit_changed(d: &Shared) {
    let s = d.borrow();
    if let Some(conn) = &s.conn {
        let args = Variant::tuple_from_iter([state_variant(&s)]);
        let _ = conn.emit_signal(None, OBJECT_PATH, IFACE, "Changed", Some(&args));
    }
}

fn state_variant(s: &Daemon) -> Variant {
    let dict = VariantDict::new(None);
    let cur = s.current.as_ref();
    let path_str = |p: &Option<PathBuf>| p.as_ref().and_then(|p| p.to_str()).unwrap_or("").to_owned();
    dict.insert("current", cur.map(|c| c.source.as_str()).unwrap_or(""));
    dict.insert("picture", cur.map(|c| path_str(&c.picture)).unwrap_or_default());
    dict.insert("playable", cur.map(|c| path_str(&c.playable)).unwrap_or_default());
    dict.insert("animated", cur.is_some_and(|c| c.playable.is_some()));
    dict.insert("blurred", cur.map(|c| path_str(&c.blurred)).unwrap_or_default());
    dict.insert("paused", s.settings.boolean("paused"));
    dict.insert("favorite", s.bag.is_favorite());
    dict.insert("count", s.library.len() as u32);
    dict.insert("remaining", s.bag.remaining() as u32);

    let active = s.media.active.as_deref().unwrap_or("");
    let path = Path::new(active);
    dict.insert("media", active);
    dict.insert("media-title", if active.is_empty() { String::new() } else { media::title_of(path) });
    dict.insert("media-position", s.media.positions.get(active).copied().unwrap_or(0.0));
    dict.insert("media-art", s.media_art.as_deref().and_then(Path::to_str).unwrap_or(""));
    // Favorites and the like can't be saved right now; they're kept and retried.
    dict.insert("save-error", s.store.error().unwrap_or_default());
    // Once ready, the music video takes the place of the wallpaper.
    if let Some(m) = &s.media_now {
        dict.insert("picture", path_str(&m.picture));
        dict.insert("playable", path_str(&m.playable));
        dict.insert("animated", true);
        dict.insert("blurred", path_str(&m.blurred));
    }
    dict.insert("audio", s.media_now.is_some());
    // What Next and Previous would show, once prepared; nothing while music plays.
    let ok = eligible(&s.library);
    let ahead = |source: Option<&str>| {
        s.preloaded
            .iter()
            .find(|p| s.media.active.is_none() && Some(p.source.as_str()) == source)
            .map(|p| path_str(&p.picture))
            .unwrap_or_default()
    };
    dict.insert("next-picture", ahead(s.bag.peek(&ok)));
    dict.insert("previous-picture", ahead(s.bag.peek_previous(&ok)));
    dict.end()
}

/// Put a music video on the desktop and hold the rotation.
fn start_media(d: &Shared, path: String) {
    let job = {
        let mut s = d.borrow_mut();
        s.media.active = Some(path.clone());
        save_media(&s);
        s.media_now = None;
        s.media_art = None;
        s.job(path.clone())
    };
    restart_timer(d);
    emit_changed(d);
    let d = d.clone();
    glib::spawn_future_local(async move {
        let result = gio::spawn_blocking(move || {
            let art = media::thumbnail_of(Path::new(&job.source));
            prepare::prepare(&job).map(|p| (p, art))
        })
        .await;
        let mut stale = false;
        let ok = {
            let mut s = d.borrow_mut();
            // Another video was picked, or music stopped, in the meantime.
            if s.media.active.as_deref() != Some(path.as_str()) {
                return;
            }
            match result {
                Ok(Ok((p, art))) => {
                    set_background(&s, p.picture.as_deref());
                    // Baked before a settings change: bake again below.
                    stale = p.baked_with != s.blur();
                    s.media_now = Some(p);
                    s.media_art = art;
                    true
                }
                Ok(Err(e)) => {
                    eprintln!("wallslaed: can't play music video: {e}");
                    false
                }
                Err(_) => false,
            }
        };
        if ok {
            emit_changed(&d);
            if stale {
                rebake(&d);
            }
        } else {
            stop_media(&d);
        }
    });
}

/// Leave music video mode without touching the background. Returns whether it was on.
fn end_media(d: &Shared) -> bool {
    let mut s = d.borrow_mut();
    let was_on = s.media.active.take().is_some();
    s.media_now = None;
    s.media_art = None;
    if was_on {
        save_media(&s);
    }
    was_on
}

/// Leave music video mode and go back to the rotation's wallpaper.
fn stop_media(d: &Shared) {
    if end_media(d) {
        resume_rotation(d);
    }
}

/// Show the rotation's current wallpaper again and restart its timer.
fn resume_rotation(d: &Shared) {
    {
        let s = d.borrow();
        set_background(&s, s.current.as_ref().and_then(|c| c.picture.as_deref()));
    }
    restart_timer(d);
    emit_changed(d);
    preload(d);
}

fn queue_download(d: &Shared, url: String) -> String {
    let id = {
        let mut s = d.borrow_mut();
        s.downloads.next_id += 1;
        let id = s.downloads.next_id.to_string();
        s.downloads.queue.push_back((id.clone(), url.trim().to_owned()));
        id
    };
    emit_progress(d, &id, &url, 0.0, "queued");
    pump_downloads(d);
    id
}

fn cancel_download(d: &Shared, id: &str) {
    let queued = {
        let mut s = d.borrow_mut();
        if let Some(r) = s.downloads.running.as_mut().filter(|r| r.id == id) {
            r.cancelled = true;
            // Still setting up: it won't start. Otherwise ask yt-dlp to stop
            // (it cleans up after itself on SIGINT), and make sure it does.
            if let Some(proc) = r.proc.clone() {
                proc.send_signal(libc::SIGINT);
                glib::timeout_add_seconds_local_once(CANCEL_GRACE_SECS, move || {
                    if proc.identifier().is_some() {
                        eprintln!("wallslaed: download didn't stop when asked; killing it");
                        proc.force_exit();
                    }
                });
            }
            return;
        }
        let q = &mut s.downloads.queue;
        let before = q.len();
        q.retain(|(qid, _)| qid != id);
        before != q.len()
    };
    if queued {
        emit_progress(d, id, "", 0.0, "cancelled");
    }
}

/// Unfinished downloads from an earlier run (a crash, a restart): nothing
/// will finish them.
fn clean_downloads(d: &Shared) {
    let temp = d.borrow().cache_dir.join("downloads");
    // Folders are named "<pid>-<id>"; ours may already be in use.
    let ours = format!("{}-", std::process::id());
    gio::spawn_blocking(move || {
        if let Ok(entries) = std::fs::read_dir(&temp) {
            for e in entries.flatten() {
                if e.file_name().to_string_lossy().starts_with(&ours) {
                    continue;
                }
                let path = e.path();
                let _ = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
            }
        }
    });
}

/// Start the next queued download unless one is already running.
fn pump_downloads(d: &Shared) {
    let (id, url, folder, temp, height) = {
        let mut s = d.borrow_mut();
        if s.downloads.running.is_some() {
            return;
        }
        let Some((id, url)) = s.downloads.queue.pop_front() else { return };
        let folder = media::folder(&s.settings);
        let temp = s.cache_dir.join("downloads").join(format!("{}-{id}", std::process::id()));
        s.downloads.running = Some(Running {
            id: id.clone(),
            title: url.clone(),
            fraction: 0.0,
            status: "downloading".into(),
            proc: None,
            temp: temp.clone(),
            cancelled: false,
        });
        (id, url, folder, temp, s.settings.uint("music-max-height"))
    };
    emit_progress(d, &id, &url, 0.0, "downloading");

    let d = d.clone();
    glib::spawn_future_local(async move {
        // Folders and programs are looked up on disk: not on the main loop.
        let (u, t) = (url.clone(), temp.clone());
        let argv = gio::spawn_blocking(move || {
            std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
            std::fs::create_dir_all(&t).map_err(|e| format!("{}: {e}", t.display()))?;
            media::command(&u, &folder, &t, height).ok_or_else(|| "yt-dlp is not installed".to_owned())
        })
        .await
        .unwrap_or_else(|_| Err("worker thread panicked".into()));
        let cancelled = d.borrow().downloads.running.as_ref().is_none_or(|r| r.cancelled);
        let proc = argv.and_then(|argv| {
            if cancelled {
                return Err(String::new());
            }
            let argv: Vec<&std::ffi::OsStr> = argv.iter().map(|a| a.as_os_str()).collect();
            let flags = gio::SubprocessFlags::STDOUT_PIPE
                | gio::SubprocessFlags::STDERR_MERGE
                | gio::SubprocessFlags::STDIN_INHERIT;
            gio::Subprocess::newv(&argv, flags).map_err(|e| e.to_string())
        });
        match proc {
            Ok(proc) => {
                if let Some(r) = d.borrow_mut().downloads.running.as_mut() {
                    r.proc = Some(proc.clone());
                }
                follow_download(&d, proc).await;
            }
            Err(e) => {
                if !cancelled {
                    eprintln!("wallslaed: download failed: {e}");
                }
                finish_download(&d, if cancelled { "cancelled".into() } else { format!("error:{e}") });
            }
        }
    });
}

/// What the downloader's output said, by the time it ended.
#[derive(Default)]
struct Outcome {
    file: Option<String>,
    error: Option<String>,
    /// The output reached its end.
    read: bool,
}

/// Read the downloader's progress until it exits.
async fn follow_download(d: &Shared, proc: gio::Subprocess) {
    let outcome: Rc<RefCell<Outcome>> = Rc::default();
    let reader = proc.stdout_pipe().map(|stdout| {
        let (d, outcome) = (d.clone(), outcome.clone());
        glib::spawn_future_local(async move {
            read_lines(&stdout, |line| {
                let Some(event) = media::parse_line(line) else { return };
                let update = {
                    let mut s = d.borrow_mut();
                    let Some(r) = s.downloads.running.as_mut() else { return };
                    match event {
                        media::Event::Title(t) => {
                            r.title = t;
                            true
                        }
                        media::Event::Progress(done, Some(total)) if total > 0 => {
                            let f = (done as f64 / total as f64).clamp(0.0, 1.0);
                            let step = (f - r.fraction).abs() >= PROGRESS_STEP || r.status != "downloading";
                            if step {
                                r.fraction = f;
                                r.status = "downloading".into();
                            }
                            step
                        }
                        media::Event::Progress(..) => false,
                        media::Event::Processing => {
                            let first = r.status != "processing";
                            r.status = "processing".into();
                            r.fraction = 1.0;
                            first
                        }
                        media::Event::File(f) => {
                            outcome.borrow_mut().file = Some(f);
                            false
                        }
                        media::Event::Error(e) => {
                            outcome.borrow_mut().error = Some(e);
                            false
                        }
                    }
                    .then(|| (r.id.clone(), r.title.clone(), r.fraction, r.status.clone()))
                };
                if let Some((id, title, fraction, status)) = update {
                    emit_progress(&d, &id, &title, fraction, &status);
                }
            })
            .await;
            outcome.borrow_mut().read = true;
        })
    });
    let ok = proc.wait_check_future().await.is_ok();
    // The last lines (the file name) may still be on their way, but a child
    // that kept the output open must not hold the queue forever.
    if let Some(reader) = reader {
        let mut waited = Duration::ZERO;
        while !outcome.borrow().read && waited < OUTPUT_GRACE {
            glib::timeout_future(Duration::from_millis(50)).await;
            waited += Duration::from_millis(50);
        }
        reader.abort();
    }
    let Outcome { file, error, .. } = std::mem::take(&mut *outcome.borrow_mut());
    let cancelled = d.borrow().downloads.running.as_ref().is_some_and(|r| r.cancelled);
    let status = if cancelled {
        "cancelled".to_owned()
    } else if ok && file.is_some() {
        "done".to_owned()
    } else {
        format!("error:{}", error.unwrap_or_else(|| "yt-dlp failed".into()))
    };
    finish_download(d, status);
}

/// Calls `f` with each line of `stream`, as text. Lines longer than LINE_MAX
/// are cut there, so a runaway line can't grow without bound.
async fn read_lines(stream: &gio::InputStream, mut f: impl FnMut(&str)) {
    let mut line: Vec<u8> = Vec::new();
    let mut skipping = false;
    loop {
        let chunk = match stream.read_bytes_future(READ_CHUNK, glib::Priority::DEFAULT).await {
            Ok(b) if !b.is_empty() => b,
            _ => break,
        };
        for part in chunk.split_inclusive(|&b| b == b'\n') {
            let end = part.last() == Some(&b'\n');
            if !skipping {
                let room = LINE_MAX - line.len();
                line.extend_from_slice(&part[..part.len().min(room)]);
                if line.len() >= LINE_MAX && !end {
                    f(&String::from_utf8_lossy(&line));
                    line.clear();
                    skipping = true;
                }
            }
            if end {
                if !skipping {
                    f(&String::from_utf8_lossy(&line));
                }
                line.clear();
                skipping = false;
            }
        }
    }
    if !line.is_empty() && !skipping {
        f(&String::from_utf8_lossy(&line));
    }
}

/// Report how a download ended, clean up after it, start the next one.
fn finish_download(d: &Shared, status: String) {
    let Some(r) = d.borrow_mut().downloads.running.take() else { return };
    // Its own folder: leftovers if it failed, empty if it worked.
    let temp = r.temp.clone();
    gio::spawn_blocking(move || {
        let _ = std::fs::remove_dir_all(&temp);
    });
    eprintln!("wallslaed: download {}: {status}", r.title);
    emit_progress(d, &r.id, &r.title, if status == "done" { 1.0 } else { r.fraction }, &status);
    pump_downloads(d);
}

fn emit_progress(d: &Shared, id: &str, title: &str, fraction: f64, status: &str) {
    let s = d.borrow();
    if let Some(conn) = &s.conn {
        let args = (id, title, fraction, status).to_variant();
        let _ = conn.emit_signal(None, OBJECT_PATH, IFACE, "DownloadProgress", Some(&args));
    }
}

fn show_in_files(d: &Shared) {
    let s = d.borrow();
    let source = s.media.active.as_deref().or(s.current.as_ref().map(|c| c.source.as_str()));
    let (Some(conn), Some(source)) = (&s.conn, source) else { return };
    let uri = gio::File::for_path(source).uri();
    let args = (vec![uri.to_string()], String::new()).to_variant();
    conn.call(
        Some("org.freedesktop.FileManager1"),
        "/org/freedesktop/FileManager1",
        "org.freedesktop.FileManager1",
        "ShowItems",
        Some(&args),
        None,
        gio::DBusCallFlags::NONE,
        -1,
        gio::Cancellable::NONE,
        |r| {
            if let Err(e) = r {
                eprintln!("wallslaed: ShowItems failed: {e}");
            }
        },
    );
}

/// Largest current monitor mode in physical pixels, so pre-scaled images stay sharp.
fn query_screen(conn: &gio::DBusConnection) -> (u32, u32) {
    let reply = conn.call_sync(
        Some("org.gnome.Mutter.DisplayConfig"),
        "/org/gnome/Mutter/DisplayConfig",
        "org.gnome.Mutter.DisplayConfig",
        "GetCurrentState",
        None,
        None,
        gio::DBusCallFlags::NONE,
        2000,
        gio::Cancellable::NONE,
    );
    let Ok(reply) = reply else { return FALLBACK_SCREEN };
    let mut best = (0u32, 0u32);
    let monitors = reply.child_value(1);
    for m in monitors.iter() {
        for mode in m.child_value(1).iter() {
            let current = glib::VariantDict::new(Some(&mode.child_value(6)))
                .lookup_value("is-current", None)
                .and_then(|v| v.get::<bool>())
                .unwrap_or(false);
            if current {
                let w = mode.child_value(1).get::<i32>().unwrap_or(0).max(0) as u32;
                let h = mode.child_value(2).get::<i32>().unwrap_or(0).max(0) as u32;
                if w * h > best.0 * best.1 {
                    best = (w, h);
                }
            }
        }
    }
    if best.0 == 0 {
        FALLBACK_SCREEN
    } else {
        best
    }
}

fn save_state(s: &Daemon) {
    if !s.ready {
        return;
    }
    let mut bag = serde_json::to_value(&s.bag).unwrap_or_default();
    // Never mark a newer version's file as ours.
    if s.bag.version < shuffle::VERSION {
        bag["version"] = shuffle::VERSION.into();
    }
    if let Ok(json) = serde_json::to_vec(&bag) {
        s.store.save(&s.state_path, json);
    }
}

fn save_media(s: &Daemon) {
    if !s.ready {
        return;
    }
    if let Ok(json) = serde_json::to_vec(&s.media) {
        s.store.save(&s.media_path, json);
    }
}

fn save_position(d: &Shared, path: String, secs: f64) {
    let mut s = d.borrow_mut();
    s.media.positions.insert(path, secs);
    save_media(&s);
}

impl Daemon {
    fn job(&self, source: String) -> Job {
        Job {
            source,
            screen: self.screen,
            prescale: self.settings.boolean("prescale"),
            cache_dir: self.cache_dir.clone(),
            blur: self.blur(),
            keep: self.in_use(),
            cache_bytes: self.cache_bytes(),
        }
    }

    /// The `cache-size` setting (MB), in bytes.
    fn cache_bytes(&self) -> u64 {
        self.settings.uint("cache-size") as u64 * 1024 * 1024
    }

    /// The blur the extension shows, if any surface uses the baked one.
    fn blur(&self) -> Option<Blur> {
        let s = &self.settings;
        (s.boolean("blur-overview") || s.boolean("blur-panel"))
            .then(|| Blur::new(s.uint("blur-strength"), s.double("blur-brightness")))
    }

    /// Generated files that are on screen or about to be: the cache keeps them.
    fn in_use(&self) -> Vec<PathBuf> {
        [&self.current, &self.media_now]
            .into_iter()
            .flatten()
            .chain(&self.preloaded)
            .flat_map(|p| [&p.picture, &p.blurred, &p.playable])
            .flatten()
            .cloned()
            .collect()
    }
}
