//! Shared image cache: fetch once per URL, decode off the UI thread, upload frames as egui textures.
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};
use egui::{ColorImage, Context, TextureHandle, TextureOptions};

pub struct Img {
    pub w: u32,
    pub h: u32,
    /// One atlas texture holding every frame in a grid (1px gutters).
    pub tex: TextureHandle,
    pub cols: u32,
    pub rows: u32,
    pub delays: Vec<u32>,
    pub total_ms: u32,
}

impl Img {
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

pub enum State {
    Loading,
    Failed,
    Ready(Arc<Img>),
}

struct Decoded {
    w: u32,
    h: u32,
    frames: Vec<(Vec<u8>, u32)>,
}

pub struct ImageCache {
    map: HashMap<String, State>,
    rt: tokio::runtime::Handle,
    client: reqwest::Client,
    tx: Sender<(String, Result<Decoded, String>)>,
    rx: Receiver<(String, Result<Decoded, String>)>,
    ctx: Context,
    sem: Arc<tokio::sync::Semaphore>,
    pub decoded_bytes: u64,
    pub textures: usize,
    pub frames: usize,
    pub loaded: usize,
    pub failed: usize,
    pub start: Instant,
    pub freeze: bool,
}

const MAX_FRAMES: usize = 150;
const MAX_PIXELS: u64 = 512 * 512;

fn decode(bytes: &[u8]) -> Result<Decoded, String> {
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

impl ImageCache {
    pub fn new(rt: tokio::runtime::Handle, client: reqwest::Client, ctx: Context) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        ImageCache {
            map: HashMap::new(),
            rt,
            client,
            tx,
            rx,
            ctx,
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
    pub fn get(&mut self, url: &str) -> &State {
        if !self.map.contains_key(url) {
            self.map.insert(url.to_string(), State::Loading);
            let (tx, client, ctx, sem, u) =
                (self.tx.clone(), self.client.clone(), self.ctx.clone(), self.sem.clone(), url.to_string());
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
                ctx.request_repaint();
            });
        }
        &self.map[url]
    }

    pub fn peek(&self, url: &str) -> Option<&State> {
        self.map.get(url)
    }

    /// Upload finished decodes as textures (bounded per frame). Returns true if more are waiting.
    pub fn pump(&mut self) -> bool {
        let t0 = Instant::now();
        while let Ok((url, res)) = self.rx.try_recv() {
            match res {
                Ok(d) => {
                    let (w, h) = (d.w as usize, d.h as usize);
                    let n = d.frames.len();
                    let (cw, ch) = (w + 2, h + 2);
                    let cols = (2048 / cw).max(1).min(n);
                    let rows = n.div_ceil(cols);
                    let (aw, ah) = (cols * cw, rows * ch);
                    let mut px = vec![0u8; aw * ah * 4];
                    let mut delays = Vec::with_capacity(n);
                    let mut total = 0;
                    for (i, (rgba, ms)) in d.frames.iter().enumerate() {
                        let (c, r) = (i % cols, i / cols);
                        for y in 0..h {
                            let dst = ((r * ch + 1 + y) * aw + c * cw + 1) * 4;
                            px[dst..dst + w * 4].copy_from_slice(&rgba[y * w * 4..(y + 1) * w * 4]);
                        }
                        total += *ms;
                        delays.push(*ms);
                    }
                    let ci = ColorImage::from_rgba_unmultiplied([aw, ah], &px);
                    let tex = self.ctx.load_texture(url.clone(), ci, TextureOptions::LINEAR);
                    self.decoded_bytes += (aw * ah * 4) as u64;
                    self.textures += 1;
                    self.frames += n;
                    self.loaded += 1;
                    self.map.insert(
                        url,
                        State::Ready(Arc::new(Img { w: d.w, h: d.h, tex, cols: cols as u32, rows: rows as u32, delays, total_ms: total })),
                    );
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

impl Img {
    /// UV rectangle of frame `i` inside the atlas.
    pub fn uv(&self, i: usize) -> egui::Rect {
        let (cw, ch) = (self.w + 2, self.h + 2);
        let (aw, ah) = (self.cols * cw, self.rows * ch);
        let (c, r) = (i as u32 % self.cols, i as u32 / self.cols);
        let (x0, y0) = (c * cw + 1, r * ch + 1);
        egui::Rect::from_min_max(
            egui::pos2(x0 as f32 / aw as f32, y0 as f32 / ah as f32),
            egui::pos2((x0 + self.w) as f32 / aw as f32, (y0 + self.h) as f32 / ah as f32),
        )
    }
}
