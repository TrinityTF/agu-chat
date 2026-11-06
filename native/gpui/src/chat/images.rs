//! GPUI side of the shared image cache (core's `images.rs`): RGBA frames from the decoder become BGRA
//! `RenderImage`s on the UI thread, one global clock picks the frame of an animated image, and a repaint is
//! scheduled only for the next frame change of an animated image that is actually being painted.
//! `animatedEmotes=false` (and `AGU_NO_ANIM=1`) freeze every image on its first frame.
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agu_chat_core::images::{Decoded, Img, ImageCache, State};
use gpui::{App, Global, RenderImage, Window};
use image::{Delay, Frame, RgbaImage};

use super::net::Net;

pub type GpuImg = Arc<Img<Arc<RenderImage>>>;

pub struct Images {
    cache: RefCell<ImageCache<Arc<RenderImage>>>,
    /// Set from settings.animatedEmotes.
    animate: Cell<bool>,
    /// Instant a repaint is already scheduled for, if any.
    due: Cell<Option<Instant>>,
    inboxes: RefCell<Vec<std::rc::Weak<RefCell<Vec<String>>>>>,
    pub painted_animated: Cell<usize>,
}

#[derive(Clone)]
pub struct ImagesHandle(pub Rc<Images>);
impl Global for ImagesHandle {}

fn upload(_url: &str, d: &Decoded) -> (Arc<RenderImage>, u64) {
    let mut frames = Vec::with_capacity(d.frames.len());
    let mut bytes = 0u64;
    for (rgba, ms) in &d.frames {
        // RGBA -> BGRA (what the renderer samples)
        let mut buf = rgba.clone();
        for px in buf.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        bytes += buf.len() as u64;
        let img = RgbaImage::from_raw(d.w, d.h, buf).expect("frame size");
        frames.push(Frame::from_parts(img, 0, 0, Delay::from_numer_denom_ms(*ms, 1)));
    }
    (Arc::new(RenderImage::new(frames)), bytes)
}

impl Images {
    /// Wake: called on background threads whenever a fetch finished; the returned receiver is drained by the
    /// UI task that calls `pump`.
    pub fn new(net: &Net, wake: agu_chat_core::Wake) -> Rc<Images> {
        Rc::new(Images {
            cache: RefCell::new(ImageCache::new(net.rt.handle().clone(), net.client.clone(), wake)),
            animate: Cell::new(true),
            due: Cell::new(None),
            inboxes: RefCell::new(Vec::new()),
            painted_animated: Cell::new(0),
        })
    }

    pub fn set_animate(&self, on: bool) {
        self.animate.set(on);
    }

    /// A pane's inbox of URLs that finished loading (rows that waited for their size re-layout).
    pub fn add_inbox(&self) -> Rc<RefCell<Vec<String>>> {
        let inbox = Rc::new(RefCell::new(Vec::new()));
        self.inboxes.borrow_mut().push(Rc::downgrade(&inbox));
        inbox
    }

    /// The decoded image, starting its fetch on first sight. None while loading or when it failed.
    pub fn get(&self, url: &str) -> Option<GpuImg> {
        if url.is_empty() {
            return None;
        }
        match self.cache.borrow_mut().get(url) {
            State::Ready(i) => Some(i.clone()),
            _ => None,
        }
    }

    /// The natural size for layout: known from the first decode on, also while the pixels are evicted (so an
    /// eviction never changes a row's height). Starts the fetch on first sight.
    pub fn dims(&self, url: &str) -> Option<(u32, u32)> {
        if url.is_empty() {
            return None;
        }
        if let Some(d) = self.cache.borrow().size_of(url) {
            return Some(d);
        }
        self.get(url).map(|i| (i.w, i.h))
    }

    pub fn failed(&self, url: &str) -> bool {
        matches!(self.cache.borrow().peek(url), Some(State::Failed))
    }

    /// Hand finished decodes to the GPU side; returns whether more are waiting.
    pub fn pump(&self) -> bool {
        let mut loaded: Vec<String> = Vec::new();
        let more = self.cache.borrow_mut().pump(|url, d| {
            loaded.push(url.to_string());
            upload(url, d)
        });
        if !loaded.is_empty() {
            let mut boxes = self.inboxes.borrow_mut();
            boxes.retain(|w| w.upgrade().is_some());
            for w in boxes.iter() {
                if let Some(b) = w.upgrade() {
                    b.borrow_mut().extend(loaded.iter().cloned());
                }
            }
        }
        more
    }

    /// Frame index to paint now, and note when the next change is due.
    pub fn frame(&self, img: &GpuImg, window: &mut Window, cx: &mut App) -> usize {
        self.free_evicted(window);
        let freeze = !self.animate.get() || self.cache.borrow().freeze;
        let t = self.cache.borrow().now_ms();
        let (idx, next_ms) = img.frame_at(t, freeze);
        if next_ms != u32::MAX {
            self.painted_animated.set(self.painted_animated.get() + 1);
            self.schedule(next_ms, window, cx);
        }
        idx
    }

    /// Frees the atlas tiles of images the cache evicted (their CPU buffers go with the last Arc).
    pub fn free_evicted(&self, window: &mut Window) {
        let gone = self.cache.borrow_mut().take_evicted();
        for img in gone {
            window.drop_image(img.gpu.clone()).ok();
        }
    }

    fn schedule(&self, ms: u32, window: &mut Window, cx: &mut App) {
        let due = Instant::now() + Duration::from_millis(ms as u64);
        if let Some(d) = self.due.get() {
            if d > Instant::now() && d <= due + Duration::from_millis(2) {
                return; // a repaint at (about) this time is already scheduled
            }
        }
        self.due.set(Some(due));
        let handle = window.window_handle();
        cx.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_millis(ms as u64)).await;
            let _ = cx.update(|cx| handle.update(cx, |_, window, _| window.refresh()));
        })
        .detach();
    }

    pub fn stats(&self) -> (usize, usize, u64) {
        let c = self.cache.borrow();
        (c.loads_total as usize, c.failed, c.decoded_bytes)
    }
}

/// Image sizes as the CSS fit rules draw them: scaled DOWN to fit `max_w` x `max_h` (aspect kept), never up.
pub fn fit_down(nat_w: f32, nat_h: f32, max_w: f32, max_h: f32) -> (f32, f32) {
    if nat_w <= 0.0 || nat_h <= 0.0 {
        return (max_h, max_h);
    }
    let s = (max_w / nat_w).min(max_h / nat_h).min(1.0);
    (nat_w * s, nat_h * s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_down_scales_only_down() {
        assert_eq!(fit_down(64.0, 64.0, 100.0, 25.0), (25.0, 25.0));
        assert_eq!(fit_down(20.0, 20.0, 100.0, 25.0), (20.0, 20.0));
        let (w, h) = fit_down(256.0, 64.0, 60.0, 25.0);
        assert!((w - 60.0).abs() < 0.01 && (h - 15.0).abs() < 0.01);
    }
}
