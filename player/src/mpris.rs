//! MPRIS for music videos, so GNOME shows them in the top bar's media controls
//! and on the lock screen, and media keys work. Only used with `--audio`.

use std::cell::RefCell;

use gtk::prelude::*;
use gtk::{gio, glib};
use glib::{Variant, VariantDict};

const NAME: &str = "org.mpris.MediaPlayer2.wallslaed";
const PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT: &str = "org.mpris.MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const TRACK_ID: &str = "/io/github/hebbihebb/Wallslaed/Track";

const INTROSPECTION: &str = r#"
<node>
  <interface name="org.mpris.MediaPlayer2">
    <method name="Raise"/>
    <method name="Quit"/>
    <property name="CanQuit" type="b" access="read"/>
    <property name="CanRaise" type="b" access="read"/>
    <property name="HasTrackList" type="b" access="read"/>
    <property name="Identity" type="s" access="read"/>
    <property name="SupportedUriSchemes" type="as" access="read"/>
    <property name="SupportedMimeTypes" type="as" access="read"/>
  </interface>
  <interface name="org.mpris.MediaPlayer2.Player">
    <method name="Next"/>
    <method name="Previous"/>
    <method name="Pause"/>
    <method name="PlayPause"/>
    <method name="Stop"/>
    <method name="Play"/>
    <method name="Seek"><arg type="x" name="Offset" direction="in"/></method>
    <method name="SetPosition">
      <arg type="o" name="TrackId" direction="in"/>
      <arg type="x" name="Position" direction="in"/>
    </method>
    <method name="OpenUri"><arg type="s" name="Uri" direction="in"/></method>
    <signal name="Seeked"><arg type="x" name="Position"/></signal>
    <property name="PlaybackStatus" type="s" access="read"/>
    <property name="LoopStatus" type="s" access="read"/>
    <property name="Rate" type="d" access="read"/>
    <property name="Shuffle" type="b" access="read"/>
    <property name="Metadata" type="a{sv}" access="read"/>
    <property name="Volume" type="d" access="readwrite"/>
    <property name="Position" type="x" access="read"/>
    <property name="MinimumRate" type="d" access="read"/>
    <property name="MaximumRate" type="d" access="read"/>
    <property name="CanGoNext" type="b" access="read"/>
    <property name="CanGoPrevious" type="b" access="read"/>
    <property name="CanPlay" type="b" access="read"/>
    <property name="CanPause" type="b" access="read"/>
    <property name="CanSeek" type="b" access="read"/>
    <property name="CanControl" type="b" access="read"/>
  </interface>
</node>
"#;

struct Track {
    url: String,
    title: String,
    art: Option<String>,
}

thread_local! {
    static TRACK: RefCell<Option<Track>> = const { RefCell::new(None) };
    static CONN: RefCell<Option<gio::DBusConnection>> = const { RefCell::new(None) };
}

/// Releases the bus name when dropped.
pub struct Handle(Option<gio::OwnerId>);

impl Drop for Handle {
    fn drop(&mut self) {
        if let Some(id) = self.0.take() {
            gio::bus_unown_name(id);
        }
        CONN.with(|c| c.borrow_mut().take());
    }
}

pub fn start(file: &str, title: &str, art: Option<&str>) -> Handle {
    TRACK.with(|t| {
        *t.borrow_mut() = Some(Track {
            url: gio::File::for_path(file).uri().into(),
            title: title.to_owned(),
            art: art.map(|a| gio::File::for_path(a).uri().into()),
        })
    });
    // While switching videos the old player can linger for a moment; the new
    // one takes the name over instead of queueing behind it.
    let id = gio::bus_own_name(
        gio::BusType::Session,
        NAME,
        gio::BusNameOwnerFlags::ALLOW_REPLACEMENT | gio::BusNameOwnerFlags::REPLACE,
        |conn, _| register(conn),
        |_, _| {},
        |_, _| {},
    );
    Handle(Some(id))
}

fn register(conn: gio::DBusConnection) {
    let info = gio::DBusNodeInfo::for_xml(INTROSPECTION).expect("valid MPRIS XML");
    for name in [ROOT, PLAYER] {
        let iface = info.lookup_interface(name).expect("interface in XML");
        let result = conn
            .register_object(PATH, &iface)
            .method_call(|conn, _, _, iface, method, params, inv| {
                if iface == Some(PLAYER) {
                    player_method(&conn, method, &params);
                }
                inv.return_value(None);
            })
            .property(|_, _, _, iface, prop| property(iface, prop))
            .set_property(|_, _, _, _, prop, value| {
                if prop != "Volume" {
                    return false;
                }
                let v = value.get::<f64>().unwrap_or(1.0).clamp(0.0, 1.0);
                crate::command(&format!("volume {v}"));
                crate::command(if v > 0.0 { "unmute" } else { "mute" });
                true
            })
            .build();
        if let Err(e) = result {
            eprintln!("wallslaed-player: MPRIS: {e}");
            return;
        }
    }
    CONN.with(|c| *c.borrow_mut() = Some(conn));
    changed();
}

fn player_method(conn: &gio::DBusConnection, method: &str, params: &Variant) {
    let us = |v: Variant| v.get::<i64>().unwrap_or(0) as f64 / 1e6;
    match method {
        "Play" => crate::command("play"),
        "Pause" => crate::command("pause"),
        "PlayPause" => crate::command("toggle"),
        "Seek" => {
            let pos = crate::status().map_or(0.0, |s| s.position);
            crate::command(&format!("seek {}", pos + us(params.child_value(0))));
        }
        "SetPosition" => crate::command(&format!("seek {}", us(params.child_value(1)))),
        // Stopping a music video means going back to the wallpaper rotation,
        // which the daemon decides; it will then stop this player.
        "Stop" => conn.call(
            Some(crate::DAEMON_NAME),
            crate::DAEMON_PATH,
            crate::DAEMON_NAME,
            "StopMedia",
            None,
            None,
            gio::DBusCallFlags::NO_AUTO_START,
            -1,
            gio::Cancellable::NONE,
            |_| {},
        ),
        _ => {}
    }
}

fn property(iface: &str, prop: &str) -> Variant {
    let status = crate::status();
    match (iface, prop) {
        (ROOT, "Identity") => "Wallslæd".to_variant(),
        (ROOT, "SupportedUriSchemes" | "SupportedMimeTypes") => Vec::<String>::new().to_variant(),
        (ROOT, _) => false.to_variant(),
        (_, "PlaybackStatus") => playback_status(status.as_ref()).to_variant(),
        (_, "LoopStatus") => "Track".to_variant(),
        (_, "Rate" | "MinimumRate" | "MaximumRate") => 1.0f64.to_variant(),
        (_, "Shuffle" | "CanGoNext" | "CanGoPrevious") => false.to_variant(),
        (_, "Metadata") => metadata(status.as_ref()),
        (_, "Volume") => status.map_or(1.0, |s| s.volume).to_variant(),
        (_, "Position") => status.map_or(0, |s| (s.position * 1e6) as i64).to_variant(),
        _ => true.to_variant(),
    }
}

fn playback_status(status: Option<&crate::Status>) -> &'static str {
    if status.is_some_and(|s| s.playing) {
        "Playing"
    } else {
        "Paused"
    }
}

fn metadata(status: Option<&crate::Status>) -> Variant {
    let dict = VariantDict::new(None);
    let track_id = glib::variant::ObjectPath::try_from(TRACK_ID).expect("valid object path");
    dict.insert_value("mpris:trackid", &track_id.to_variant());
    TRACK.with(|t| {
        if let Some(t) = &*t.borrow() {
            dict.insert("xesam:title", t.title.as_str());
            dict.insert("xesam:url", t.url.as_str());
            if let Some(art) = &t.art {
                dict.insert("mpris:artUrl", art.as_str());
            }
        }
    });
    if let Some(len) = status.and_then(|s| s.duration) {
        dict.insert("mpris:length", (len * 1e6) as i64);
    }
    dict.end()
}

/// Announce new play state, volume and metadata to MPRIS clients.
pub fn changed() {
    let Some(conn) = CONN.with(|c| c.borrow().clone()) else { return };
    let status = crate::status();
    let props = VariantDict::new(None);
    props.insert("PlaybackStatus", playback_status(status.as_ref()));
    props.insert("Volume", status.as_ref().map_or(1.0, |s| s.volume));
    props.insert_value("Metadata", &metadata(status.as_ref()));
    let args = Variant::tuple_from_iter([
        PLAYER.to_variant(),
        props.end(),
        Vec::<String>::new().to_variant(),
    ]);
    let _ = conn.emit_signal(None, PATH, "org.freedesktop.DBus.Properties", "PropertiesChanged", Some(&args));
}

pub fn seeked(secs: f64) {
    let Some(conn) = CONN.with(|c| c.borrow().clone()) else { return };
    let args = ((secs.max(0.0) * 1e6) as i64,).to_variant();
    let _ = conn.emit_signal(None, PATH, PLAYER, "Seeked", Some(&args));
}
