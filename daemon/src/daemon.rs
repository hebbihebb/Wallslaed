//! The long-running service: owns the timer, the shuffle bag, and the D-Bus API.
//! Everything runs on one GLib main loop; image work goes to a worker thread.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gio::prelude::*;
use glib::{ControlFlow, SourceId, Variant, VariantDict};

use crate::library::{self, Kind};
use crate::prepare::{self, Job, Prepared};
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
    <signal name="Changed"><arg type="a{sv}" name="state"/></signal>
  </interface>
</node>
"#;

/// Consecutive files that may fail to load before we stop trying.
const MAX_FAILURES: u32 = 8;
const RESCAN_DELAY_SECS: u32 = 3;
const FALLBACK_SCREEN: (u32, u32) = (3840, 2160);

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
}

fn handle_method(d: &Shared, method: &str, params: &Variant, inv: gio::DBusMethodInvocation) {
    match method {
        "Next" => advance(d, Step::Next),
        "Previous" => advance(d, Step::Previous),
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
        "folder" | "recursive" | "animated-enabled" => rescan(&d1, false),
        "interval" => restart_timer(&d1),
        "paused" => {
            restart_timer(&d1);
            emit_changed(&d1);
        }
        "prescale" => {
            d1.borrow_mut().preloaded = None;
            advance(&d1, Step::Reload);
        }
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

fn advance(d: &Shared, step: Step) {
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
        Some(path) => show(d, path),
        None if matches!(step, Step::Previous) => {}
        None => eprintln!("wallslaed: no wallpapers found in {}", folder(&d.borrow().settings).display()),
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
        if let Some(pic) = &p.picture {
            let uri = gio::File::for_path(pic).uri();
            // Set both so dark mode doesn't keep showing the old one.
            let _ = s.background.set_string("picture-uri", &uri);
            let _ = s.background.set_string("picture-uri-dark", &uri);
        }
        s.current = Some(p);
        save_state(&s);
    }
    emit_changed(d);
    restart_timer(d);
    preload(d);
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
            if s.generation == generation {
                s.preloaded = Some(p);
            }
        }
    });
}

fn restart_timer(d: &Shared) {
    let mut s = d.borrow_mut();
    if let Some(id) = s.timer.take() {
        id.remove();
    }
    if s.settings.boolean("paused") {
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
    dict.insert("paused", s.settings.boolean("paused"));
    dict.insert("favorite", s.bag.is_favorite());
    dict.insert("count", s.library.len() as u32);
    dict.insert("remaining", s.bag.remaining() as u32);
    dict.end()
}

fn show_in_files(d: &Shared) {
    let s = d.borrow();
    let (Some(conn), Some(cur)) = (&s.conn, &s.current) else { return };
    let uri = gio::File::for_path(&cur.source).uri();
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
        }
    }
}
