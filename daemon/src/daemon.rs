//! The long-running service: owns the timer, the shuffle bag, and the D-Bus API.
//! Everything runs on one GLib main loop; image work goes to a worker thread.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gio::prelude::*;
use glib::{ControlFlow, SourceId, Variant, VariantDict};

use crate::library::{self, Kind};
use crate::media::{self, MediaState};
use crate::prepare::{self, Blur, Job, Prepared};
use crate::shuffle::Bag;
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
const FALLBACK_SCREEN: (u32, u32) = (3840, 2160);
/// Download progress is announced in steps of this fraction.
const PROGRESS_STEP: f64 = 0.01;
const SIGINT: i32 = 2;

#[derive(Clone, Copy)]
enum Step {
    Next,
    Previous,
    Reload,
}

struct Daemon {
    settings: gio::Settings,
    background: gio::Settings,
    library: Vec<String>,
    bag: Bag,
    rng: fastrand::Rng,
    current: Option<Prepared>,
    preloaded: Option<Prepared>,
    /// Bumped on every user-visible change so stale worker results are dropped.
    generation: u64,
    failures: u32,
    timer: Option<SourceId>,
    rescan_timer: Option<SourceId>,
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
    /// The active music video once its poster is ready.
    media_now: Option<Prepared>,
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
    proc: gio::Subprocess,
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
    let _ = std::fs::create_dir_all(&state_dir);
    let state_path = state_dir.join("state.json");
    let media_path = state_dir.join("media.json");
    let bag = std::fs::read(&state_path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();

    let d: Shared = Rc::new(RefCell::new(Daemon {
        settings: gio::Settings::new(SCHEMA),
        background: gio::Settings::new("org.gnome.desktop.background"),
        library: Vec::new(),
        bag,
        rng: fastrand::Rng::new(),
        current: None,
        preloaded: None,
        generation: 0,
        failures: 0,
        timer: None,
        rescan_timer: None,
        monitors: Vec::new(),
        locked: false,
        pending_tick: false,
        screen: FALLBACK_SCREEN,
        conn: None,
        state_path,
        cache_dir,
        media: MediaState::load(&media_path),
        media_path,
        media_now: None,
        downloads: Downloads::default(),
    }));

    connect_settings(&d);

    let main_loop = glib::MainLoop::new(None, false);
    {
        let d1 = d.clone();
        let ml = main_loop.clone();
        let _owner = gio::bus_own_name(
            gio::BusType::Session,
            BUS_NAME,
            gio::BusNameOwnerFlags::NONE,
            move |conn, _| on_bus(&d1, conn),
            |_, _| {},
            move |_, _| {
                eprintln!("wallslaed: lost bus name {BUS_NAME} (already running?)");
                ml.quit();
            },
        );
        // State is saved on every change, so a plain SIGTERM loses nothing.
        main_loop.run();
    }
    Ok(())
}

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
                    s.preloaded = None;
                }
            },
        );
    std::mem::forget(_sub);

    {
        let mut s = d.borrow_mut();
        s.screen = query_screen(&conn);
        s.conn = Some(conn);
    }
    rescan(d, true);
    // Pick a music video back up after a restart or a new login.
    let active = d.borrow().media.active.clone();
    if let Some(path) = active {
        if media::is_music_video(&media::folder(&d.borrow().settings), Path::new(&path)) {
            start_media(d, path);
        } else {
            end_media(d);
        }
    }
}

fn handle_method(d: &Shared, method: &str, params: &Variant, inv: gio::DBusMethodInvocation) {
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
            if let Some(p) = d.borrow_mut().bag.ban_current() {
                eprintln!("wallslaed: never showing {p} again");
            }
            advance(d, Step::Next);
        }
        "Rescan" => rescan(d, false),
        "ShowInFiles" => show_in_files(d),
        "GetState" => {
            let state = state_variant(&d.borrow());
            inv.return_value(Some(&Variant::tuple_from_iter([state])));
            return;
        }
        "PlayMedia" => {
            let path = params.child_value(0).get::<String>().unwrap_or_default();
            if !media::is_music_video(&media::folder(&d.borrow().settings), Path::new(&path)) {
                inv.return_error(gio::DBusError::InvalidArgs, "not a video in the music folder");
                return;
            }
            start_media(d, path);
        }
        "StopMedia" => stop_media(d),
        "ReportPosition" => {
            let (path, secs) = params.get::<(String, f64)>().unwrap_or_default();
            let mut s = d.borrow_mut();
            if secs.is_finite() && media::is_music_video(&media::folder(&s.settings), Path::new(&path)) {
                s.media.positions.insert(path, secs.max(0.0));
                s.media.save(&s.media_path);
            }
        }
        "ListMedia" => {
            let folder = media::folder(&d.borrow().settings);
            let items: Vec<Variant> = media::list(&folder)
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
        "folder" | "recursive" | "animated-enabled" | "music-folder" => rescan(&d1, false),
        "interval" => restart_timer(&d1),
        "paused" => {
            restart_timer(&d1);
            emit_changed(&d1);
        }
        "prescale" => {
            d1.borrow_mut().preloaded = None;
            advance(&d1, Step::Reload);
        }
        "blur-strength" | "blur-brightness" | "blur-overview" | "blur-panel" => rebake(&d1),
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

/// Rescan the folder on a worker thread, then reconcile the bag and watchers.
fn rescan(d: &Shared, startup: bool) {
    let (root, recursive, animated) = {
        let s = d.borrow();
        (
            folder(&s.settings),
            s.settings.boolean("recursive"),
            s.settings.boolean("animated-enabled"),
        )
    };
    let d = d.clone();
    glib::spawn_future_local(async move {
        let Ok(mut scan) = gio::spawn_blocking(move || library::scan(&root, recursive)).await
        else {
            return;
        };
        if !animated {
            scan.files
                .retain(|p| library::kind_of(Path::new(p)) == Some(Kind::Static));
        }
        // Music videos play with sound; never pick them at random. If the music
        // folder holds the whole wallpaper folder, that would leave nothing, so
        // the exclusion only applies to a music folder inside it.
        let music = media::folder(&d.borrow().settings);
        if !folder(&d.borrow().settings).starts_with(&music) {
            scan.files.retain(|p| !Path::new(p).starts_with(&music));
        }
        let had_current = {
            let mut s = d.borrow_mut();
            let s = &mut *s;
            s.bag.sync(&scan.files, &mut s.rng);
            s.library = scan.files;
            s.monitors = watch(&d, &scan.dirs);
            s.preloaded = None;
            eprintln!(
                "wallslaed: {} wallpapers, {} left in this cycle",
                s.library.len(),
                s.bag.remaining()
            );
            save_state(s);
            s.bag.current().is_some()
        };
        if had_current && startup {
            advance(&d, Step::Reload);
        } else if !had_current {
            advance(&d, Step::Next);
        } else {
            emit_changed(&d);
            preload(&d);
        }
    });
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
                let mut s = d.borrow_mut();
                if let Some(id) = s.rescan_timer.take() {
                    id.remove();
                }
                let d2 = d.clone();
                s.rescan_timer = Some(glib::timeout_add_seconds_local(RESCAN_DELAY_SECS, move || {
                    d2.borrow_mut().rescan_timer = None;
                    rescan(&d2, false);
                    ControlFlow::Break
                }));
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
        match step {
            Step::Next => s.bag.next(&s.library, &ok, weight, &mut s.rng),
            Step::Previous => s.bag.previous(&ok),
            Step::Reload => s.bag.current().map(str::to_owned),
        }
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
        let ready = s.preloaded.take().filter(|p| p.source == path);
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
        s.current = Some(p);
        save_state(&s);
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

/// Bake the blur again for what's on screen, after its settings changed.
fn rebake(d: &Shared) {
    let (blur, cache_dir, current, media) = {
        let mut s = d.borrow_mut();
        s.preloaded = None;
        let pic = |p: &Option<Prepared>| p.as_ref().and_then(|p| p.picture.clone());
        (s.blur(), s.cache_dir.clone(), pic(&s.current), pic(&s.media_now))
    };
    let d = d.clone();
    glib::spawn_future_local(async move {
        let bake = move |pic: &Option<PathBuf>| {
            prepare::blur_only(pic.as_deref()?, &cache_dir, blur?)
        };
        let (cur, med) = (current.clone(), media.clone());
        let Ok((current_blur, media_blur)) =
            gio::spawn_blocking(move || (bake(&cur), bake(&med))).await
        else {
            return;
        };
        {
            let mut s = d.borrow_mut();
            // Only if the same pictures are still up; a newer one brings its own blur.
            if let Some(c) = s.current.as_mut().filter(|c| c.picture == current) {
                c.blurred = current_blur;
                c.baked_with = blur;
            }
            if let Some(m) = s.media_now.as_mut().filter(|m| m.picture == media) {
                m.blurred = media_blur;
                m.baked_with = blur;
            }
        }
        emit_changed(&d);
        preload(&d);
    });
}

/// Prepare the next wallpaper now so switching later is instant.
fn preload(d: &Shared) {
    let (generation, job) = {
        let s = d.borrow();
        let ok = eligible(&s.library);
        let Some(next) = s.bag.peek(&ok) else { return };
        if s.preloaded.as_ref().is_some_and(|p| p.source == next) {
            return;
        }
        (s.generation, s.job(next.to_owned()))
    };
    let d = d.clone();
    glib::spawn_future_local(async move {
        if let Ok(Ok(p)) = gio::spawn_blocking(move || prepare::prepare(&job)).await {
            let mut s = d.borrow_mut();
            // Dropped if the blur settings changed while it was being prepared.
            if s.generation == generation && p.baked_with == s.blur() {
                s.preloaded = Some(p);
            }
        }
    });
}

fn set_background(s: &Daemon, picture: Option<&Path>) {
    let Some(pic) = picture else { return };
    let uri = gio::File::for_path(pic).uri();
    // Set both so dark mode doesn't keep showing the old one.
    let _ = s.background.set_string("picture-uri", &uri);
    let _ = s.background.set_string("picture-uri-dark", &uri);
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
    dict.insert(
        "media-art",
        media::thumbnail_of(path).as_deref().and_then(Path::to_str).unwrap_or(""),
    );
    // Once ready, the music video takes the place of the wallpaper.
    if let Some(m) = &s.media_now {
        dict.insert("picture", path_str(&m.picture));
        dict.insert("playable", path_str(&m.playable));
        dict.insert("animated", true);
        dict.insert("blurred", path_str(&m.blurred));
    }
    dict.insert("audio", s.media_now.is_some());
    dict.end()
}

/// Put a music video on the desktop and hold the rotation.
fn start_media(d: &Shared, path: String) {
    let job = {
        let mut s = d.borrow_mut();
        s.media.active = Some(path.clone());
        s.media.save(&s.media_path);
        s.media_now = None;
        s.job(path)
    };
    restart_timer(d);
    emit_changed(d);
    let d = d.clone();
    glib::spawn_future_local(async move {
        let result = gio::spawn_blocking(move || prepare::prepare(&job)).await;
        let ok = {
            let mut s = d.borrow_mut();
            match result {
                // Ignore it if another video was picked in the meantime.
                Ok(Ok(p)) if s.media.active.as_deref() == Some(p.source.as_str()) => {
                    set_background(&s, p.picture.as_deref());
                    s.media_now = Some(p);
                    true
                }
                Ok(Ok(_)) => return,
                Ok(Err(e)) => {
                    eprintln!("wallslaed: can't play music video: {e}");
                    false
                }
                Err(_) => false,
            }
        };
        if ok {
            emit_changed(&d);
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
    if was_on {
        s.media.save(&s.media_path);
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
            // yt-dlp cleans up after itself on SIGINT.
            r.proc.send_signal(SIGINT);
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

/// Start the next queued download unless one is already running.
fn pump_downloads(d: &Shared) {
    let (id, url, argv, temp) = {
        let mut s = d.borrow_mut();
        if s.downloads.running.is_some() {
            return;
        }
        let Some((id, url)) = s.downloads.queue.pop_front() else { return };
        let folder = media::folder(&s.settings);
        let temp = s.cache_dir.join("downloads");
        let _ = std::fs::create_dir_all(&folder);
        let _ = std::fs::create_dir_all(&temp);
        let argv = media::command(&url, &folder, &temp, s.settings.uint("music-max-height"));
        (id, url, argv, temp)
    };
    let proc = argv.ok_or_else(|| "yt-dlp is not installed".to_owned()).and_then(|argv| {
        let argv: Vec<&std::ffi::OsStr> = argv.iter().map(|a| a.as_os_str()).collect();
        let flags = gio::SubprocessFlags::STDOUT_PIPE
            | gio::SubprocessFlags::STDERR_MERGE
            | gio::SubprocessFlags::STDIN_INHERIT;
        gio::Subprocess::newv(&argv, flags).map_err(|e| e.to_string())
    });
    let proc = match proc {
        Ok(p) => p,
        Err(e) => {
            eprintln!("wallslaed: download failed: {e}");
            emit_progress(d, &id, &url, 0.0, &format!("error:{e}"));
            pump_downloads(d);
            return;
        }
    };
    d.borrow_mut().downloads.running = Some(Running {
        id: id.clone(),
        title: url.clone(),
        fraction: 0.0,
        status: "downloading".into(),
        proc: proc.clone(),
        cancelled: false,
    });
    emit_progress(d, &id, &url, 0.0, "downloading");

    let d = d.clone();
    glib::spawn_future_local(async move {
        let Some(stdout) = proc.stdout_pipe() else { return };
        let input = gio::DataInputStream::new(&stdout);
        let mut file = None;
        let mut error = None;
        while let Ok(Some(line)) = input.read_line_future(glib::Priority::DEFAULT).await {
            let line = String::from_utf8_lossy(&line);
            let Some(event) = media::parse_line(&line) else { continue };
            let update = {
                let mut s = d.borrow_mut();
                let Some(r) = s.downloads.running.as_mut() else { break };
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
                        file = Some(f);
                        false
                    }
                    media::Event::Error(e) => {
                        error = Some(e);
                        false
                    }
                }
                .then(|| (r.id.clone(), r.title.clone(), r.fraction, r.status.clone()))
            };
            if let Some((id, title, fraction, status)) = update {
                emit_progress(&d, &id, &title, fraction, &status);
            }
        }
        let ok = proc.wait_check_future().await.is_ok();
        let Some(r) = d.borrow_mut().downloads.running.take() else { return };
        let status = if r.cancelled {
            "cancelled".to_owned()
        } else if ok && file.is_some() {
            "done".to_owned()
        } else {
            format!("error:{}", error.unwrap_or_else(|| "yt-dlp failed".into()))
        };
        if status != "done" {
            // Only one download runs at a time, so anything here is its leftovers.
            if let Ok(entries) = std::fs::read_dir(&temp) {
                for e in entries.flatten() {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        eprintln!("wallslaed: download {}: {status}", r.title);
        emit_progress(&d, &r.id, &r.title, if status == "done" { 1.0 } else { r.fraction }, &status);
        pump_downloads(&d);
    });
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
    let Ok(json) = serde_json::to_vec(&s.bag) else { return };
    let tmp = s.state_path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, &s.state_path);
    }
}

impl Daemon {
    fn job(&self, source: String) -> Job {
        Job {
            source,
            screen: self.screen,
            prescale: self.settings.boolean("prescale"),
            cache_dir: self.cache_dir.clone(),
            blur: self.blur(),
        }
    }

    /// The blur the extension shows, if any surface uses the baked one.
    fn blur(&self) -> Option<Blur> {
        let s = &self.settings;
        (s.boolean("blur-overview") || s.boolean("blur-panel")).then(|| Blur {
            strength: s.uint("blur-strength"),
            brightness: s.double("blur-brightness"),
        })
    }
}
