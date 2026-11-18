//! The image bytes on disk: every downloaded image (emote, badge, thumbnail) is kept as the encoded file the
//! server sent, under `<data dir>/image-cache/`, so the next start (or the next decode after an eviction) does not
//! download it again. One file per URL, named by a hash of the URL without its `#fragment` (the fragment never
//! reaches the server; `#square` is only how the UI asks for a cropped copy).
//!
//! Writes are atomic (a temp file renamed into place), only one write per URL runs at a time, failures are never
//! written, and the folder is kept under a size cap: the files not used for the longest go first (a hit bumps the
//! file's modified time, which is what the pruning sorts by; access times are not reliable across systems).
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

/// The folder is pruned back under this.
pub const CAP_BYTES: u64 = 300 * 1024 * 1024;
/// Files bigger than this are never written (a huge thumbnail is not worth the space).
pub const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
/// Pruning stops once the folder is down to this share of the cap, so it does not run after every write.
const PRUNE_TO: f64 = 0.9;
/// A temp file older than this is left over from a crash.
const STALE_TMP: Duration = Duration::from_secs(3600);

pub struct DiskCache {
    dir: PathBuf,
    cap: u64,
    /// Keys being written right now: a second write of the same URL is skipped.
    writing: Mutex<HashSet<String>>,
    /// Bytes in the folder as last counted plus what was written since (a scan sets it right).
    approx: AtomicU64,
    pruning: AtomicBool,
}

/// The file name part of a URL: a hash of it without the `#fragment`.
pub fn key(url: &str) -> String {
    let base = url.split('#').next().unwrap_or(url);
    let h = Sha256::digest(base.as_bytes());
    h[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether a URL is worth keeping on disk: plain http(s) URLs, not the per-session signed ones (they never come back).
pub fn cacheable(url: &str) -> bool {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return false;
    }
    let q = match url.split('#').next().unwrap_or(url).split_once('?') {
        Some((_, q)) => q.to_ascii_lowercase(),
        None => return true,
    };
    !["expires=", "signature=", "x-amz-", "token=", "sig="].iter().any(|m| q.split('&').any(|p| p.starts_with(m)))
}

impl DiskCache {
    pub fn new(dir: PathBuf, cap: u64) -> DiskCache {
        DiskCache { dir, cap, writing: Mutex::new(HashSet::new()), approx: AtomicU64::new(0), pruning: AtomicBool::new(false) }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path_of(&self, k: &str) -> PathBuf {
        self.dir.join(format!("{k}.img"))
    }

    /// The stored bytes of `url`, if any (blocking). A hit marks the file as just used.
    pub fn read(&self, url: &str) -> Option<Vec<u8>> {
        let p = self.path_of(&key(url));
        let bytes = std::fs::read(&p).ok()?;
        if bytes.is_empty() {
            let _ = std::fs::remove_file(&p);
            return None;
        }
        touch(&p);
        Some(bytes)
    }

    /// Drops the stored bytes of `url` (they did not decode).
    pub fn remove(&self, url: &str) {
        let _ = std::fs::remove_file(self.path_of(&key(url)));
    }

    /// Stores `bytes` for `url` (blocking): written to a temp file and renamed into place. Skipped when the file is
    /// too big or empty, or another write of the same URL is running. Returns whether it was written.
    pub fn write(&self, url: &str, bytes: &[u8]) -> bool {
        if bytes.is_empty() || bytes.len() > MAX_FILE_BYTES {
            return false;
        }
        let k = key(url);
        if !self.writing.lock().unwrap().insert(k.clone()) {
            return false;
        }
        let ok = self.write_file(&k, bytes).is_ok();
        self.writing.lock().unwrap().remove(&k);
        if ok {
            let total = self.approx.fetch_add(bytes.len() as u64, Ordering::Relaxed) + bytes.len() as u64;
            if total > self.cap {
                self.prune();
            }
        }
        ok
    }

    fn write_file(&self, k: &str, bytes: &[u8]) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join(format!("{k}.{:08x}.tmp", rand::random::<u32>()));
        let res = (|| {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all().ok();
            drop(f);
            std::fs::rename(&tmp, self.path_of(k))
        })();
        if res.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        res
    }

    /// Counts the folder and deletes the least recently used files until it is under the cap (blocking; run in the
    /// background at start and when a write pushes it over). Also clears temp files left by a crash.
    /// Returns (bytes kept, files deleted).
    pub fn prune(&self) -> (u64, usize) {
        if self.pruning.swap(true, Ordering::AcqRel) {
            return (self.approx.load(Ordering::Relaxed), 0);
        }
        let mut files: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
        let now = SystemTime::now();
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                let p = e.path();
                let Ok(md) = e.metadata() else { continue };
                if !md.is_file() {
                    continue;
                }
                let mtime = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                match p.extension().and_then(|x| x.to_str()) {
                    Some("img") => files.push((mtime, md.len(), p)),
                    Some("tmp") if now.duration_since(mtime).map(|d| d > STALE_TMP).unwrap_or(false) => {
                        let _ = std::fs::remove_file(&p);
                    }
                    _ => {}
                }
            }
        }
        let mut total: u64 = files.iter().map(|f| f.1).sum();
        let mut deleted = 0;
        if total > self.cap {
            let target = (self.cap as f64 * PRUNE_TO) as u64;
            files.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));
            for (_, len, p) in files {
                if total <= target {
                    break;
                }
                if std::fs::remove_file(&p).is_ok() {
                    total -= len;
                    deleted += 1;
                }
            }
            crate::logf!("image disk cache: pruned {deleted} files, {} MB left", total / 1024 / 1024);
        }
        self.approx.store(total, Ordering::Relaxed);
        self.pruning.store(false, Ordering::Release);
        (total, deleted)
    }
}

/// Marks a file as just used (its modified time is the LRU clock).
fn touch(p: &Path) {
    if let Ok(f) = std::fs::OpenOptions::new().write(true).open(p) {
        let _ = f.set_modified(SystemTime::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("agu-img-disk-test-{name}-{:08x}", rand::random::<u32>()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn set_mtime(c: &DiskCache, url: &str, secs_ago: u64) {
        let p = c.path_of(&key(url));
        let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(secs_ago)).unwrap();
    }

    #[test]
    fn keys_ignore_the_fragment_and_differ_per_url() {
        assert_eq!(key("https://a/x.png#square"), key("https://a/x.png"));
        assert_ne!(key("https://a/x.png"), key("https://a/y.png"));
        assert_eq!(key("https://a/x.png").len(), 32);
        assert!(cacheable("https://cdn.7tv.app/emote/01ABC/1x.webp"));
        assert!(cacheable("https://static-cdn.jtvnw.net/emoticons/v2/25/default/dark/1.0"));
        assert!(cacheable("https://i.ytimg.com/vi/x/hqdefault.jpg?sqp=abc"));
        assert!(!cacheable("https://cdn-cf-east.streamable.com/image/x.jpg?Expires=1&Signature=a&Key-Pair-Id=b"));
        assert!(!cacheable("kick-badge:moderator"));
    }

    #[test]
    fn miss_write_hit() {
        let d = temp_dir("hit");
        let c = DiskCache::new(d.clone(), CAP_BYTES);
        assert!(c.read("https://a/1.webp").is_none());
        assert!(c.write("https://a/1.webp", b"hello"));
        assert_eq!(c.read("https://a/1.webp").as_deref(), Some(&b"hello"[..]));
        // the cropped copy reads the same file
        assert_eq!(c.read("https://a/1.webp#square").as_deref(), Some(&b"hello"[..]));
        assert!(c.read("https://a/2.webp").is_none());
        // too big or empty: never written
        assert!(!c.write("https://a/big", &vec![0u8; MAX_FILE_BYTES + 1]));
        assert!(!c.write("https://a/empty", b""));
        assert!(c.read("https://a/big").is_none());
        // no temp files left behind
        let tmp = std::fs::read_dir(&d).unwrap().flatten().filter(|e| e.path().extension().map(|x| x == "tmp").unwrap_or(false)).count();
        assert_eq!(tmp, 0);
        c.remove("https://a/1.webp");
        assert!(c.read("https://a/1.webp").is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn prune_drops_the_least_recently_used_until_under_the_cap() {
        let d = temp_dir("prune");
        let mut c = DiskCache::new(d.clone(), CAP_BYTES);
        // five files of 300 KB (1.5 MB), then the cap drops to 1 MB
        for i in 0..5 {
            assert!(c.write(&format!("https://a/{i}"), &vec![i as u8; 300_000]));
        }
        c.cap = 1_000_000;
        for i in 0..5 {
            set_mtime(&c, &format!("https://a/{i}"), 1000 - i * 100);
        }
        // 0 is the oldest, but it was just used: it moves to the front
        set_mtime(&c, "https://a/0", 1000);
        c.read("https://a/0").unwrap();
        let (kept, deleted) = c.prune();
        assert!(kept <= 900_000, "{kept}");
        assert_eq!(deleted, 2);
        assert!(c.read("https://a/1").is_none() && c.read("https://a/2").is_none(), "the least recently used go first");
        assert!(c.read("https://a/0").is_some() && c.read("https://a/3").is_some() && c.read("https://a/4").is_some());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_write_over_the_cap_prunes_in_place() {
        let d = temp_dir("cap");
        let c = DiskCache::new(d.clone(), 1_000_000);
        for i in 0..10 {
            c.write(&format!("https://a/{i}"), &vec![1u8; 200_000]);
        }
        let on_disk: u64 = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.metadata().unwrap().len()).sum();
        assert!(on_disk <= 1_000_000, "{on_disk}");
        assert!(c.read("https://a/9").is_some(), "the newest stays");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn concurrent_writes_of_one_url_leave_one_whole_file() {
        let d = temp_dir("race");
        let c = std::sync::Arc::new(DiskCache::new(d.clone(), CAP_BYTES));
        let threads: Vec<_> = (0..16u8)
            .map(|i| {
                let c = c.clone();
                std::thread::spawn(move || {
                    // same URL, different contents; plus a URL of its own
                    c.write("https://a/same", &vec![i; 64 * 1024]);
                    assert!(c.write(&format!("https://a/own{i}"), &vec![i; 1024]));
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let got = c.read("https://a/same").expect("one of them is stored");
        assert_eq!(got.len(), 64 * 1024);
        assert!(got.iter().all(|b| *b == got[0]), "never a mix of two writes");
        for i in 0..16u8 {
            assert_eq!(c.read(&format!("https://a/own{i}")).unwrap(), vec![i; 1024]);
        }
        let tmp = std::fs::read_dir(&d).unwrap().flatten().filter(|e| e.path().extension().map(|x| x == "tmp").unwrap_or(false)).count();
        assert_eq!(tmp, 0);
        let _ = std::fs::remove_dir_all(&d);
    }
}
