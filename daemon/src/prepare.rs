//! Turning a wallpaper file into something cheap for the compositor to show.
//! Runs on a worker thread; the only GLib objects it touches are glycin's own,
//! on a main context of their own.

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
    /// Also bake a blurred copy for the extension's blur, if set.
    pub blur: Option<Blur>,
}

/// How the extension wants its blurred wallpaper.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Blur {
    /// Gaussian sigma, in pixels at screen size.
    pub strength: u32,
    /// 0 is black, 1 leaves the colours alone.
    pub brightness: f64,
}

#[derive(Debug, Clone)]
pub struct Prepared {
    pub source: String,
    /// File to hand to GNOME as the background, if any.
    pub picture: Option<PathBuf>,
    /// For animated wallpapers: the file the player should loop.
    pub playable: Option<PathBuf>,
    /// A small, blurred and darkened copy of `picture`, if one was asked for
    /// and the picture could be read.
    pub blurred: Option<PathBuf>,
    /// The settings `blurred` was asked for with, even if baking failed.
    pub baked_with: Option<Blur>,
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
            blurred: None,
        baked_with: None,
        }),
    };
    let result = result.map(|mut p| {
        if let (Some(blur), Some(picture)) = (job.blur, &p.picture) {
            p.blurred = bake_blur(picture, &job.cache_dir, blur);
        }
        p.baked_with = job.blur;
        p
    });
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
        blurred: None,
        baked_with: None,
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
    // Smallest size that still covers the screen ("zoom" in GNOME terms).
    let cover = |w: u32, h: u32| (sw / w as f64).max(sh / h as f64);
    let img = match reader(src).and_then(|r| r.into_dimensions().map_err(|e| e.to_string())) {
        Ok((w, h)) => {
            if cover(w, h) * SCALE_SLACK >= 1.0 {
                return Ok(done(src.to_path_buf()));
            }
            let mut decoder = reader(src)?.into_decoder().map_err(|e| e.to_string())?;
            let orientation = decoder.orientation().map_err(|e| e.to_string())?;
            let mut img = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
            img.apply_orientation(orientation);
            DynamicImage::ImageRgb8(img.into_rgb8())
        }
        // JXL, AVIF, HEIC...: GNOME would decode these itself, which costs
        // GNOME Shell far more than a JPEG, so they become one at any size.
        Err(_) => match decode_glycin(src) {
            Ok(img) => img,
            Err(e) => {
                eprintln!("wallslaed: can't decode {}: {e}", src.display());
                return Ok(done(src.to_path_buf()));
            }
        },
    };

    let s = cover(img.width(), img.height()).min(1.0);
    let tw = ((img.width() as f64 * s).round() as u32).max(1);
    let th = ((img.height() as f64 * s).round() as u32).max(1);
    let dst = if (tw, th) == (img.width(), img.height()) {
        img
    } else {
        let mut dst = DynamicImage::new_rgb8(tw, th);
        Resizer::new()
            .resize(
                &img,
                &mut dst,
                &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3)),
            )
            .map_err(|e| e.to_string())?;
        dst
    };

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
        blurred: None,
        baked_with: None,
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

/// Bake the blur for `picture`; see [`bake_blur`]. Takes the decode lock, for
/// callers outside [`prepare`].
pub fn blur_only(picture: &Path, cache_dir: &Path, blur: Blur) -> Option<PathBuf> {
    let _guard = JOB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::fs::create_dir_all(cache_dir).ok()?;
    let out = bake_blur(picture, cache_dir, blur);
    trim_cache(cache_dir);
    unsafe {
        libc::malloc_trim(0);
    }
    out
}

/// Write a blurred, darkened copy of `picture` at a fraction of its size, so
/// GNOME Shell can show blur by scaling up a tiny texture instead of running a
/// blur shader over a full-screen copy. A blurred image has no fine detail, so
/// nothing is lost. The aspect ratio is kept: the Shell crops it like the
/// wallpaper. PNG, because JPEG blocks would show once scaled up.
fn bake_blur(picture: &Path, cache_dir: &Path, blur: Blur) -> Option<PathBuf> {
    let meta = std::fs::metadata(picture).ok()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    picture.hash(&mut h);
    meta.len().hash(&mut h);
    meta.modified().ok().hash(&mut h);
    let brightness = blur.brightness.clamp(0.0, 1.0);
    let out = cache_dir.join(format!(
        "{:016x}-blur-{}-{}.png",
        h.finish(),
        blur.strength,
        (brightness * 100.0).round() as u32
    ));
    if out.exists() {
        touch(&out);
        return Some(out);
    }

    // Shrink so the blur is about 4 pixels wide at the small size: smooth once
    // scaled up, and as little work as possible.
    let factor = (blur.strength as f64 / 4.0).clamp(1.0, 8.0);
    let mut decoder = reader(picture).ok()?.into_decoder().ok()?;
    let orientation = decoder.orientation().ok()?;
    let mut img = DynamicImage::from_decoder(decoder).ok()?;
    img.apply_orientation(orientation);
    let img = DynamicImage::ImageRgb8(img.into_rgb8());
    let tw = ((img.width() as f64 / factor).round() as u32).max(1);
    let th = ((img.height() as f64 / factor).round() as u32).max(1);
    let mut small = DynamicImage::new_rgb8(tw, th);
    Resizer::new()
        .resize(
            &img,
            &mut small,
            &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear)),
        )
        .ok()?;
    drop(img);

    let mut small = small.into_rgb8();
    if blur.strength > 0 {
        small = image::imageops::fast_blur(&small, (blur.strength as f64 / factor) as f32);
    }
    if brightness < 1.0 {
        for px in small.pixels_mut() {
            for c in &mut px.0 {
                *c = (*c as f64 * brightness).round() as u8;
            }
        }
    }

    let tmp = out.with_extension("png.part");
    if small.save_with_format(&tmp, image::ImageFormat::Png).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    std::fs::rename(&tmp, &out).ok()?;
    Some(out)
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

/// Decodes with glycin, the sandboxed loaders GNOME itself uses, into RGB with
/// the orientation and colour profile applied.
fn decode_glycin(src: &Path) -> Result<DynamicImage, String> {
    let mut loader = glycin::Loader::new(gio::File::for_path(src));
    loader
        .accepted_memory_formats(glycin::MemoryFormatSelection::R8g8b8)
        // Not the daemon's main loop: this thread blocks until it's done.
        .main_context_selector(glycin::MainContextSelector::Managed);
    let frame = glib::MainContext::new().block_on(async move {
        let mut image = loader.load().await.map_err(|e| e.to_string())?;
        image.next_frame().await.map_err(|e| e.to_string())
    })?;
    if frame.memory_format() != glycin::MemoryFormat::R8g8b8 {
        return Err(format!("unexpected memory format {:?}", frame.memory_format()));
    }
    let (w, h, stride) = (frame.width(), frame.height(), frame.stride() as usize);
    let row = w as usize * 3;
    let buf = frame.buf_slice();
    let mut rgb = Vec::with_capacity(row * h as usize);
    for y in 0..h as usize {
        rgb.extend_from_slice(buf.get(y * stride..y * stride + row).ok_or("short frame")?);
    }
    image::RgbImage::from_raw(w, h, rgb)
        .map(DynamicImage::ImageRgb8)
        .ok_or_else(|| "bad frame size".to_owned())
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
            blur: None,
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

    #[test]
    fn bakes_a_small_dark_blurred_copy() {
        let dir = std::env::temp_dir().join(format!("wallslaed-blur-{}", std::process::id()));
        let cache = dir.join("cache");
        std::fs::create_dir_all(&dir).unwrap();
        // White on the left, black on the right: blurring must soften the edge.
        let src = dir.join("half.png");
        image::RgbImage::from_fn(1920, 1080, |x, _| {
            if x < 960 { image::Rgb([255, 255, 255]) } else { image::Rgb([0, 0, 0]) }
        })
        .save(&src)
        .unwrap();

        let job = Job {
            source: src.to_str().unwrap().into(),
            screen: (1920, 1080),
            prescale: true,
            cache_dir: cache.clone(),
            blur: Some(Blur { strength: 32, brightness: 0.5 }),
        };
        let out = prepare(&job).unwrap();
        let blurred = out.blurred.expect("a blurred copy");
        assert!(blurred.starts_with(&cache));
        let img = image::open(&blurred).unwrap().into_rgb8();
        assert_eq!(img.dimensions(), (240, 135), "an eighth of the size, same aspect");
        // Darkened: far-left white becomes about half.
        let left = img.get_pixel(2, 67).0[0];
        assert!((120..=135).contains(&left), "brightness applied, got {left}");
        // Blurred: the middle is grey, not a hard edge.
        let mid = img.get_pixel(120, 67).0[0];
        assert!((40..=90).contains(&mid), "edge softened, got {mid}");
        let near = img.get_pixel(116, 67).0[0];
        assert!(near > mid && near < left, "a gradient across the edge");

        // Asking again reuses the file.
        let again = blur_only(out.picture.as_deref().unwrap(), &cache, job.blur.unwrap());
        assert_eq!(again.as_deref(), Some(blurred.as_path()));

        // No blur asked for, none made.
        let plain = prepare(&Job { blur: None, ..job }).unwrap();
        assert!(plain.blurred.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // Needs ffmpeg with libjxl to make the files, and glycin's JXL loader.
    #[test]
    fn converts_jxl_to_jpeg() {
        let dir = std::env::temp_dir().join(format!("wallslaed-jxl-{}", std::process::id()));
        let cache = dir.join("cache");
        std::fs::create_dir_all(&dir).unwrap();
        let make = |name: &str, size: &str| {
            let path = dir.join(name);
            let ok = Command::new("ffmpeg")
                .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i"])
                .arg(format!("testsrc2=size={size}"))
                .args(["-frames:v", "1", "-c:v", "libjxl"])
                .arg(&path)
                .status()
                .is_ok_and(|s| s.success());
            ok.then_some(path)
        };
        let (Some(big), Some(small)) = (make("big.jxl", "4000x2000"), make("small.jxl", "1000x500"))
        else {
            eprintln!("skipped: ffmpeg can't write JXL");
            return;
        };
        let job = |p: &Path| Job {
            source: p.to_str().unwrap().into(),
            screen: (1920, 1080),
            prescale: true,
            cache_dir: cache.clone(),
            blur: None,
        };
        for (src, size) in [(&big, (2160, 1080)), (&small, (1000, 500))] {
            let pic = prepare(&job(src)).unwrap().picture.unwrap();
            assert!(pic.starts_with(&cache), "{} becomes a cached JPEG", src.display());
            assert_eq!(image::image_dimensions(&pic).unwrap(), size, "{}", src.display());
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
