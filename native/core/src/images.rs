//! Shared image cache: fetch once per URL, decode off the UI thread into UI-neutral RGBA frames,
//! then let the frontend turn them into its own GPU objects (`T`) on the UI thread.
//!
//! Frames are shrunk at decode time to the largest box the image is drawn in (physical pixels), so an emote
//! fetched at 2x/4x costs what it is shown at, not what the CDN served. Layout keeps using the natural size.
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};

use crate::Wake;

/// A decoded image: straight-alpha RGBA frames with per-frame delays.
pub struct Decoded {
    /// Natural (source) size, for layout.
    pub w: u32,
    pub h: u32,
    /// Size of the stored frames (the natural size shrunk to the requested box).
    pub fw: u32,
    pub fh: u32,
    /// (RGBA bytes at `fw` x `fh`, delay in ms)
    pub frames: Vec<(Vec<u8>, u32)>,
    /// Bytes the frames would take at the source resolution (stats only).
    pub source_bytes: u64,
    /// Only the first frame was decoded (animations are frozen); fetched again when they are switched on.
    pub cut: bool,
}

/// A finished image plus whatever the frontend made out of it.
pub struct Img<T> {
    /// Natural size.
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

/// The box (physical pixels) an image will be drawn in at most; `(0, 0)` = no limit (keep the source size).
pub type Want = (u32, u32);

/// One cache slot: the state, what a ready image holds in memory, and when it was last asked for.
struct Entry<T> {
    state: State<T>,
    bytes: u64,
    used_ms: u64,
    /// Size of the stored frames.
    fw: u32,
    fh: u32,
    /// A bigger decode is on its way (the box grew after the first decode, e.g. a DPI change).
    upgrading: bool,
    /// Decoded as its first frame only (see `Decoded::cut`).
    cut: bool,
}

type Msg = (String, Result<Decoded, String>);

pub struct ImageCache<T> {
    map: HashMap<String, Entry<T>>,
    /// Images pushed out by the memory budget; the frontend frees their GPU side (`take_evicted`).
    evicted: Vec<Arc<Img<T>>>,
    rt: tokio::runtime::Handle,
    client: reqwest::Client,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    wake: Wake,
    sem: Arc<tokio::sync::Semaphore>,
    /// Bytes the frontend reported for its uploaded images.
    pub decoded_bytes: u64,
    /// What the same frames would take at source resolution (stats only).
    pub source_bytes: u64,
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
    /// settings.animatedEmotes is off (decodes keep the first frame only).
    pub anim_off: bool,
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

/// The size an image of `w` x `h` is stored at for the box `want`: scaled down to fit, never up.
pub fn stored_size(w: u32, h: u32, want: Want) -> (u32, u32) {
    if want.0 == 0 || want.1 == 0 || w == 0 || h == 0 {
        return (w, h);
    }
    let s = (want.0 as f32 / w as f32).min(want.1 as f32 / h as f32);
    // within 10% of the source is not worth a resample
    if s >= 0.9 {
        return (w, h);
    }
    (((w as f32 * s).round() as u32).max(1), ((h as f32 * s).round() as u32).max(1))
}

/// Shrinks one straight-alpha RGBA frame (Lanczos3 on premultiplied pixels, so transparent edges do not
/// fringe dark), back to straight alpha.
fn shrink(rgba: Vec<u8>, w: u32, h: u32, fw: u32, fh: u32) -> Vec<u8> {
    let mut pm = rgba;
    for px in pm.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a != 255 {
            px[0] = ((px[0] as u32 * a + 127) / 255) as u8;
            px[1] = ((px[1] as u32 * a + 127) / 255) as u8;
            px[2] = ((px[2] as u32 * a + 127) / 255) as u8;
        }
    }
    let src = image::RgbaImage::from_raw(w, h, pm).expect("frame size");
    let mut out = image::imageops::resize(&src, fw, fh, image::imageops::FilterType::Lanczos3).into_raw();
    for px in out.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a != 0 && a != 255 {
            px[0] = ((px[0] as u32 * 255 + a / 2) / a).min(255) as u8;
            px[1] = ((px[1] as u32 * 255 + a / 2) / a).min(255) as u8;
            px[2] = ((px[2] as u32 * 255 + a / 2) / a).min(255) as u8;
        }
    }
    out
}

/// Frames collected while decoding, already shrunk to the wanted box.
struct Acc {
    want: Want,
    frames: Vec<(Vec<u8>, u32)>,
    w: u32,
    h: u32,
    fw: u32,
    fh: u32,
    source_bytes: u64,
}

impl Acc {
    fn push(&mut self, f: image::Frame) {
        let (n, d) = f.delay().numer_denom_ms();
        let mut ms = if d == 0 { 100 } else { n / d };
        if ms == 0 {
            ms = 100;
        }
        ms = ms.max(20);
        let buf = f.into_buffer();
        let (w, h) = (buf.width(), buf.height());
        let (fw, fh) = stored_size(w, h, self.want);
        (self.w, self.h, self.fw, self.fh) = (w, h, fw, fh);
        self.source_bytes += buf.as_raw().len() as u64;
        let raw = buf.into_raw();
        self.frames.push((if (fw, fh) != (w, h) { shrink(raw, w, h, fw, fh) } else { raw }, ms));
    }
}

pub fn decode(bytes: &[u8], want: Want, one_frame: bool) -> Result<Decoded, String> {
    use image::AnimationDecoder;
    let fmt = image::guess_format(bytes).map_err(|e| e.to_string())?;
    let mut acc = Acc { want, frames: Vec::new(), w: 0, h: 0, fw: 0, fh: 0, source_bytes: 0 };
    match fmt {
        image::ImageFormat::Gif => {
            let dec = image::codecs::gif::GifDecoder::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
            for f in dec.into_frames().take(if one_frame { 1 } else { MAX_FRAMES }) {
                acc.push(f.map_err(|e| e.to_string())?);
            }
        }
        image::ImageFormat::WebP => {
            let mut dec = image::codecs::webp::WebPDecoder::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
            // A frame marked "dispose" must leave its area cleared for the next one. The decoder only clears when
            // a background colour is set, and none is by default, so every frame was drawn over the last ones
            // (animated emotes left trails). Browsers dispose to transparent, whatever colour the file names.
            if dec.has_animation() {
                let _ = dec.set_background_color(image::Rgba([0, 0, 0, 0]));
            }
            for f in dec.into_frames().take(if one_frame { 1 } else { MAX_FRAMES }) {
                acc.push(f.map_err(|e| e.to_string())?);
            }
            if acc.frames.is_empty() {
                // static WebP: the animation iterator yields nothing
                acc.push(image::Frame::new(image::load_from_memory(bytes).map_err(|e| e.to_string())?.to_rgba8()));
            }
        }
        _ => acc.push(image::Frame::new(image::load_from_memory(bytes).map_err(|e| e.to_string())?.to_rgba8())),
    }
    let Acc { mut frames, w, h, fw, fh, source_bytes, .. } = acc;
    if frames.is_empty() || w == 0 || h == 0 {
        return Err("empty image".into());
    }
    if (w as u64) * (h as u64) > MAX_PIXELS {
        return Err("image too large".into());
    }
    if frames.len() > 1 && (fw as u64) * (fh as u64) * 4 * frames.len() as u64 > MAX_IMAGE_BYTES {
        frames.truncate(1);
    }
    let cut = one_frame && matches!(fmt, image::ImageFormat::Gif | image::ImageFormat::WebP);
    Ok(Decoded { w, h, fw, fh, frames, source_bytes, cut })
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
            source_bytes: 0,
            sizes: HashMap::new(),
            loads_total: 0,
            textures: 0,
            frames: 0,
            loaded: 0,
            failed: 0,
            start: Instant::now(),
            freeze: std::env::var("AGU_NO_ANIM").map(|v| v == "1").unwrap_or(false),
            anim_off: false,
        }
    }

    pub fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    fn frozen(&self) -> bool {
        self.freeze || self.anim_off
    }

    /// Fetch + decode `url` for the box `want` on the runtime. The encoded bytes are dropped right after the decode.
    fn spawn_fetch(&self, url: &str, want: Want) {
        let one_frame = self.frozen();
        let (tx, client, wake, sem, u) = (self.tx.clone(), self.client.clone(), self.wake.clone(), self.sem.clone(), url.to_string());
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
                tokio::task::spawn_blocking(move || decode(&bytes, want, one_frame)).await.map_err(|e| e.to_string())?
            }
            .await;
            let _ = tx.send((u, res));
            wake();
        });
    }

    /// State of a URL, to be drawn inside the box `want` (physical pixels); starts the fetch on first sight and
    /// fetches again at a bigger size when a ready image was decoded smaller than it is now drawn.
    pub fn get(&mut self, url: &str, want: Want) -> &State<T> {
        let now = self.now_ms();
        let mut fetch = false;
        if let Some(e) = self.map.get_mut(url) {
            e.used_ms = now;
            if let State::Ready(img) = &e.state {
                if !e.upgrading {
                    let (tw, th) = stored_size(img.w, img.h, want);
                    let frozen = self.freeze || self.anim_off;
                    if (e.cut && !frozen) || tw as f32 > e.fw as f32 * 1.1 || th as f32 > e.fh as f32 * 1.1 {
                        e.upgrading = true;
                        fetch = true;
                    }
                }
            }
        } else {
            self.map.insert(url.to_string(), Entry { state: State::Loading, bytes: 0, used_ms: now, fw: 0, fh: 0, upgrading: false, cut: false });
            fetch = true;
        }
        if fetch {
            self.spawn_fetch(url, want);
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
                    // a bigger decode replaces the smaller one: the frontend frees the old GPU side
                    if let Some(old) = self.map.remove(&url) {
                        if let State::Ready(img) = old.state {
                            self.decoded_bytes -= old.bytes;
                            self.textures -= 1;
                            self.loaded -= 1;
                            self.frames -= img.delays.len();
                            self.evicted.push(img);
                        }
                    }
                    self.decoded_bytes += bytes;
                    self.source_bytes += d.source_bytes;
                    self.textures += 1;
                    self.frames += delays.len();
                    self.loaded += 1;
                    self.loads_total += 1;
                    self.sizes.insert(url.clone(), (d.w, d.h));
                    let now = self.now_ms();
                    self.map.insert(
                        url,
                        Entry { state: State::Ready(Arc::new(Img { w: d.w, h: d.h, delays, total_ms: total, gpu })), bytes, used_ms: now, fw: d.fw, fh: d.fh, upgrading: false, cut: d.cut },
                    );
                }
                Err(e) => {
                    crate::logf!("image failed {url}: {e}");
                    if let Some(old) = self.map.get_mut(&url).filter(|o| matches!(o.state, State::Ready(_))) {
                        // a failed bigger fetch: keep the small one
                        old.upgrading = false;
                    } else {
                        self.failed += 1;
                        let now = self.now_ms();
                        self.map.insert(url, Entry { state: State::Failed, bytes: 0, used_ms: now, fw: 0, fh: 0, upgrading: false, cut: false });
                    }
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
                    self.frames -= img.delays.len();
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
        Decoded { w: 2, h: 2, fw: 2, fh: 2, frames: (0..frames).map(|_| (vec![0u8; 16], 50)).collect(), source_bytes: 0, cut: false }
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

    #[test]
    fn decode_shrinks_to_the_wanted_box_and_keeps_the_natural_size() {
        let mut png = Vec::new();
        let im = image::RgbaImage::from_fn(112, 112, |x, _| if x < 56 { image::Rgba([255, 0, 0, 255]) } else { image::Rgba([0, 0, 255, 0]) });
        im.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let d = decode(&png, (56, 56), false).unwrap();
        assert_eq!((d.w, d.h, d.fw, d.fh), (112, 112, 56, 56));
        assert_eq!(d.frames[0].0.len(), 56 * 56 * 4);
        assert_eq!(d.source_bytes, 112 * 112 * 4);
        // red stays red and the transparent half does not fringe
        assert_eq!(&d.frames[0].0[(10 * 56 + 5) * 4..][..4], &[255, 0, 0, 255]);
        assert_eq!(d.frames[0].0[(10 * 56 + 50) * 4 + 3], 0);
        // no limit / a bigger box keeps the source
        assert_eq!(decode(&png, (0, 0), false).unwrap().fw, 112);
        assert_eq!(decode(&png, (500, 500), false).unwrap().fw, 112);
        assert_eq!(stored_size(112, 56, (28, 28)), (28, 14));
    }

    #[test]
    fn frozen_decode_keeps_the_first_frame_of_an_animation_only() {
        let mut gif = Vec::new();
        {
            let mut enc = image::codecs::gif::GifEncoder::new(&mut gif);
            for c in [255u8, 0] {
                enc.encode_frame(image::Frame::new(image::RgbaImage::from_pixel(8, 8, image::Rgba([c, 0, 0, 255])))).unwrap();
            }
        }
        let all = decode(&gif, (0, 0), false).unwrap();
        assert_eq!((all.frames.len(), all.cut), (2, false));
        let one = decode(&gif, (0, 0), true).unwrap();
        assert_eq!((one.frames.len(), one.cut), (1, true));
    }
}
