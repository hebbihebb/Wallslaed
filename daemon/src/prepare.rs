//! Turning a wallpaper file into something cheap for the compositor to show.
//! Runs on a worker thread; the only GLib objects it touches are glycin's own,
//! on a main context of their own.

use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use fast_image_resize::{FilterType, ResizeAlg, ResizeOptions, Resizer};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{DynamicImage, ImageDecoder, ImageEncoder, ImageReader};

use crate::library::{self, Kind};

/// Number of baked blurs to keep in the cache directory. Generated pictures
/// (conversions, scaled copies, posters, videos) are kept by size instead, see
/// [`Job::cache_bytes`]: a JPEG XL conversion costs about half a second.
const BLUR_KEEP: usize = 40;
/// Unfinished files older than this were abandoned (a failed rename, a crash).
const PART_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(3600);
/// Don't bother scaling images that are at most this much bigger than the screen.
const SCALE_SLACK: f64 = 1.10;
/// Pictures bigger than this aren't blurred here (the extension falls back to
/// a GPU blur): decoding one costs 3 bytes a pixel, ~120 MB at this size.
const BLUR_MAX_SOURCE_PIXELS: u64 = 40_000_000;
/// The blurred copy has at most this many pixels, whatever the strength.
const BLUR_MAX_PIXELS: f64 = 2_000_000.0;
/// ffmpeg reading one frame for a poster: anything longer is stuck (a broken
/// file, a source that stopped answering).
const POSTER_LIMIT: Duration = Duration::from_secs(30);
/// Converting a GIF to video, at the lowest priority.
const TRANSCODE_LIMIT: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone)]
pub struct Job {
    pub source: String,
    pub screen: (u32, u32),
    pub prescale: bool,
    pub cache_dir: PathBuf,
    /// Also bake a blurred copy for the extension's blur, if set.
    pub blur: Option<Blur>,
    /// Files in use (on screen, or prepared next): never trimmed.
    pub keep: Vec<PathBuf>,
    /// How much the generated pictures in the cache may take up, in bytes.
    pub cache_bytes: u64,
}

/// How the extension wants its blurred wallpaper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Blur {
    /// Gaussian sigma, in pixels at screen size.
    pub strength: u32,
    /// Percent: 0 is black, 100 leaves the colours alone. Whole percents, so
    /// settings that would bake the same file compare equal.
    pub brightness: u32,
}

impl Blur {
    pub fn new(strength: u32, brightness: f64) -> Self {
        Blur { strength, brightness: (brightness.clamp(0.0, 1.0) * 100.0).round() as u32 }
    }
}

#[derive(Debug, Clone)]
pub struct Prepared {
    pub source: String,
    /// The source's identity (path, size, mtime) and the screen size; names
    /// every file made from it.
    pub key: String,
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
/// One decode at a time keeps peak memory to a single image. Only decoding
/// takes it: a poster or conversion is ffmpeg's memory, not ours, and a stuck
/// one must not hold up every other wallpaper.
static JOB_LOCK: Mutex<()> = Mutex::new(());

fn decode_lock() -> std::sync::MutexGuard<'static, ()> {
    JOB_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn prepare(job: &Job) -> Result<Prepared, String> {
    let src = Path::new(&job.source);
    let kind = library::kind_of(src).ok_or("unsupported file")?;
    let meta = std::fs::metadata(src).map_err(|e| e.to_string())?;
    let key = cache_key(&job.source, &meta, job.screen);
    std::fs::create_dir_all(&job.cache_dir).map_err(|e| e.to_string())?;

    let result = match kind {
        Kind::Gif if library::gif_is_animated(src) => prepare_gif(job, &key),
        Kind::Static | Kind::Gif => {
            let _guard = decode_lock();
            prepare_static(job, &key).map(|(p, pixels)| {
                // Bake from the pixels just decoded, if any, rather than
                // decoding the picture again.
                if let (Some(blur), Some(img)) = (job.blur, pixels) {
                    return Prepared { blurred: bake_blur_image(&img, &key, &job.cache_dir, blur), ..p };
                }
                p
            })
        }
        // No frame means ffmpeg can't read it: playing it would fail too.
        Kind::Video => match poster(job, &key) {
            Some(picture) => Ok(Prepared {
                source: job.source.clone(),
                picture: Some(picture),
                playable: Some(src.to_path_buf()),
                blurred: None,
                baked_with: None,
                key: String::new(),
            }),
            None => Err("can't read this video".into()),
        },
    };
    let result = result.map(|mut p| {
        p.key = key.clone();
        if let (Some(blur), Some(picture), None) = (job.blur, &p.picture, &p.blurred) {
            let _guard = decode_lock();
            p.blurred = bake_blur(picture, &key, &job.cache_dir, blur);
        }
        p.baked_with = job.blur;
        p
    });
    // What we're about to hand back is in use too, however big it is.
    let mut keep = job.keep.clone();
    if let Ok(p) = &result {
        keep.extend([&p.picture, &p.playable, &p.blurred].into_iter().flatten().cloned());
    }
    trim_cache(&job.cache_dir, &keep, job.cache_bytes);
    // Decoding big images grows the heap; give it back to the OS.
    unsafe {
        libc::malloc_trim(0);
    }
    result
}

/// Also returns the pixels if it decoded the picture, for the blur.
fn prepare_static(job: &Job, key: &str) -> Result<(Prepared, Option<DynamicImage>), String> {
    let src = Path::new(&job.source);
    let done = |picture: PathBuf| Prepared {
        source: job.source.clone(),
        picture: Some(picture),
        playable: None,
        blurred: None,
        baked_with: None,
        key: String::new(),
    };
    if !job.prescale {
        return Ok((done(src.to_path_buf()), None));
    }

    let out = job.cache_dir.join(format!("{key}.jpg"));
    if reusable(&out) {
        touch(&out);
        return Ok((done(out), None));
    }

    let (sw, sh) = (job.screen.0.max(1) as f64, job.screen.1.max(1) as f64);
    // Smallest size that still covers the screen ("zoom" in GNOME terms).
    let cover = |w: u32, h: u32| (sw / w as f64).max(sh / h as f64);
    let img = match reader(src).and_then(|r| r.into_dimensions().map_err(|e| e.to_string())) {
        Ok((w, h)) => {
            if cover(w, h) * SCALE_SLACK >= 1.0 {
                return Ok((done(src.to_path_buf()), None));
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
                return Ok((done(src.to_path_buf()), None));
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

    write_file(&out, "jpg", |w| JpegEncoder::new_with_quality(w, 92).encode_image(&dst))?;
    Ok((done(out), Some(dst)))
}

fn prepare_gif(job: &Job, key: &str) -> Result<Prepared, String> {
    let src = Path::new(&job.source);
    let webm = job.cache_dir.join(format!("{key}.webm"));
    let playable = if reusable(&webm) {
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
        key: String::new(),
    })
}

/// A still frame for the overview, lock screen, and while paused.
fn poster(job: &Job, key: &str) -> Option<PathBuf> {
    let out = job.cache_dir.join(format!("{key}-poster.jpg"));
    if reusable(&out) {
        touch(&out);
        return Some(out);
    }
    let tmp = job.cache_dir.join(format!("{key}-poster.part.jpg"));
    let (w, h) = job.screen;
    let ok = run(
        Command::new("ffmpeg")
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
            .arg(&tmp),
        POSTER_LIMIT,
    );
    if ok && reusable(&tmp) && std::fs::rename(&tmp, &out).is_ok() {
        Some(out)
    } else {
        let _ = std::fs::remove_file(&tmp);
        None
    }
}

/// Bake the blur for a prepared `picture`; see [`bake_blur`]. Takes the decode
/// lock, for callers outside [`prepare`].
pub fn blur_only(
    picture: &Path,
    key: &str,
    cache_dir: &Path,
    blur: Blur,
    keep: &[PathBuf],
    cache_bytes: u64,
) -> Option<PathBuf> {
    let _guard = decode_lock();
    std::fs::create_dir_all(cache_dir).ok()?;
    let out = bake_blur(picture, key, cache_dir, blur);
    let mut keep = keep.to_vec();
    keep.extend(out.clone());
    trim_cache(cache_dir, &keep, cache_bytes);
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
///
/// Named after the source (`key`), not `picture`: touching a cached picture
/// to keep it changes its mtime, not its pixels.
fn bake_blur(picture: &Path, key: &str, cache_dir: &Path, blur: Blur) -> Option<PathBuf> {
    let out = blur_path(key, cache_dir, blur);
    if reusable(&out) {
        touch(&out);
        return Some(out);
    }
    let (w, h) = reader(picture).ok()?.into_dimensions().ok()?;
    if w as u64 * h as u64 > BLUR_MAX_SOURCE_PIXELS {
        return None;
    }
    let mut decoder = reader(picture).ok()?.into_decoder().ok()?;
    let orientation = decoder.orientation().ok()?;
    let mut img = DynamicImage::from_decoder(decoder).ok()?;
    img.apply_orientation(orientation);
    bake_blur_image(&DynamicImage::ImageRgb8(img.into_rgb8()), key, cache_dir, blur)
}

fn blur_path(key: &str, cache_dir: &Path, blur: Blur) -> PathBuf {
    cache_dir.join(format!("{key}-blur-{}-{}.png", blur.strength, blur.brightness))
}

/// [`bake_blur`] from pixels already decoded (and oriented).
fn bake_blur_image(img: &DynamicImage, key: &str, cache_dir: &Path, blur: Blur) -> Option<PathBuf> {
    let out = blur_path(key, cache_dir, blur);
    if reusable(&out) {
        touch(&out);
        return Some(out);
    }
    if img.width() as u64 * img.height() as u64 > BLUR_MAX_SOURCE_PIXELS {
        return None;
    }
    // Shrink so the blur is about 4 pixels wide at the small size: smooth once
    // scaled up, and as little work as possible. However weak the blur, the
    // copy stays small, so memory doesn't depend on the strength.
    let bound = (img.width() as f64 * img.height() as f64 / BLUR_MAX_PIXELS).sqrt();
    let factor = (blur.strength as f64 / 4.0).clamp(1.0, 8.0).max(bound);
    let tw = ((img.width() as f64 / factor).round() as u32).max(1);
    let th = ((img.height() as f64 / factor).round() as u32).max(1);
    let mut small = DynamicImage::new_rgb8(tw, th);
    Resizer::new()
        .resize(
            img,
            &mut small,
            &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear)),
        )
        .ok()?;

    let mut small = small.into_rgb8();
    if blur.strength > 0 {
        small = image::imageops::fast_blur(&small, (blur.strength as f64 / factor) as f32);
    }
    if blur.brightness < 100 {
        let brightness = blur.brightness as f64 / 100.0;
        for px in small.pixels_mut() {
            for c in &mut px.0 {
                *c = (*c as f64 * brightness).round() as u8;
            }
        }
    }

    let (w, h) = small.dimensions();
    write_file(&out, "png", |wr| PngEncoder::new(wr).write_image(small.as_raw(), w, h, image::ExtendedColorType::Rgb8))
        .ok()?;
    Some(out)
}

/// Encode into a temporary file next to `out`, then rename it into place.
/// Every write is checked, the buffer's last one included, so a full disk
/// can't leave a cut-off file under the final name.
fn write_file(
    out: &Path,
    ext: &str,
    encode: impl FnOnce(&mut BufWriter<File>) -> image::ImageResult<()>,
) -> Result<(), String> {
    let tmp = out.with_extension(format!("{ext}.part"));
    let result = (|| {
        let mut w = BufWriter::new(File::create(&tmp).map_err(|e| e.to_string())?);
        encode(&mut w).map_err(|e| e.to_string())?;
        w.flush().map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, out).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// A cached file that looks whole. One that doesn't (cut off by a full disk,
/// emptied) is removed so it gets made again.
fn reusable(path: &Path) -> bool {
    let Ok(mut f) = File::open(path) else { return false };
    let len = f.metadata().map_or(0, |m| m.len());
    // JPEG ends with its end-of-image marker; PNG with its IEND chunk.
    let tail: &[u8] = match path.extension().and_then(|e| e.to_str()) {
        Some("jpg") => &[0xFF, 0xD9],
        Some("png") => &[b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82],
        _ => &[],
    };
    let mut end = vec![0; tail.len()];
    let whole = len > tail.len() as u64
        && f.seek(SeekFrom::End(-(tail.len() as i64))).is_ok()
        && f.read_exact(&mut end).is_ok()
        && end == tail;
    if !whole {
        let _ = std::fs::remove_file(path);
    }
    whole
}

/// Runs `cmd`, killing it if it takes longer than `limit`. True if it succeeded.
fn run(cmd: &mut Command, limit: Duration) -> bool {
    let Ok(mut child) = cmd.stdin(Stdio::null()).spawn() else { return false };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                eprintln!("wallslaed: {:?} took too long; stopped it", cmd.get_program());
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn transcode_gif_later(src: PathBuf, out: PathBuf, max_width: u32) {
    if TRANSCODING.swap(true, Ordering::AcqRel) {
        return;
    }
    /// Lets the next GIF convert however this one ends.
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            TRANSCODING.store(false, Ordering::Release);
        }
    }
    std::thread::spawn(move || {
        let _done = Done;
        let tmp = out.with_extension("part.webm");
        let filter = format!("scale='trunc(min(iw\\,{max_width})/2)*2':-2:flags=lanczos");
        let ok = run(
            Command::new("nice")
                .args(["-n", "19", "ffmpeg", "-nostdin", "-v", "error", "-y", "-i"])
                .arg(&src)
                .args([
                    "-vf", &filter, "-c:v", "libvpx-vp9", "-crf", "34", "-b:v", "0", "-row-mt", "1",
                    "-deadline", "good", "-cpu-used", "5", "-pix_fmt", "yuv420p", "-an",
                ])
                .arg(&tmp),
            TRANSCODE_LIMIT,
        );
        if !(ok && reusable(&tmp) && std::fs::rename(&tmp, &out).is_ok()) {
            let _ = std::fs::remove_file(&tmp);
        }
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

/// Keeps every file in `keep`, the newest BLUR_KEEP baked blurs, and the
/// newest generated pictures that fit in `budget` bytes.
fn trim_cache(dir: &Path, keep: &[PathBuf], budget: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let now = SystemTime::now();
    let mut blurs = Vec::new();
    let mut pictures = Vec::new();
    for e in entries.flatten() {
        let Ok(m) = e.metadata() else { continue };
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(modified) = m.modified() else { continue };
        if !m.is_file() || keep.contains(&e.path()) {
            continue;
        }
        // Leave in-progress transcodes alone; abandoned ones go.
        if name.contains(".part") {
            if now.duration_since(modified).is_ok_and(|age| age > PART_MAX_AGE) {
                let _ = std::fs::remove_file(e.path());
            }
            continue;
        }
        if name.contains("-blur-") {
            blurs.push((modified, e.path()));
        } else {
            pictures.push((modified, m.len(), e.path()));
        }
    }
    blurs.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in blurs.into_iter().skip(BLUR_KEEP) {
        let _ = std::fs::remove_file(path);
    }
    pictures.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    let mut total = 0u64;
    for (_, len, path) in pictures {
        total = total.saturating_add(len);
        if total > budget {
            let _ = std::fs::remove_file(path);
        }
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
            keep: Vec::new(),
            cache_bytes: u64::MAX,
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
            blur: Some(Blur::new(32, 0.5)),
            keep: Vec::new(),
            cache_bytes: u64::MAX,
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
        let again = blur_only(out.picture.as_deref().unwrap(), &out.key, &cache, job.blur.unwrap(), &[], u64::MAX);
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
            keep: Vec::new(),
            cache_bytes: u64::MAX,
        };
        for (src, size) in [(&big, (2160, 1080)), (&small, (1000, 500))] {
            let pic = prepare(&job(src)).unwrap().picture.unwrap();
            assert!(pic.starts_with(&cache), "{} becomes a cached JPEG", src.display());
            assert_eq!(image::image_dimensions(&pic).unwrap(), size, "{}", src.display());
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn scratch(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("wallslaed-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cache = dir.join("cache");
        (dir, cache)
    }

    fn blur_job(src: &Path, cache: &Path, blur: Blur) -> Job {
        Job {
            source: src.to_str().unwrap().into(),
            screen: (1920, 1080),
            prescale: true,
            cache_dir: cache.to_path_buf(),
            blur: Some(blur),
            keep: Vec::new(),
            cache_bytes: u64::MAX,
        }
    }

    // SB-03: a cache hit touches the picture; the blur must not change name.
    #[test]
    fn blur_name_survives_touching_the_picture() {
        let (dir, cache) = scratch("blurkey");
        let src = dir.join("big.png");
        image::RgbImage::new(4000, 2000).save(&src).unwrap();
        let job = blur_job(&src, &cache, Blur::new(30, 0.6));
        let first = prepare(&job).unwrap();
        for _ in 0..45 {
            let again = prepare(&job).unwrap();
            assert_eq!(again.picture, first.picture);
            assert_eq!(again.blurred, first.blurred, "same pixels, same settings, same file");
        }
        let blurs = std::fs::read_dir(&cache).unwrap().flatten()
            .filter(|e| e.file_name().to_str().unwrap().contains("-blur-"))
            .count();
        assert_eq!(blurs, 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // SB-04: rebaking at many strengths must not trim the picture on screen.
    #[test]
    fn trimming_keeps_files_in_use() {
        let (dir, cache) = scratch("keep");
        let src = dir.join("big.png");
        image::RgbImage::new(4000, 2000).save(&src).unwrap();
        let p = prepare(&blur_job(&src, &cache, Blur::new(30, 0.6))).unwrap();
        let picture = p.picture.clone().unwrap();
        for strength in 31..80 {
            let blurred = blur_only(&picture, &p.key, &cache, Blur::new(strength, 0.6), &[picture.clone()], 0);
            assert!(blurred.is_some(), "strength {strength}");
            assert!(picture.exists(), "picture trimmed at strength {strength}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // P-06: generated pictures are kept by size, blurs by count.
    #[test]
    fn trimming_keeps_pictures_within_the_budget() {
        let (dir, cache) = scratch("budget");
        std::fs::create_dir_all(&cache).unwrap();
        let file = |name: &str, len: usize, age: u64| {
            let path = cache.join(name);
            std::fs::write(&path, vec![0u8; len]).unwrap();
            let t = SystemTime::now() - std::time::Duration::from_secs(age);
            File::options().append(true).open(&path).unwrap().set_modified(t).unwrap();
            path
        };
        let newest = file("a-3840x2160.jpg", 400, 10);
        let middle = file("b-3840x2160.jpg", 400, 20);
        let oldest = file("c-3840x2160.jpg", 400, 30);
        let kept = file("d-3840x2160.jpg", 400, 40);
        let blurs: Vec<PathBuf> = (0..BLUR_KEEP + 2).map(|i| file(&format!("e{i}-blur-30-60.png"), 10, i as u64)).collect();
        trim_cache(&cache, &[kept.clone()], 1000);
        assert!(newest.exists() && middle.exists(), "newest within the budget stay");
        assert!(!oldest.exists(), "over the budget goes");
        assert!(kept.exists(), "in use stays whatever the budget");
        assert_eq!(blurs.iter().filter(|p| p.exists()).count(), BLUR_KEEP);
        assert!(blurs[0].exists() && !blurs[BLUR_KEEP + 1].exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // P-06: a picture decoded for scaling bakes its blur from those pixels,
    // the same as from the written file.
    #[test]
    fn blur_from_decoded_pixels_matches() {
        let (dir, cache) = scratch("pixels");
        let src = dir.join("big.png");
        let mut img = image::RgbImage::new(4000, 2000);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = image::Rgb([(x % 256) as u8, (y % 256) as u8, 128]);
        }
        img.save(&src).unwrap();
        let p = prepare(&blur_job(&src, &cache, Blur::new(30, 0.6))).unwrap();
        let from_pixels = image::open(p.blurred.as_ref().unwrap()).unwrap().into_rgb8();
        std::fs::remove_file(p.blurred.as_ref().unwrap()).unwrap();
        let again = bake_blur(p.picture.as_ref().unwrap(), &p.key, &cache, Blur::new(30, 0.6)).unwrap();
        let from_file = image::open(again).unwrap().into_rgb8();
        assert_eq!(from_pixels.dimensions(), from_file.dimensions());
        // Only JPEG's loss separates them.
        let diff = from_pixels.pixels().zip(from_file.pixels())
            .map(|(a, b)| a.0.iter().zip(b.0).map(|(x, y)| x.abs_diff(y) as u32).max().unwrap())
            .max()
            .unwrap();
        assert!(diff <= 4, "max channel difference {diff}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // SB-02: memory must not depend on the strength.
    #[test]
    fn weak_blurs_stay_small_and_huge_pictures_are_declined() {
        let (dir, cache) = scratch("blursize");
        let src = dir.join("square.png");
        image::RgbImage::new(4000, 4000).save(&src).unwrap();
        let mut job = blur_job(&src, &cache, Blur::new(1, 1.0));
        job.prescale = false; // so the 16 MP original is what gets blurred
        let out = prepare(&job).unwrap().blurred.unwrap();
        let (w, h) = image::image_dimensions(&out).unwrap();
        assert!(w as f64 * h as f64 <= BLUR_MAX_PIXELS * 1.01, "{w}x{h}");

        let huge = dir.join("huge.png");
        image::RgbImage::new(7000, 6000).save(&huge).unwrap();
        let mut job = blur_job(&huge, &cache, Blur::new(30, 0.6));
        job.prescale = false;
        assert!(prepare(&job).unwrap().blurred.is_none(), "42 MP is over the budget");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // SB-13: settings that bake the same file compare equal.
    #[test]
    fn brightness_is_whole_percents() {
        assert_eq!(Blur::new(30, 0.601), Blur::new(30, 0.604));
        assert_ne!(Blur::new(30, 0.601), Blur::new(30, 0.62));
        assert_eq!(Blur::new(30, 1.7).brightness, 100);
    }

    // DS-11: a cut-off or emptied cache file is made again, not reused.
    #[test]
    fn broken_cache_files_are_remade() {
        let (dir, cache) = scratch("broken");
        let src = dir.join("big.png");
        image::RgbImage::new(4000, 2000).save(&src).unwrap();
        let job = blur_job(&src, &cache, Blur::new(30, 0.6));
        let first = prepare(&job).unwrap();
        let (pic, blurred) = (first.picture.unwrap(), first.blurred.unwrap());
        let whole = std::fs::read(&pic).unwrap();
        std::fs::write(&pic, &whole[..whole.len() / 2]).unwrap();
        std::fs::write(&blurred, b"").unwrap();
        let again = prepare(&job).unwrap();
        assert_eq!(again.picture.as_ref(), Some(&pic));
        assert!(image::open(&pic).is_ok(), "picture remade whole");
        assert!(image::open(again.blurred.unwrap()).is_ok(), "blur remade");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // DS-14: trimming never deletes what prepare is about to return.
    #[test]
    fn result_survives_a_tiny_budget() {
        let (dir, cache) = scratch("tiny");
        let src = dir.join("big.png");
        image::RgbImage::new(4000, 2000).save(&src).unwrap();
        let mut job = blur_job(&src, &cache, Blur::new(30, 0.6));
        job.cache_bytes = 1;
        let p = prepare(&job).unwrap();
        assert!(p.picture.unwrap().exists());
        assert!(p.blurred.unwrap().exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // DS-12: a video ffmpeg can't read is refused, not played.
    #[test]
    fn unreadable_video_is_an_error() {
        let (dir, cache) = scratch("video");
        let src = dir.join("empty.mp4");
        std::fs::write(&src, b"").unwrap();
        let job = Job { blur: None, ..blur_job(&src, &cache, Blur::new(30, 0.6)) };
        assert!(prepare(&job).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // DS-08: a stuck helper is stopped at its deadline.
    #[test]
    fn stuck_commands_are_killed() {
        let start = Instant::now();
        assert!(!run(Command::new("sleep").arg("30"), Duration::from_millis(300)));
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(run(&mut Command::new("true"), Duration::from_secs(5)));
    }

    // SB-14: abandoned temporary files don't pile up.
    #[test]
    fn old_partial_files_are_removed() {
        let (dir, cache) = scratch("parts");
        std::fs::create_dir_all(&cache).unwrap();
        let old = cache.join("x-blur-30-60.png.part");
        let fresh = cache.join("y.part.webm");
        std::fs::write(&old, b"x").unwrap();
        std::fs::write(&fresh, b"x").unwrap();
        File::options().append(true).open(&old).unwrap()
            .set_modified(SystemTime::now() - PART_MAX_AGE * 2).unwrap();
        trim_cache(&cache, &[], u64::MAX);
        assert!(!old.exists());
        assert!(fresh.exists(), "a transcode may still be writing it");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
