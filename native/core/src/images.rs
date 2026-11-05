//! Shared image cache: fetch once per URL, decode off the UI thread into UI-neutral RGBA frames,
//! then let the frontend turn them into its own GPU objects (`T`) on the UI thread.
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};

use crate::net::Wake;

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

pub struct ImageCache<T> {
    map: HashMap<String, State<T>>,
    rt: tokio::runtime::Handle,
    client: reqwest::Client,
    tx: Sender<(String, Result<Decoded, String>)>,
    rx: Receiver<(String, Result<Decoded, String>)>,
    wake: Wake,
    sem: Arc<tokio::sync::Semaphore>,
    /// Bytes the frontend reported for its uploaded images.
    pub decoded_bytes: u64,
    /// GPU image objects (one per image).
    pub textures: usize,
    pub frames: usize,
    pub loaded: usize,
    pub failed: usize,
    pub start: Instant,
    pub freeze: bool,
}

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
    Ok(Decoded { w, h, frames })
}

impl<T> ImageCache<T> {
    pub fn new(rt: tokio::runtime::Handle, client: reqwest::Client, wake: Wake) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        ImageCache {
            map: HashMap::new(),
            rt,
            client,
            tx,
            rx,
            wake,
            sem: Arc::new(tokio::sync::Semaphore::new(12)),
            decoded_bytes: 0,
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
        if !self.map.contains_key(url) {
            self.map.insert(url.to_string(), State::Loading);
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
        &self.map[url]
    }

    pub fn peek(&self, url: &str) -> Option<&State<T>> {
        self.map.get(url)
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
                    self.map.insert(url, State::Ready(Arc::new(Img { w: d.w, h: d.h, delays, total_ms: total, gpu })));
                }
                Err(e) => {
                    crate::logf!("image failed {url}: {e}");
                    self.failed += 1;
                    self.map.insert(url, State::Failed);
                }
            }
            if t0.elapsed().as_millis() >= 4 {
                return !self.rx.is_empty();
            }
        }
        false
    }
}
