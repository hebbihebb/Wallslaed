//! Music videos: a separate folder of videos that play with sound, outside the
//! shuffle bag, and downloading them with yt-dlp.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use gio::prelude::*;
use serde::{Deserialize, Serialize};

use crate::library::{self, Kind};

/// Thumbnails yt-dlp writes next to the video.
const THUMB_EXTS: &[&str] = &["jpg", "webp", "png"];

/// What's playing and where each video was left off. Saved separately from
/// the bag so the bag's format doesn't change.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct MediaState {
    pub active: Option<String>,
    #[serde(default)]
    pub positions: HashMap<String, f64>,
}

impl MediaState {
    pub fn load(path: &Path) -> MediaState {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) {
        let Ok(json) = serde_json::to_vec(self) else { return };
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

pub struct Item {
    pub path: String,
    pub title: String,
    pub thumbnail: Option<PathBuf>,
}

pub fn folder(settings: &gio::Settings) -> PathBuf {
    let f = settings.string("music-folder");
    if f.is_empty() {
        glib::user_special_dir(glib::UserDirectory::Videos)
            .unwrap_or_else(|| glib::home_dir().join("Videos"))
            .join("Music")
    } else if let Some(rest) = f.strip_prefix("~/") {
        glib::home_dir().join(rest)
    } else {
        PathBuf::from(f.as_str())
    }
}

/// Videos in the music folder, sorted by title.
pub fn list(folder: &Path) -> Vec<Item> {
    let scan = library::scan(folder, true);
    let mut items: Vec<Item> = scan
        .files
        .into_iter()
        .filter(|p| library::kind_of(Path::new(p)) == Some(Kind::Video))
        .map(|path| Item {
            title: title_of(Path::new(&path)),
            thumbnail: thumbnail_of(Path::new(&path)),
            path,
        })
        .collect();
    items.sort_by_cached_key(|i| i.title.to_lowercase());
    items
}

pub fn is_music_video(folder: &Path, path: &Path) -> bool {
    path.starts_with(folder) && library::kind_of(path) == Some(Kind::Video) && path.is_file()
}

/// Display title: the file name without yt-dlp's trailing " [videoid]".
pub fn title_of(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    match stem.rsplit_once(" [") {
        Some((title, id)) if id.ends_with(']') && !title.is_empty() => title.to_owned(),
        _ => stem.to_owned(),
    }
}

pub fn thumbnail_of(path: &Path) -> Option<PathBuf> {
    THUMB_EXTS
        .iter()
        .map(|ext| path.with_extension(ext))
        .find(|p| p.is_file())
}

pub fn valid_url(url: &str) -> bool {
    let url = url.trim();
    (url.starts_with("https://") || url.starts_with("http://")) && !url.contains(char::is_whitespace)
}

fn yt_dlp() -> Option<PathBuf> {
    let local = glib::home_dir().join(".local/bin/yt-dlp");
    if local.is_file() {
        return Some(local);
    }
    glib::find_program_in_path("yt-dlp")
}

/// The full command line for one download. Intermediate files go to `temp`, so
/// half-finished downloads never show up in the music folder.
pub fn command(url: &str, folder: &Path, temp: &Path, max_height: u32) -> Option<Vec<OsString>> {
    let yt_dlp = yt_dlp()?;
    let h = max_height.max(144);
    let mut argv: Vec<OsString> = Vec::new();
    // Run in its own scope: the daemon's MemoryHigh would otherwise count the
    // page cache of a multi-gigabyte download against the daemon.
    let manager = glib::user_runtime_dir().join("systemd/private").exists();
    if let Some(run) = glib::find_program_in_path("systemd-run").filter(|_| manager) {
        argv.extend([run.into(), "--user".into(), "--scope".into(), "--quiet".into(), "--".into()]);
    }
    argv.extend(["nice".into(), "-n".into(), "19".into(), yt_dlp.into()]);
    // YouTube needs a JavaScript runtime, and yt-dlp only looks for deno by
    // default. Fall back to node when that's what is installed.
    if glib::find_program_in_path("deno").is_none() && glib::find_program_in_path("node").is_some() {
        argv.extend(["--js-runtimes".into(), "node".into()]);
    }
    for a in [
        "--no-playlist",
        "--newline",
        "--no-colors",
        "--no-simulate",
        "-f",
        &format!("bv*[height<={h}]+ba/b[height<={h}]/b"),
        "--merge-output-format",
        "mkv",
        "--write-thumbnail",
        "--convert-thumbnails",
        "jpg",
        "--progress-template",
        "download:wsl-progress %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s",
        "--print",
        "before_dl:wsl-title %(title)s",
        "--print",
        "after_move:wsl-file %(filepath)s",
        "-o",
        "%(title)s [%(id)s].%(ext)s",
    ] {
        argv.push(a.into());
    }
    argv.push("-P".into());
    argv.push(folder.into());
    let mut temp_arg = OsString::from("temp:");
    temp_arg.push(temp);
    argv.push("-P".into());
    argv.push(temp_arg);
    argv.push("--".into());
    argv.push(url.trim().into());
    Some(argv)
}

#[derive(Debug, PartialEq)]
pub enum Event {
    Title(String),
    /// Bytes done and expected for the stream being downloaded. Video and
    /// audio arrive as separate streams, one after the other.
    Progress(u64, Option<u64>),
    /// Merging, converting the thumbnail, moving into place.
    Processing,
    File(String),
    Error(String),
}

pub fn parse_line(line: &str) -> Option<Event> {
    let line = line.trim_end();
    if let Some(rest) = line.strip_prefix("wsl-title ") {
        return Some(Event::Title(rest.to_owned()));
    }
    if let Some(rest) = line.strip_prefix("wsl-file ") {
        return Some(Event::File(rest.to_owned()));
    }
    if let Some(rest) = line.strip_prefix("wsl-progress ") {
        let num = |s: Option<&str>| s.and_then(|s| s.parse::<f64>().ok()).map(|n| n as u64);
        let mut it = rest.split(' ');
        let done = num(it.next())?;
        let total = num(it.next()).or(num(it.next()));
        return Some(Event::Progress(done, total));
    }
    if let Some(rest) = line.strip_prefix("ERROR: ") {
        return Some(Event::Error(rest.to_owned()));
    }
    if ["[Merger]", "[ThumbnailsConvertor]", "[MoveFiles]", "[FixupM3u8]"]
        .iter()
        .any(|p| line.starts_with(p))
    {
        return Some(Event::Processing);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_progress() {
        assert_eq!(
            parse_line("wsl-progress 1024 4096 NA"),
            Some(Event::Progress(1024, Some(4096)))
        );
        // Only an estimate while a fragmented stream is still starting.
        assert_eq!(
            parse_line("wsl-progress 10 NA 2000.5"),
            Some(Event::Progress(10, Some(2000)))
        );
        assert_eq!(parse_line("wsl-progress 10 NA NA"), Some(Event::Progress(10, None)));
        assert_eq!(parse_line("wsl-progress NA NA NA"), None);
    }

    #[test]
    fn parses_markers() {
        assert_eq!(
            parse_line("wsl-title Chill mix [2 hours]"),
            Some(Event::Title("Chill mix [2 hours]".into()))
        );
        assert_eq!(
            parse_line("wsl-file /m/a [x].mkv\n"),
            Some(Event::File("/m/a [x].mkv".into()))
        );
        assert_eq!(
            parse_line("[Merger] Merging formats into \"a.mkv\""),
            Some(Event::Processing)
        );
        assert_eq!(
            parse_line("ERROR: [youtube] abc: Video unavailable"),
            Some(Event::Error("[youtube] abc: Video unavailable".into()))
        );
        assert_eq!(parse_line("[youtube] Extracting URL"), None);
    }

    #[test]
    fn titles_drop_video_id() {
        assert_eq!(title_of(Path::new("/m/Chill Mix [Td0iFptEmo0].mkv")), "Chill Mix");
        assert_eq!(title_of(Path::new("/m/[intro].mkv")), "[intro]");
        assert_eq!(title_of(Path::new("/m/plain.mp4")), "plain");
    }

    #[test]
    fn urls() {
        assert!(valid_url("https://youtu.be/Td0iFptEmo0?list=x"));
        assert!(!valid_url("file:///etc/passwd"));
        assert!(!valid_url("--exec=rm"));
        assert!(!valid_url("https://a b"));
    }

    #[test]
    fn state_round_trip() {
        let dir = std::env::temp_dir().join(format!("wallslaed-media-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("media.json");
        let mut s = MediaState { active: Some("/m/a.mkv".into()), ..Default::default() };
        s.positions.insert("/m/a.mkv".into(), 123.5);
        s.save(&path);
        let back = MediaState::load(&path);
        assert_eq!(back.active.as_deref(), Some("/m/a.mkv"));
        assert_eq!(back.positions["/m/a.mkv"], 123.5);
        assert!(MediaState::load(&dir.join("missing.json")).active.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
