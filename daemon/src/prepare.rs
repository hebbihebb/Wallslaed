//! Turning a wallpaper file into something cheap for the compositor to show.
//! Runs on a worker thread; never touches GLib objects.

use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::SystemTime;

use fast_image_resize::{FilterType, ResizeAlg, ResizeOptions, Resizer};
use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ImageDecoder, ImageReader};

use crate::library::{self, Kind};

/// Number of generated files to keep in the cache directory.
const CACHE_KEEP: usize = 40;
/// Don't bother scaling images that are at most this much bigger than the screen.
const SCALE_SLACK: f64 = 1.10;

#[derive(Debug, Clone)]
pub struct Job {
    pub source: String,
    pub screen: (u32, u32),
    pub prescale: bool,
    pub cache_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Prepared {
    pub source: String,
    /// File to hand to GNOME as the background, if any.
    pub picture: Option<PathBuf>,
    /// For animated wallpapers: the file the player should loop.
    pub playable: Option<PathBuf>,
}

static TRANSCODING: AtomicBool = AtomicBool::new(false);
/// One decode at a time keeps peak memory to a single image.
static JOB_LOCK: Mutex<()> = Mutex::new(());

pub fn prepare(job: &Job) -> Result<Prepared, String> {
    let _guard = JOB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let src = Path::new(&job.source);
    let kind = library::kind_of(src).ok_or("unsupported file")?;
    let meta = std::fs::metadata(src).map_err(|e| e.to_string())?;
    let key = cache_key(&job.source, &meta, job.screen);
    std::fs::create_dir_all(&job.cache_dir).map_err(|e| e.to_string())?;

    let result = match kind {
        Kind::Gif if library::gif_is_animated(src) => prepare_gif(job, &key),
        Kind::Static | Kind::Gif => prepare_static(job, &key),
        Kind::Video => Ok(Prepared {
            source: job.source.clone(),
            picture: poster(job, &key),
            playable: Some(src.to_path_buf()),
        }),
    };
    trim_cache(&job.cache_dir);
    // Decoding big images grows the heap; give it back to the OS.
    unsafe {
        libc::malloc_trim(0);
    }
    result
}

fn prepare_static(job: &Job, key: &str) -> Result<Prepared, String> {
    let src = Path::new(&job.source);
    let done = |picture: PathBuf| Prepared {
        source: job.source.clone(),
        picture: Some(picture),
        playable: None,
    };
    if !job.prescale {
        return Ok(done(src.to_path_buf()));
    }

    let out = job.cache_dir.join(format!("{key}.jpg"));
    if out.exists() {
        touch(&out);
        return Ok(done(out));
    }

    let (sw, sh) = (job.screen.0.max(1) as f64, job.screen.1.max(1) as f64);
    let Ok((w, h)) = reader(src).and_then(|r| r.into_dimensions().map_err(|e| e.to_string()))
    else {
        // Formats we can't read (AVIF, HEIC, JXL...) go to GNOME untouched.
        return Ok(done(src.to_path_buf()));
    };
    // Smallest size that still covers the screen ("zoom" in GNOME terms).
    let scale = (sw / w as f64).max(sh / h as f64);
    if scale * SCALE_SLACK >= 1.0 {
        return Ok(done(src.to_path_buf()));
    }

    let mut decoder = reader(src)?.into_decoder().map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().map_err(|e| e.to_string())?;
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    img.apply_orientation(orientation);
    let img = DynamicImage::ImageRgb8(img.into_rgb8());

    let s = (sw / img.width() as f64).max(sh / img.height() as f64);
    let tw = ((img.width() as f64 * s).round() as u32).max(1);
    let th = ((img.height() as f64 * s).round() as u32).max(1);
    let mut dst = DynamicImage::new_rgb8(tw, th);
    Resizer::new()
        .resize(
            &img,
            &mut dst,
            &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3)),
        )
        .map_err(|e| e.to_string())?;
    drop(img);

    let tmp = out.with_extension("jpg.part");
    {
        let f = File::create(&tmp).map_err(|e| e.to_string())?;
        let mut enc = JpegEncoder::new_with_quality(BufWriter::new(f), 92);
        enc.encode_image(&dst).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, &out).map_err(|e| e.to_string())?;
    Ok(done(out))
}

fn prepare_gif(job: &Job, key: &str) -> Result<Prepared, String> {
    let src = Path::new(&job.source);
    let webm = job.cache_dir.join(format!("{key}.webm"));
    let playable = if webm.exists() {
        touch(&webm);
        webm
    } else {
        // GIF decoding is CPU-only and runs every frame. Convert to VP9 once in
        // the background and play the GIF directly until then.
        transcode_gif_later(src.to_path_buf(), webm, job.screen.0);
        src.to_path_buf()
    };
    Ok(Prepared {
        source: job.source.clone(),
        picture: poster(job, key),
        playable: Some(playable),
    })
}

/// A still frame for the overview, lock screen, and while paused.
fn poster(job: &Job, key: &str) -> Option<PathBuf> {
    let out = job.cache_dir.join(format!("{key}-poster.jpg"));
    if out.exists() {
        touch(&out);
        return Some(out);
    }
    let tmp = job.cache_dir.join(format!("{key}-poster.part.jpg"));
    let (w, h) = job.screen;
    let ok = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-i"])
        .arg(&job.source)
        .args([
            "-frames:v",
            "1",
            "-vf",
            &format!("scale={w}:{h}:force_original_aspect_ratio=increase"),
            "-q:v",
            "3",
        ])
        .arg(&tmp)
        .stdin(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if ok && std::fs::rename(&tmp, &out).is_ok() {
        Some(out)
    } else {
        let _ = std::fs::remove_file(&tmp);
        None
    }
}

fn transcode_gif_later(src: PathBuf, out: PathBuf, max_width: u32) {
    if TRANSCODING.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(move || {
        let tmp = out.with_extension("part.webm");
        let filter = format!("scale='trunc(min(iw\\,{max_width})/2)*2':-2:flags=lanczos");
        let ok = Command::new("nice")
            .args(["-n", "19", "ffmpeg", "-nostdin", "-v", "error", "-y", "-i"])
            .arg(&src)
            .args([
                "-vf", &filter, "-c:v", "libvpx-vp9", "-crf", "34", "-b:v", "0", "-row-mt", "1",
                "-deadline", "good", "-cpu-used", "5", "-pix_fmt", "yuv420p", "-an",
            ])
            .arg(&tmp)
            .stdin(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !(ok && std::fs::rename(&tmp, &out).is_ok()) {
            let _ = std::fs::remove_file(&tmp);
        }
        TRANSCODING.store(false, Ordering::Release);
    });
}

fn reader(src: &Path) -> Result<ImageReader<std::io::BufReader<File>>, String> {
    ImageReader::open(src)
        .and_then(|r| r.with_guessed_format())
        .map_err(|e| e.to_string())
}

fn cache_key(source: &str, meta: &std::fs::Metadata, screen: (u32, u32)) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut h);
    meta.len().hash(&mut h);
    meta.modified().ok().hash(&mut h);
    format!("{:016x}-{}x{}", h.finish(), screen.0, screen.1)
}

fn touch(path: &Path) {
    if let Ok(f) = File::options().append(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

fn trim_cache(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            let name = e.file_name();
            let name = name.to_str()?;
            // Leave in-progress transcodes alone.
            if !m.is_file() || name.contains(".part") {
                return None;
            }
            Some((m.modified().ok()?, e.path()))
        })
        .collect();
    if files.len() <= CACHE_KEEP {
        return;
    }
    files.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in files.drain(CACHE_KEEP..) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_large_images_and_passes_small_ones() {
        let dir = std::env::temp_dir().join(format!("wallslaed-prep-{}", std::process::id()));
        let cache = dir.join("cache");
        std::fs::create_dir_all(&dir).unwrap();
        let big = dir.join("big.png");
        let small = dir.join("small.png");
        image::RgbImage::new(4000, 2000).save(&big).unwrap();
        image::RgbImage::new(1000, 500).save(&small).unwrap();

        let job = |p: &Path| Job {
            source: p.to_str().unwrap().into(),
            screen: (1920, 1080),
            prescale: true,
            cache_dir: cache.clone(),
        };
        let out = prepare(&job(&big)).unwrap();
        let pic = out.picture.unwrap();
        assert!(pic.starts_with(&cache));
        let (w, h) = image::image_dimensions(&pic).unwrap();
        assert_eq!((w, h), (2160, 1080), "covers the screen, keeps aspect");
        assert!(out.playable.is_none());

        let out = prepare(&job(&small)).unwrap();
        assert_eq!(out.picture.unwrap(), small, "small images are used as-is");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
