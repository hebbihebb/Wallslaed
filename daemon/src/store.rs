//! Saving and loading the state files (favorites, bans, history, music
//! positions). They can't be recreated, so:
//!
//! - A file that exists but can't be read is moved aside, never overwritten,
//!   and the last good copy (`.bak`, taken at each start) is used instead.
//! - Writes go through a temporary file that is synced before it replaces the
//!   old one, so a crash or power cut leaves the old or the new file, whole.
//! - Writes happen on a thread of their own: a stuck disk must not stop the
//!   daemon answering. A failed write is kept and retried, and reported.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::de::DeserializeOwned;

/// How long a failed write waits before the next attempt.
const RETRY: Duration = Duration::from_secs(30);

/// Reads `path`. No file means a first run (or the backup, if one exists). A
/// file that can't be read or parsed is renamed to `<name>.broken-<time>` and
/// the backup is tried; it is never silently replaced by an empty state.
pub fn load<T: DeserializeOwned + Default>(path: &Path) -> T {
    match read(path) {
        Ok(Some(value)) => return value,
        Ok(None) => {}
        Err(e) => {
            let aside = broken_name(path);
            match std::fs::rename(path, &aside) {
                Ok(()) => eprintln!(
                    "wallslaed: {} is unreadable ({e}); kept as {}",
                    path.display(),
                    aside.display()
                ),
                Err(re) => eprintln!("wallslaed: {} is unreadable ({e}) and can't be moved aside: {re}", path.display()),
            }
        }
    }
    match read(&backup_name(path)) {
        Ok(Some(value)) => {
            eprintln!("wallslaed: restored {} from its backup", path.display());
            value
        }
        _ => T::default(),
    }
}

/// `Ok(None)` if there is no file.
fn read<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

fn backup_name(path: &Path) -> PathBuf {
    with_suffix(path, ".bak")
}

fn broken_name(path: &Path) -> PathBuf {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    with_suffix(path, &format!(".broken-{secs}"))
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

enum Msg {
    Write(PathBuf, Vec<u8>),
    Flush(mpsc::Sender<()>),
}

/// The writer thread. Only the newest contents of each file are written.
pub struct Store {
    tx: mpsc::Sender<Msg>,
    error: Arc<Mutex<Option<String>>>,
}

impl Store {
    pub fn new() -> Store {
        let (tx, rx) = mpsc::channel();
        let error = Arc::new(Mutex::new(None));
        let e = error.clone();
        std::thread::Builder::new()
            .name("wallslaed-store".into())
            .spawn(move || writer(rx, e))
            .expect("start the state writer");
        Store { tx, error }
    }

    pub fn save(&self, path: &Path, bytes: Vec<u8>) {
        let _ = self.tx.send(Msg::Write(path.to_path_buf(), bytes));
    }

    /// Keep a copy of a file that just loaded fine, for [`load`] to fall back on.
    pub fn back_up(&self, path: &Path) {
        if let Ok(bytes) = std::fs::read(path) {
            self.save(&backup_name(path), bytes);
        }
    }

    /// The last write error, until a write succeeds again.
    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Wait up to `timeout` for everything queued to be written.
    pub fn flush(&self, timeout: Duration) -> bool {
        let (ack, done) = mpsc::channel();
        self.tx.send(Msg::Flush(ack)).is_ok() && done.recv_timeout(timeout).is_ok()
    }
}

fn writer(rx: mpsc::Receiver<Msg>, error: Arc<Mutex<Option<String>>>) {
    let mut pending: HashMap<PathBuf, Vec<u8>> = HashMap::new();
    let mut acks = Vec::new();
    loop {
        let msg = if pending.is_empty() {
            match rx.recv() {
                Ok(m) => Some(m),
                Err(_) => return,
            }
        } else {
            match rx.recv_timeout(RETRY) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        };
        // Take everything queued, so a burst of changes is one write.
        for m in msg.into_iter().chain(std::iter::from_fn(|| rx.try_recv().ok())) {
            match m {
                Msg::Write(path, bytes) => {
                    pending.insert(path, bytes);
                }
                Msg::Flush(ack) => acks.push(ack),
            }
        }
        let mut failed = None;
        pending.retain(|path, bytes| match write_atomic(path, bytes) {
            Ok(()) => false,
            Err(e) => {
                failed = Some(format!("{}: {e}", path.display()));
                true
            }
        });
        let mut last = error.lock().unwrap_or_else(|e| e.into_inner());
        if failed != *last {
            match &failed {
                Some(e) => eprintln!("wallslaed: can't save {e}; will retry"),
                None => eprintln!("wallslaed: saving works again"),
            }
            *last = failed;
        }
        drop(last);
        // A flush waits for success, or for one honest attempt.
        for ack in acks.drain(..) {
            let _ = ack.send(());
        }
    }
}

/// Replace `path` with `bytes` so that a crash at any point leaves either the
/// old file or the new one, complete.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    // Unique per process: two daemons must never write each other's file.
    let tmp = with_suffix(path, &format!(".tmp-{}", std::process::id()));
    let result = (|| {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)?;
        File::open(dir)?.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    struct S {
        n: u32,
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wallslaed-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().into_string().unwrap())
            .collect();
        v.sort();
        v
    }

    // DS-01: a broken file is kept, and the backup is used.
    #[test]
    fn broken_file_is_moved_aside_and_backup_used() {
        let dir = scratch("broken");
        let path = dir.join("state.json");
        std::fs::write(&path, b"{\"n\": 7").unwrap(); // truncated
        assert_eq!(load::<S>(&path), S::default(), "no backup yet: empty");
        let kept: Vec<String> = names(&dir).into_iter().filter(|n| n.starts_with("state.json.broken-")).collect();
        assert_eq!(kept.len(), 1, "original kept: {:?}", names(&dir));
        assert_eq!(std::fs::read(dir.join(&kept[0])).unwrap(), b"{\"n\": 7");
        assert!(!path.exists());

        std::fs::write(dir.join("state.json.bak"), b"{\"n\": 5}").unwrap();
        std::fs::write(&path, b"").unwrap();
        assert_eq!(load::<S>(&path), S { n: 5 }, "empty file: backup restored");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_file_is_a_first_run() {
        let dir = scratch("missing");
        assert_eq!(load::<S>(&dir.join("state.json")), S::default());
        assert!(names(&dir).is_empty(), "nothing written by loading");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn writes_replace_whole_files_and_leave_no_temporaries() {
        let dir = scratch("write");
        let path = dir.join("state.json");
        let store = Store::new();
        for n in 0..50 {
            store.save(&path, serde_json::to_vec(&S { n }).unwrap());
        }
        assert!(store.flush(Duration::from_secs(5)));
        assert_eq!(load::<S>(&path), S { n: 49 });
        store.back_up(&path);
        assert!(store.flush(Duration::from_secs(5)));
        assert_eq!(names(&dir), ["state.json", "state.json.bak"]);
        assert!(store.error().is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // DS-04: a failed write is reported and retried, not dropped.
    #[test]
    fn failed_writes_are_reported_and_kept() {
        let dir = scratch("fail");
        // A file where the state directory should be: every write fails.
        let blocker = dir.join("state");
        std::fs::write(&blocker, b"").unwrap();
        let store = Store::new();
        store.save(&blocker.join("state.json"), b"{}".to_vec());
        assert!(store.flush(Duration::from_secs(5)));
        assert!(store.error().is_some());
        std::fs::remove_file(&blocker).unwrap();
        // The retry is RETRY away; a flush makes the next attempt now.
        assert!(store.flush(Duration::from_secs(5)));
        assert!(store.error().is_none(), "{:?}", store.error());
        assert_eq!(std::fs::read(blocker.join("state.json")).unwrap(), b"{}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
