//! Plays one animated wallpaper, looped, in one undecorated window per monitor.
//! The Wallslæd extension hides these windows and shows them on the desktop.
//! Commands arrive on stdin, one per line: `play`, `pause`, `toggle`, `mute`,
//! `unmute`, `volume <0..1>`, `seek <secs>`, `quit`. Closing stdin also quits,
//! so the player never outlives GNOME Shell, and it exits within two seconds
//! of either even if something hangs. Every change of play/mute state is
//! reported on stdout as `state <playing|paused> <muted 0|1>`.
//!
//! Wallpapers are muted. With `--audio` (music videos) the player plays sound,
//! shows up as a media player in GNOME (MPRIS), and reports its position to the
//! daemon so playback resumes where it left off.
//!
//! Decoding goes through GStreamer's playbin3 into gtk4paintablesink, which takes
//! hardware-decoded frames as DMA-BUFs without copying them through the CPU. If
//! that sink isn't installed we fall back to GtkMediaFile.

mod gst;
mod mpris;

use std::cell::RefCell;
use std::io::{BufRead, Write};

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

/// NVDEC decoders, demoted when the compositor runs on another GPU so video
/// decodes there instead of waking the NVIDIA card.
const DGPU_DECODERS: &[&str] = &[
    "nvh264dec", "nvh265dec", "nvvp8dec", "nvvp9dec", "nvav1dec", "nvmpegvideodec",
    "nvmpeg2videodec", "nvmpeg4videodec", "nvjpegdec", "nvh264sldec", "nvh265sldec",
    "nvvp8sldec", "nvvp9sldec", "nvav1sldec",
];

/// How often a music video's position is saved with the daemon.
const REPORT_SECS: u32 = 15;
/// After `quit` or end of stdin, how long a clean exit may take.
const QUIT_GRACE_SECS: u64 = 2;

const DAEMON_NAME: &str = "io.github.hebbihebb.Wallslaed";
const DAEMON_PATH: &str = "/io/github/hebbihebb/Wallslaed";

pub struct Options {
    pub file: String,
    pub audio: bool,
    pub start: f64,
    pub title: Option<String>,
    pub art: Option<String>,
}

impl Options {
    fn parse(mut args: impl Iterator<Item = String>) -> Option<Options> {
        let mut o = Options { file: String::new(), audio: false, start: 0.0, title: None, art: None };
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--audio" => o.audio = true,
                "--start" => o.start = args.next()?.parse().ok()?,
                "--title" => o.title = args.next(),
                "--art" => o.art = args.next(),
                _ if o.file.is_empty() => o.file = arg,
                _ => return None,
            }
        }
        (!o.file.is_empty()).then_some(o)
    }
}

enum Backend {
    Gst(gst::Player),
    Gtk(gtk::MediaFile),
}

impl Backend {
    fn new(o: &Options) -> (Backend, gdk::Paintable) {
        if std::env::var_os("WALLSLAED_PLAYER_GTK").is_none() {
            match gst::Player::new(&gio::File::for_path(&o.file).uri(), o.audio, o.start) {
                Ok((player, paintable)) => return (Backend::Gst(player), paintable),
                Err(e) => eprintln!("wallslaed-player: {e}; using GtkMediaFile"),
            }
        }
        let media = gtk::MediaFile::for_filename(&o.file);
        media.set_loop(true);
        media.set_muted(!o.audio);
        media.connect_error_notify(|m| {
            if let Some(e) = m.error() {
                eprintln!("wallslaed-player: {e}");
            }
        });
        if o.start > 0.0 {
            let us = (o.start * 1e6) as i64;
            media.connect_prepared_notify(move |m| {
                if m.is_prepared() {
                    m.seek(us);
                }
            });
        }
        let paintable = media.clone().upcast();
        (Backend::Gtk(media), paintable)
    }

    fn set_playing(&self, playing: bool) {
        match self {
            Backend::Gst(p) => p.set_playing(playing),
            Backend::Gtk(m) if playing => m.play(),
            Backend::Gtk(m) => m.pause(),
        }
    }

    fn set_muted(&self, muted: bool) {
        match self {
            Backend::Gst(p) => p.set_muted(muted),
            Backend::Gtk(m) => m.set_muted(muted),
        }
    }

    fn set_volume(&self, volume: f64) {
        match self {
            Backend::Gst(p) => p.set_volume(volume),
            Backend::Gtk(m) => m.set_volume(volume.clamp(0.0, 1.0)),
        }
    }

    fn position(&self) -> Option<f64> {
        match self {
            Backend::Gst(p) => p.position(),
            Backend::Gtk(m) => Some(m.timestamp() as f64 / 1e6),
        }
    }

    fn duration(&self) -> Option<f64> {
        match self {
            Backend::Gst(p) => p.duration(),
            Backend::Gtk(m) => (m.duration() > 0).then(|| m.duration() as f64 / 1e6),
        }
    }

    fn seek(&self, secs: f64) {
        match self {
            Backend::Gst(p) => p.seek(secs),
            Backend::Gtk(m) => m.seek((secs.max(0.0) * 1e6) as i64),
        }
    }
}

struct Playback {
    backend: Backend,
    file: String,
    audio: bool,
    playing: bool,
    muted: bool,
    volume: f64,
}

/// What MPRIS and the extension get to see.
pub struct Status {
    pub playing: bool,
    pub volume: f64,
    pub position: f64,
    pub duration: Option<f64>,
}

thread_local! {
    static PLAYBACK: RefCell<Option<Playback>> = const { RefCell::new(None) };
    /// Commands that arrived before playback was set up, run once it is.
    static EARLY: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn main() -> glib::ExitCode {
    let Some(opts) = Options::parse(std::env::args().skip(1)) else {
        eprintln!("usage: wallslaed-player <file> [--audio] [--start <secs>] [--title <t>] [--art <jpg>]");
        return glib::ExitCode::FAILURE;
    };
    match compositor_gpu_vendor() {
        Some(NVIDIA) => {
            // Decode with NVDEC on the same GPU. GTK's GL renderer imports those
            // frames more cheaply than the Vulkan one on NVIDIA.
            if std::env::var_os("GSK_RENDERER").is_none() {
                std::env::set_var("GSK_RENDERER", "gl");
            }
        }
        _ if std::env::var_os("WALLSLAED_ALLOW_DGPU").is_none() => prefer_integrated_gpu(),
        _ => {}
    }

    // Listen before anything that might hang (GTK, the GPU driver, the first
    // frame), so `quit` or end of input always ends us within two seconds.
    let main_loop = glib::MainLoop::new(None, false);
    let ctx = glib::MainContext::default();
    let ml = main_loop.clone();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            let line = line.trim().to_owned();
            if line == "quit" {
                break;
            }
            ctx.invoke(move || command(&line));
        }
        // Leave even if the main loop is stuck, so the extension never has to
        // kill us; the grace period lets a music video save its position.
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(QUIT_GRACE_SECS));
            std::process::exit(0);
        });
        ctx.invoke(move || ml.quit());
    });

    if gtk::init().is_err() {
        eprintln!("wallslaed-player: no display");
        return glib::ExitCode::FAILURE;
    }
    let (backend, paintable) = Backend::new(&opts);

    let display = gdk::Display::default().expect("display after gtk::init");
    let monitors = display.monitors();
    let mut windows = Vec::new();
    for i in 0..monitors.n_items() {
        let Some(monitor) = monitors.item(i).and_downcast::<gdk::Monitor>() else { continue };
        let geometry = monitor.geometry();
        let picture = gtk::Picture::for_paintable(&paintable);
        picture.set_content_fit(gtk::ContentFit::Cover);
        // Hand decoded frames straight to the compositor when the format allows it.
        let offload = gtk::GraphicsOffload::new(Some(&picture));
        offload.set_enabled(gtk::GraphicsOffloadEnabled::Enabled);
        let window = gtk::Window::builder()
            .title(format!("wallslaed-player-{i}"))
            .decorated(false)
            .default_width(geometry.width())
            .default_height(geometry.height())
            .child(&offload)
            .build();
        window.present();
        windows.push(window);
    }
    backend.set_playing(true);
    PLAYBACK.with(|p| {
        *p.borrow_mut() = Some(Playback {
            backend,
            file: opts.file.clone(),
            audio: opts.audio,
            playing: true,
            muted: !opts.audio,
            volume: 1.0,
        })
    });
    report_state();
    for line in EARLY.with(|e| std::mem::take(&mut *e.borrow_mut())) {
        command(&line);
    }

    let _mpris = opts.audio.then(|| {
        let title = opts.title.clone().unwrap_or_else(|| title_from_path(&opts.file));
        mpris::start(&opts.file, &title, opts.art.as_deref())
    });
    if opts.audio {
        glib::timeout_add_seconds_local(REPORT_SECS, || {
            report_position(false);
            glib::ControlFlow::Continue
        });
    }

    main_loop.run();
    report_position(true);
    PLAYBACK.with(|p| p.borrow_mut().take());
    drop(windows);
    glib::ExitCode::SUCCESS
}

/// Run one control command. Shared by stdin and MPRIS.
pub fn command(line: &str) {
    let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
    let arg = arg.trim().parse::<f64>().ok();
    let changed = PLAYBACK.with(|p| {
        let mut p = p.borrow_mut();
        let Some(p) = p.as_mut() else {
            EARLY.with(|e| e.borrow_mut().push(line.to_owned()));
            return false;
        };
        let (playing, muted) = (p.playing, p.muted);
        match (cmd, arg) {
            ("play", _) => p.playing = true,
            ("pause", _) => p.playing = false,
            ("toggle", _) => p.playing = !p.playing,
            ("mute", _) => p.muted = true,
            ("unmute", _) => p.muted = false,
            ("volume", Some(v)) => {
                p.volume = v.clamp(0.0, 1.0);
                p.backend.set_volume(p.volume);
            }
            ("seek", Some(secs)) => {
                p.backend.seek(secs);
                mpris::seeked(secs);
            }
            _ => {}
        }
        if p.playing != playing {
            p.backend.set_playing(p.playing);
        }
        if p.muted != muted {
            p.backend.set_muted(p.muted);
        }
        true
    });
    if changed {
        report_state();
    }
}

pub fn status() -> Option<Status> {
    PLAYBACK.with(|p| {
        let p = p.borrow();
        let p = p.as_ref()?;
        Some(Status {
            playing: p.playing,
            volume: if p.muted { 0.0 } else { p.volume },
            position: p.backend.position().unwrap_or(0.0),
            duration: p.backend.duration(),
        })
    })
}

/// Tell the extension (stdout) and MPRIS clients what changed.
fn report_state() {
    let line = PLAYBACK.with(|p| {
        let p = p.borrow();
        let p = p.as_ref()?;
        Some(format!(
            "state {} {}",
            if p.playing { "playing" } else { "paused" },
            u8::from(p.muted)
        ))
    });
    if let Some(line) = line {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}").and_then(|_| out.flush());
    }
    mpris::changed();
}

/// Save a music video's position with the daemon. `blocking` is for exit,
/// when there's no main loop left to finish an async call.
fn report_position(blocking: bool) {
    let Some((file, pos)) = PLAYBACK.with(|p| {
        let p = p.borrow();
        let p = p.as_ref().filter(|p| p.audio)?;
        Some((p.file.clone(), p.backend.position()?))
    }) else {
        return;
    };
    let Ok(conn) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else { return };
    let args = Some((file, pos).to_variant());
    let flags = gio::DBusCallFlags::NO_AUTO_START;
    if blocking {
        let _ = conn.call_sync(Some(DAEMON_NAME), DAEMON_PATH, DAEMON_NAME, "ReportPosition",
            args.as_ref(), None, flags, 1000, gio::Cancellable::NONE);
    } else {
        conn.call(Some(DAEMON_NAME), DAEMON_PATH, DAEMON_NAME, "ReportPosition",
            args.as_ref(), None, flags, -1, gio::Cancellable::NONE, |_| {});
    }
}

fn title_from_path(file: &str) -> String {
    let stem = std::path::Path::new(file)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(file);
    // yt-dlp names files "Title [videoid]"; the id is noise in a media widget.
    match stem.rsplit_once(" [") {
        Some((title, id)) if id.ends_with(']') => title.to_owned(),
        _ => stem.to_owned(),
    }
}

const NVIDIA: u32 = 0x10de;

/// PCI vendor of the GPU Mutter composites on. Like Mutter, prefer the GPU that
/// drives the built-in panel, then any GPU with a connected display.
fn compositor_gpu_vendor() -> Option<u32> {
    let mut fallback = None;
    for entry in std::fs::read_dir("/sys/class/drm").ok()?.flatten() {
        let name = entry.file_name().into_string().ok()?;
        let Some((card, connector)) = name.split_once('-') else { continue };
        if !card.starts_with("card") {
            continue;
        }
        let connected = std::fs::read_to_string(entry.path().join("status"))
            .is_ok_and(|s| s.trim() == "connected");
        if !connected {
            continue;
        }
        let vendor = std::fs::read_to_string(format!("/sys/class/drm/{card}/device/vendor"))
            .ok()
            .and_then(|v| u32::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok());
        if ["eDP", "LVDS", "DSI"].iter().any(|p| connector.starts_with(p)) {
            return vendor;
        }
        fallback = fallback.or(vendor);
    }
    fallback
}

fn prefer_integrated_gpu() {
    let mut ranks: Vec<String> = DGPU_DECODERS.iter().map(|d| format!("{d}:NONE")).collect();
    if let Ok(existing) = std::env::var("GST_PLUGIN_FEATURE_RANK") {
        ranks.push(existing);
    }
    // Single-threaded at this point, before GTK or GStreamer start.
    // A global LIBVA_DRIVER_NAME=nvidia would stop VA-API from using the iGPU.
    std::env::remove_var("LIBVA_DRIVER_NAME");
    std::env::set_var("GST_PLUGIN_FEATURE_RANK", ranks.join(","));
}
