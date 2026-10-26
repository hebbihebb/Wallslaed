mod daemon;
mod library;
mod media;
mod prepare;
mod shuffle;
mod store;

use gio::prelude::*;

pub const SCHEMA: &str = "io.github.hebbihebb.Wallslaed";
pub const BUS_NAME: &str = "io.github.hebbihebb.Wallslaed";
pub const IFACE: &str = "io.github.hebbihebb.Wallslaed";
pub const OBJECT_PATH: &str = "/io/github/hebbihebb/Wallslaed";

const USAGE: &str = "\
usage: wallslaed <command>

  daemon        run the service (normally started by systemd)
  next          show the next wallpaper
  prev          go back to the previous wallpaper
  pause|resume  stop or restart the timer
  toggle        toggle pause
  favorite      toggle favorite on the current wallpaper
  ban           never show the current wallpaper again
  show          open the current wallpaper's folder in Files
  rescan        re-read the wallpaper folder
  status        print what's showing
  music         list the music videos
  play <file>   play a music video (with sound) instead of the rotation
  stop          stop the music video and go back to the rotation
  download <url>  download a music video with yt-dlp";

fn main() {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    let result = match cmd.as_str() {
        "daemon" => daemon::run(),
        "next" => call("Next", None),
        "prev" | "previous" => call("Previous", None),
        "pause" => call("SetPaused", Some((true,).to_variant())),
        "resume" => call("SetPaused", Some((false,).to_variant())),
        "toggle" => call("TogglePause", None),
        "favorite" | "fav" => call("ToggleFavorite", None),
        "ban" => call("Ban", None),
        "show" | "reveal" => call("ShowInFiles", None),
        "rescan" => call("Rescan", None),
        "status" => status(),
        "music" => list_media(),
        "play" => match std::env::args().nth(2) {
            Some(f) => {
                let path = std::fs::canonicalize(&f).map_err(|e| format!("{f}: {e}"));
                path.and_then(|p| call("PlayMedia", Some((p.to_string_lossy().into_owned(),).to_variant())))
            }
            None => Err(USAGE.to_owned()),
        },
        "stop" => call("StopMedia", None),
        "download" => match std::env::args().nth(2) {
            Some(url) => download(&url),
            None => Err(USAGE.to_owned()),
        },
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(())
        }
        _ => Err(USAGE.to_owned()),
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn call(method: &str, args: Option<glib::Variant>) -> Result<(), String> {
    request(method, args).map(drop)
}

fn request(method: &str, args: Option<glib::Variant>) -> Result<glib::Variant, String> {
    let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
        .map_err(|e| e.to_string())?;
    conn.call_sync(
        Some(BUS_NAME),
        OBJECT_PATH,
        IFACE,
        method,
        args.as_ref(),
        None,
        gio::DBusCallFlags::NO_AUTO_START,
        5000,
        gio::Cancellable::NONE,
    )
    .map_err(|e| format!("wallslaed daemon not reachable: {e}"))
}

fn status() -> Result<(), String> {
    let reply = request("GetState", None)?;
    let dict = glib::VariantDict::new(Some(&reply.child_value(0)));
    let get = |k: &str| dict.lookup_value(k, None);
    let s = |k: &str| get(k).and_then(|v| v.get::<String>()).unwrap_or_default();
    let b = |k: &str| get(k).and_then(|v| v.get::<bool>()).unwrap_or(false);
    let u = |k: &str| get(k).and_then(|v| v.get::<u32>()).unwrap_or(0);
    if !s("media").is_empty() {
        println!("music:     {}", s("media-title"));
    }
    println!("current:   {}", s("current"));
    if b("animated") {
        println!("playing:   {}", s("playable"));
    }
    println!("favorite:  {}", b("favorite"));
    println!("paused:    {}", b("paused"));
    println!("library:   {} wallpapers, {} left this cycle", u("count"), u("remaining"));
    Ok(())
}

fn list_media() -> Result<(), String> {
    let reply = request("ListMedia", None)?;
    for item in reply.child_value(0).iter() {
        let dict = glib::VariantDict::new(Some(&item));
        let path = dict.lookup_value("path", None).and_then(|v| v.get::<String>()).unwrap_or_default();
        println!("{path}");
    }
    Ok(())
}

/// Queue a download, then follow its progress until it ends.
fn download(url: &str) -> Result<(), String> {
    let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
        .map_err(|e| e.to_string())?;
    let main_loop = glib::MainLoop::new(None, false);
    let wanted = std::rc::Rc::new(std::cell::RefCell::new(None::<String>));
    let failed = std::rc::Rc::new(std::cell::Cell::new(false));
    let (w, f, ml) = (wanted.clone(), failed.clone(), main_loop.clone());
    let _sub = conn.subscribe_to_signal(
        Some(BUS_NAME),
        Some(IFACE),
        Some("DownloadProgress"),
        Some(OBJECT_PATH),
        None,
        gio::DBusSignalFlags::NONE,
        move |sig| {
            let Some((id, title, fraction, status)) = sig.parameters.get::<(String, String, f64, String)>() else { return };
            if w.borrow().as_deref() != Some(id.as_str()) {
                return;
            }
            match status.as_str() {
                "downloading" => eprint!("\r{title}: {:.0}%   ", fraction * 100.0),
                "processing" => eprint!("\r{title}: finishing...   "),
                "queued" => eprint!("\rwaiting for another download...   "),
                _ => {
                    eprintln!("\r{title}: {status}");
                    f.set(status != "done");
                    ml.quit();
                }
            }
        },
    );
    let reply = request("Download", Some((url,).to_variant()))?;
    *wanted.borrow_mut() = reply.child_value(0).get::<String>();
    main_loop.run();
    if failed.get() {
        Err("download failed".into())
    } else {
        Ok(())
    }
}
