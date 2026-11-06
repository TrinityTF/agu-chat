//! Shared image cache: fetch once per URL, decode off the UI thread into UI-neutral RGBA frames,
//! then let the frontend turn them into its own GPU objects (`T`) on the UI thread.
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};

use crate::Wake;

/// A decoded image: straight-alpha RGBA frames with per-frame delays.
pub struct Decoded {
    pub w: u32,
    pub h: u32,
    /// (RGBA bytes, delay in ms)
    pub frames: Vec<(Vec<u8>, u32)>,
}

/// A finished image plus whatever the frontend made out of it.
pub struct Img<T> {
    pub w: u32,
    pub h: u32,
    pub delays: Vec<u32>,
    pub total_ms: u32,
    pub gpu: T,
}

impl<T> Img<T> {
    /// Current frame index and ms until the next frame change.
    pub fn frame_at(&self, t_ms: u64, freeze: bool) -> (usize, u32) {
        if freeze || self.delays.len() < 2 || self.total_ms == 0 {
            return (0, u32::MAX);
        }
        let mut t = (t_ms % self.total_ms as u64) as u32;
        for (i, d) in self.delays.iter().enumerate() {
            if t < *d {
                return (i, *d - t);
            }
            t -= *d;
        }
        (0, 50)
    }
}

pub enum State<T> {
    Loading,
    Failed,
    Ready(Arc<Img<T>>),
}

/// One cache slot: the state, what a ready image holds in memory, and when it was last asked for.
struct Entry<T> {
    state: State<T>,
    bytes: u64,
    used_ms: u64,
}

pub struct ImageCache<T> {
    map: HashMap<String, Entry<T>>,
    /// Images pushed out by the memory budget; the frontend frees their GPU side (`take_evicted`).
    evicted: Vec<Arc<Img<T>>>,
    rt: tokio::runtime::Handle,
    client: reqwest::Client,
    tx: Sender<(String, Result<Decoded, String>)>,
    rx: Receiver<(String, Result<Decoded, String>)>,
    wake: Wake,
    sem: Arc<tokio::sync::Semaphore>,
    /// Bytes the frontend reported for its uploaded images.
    pub decoded_bytes: u64,
    /// Natural size of every image ever decoded: layout needs it after the pixels were evicted.
    sizes: HashMap<String, (u32, u32)>,
    /// Images decoded since start (never goes down; layouts are keyed by it).
    pub loads_total: u64,
    /// GPU image objects (one per image).
    pub textures: usize,
    pub frames: usize,
    pub loaded: usize,
    pub failed: usize,
    pub start: Instant,
    pub freeze: bool,
}

/// Decoded images stay in memory (RGBA/BGRA per frame, the animated ones with all frames), so the cache has a
/// budget: past it the images not asked for the longest are dropped and fetched again if they come back.
pub const BUDGET_BYTES: u64 = 160 * 1024 * 1024;

/// `AGU_IMAGE_BUDGET_MB` overrides the budget (tuning / measuring).
fn budget() -> u64 {
    static B: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *B.get_or_init(|| std::env::var("AGU_IMAGE_BUDGET_MB").ok().and_then(|v| v.parse::<u64>().ok()).map(|m| m * 1024 * 1024).unwrap_or(BUDGET_BYTES))
}
/// An animation bigger than this keeps only its first frame.
const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
/// Images used within this long are never evicted (they are on screen).
const KEEP_MS: u64 = 3_000;
const MAX_FRAMES: usize = 150;
const MAX_PIXELS: u64 = 512 * 512;

pub fn decode(bytes: &[u8]) -> Result<Decoded, String> {
    use image::AnimationDecoder;
    let fmt = image::guess_format(bytes).map_err(|e| e.to_string())?;
    let mut frames: Vec<(Vec<u8>, u32)> = Vec::new();
    let (mut w, mut h) = (0u32, 0u32);
    let mut push = |f: image::Frame| {
        let (n, d) = f.delay().numer_denom_ms();
        let mut ms = if d == 0 { 100 } else { n / d };
        if ms == 0 {
            ms = 100;
        }
        ms = ms.max(20);
        let buf = f.into_buffer();
        w = buf.width();
        h = buf.height();
        frames.push((buf.into_raw(), ms));
    };
    match fmt {
        image::ImageFormat::Gif => {
            let dec = image::codecs::gif::GifDecoder::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
            for f in dec.into_frames().take(MAX_FRAMES) {
                push(f.map_err(|e| e.to_string())?);
            }
        }
        image::ImageFormat::WebP => {
            let dec = image::codecs::webp::WebPDecoder::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
            for f in dec.into_frames().take(MAX_FRAMES) {
                push(f.map_err(|e| e.to_string())?);
            }
        }
        _ => {
            let im = image::load_from_memory(bytes).map_err(|e| e.to_string())?.to_rgba8();
            w = im.width();
            h = im.height();
            frames.push((im.into_raw(), 100));
        }
    }
    if frames.is_empty() && fmt == image::ImageFormat::WebP {
        // static WebP: the animation iterator yields nothing
        let im = image::load_from_memory(bytes).map_err(|e| e.to_string())?.to_rgba8();
        w = im.width();
        h = im.height();
        frames.push((im.into_raw(), 100));
    }
    if frames.is_empty() || w == 0 || h == 0 {
        return Err("empty image".into());
    }
    if (w as u64) * (h as u64) > MAX_PIXELS {
        return Err("image too large".into());
    }
    if frames.len() > 1 && (w as u64) * (h as u64) * 4 * frames.len() as u64 > MAX_IMAGE_BYTES {
        frames.truncate(1);
    }
    Ok(Decoded { w, h, frames })
}

impl<T> ImageCache<T> {
    pub fn new(rt: tokio::runtime::Handle, client: reqwest::Client, wake: Wake) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        ImageCache {
            map: HashMap::new(),
            evicted: Vec::new(),
            rt,
            client,
            tx,
            rx,
            wake,
            sem: Arc::new(tokio::sync::Semaphore::new(12)),
            decoded_bytes: 0,
            sizes: HashMap::new(),
            loads_total: 0,
            textures: 0,
            frames: 0,
            loaded: 0,
            failed: 0,
            start: Instant::now(),
            freeze: std::env::var("AGU_NO_ANIM").map(|v| v == "1").unwrap_or(false),
        }
    }

    pub fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    /// State of a URL; starts the fetch on first sight.
    pub fn get(&mut self, url: &str) -> &State<T> {
        let now = self.now_ms();
        if let Some(e) = self.map.get_mut(url) {
            e.used_ms = now;
        } else {
            self.map.insert(url.to_string(), Entry { state: State::Loading, bytes: 0, used_ms: now });
            let (tx, client, wake, sem, u) =
                (self.tx.clone(), self.client.clone(), self.wake.clone(), self.sem.clone(), url.to_string());
            self.rt.spawn(async move {
                let _permit = sem.acquire().await;
                let res: Result<Decoded, String> = async {
                    let bytes = client
                        .get(&u)
                        .send()
                        .await
                        .and_then(|r| r.error_for_status())
                        .map_err(|e| e.to_string())?
                        .bytes()
                        .await
                        .map_err(|e| e.to_string())?;
                    tokio::task::spawn_blocking(move || decode(&bytes)).await.map_err(|e| e.to_string())?
                }
                .await;
                let _ = tx.send((u, res));
                wake();
            });
        }
        &self.map[url].state
    }

    /// Natural size of a decoded image, even after it was evicted; None until it first loaded.
    pub fn size_of(&self, url: &str) -> Option<(u32, u32)> {
        self.sizes.get(url).copied()
    }

    pub fn peek(&self, url: &str) -> Option<&State<T>> {
        self.map.get(url).map(|e| &e.state)
    }

    /// Hand finished decodes to the frontend (bounded per call). `upload` returns the frontend's
    /// handle and how many bytes it holds. Returns true if more are waiting.
    pub fn pump(&mut self, mut upload: impl FnMut(&str, &Decoded) -> (T, u64)) -> bool {
        let t0 = Instant::now();
        while let Ok((url, res)) = self.rx.try_recv() {
            match res {
                Ok(d) => {
                    let (gpu, bytes) = upload(&url, &d);
                    let delays: Vec<u32> = d.frames.iter().map(|f| f.1).collect();
                    let total = delays.iter().sum();
                    self.decoded_bytes += bytes;
                    self.textures += 1;
                    self.frames += delays.len();
                    self.loaded += 1;
                    self.loads_total += 1;
                    self.sizes.insert(url.clone(), (d.w, d.h));
                    let now = self.now_ms();
                    self.map.insert(url, Entry { state: State::Ready(Arc::new(Img { w: d.w, h: d.h, delays, total_ms: total, gpu })), bytes, used_ms: now });
                }
                Err(e) => {
                    crate::logf!("image failed {url}: {e}");
                    self.failed += 1;
                    let now = self.now_ms();
                    self.map.insert(url, Entry { state: State::Failed, bytes: 0, used_ms: now });
                }
            }
            if t0.elapsed().as_millis() >= 4 {
                self.evict();
                return !self.rx.is_empty();
            }
        }
        self.evict();
        false
    }

    /// Over the budget: drop the least recently used ready images (not the ones used in the last seconds)
    /// until the cache is back to 3/4 of it.
    fn evict(&mut self) {
        if self.decoded_bytes <= budget() {
            return;
        }
        let now = self.now_ms();
        let mut cand: Vec<(u64, String)> = self.map.iter().filter(|(_, e)| e.bytes > 0 && now.saturating_sub(e.used_ms) > KEEP_MS).map(|(k, e)| (e.used_ms, k.clone())).collect();
        cand.sort();
        let target = budget() / 4 * 3;
        let mut dropped = 0;
        for (_, url) in cand {
            if self.decoded_bytes <= target {
                break;
            }
            if let Some(e) = self.map.remove(&url) {
                self.decoded_bytes -= e.bytes;
                self.textures -= 1;
                self.loaded -= 1;
                if let State::Ready(img) = e.state {
                    self.evicted.push(img);
                }
                dropped += 1;
            }
        }
        crate::logf!("image cache: evicted {dropped}, {} MB in {} images", self.decoded_bytes / 1024 / 1024, self.loaded);
    }

    /// Images evicted since the last call, for the frontend to free what it uploaded.
    pub fn take_evicted(&mut self) -> Vec<Arc<Img<T>>> {
        std::mem::take(&mut self.evicted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn cache() -> (tokio::runtime::Runtime, ImageCache<u32>) {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let c = ImageCache::new(rt.handle().clone(), reqwest::Client::new(), Arc::new(|| {}));
        (rt, c)
    }

    fn decoded(frames: usize) -> Decoded {
        Decoded { w: 2, h: 2, frames: (0..frames).map(|_| (vec![0u8; 16], 50)).collect() }
    }

    #[test]
    fn past_the_budget_the_longest_unused_images_are_evicted() {
        let (_rt, mut c) = cache();
        c.start = Instant::now() - Duration::from_secs(120);
        // three images of 60 MB each (reported by the frontend), asked for 100 s, 50 s and just now
        for (i, name) in ["old", "mid", "new"].iter().enumerate() {
            c.tx.send((name.to_string(), Ok(decoded(1)))).unwrap();
            c.pump(|_, _| (i as u32, 60 * 1024 * 1024));
        }
        c.map.get_mut("old").unwrap().used_ms = 20_000;
        c.map.get_mut("mid").unwrap().used_ms = 70_000;
        c.map.get_mut("new").unwrap().used_ms = c.now_ms();
        // a fourth one tips it over 160 MB (4 x 60 = 240 MB): the oldest go until 3/4 of the budget is left
        c.tx.send(("extra".to_string(), Ok(decoded(1)))).unwrap();
        c.pump(|_, _| (9, 60 * 1024 * 1024));
        assert!(c.decoded_bytes <= budget(), "{}", c.decoded_bytes);
        assert!(c.peek("old").is_none() && c.peek("mid").is_none(), "least recently used go first");
        assert!(c.peek("new").is_some(), "an image used just now stays");
        assert_eq!(c.take_evicted().len(), 2);
        assert!(c.take_evicted().is_empty());
        assert_eq!(c.loaded, c.map.values().filter(|e| e.bytes > 0).count());
    }
}
