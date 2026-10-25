//! Plays one animated wallpaper, muted and looped, in one undecorated window per
//! monitor. The Wallslæd extension hides these windows and shows them on the
//! desktop. Commands arrive on stdin, one per line: `play`, `pause`, `quit`.
//! Closing stdin also quits, so the player never outlives GNOME Shell, and it
//! exits within two seconds of either even if something hangs.
//!
//! Decoding goes through GStreamer's playbin3 into gtk4paintablesink, which takes
//! hardware-decoded frames as DMA-BUFs without copying them through the CPU. If
//! that sink isn't installed we fall back to GtkMediaFile.

mod gst;

use std::cell::RefCell;
use std::io::BufRead;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

/// NVDEC decoders, demoted when the compositor runs on another GPU so video
/// decodes there instead of waking the NVIDIA card.
const DGPU_DECODERS: &[&str] = &[
    "nvh264dec", "nvh265dec", "nvvp8dec", "nvvp9dec", "nvav1dec", "nvmpegvideodec",
    "nvmpeg2videodec", "nvmpeg4videodec", "nvjpegdec", "nvh264sldec", "nvh265sldec",
    "nvvp8sldec", "nvvp9sldec", "nvav1sldec",
];

/// After `quit` or end of stdin, how long a clean exit may take.
const QUIT_GRACE_SECS: u64 = 2;

enum Backend {
    Gst(gst::Player),
    Gtk(gtk::MediaFile),
}

impl Backend {
    fn new(file: &str) -> (Backend, gdk::Paintable) {
        if std::env::var_os("WALLSLAED_PLAYER_GTK").is_none() {
            match gst::Player::new(&gio::File::for_path(file).uri()) {
                Ok((player, paintable)) => return (Backend::Gst(player), paintable),
                Err(e) => eprintln!("wallslaed-player: {e}; using GtkMediaFile"),
            }
        }
        let media = gtk::MediaFile::for_filename(file);
        media.set_loop(true);
        media.set_muted(true);
        media.connect_error_notify(|m| {
            if let Some(e) = m.error() {
                eprintln!("wallslaed-player: {e}");
            }
        });
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
}

thread_local! {
    static BACKEND: RefCell<Option<Backend>> = const { RefCell::new(None) };
}

fn main() -> glib::ExitCode {
    let Some(file) = std::env::args().nth(1) else {
        eprintln!("usage: wallslaed-player <file>");
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
    if gtk::init().is_err() {
        eprintln!("wallslaed-player: no display");
        return glib::ExitCode::FAILURE;
    }

    let main_loop = glib::MainLoop::new(None, false);
    let (backend, paintable) = Backend::new(&file);

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
    BACKEND.with(|b| *b.borrow_mut() = Some(backend));

    let ctx = glib::MainContext::default();
    let ml = main_loop.clone();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            match line.trim() {
                "play" => ctx.invoke(|| set_playing(true)),
                "pause" => ctx.invoke(|| set_playing(false)),
                "quit" => break,
                _ => {}
            }
        }
        // Leave even if the main loop is stuck, so the extension never has to
        // kill us.
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(QUIT_GRACE_SECS));
            std::process::exit(0);
        });
        ctx.invoke(move || ml.quit());
    });

    main_loop.run();
    BACKEND.with(|b| b.borrow_mut().take());
    drop(windows);
    glib::ExitCode::SUCCESS
}

fn set_playing(playing: bool) {
    BACKEND.with(|b| {
        if let Some(backend) = &*b.borrow() {
            backend.set_playing(playing);
        }
    });
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
