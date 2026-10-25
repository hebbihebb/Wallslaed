//! The handful of GStreamer calls the player needs, declared by hand so the
//! build doesn't require GStreamer development headers. Properties go through
//! GObject (via glib-rs), which needs no GStreamer-specific bindings.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr;

use gtk::glib::translate::{from_glib_full, from_glib_none, ToGlibPtr};
use gtk::prelude::*;
use gtk::{gdk, glib};

const STATE_NULL: c_int = 1;
const STATE_PAUSED: c_int = 3;
const STATE_PLAYING: c_int = 4;
const MESSAGE_EOS: c_int = 1 << 0;
const MESSAGE_ERROR: c_int = 1 << 1;
const MESSAGE_ASYNC_DONE: c_int = 1 << 21;
const FORMAT_TIME: c_int = 3;
const SEEK_FLUSH: c_int = 1 << 0;
const SEEK_ACCURATE: c_int = 1 << 1;
const SEEK_KEY_UNIT: c_int = 1 << 2;
const CLOCK_TIME_NONE: u64 = u64::MAX;

extern "C" {
    fn gst_init_check(argc: *mut c_int, argv: *mut *mut *mut c_char, error: *mut *mut glib::ffi::GError) -> c_int;
    fn gst_element_factory_make(factory: *const c_char, name: *const c_char) -> *mut c_void;
    fn gst_element_set_state(element: *mut c_void, state: c_int) -> c_int;
    fn gst_element_get_bus(element: *mut c_void) -> *mut c_void;
    fn gst_bus_timed_pop_filtered(bus: *mut c_void, timeout: u64, types: c_int) -> *mut c_void;
    fn gst_message_get_structure(message: *mut c_void) -> *const c_void;
    fn gst_mini_object_unref(object: *mut c_void);
    fn gst_object_unref(object: *mut c_void);
    fn gst_element_seek_simple(element: *mut c_void, format: c_int, flags: c_int, pos: i64) -> c_int;
    fn gst_element_query_position(element: *mut c_void, format: c_int, cur: *mut i64) -> c_int;
    fn gst_element_query_duration(element: *mut c_void, format: c_int, dur: *mut i64) -> c_int;
    fn gst_structure_get_name(structure: *const c_void) -> *const c_char;
}

pub struct Player {
    pipeline: glib::Object,
}

struct SendPtr(*mut c_void);
// GstBus and GstElement are thread-safe; we only hand them to the bus thread.
unsafe impl Send for SendPtr {}

fn make(factory: &str) -> Option<glib::Object> {
    let name = CString::new(factory).ok()?;
    let ptr = unsafe { gst_element_factory_make(name.as_ptr(), ptr::null()) };
    // from_glib_none sinks the floating reference.
    (!ptr.is_null()).then(|| unsafe { from_glib_none(ptr as *mut glib::gobject_ffi::GObject) })
}

impl Player {
    /// `audio` turns sound on (music videos); `start` is where to begin, in seconds.
    pub fn new(uri: &str, audio: bool, start: f64) -> Result<(Player, gdk::Paintable), String> {
        unsafe {
            let mut err = ptr::null_mut();
            if gst_init_check(ptr::null_mut(), ptr::null_mut(), &mut err) == 0 {
                let e: glib::Error = from_glib_full(err);
                return Err(format!("GStreamer init failed: {e}"));
            }
        }
        let sink = make("gtk4paintablesink").ok_or("gtk4paintablesink not installed")?;
        let pipeline = make("playbin3")
            .or_else(|| make("playbin"))
            .ok_or("playbin not available")?;

        // Video only unless asked for sound, and no CPU conversion elements
        // between decoder and sink.
        let flags_type = pipeline.property_type("flags").ok_or("playbin has no flags")?;
        let flags = glib::FlagsClass::with_type(flags_type)
            .and_then(|c| {
                let b = c.builder().set_by_nick("video").set_by_nick("native-video");
                if audio {
                    b.set_by_nick("audio").set_by_nick("soft-volume").build()
                } else {
                    b.build()
                }
            })
            .ok_or("unexpected playbin flags")?;
        pipeline.set_property_from_value("flags", &flags);
        pipeline.set_property("uri", uri);
        pipeline.set_property("video-sink", &sink);
        let paintable = sink.property::<gdk::Paintable>("paintable");

        // Loop on end-of-stream; exit on errors so the extension falls back to
        // the still frame. The start position can only be sought once the
        // pipeline has prerolled, which the first async-done message signals.
        let element = SendPtr(pipeline.as_ptr() as *mut c_void);
        let bus = SendPtr(unsafe { gst_element_get_bus(element.0) });
        let mut start = (start > 0.0).then(|| (start * 1e9) as i64);
        std::thread::spawn(move || {
            let (element, bus) = (element, bus);
            let types = MESSAGE_EOS | MESSAGE_ERROR | MESSAGE_ASYNC_DONE;
            loop {
                let msg = unsafe { gst_bus_timed_pop_filtered(bus.0, CLOCK_TIME_NONE, types) };
                if msg.is_null() {
                    break;
                }
                // EOS carries no structure; the others are told apart by name.
                let name = unsafe {
                    let st = gst_message_get_structure(msg);
                    (!st.is_null()).then(|| CStr::from_ptr(gst_structure_get_name(st)).to_bytes().to_vec())
                };
                unsafe { gst_mini_object_unref(msg) };
                let pos = match name.as_deref() {
                    None => 0,
                    Some(b"GstMessageAsyncDone") => match start.take() {
                        Some(pos) => pos,
                        None => continue,
                    },
                    Some(_) => {
                        eprintln!("wallslaed-player: playback error");
                        std::process::exit(1);
                    }
                };
                unsafe { gst_element_seek_simple(element.0, FORMAT_TIME, SEEK_FLUSH | SEEK_KEY_UNIT, pos) };
            }
            unsafe { gst_object_unref(bus.0) };
        });

        Ok((Player { pipeline }, paintable))
    }

    pub fn set_playing(&self, playing: bool) {
        let state = if playing { STATE_PLAYING } else { STATE_PAUSED };
        unsafe { gst_element_set_state(self.as_ptr(), state) };
    }

    pub fn set_muted(&self, muted: bool) {
        self.pipeline.set_property("mute", muted);
    }

    pub fn set_volume(&self, volume: f64) {
        self.pipeline.set_property("volume", volume.clamp(0.0, 1.0));
    }

    /// Current position in seconds.
    pub fn position(&self) -> Option<f64> {
        let mut ns = -1;
        let ok = unsafe { gst_element_query_position(self.as_ptr(), FORMAT_TIME, &mut ns) } != 0;
        (ok && ns >= 0).then(|| ns as f64 / 1e9)
    }

    /// Length in seconds.
    pub fn duration(&self) -> Option<f64> {
        let mut ns = -1;
        let ok = unsafe { gst_element_query_duration(self.as_ptr(), FORMAT_TIME, &mut ns) } != 0;
        (ok && ns >= 0).then(|| ns as f64 / 1e9)
    }

    /// Seek exactly where asked; resuming and looping snap to a keyframe instead.
    pub fn seek(&self, secs: f64) {
        let ns = (secs.max(0.0) * 1e9) as i64;
        unsafe { gst_element_seek_simple(self.as_ptr(), FORMAT_TIME, SEEK_FLUSH | SEEK_ACCURATE, ns) };
    }

    fn as_ptr(&self) -> *mut c_void {
        let p: *mut glib::gobject_ffi::GObject = self.pipeline.to_glib_none().0;
        p as *mut c_void
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        unsafe { gst_element_set_state(self.as_ptr(), STATE_NULL) };
    }
}
