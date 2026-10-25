//! Finding wallpapers on disk.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Static,
    /// GIFs: may be a single frame, checked when shown.
    Gif,
    Video,
}

pub fn kind_of(path: &Path) -> Option<Kind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" | "png" | "webp" | "bmp" | "tif" | "tiff" | "avif" | "heic" | "jxl" => {
            Some(Kind::Static)
        }
        "gif" => Some(Kind::Gif),
        "mp4" | "webm" | "mkv" | "mov" | "m4v" => Some(Kind::Video),
        _ => None,
    }
}

pub struct Scan {
    pub files: Vec<String>,
    pub dirs: Vec<PathBuf>,
}

/// Walk `root` and return every wallpaper path (sorted) plus every directory
/// visited, so the caller can watch them. Hidden entries are skipped.
pub fn scan(root: &Path, recursive: bool) -> Scan {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        dirs.push(dir);
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.as_encoded_bytes().first() == Some(&b'.') {
                continue;
            }
            let Ok(ft) = entry.file_type() else { continue };
            let path = entry.path();
            // Follow symlinks for files, but not for directories (avoids loops).
            if ft.is_dir() {
                if recursive {
                    stack.push(path);
                }
            } else if kind_of(&path).is_some() && (ft.is_file() || path.is_file()) {
                if let Some(s) = path.to_str() {
                    files.push(s.to_owned());
                }
            }
        }
    }
    files.sort_unstable();
    Scan { files, dirs }
}

/// True if a GIF has more than one frame. Walks the block structure without
/// decoding any pixels.
pub fn gif_is_animated(path: &Path) -> bool {
    fn inner(path: &Path) -> std::io::Result<bool> {
        let mut r = BufReader::with_capacity(16 * 1024, File::open(path)?);
        let mut header = [0u8; 13];
        r.read_exact(&mut header)?;
        if &header[..3] != b"GIF" {
            return Ok(false);
        }
        if header[10] & 0x80 != 0 {
            skip(&mut r, 3 * (1usize << ((header[10] & 0x07) + 1)))?;
        }
        let mut frames = 0;
        loop {
            match byte(&mut r)? {
                0x21 => {
                    byte(&mut r)?;
                    skip_sub_blocks(&mut r)?;
                }
                0x2C => {
                    let mut desc = [0u8; 9];
                    r.read_exact(&mut desc)?;
                    if desc[8] & 0x80 != 0 {
                        skip(&mut r, 3 * (1usize << ((desc[8] & 0x07) + 1)))?;
                    }
                    byte(&mut r)?;
                    skip_sub_blocks(&mut r)?;
                    frames += 1;
                    if frames > 1 {
                        return Ok(true);
                    }
                }
                _ => return Ok(false),
            }
        }
    }
    fn byte(r: &mut impl Read) -> std::io::Result<u8> {
        let mut b = [0u8; 1];
        r.read_exact(&mut b)?;
        Ok(b[0])
    }
    fn skip(r: &mut impl Read, n: usize) -> std::io::Result<()> {
        std::io::copy(&mut r.take(n as u64), &mut std::io::sink())?;
        Ok(())
    }
    fn skip_sub_blocks(r: &mut impl Read) -> std::io::Result<()> {
        loop {
            let len = byte(r)?;
            if len == 0 {
                return Ok(());
            }
            skip(r, len as usize)?;
        }
    }
    inner(path).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(kind_of(Path::new("/a/b.JPG")), Some(Kind::Static));
        assert_eq!(kind_of(Path::new("/a/b.gif")), Some(Kind::Gif));
        assert_eq!(kind_of(Path::new("/a/b.webm")), Some(Kind::Video));
        assert_eq!(kind_of(Path::new("/a/b.txt")), None);
        assert_eq!(kind_of(Path::new("/a/noext")), None);
    }

    #[test]
    fn gif_frames() {
        let dir = std::env::temp_dir().join(format!("wallslaed-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let one = dir.join("one.gif");
        let two = dir.join("two.gif");
        let frame = image::Frame::new(image::RgbaImage::new(4, 4));
        {
            let mut enc = image::codecs::gif::GifEncoder::new(File::create(&one).unwrap());
            enc.encode_frame(frame.clone()).unwrap();
        }
        {
            let mut enc = image::codecs::gif::GifEncoder::new(File::create(&two).unwrap());
            enc.encode_frame(frame.clone()).unwrap();
            enc.encode_frame(frame).unwrap();
        }
        assert!(!gif_is_animated(&one));
        assert!(gif_is_animated(&two));

        std::fs::write(dir.join("a.jpg"), b"x").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/b.png"), b"x").unwrap();
        std::fs::write(dir.join(".hidden.png"), b"x").unwrap();
        let s = scan(&dir, true);
        assert_eq!(s.files.len(), 4);
        assert_eq!(scan(&dir, false).files.len(), 3);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
