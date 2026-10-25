mod daemon;
mod library;
mod prepare;
mod shuffle;

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
  status        print what's showing";

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
    println!("current:   {}", s("current"));
    if b("animated") {
        println!("playing:   {}", s("playable"));
    }
    println!("favorite:  {}", b("favorite"));
    println!("paused:    {}", b("paused"));
    println!("library:   {} wallpapers, {} left this cycle", u("count"), u("remaining"));
    Ok(())
}
