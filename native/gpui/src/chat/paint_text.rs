//! 7TV name paints drawn like Chromium draws the Electron app's CSS (`color: transparent; background-clip: text;
//! background-image: ...; filter: drop-shadow(...)`).
//!
//! GPUI can neither fill glyphs with a gradient nor draw text shadows, so a painted name is rasterised here
//! with DirectWrite + Direct2D (the stack Chromium uses on Windows): the glyphs are drawn white on a premultiplied
//! BGRA WIC bitmap with grayscale antialiasing, which gives the coverage mask. The paint is then evaluated per
//! pixel with the CSS geometry of the name's box (linear / radial, repeating or not, an image layer on top
//! with `background-size: cover`) and multiplied by the mask. Finally every `drop-shadow()` of the filter is
//! applied in order, each to the result of the previous one: the layer's alpha is blurred with a Gaussian of
//! std-dev = radius (Filter Effects: drop-shadow's length is the standard deviation, unlike box-shadow's; checked against Chromium), tinted, offset and put under the layer. The result is straight-alpha BGRA, handed to GPUI
//! as a `RenderImage`, cached per (text, paint, size, scale, weight).
//!
//! The pure parts (gradient line geometry, stop fix-up, colour lookup, shadow extents, blur) are unit tested;
//! only the glyph mask needs Windows.
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;

use gpui::RenderImage;
use image::{Delay, Frame, RgbaImage};

use super::twitch::cosmetics::{decode_rgba, SevenTvPaint};

/// Premultiplied colour (r, g, b, a), 0..1.
pub type Pm = [f32; 4];

fn premul(c: u32) -> Pm {
    let (r, g, b, a) = decode_rgba(c);
    [r * a, g * a, b * a, a]
}

// ---- CSS gradient geometry -----------------------------------------------------------------------------

/// Length of the CSS gradient line of a `w` x `h` box for `angle_deg` (0deg points up, clockwise).
pub fn gradient_line_len(angle_deg: f32, w: f32, h: f32) -> f32 {
    let a = angle_deg.to_radians();
    (w * a.sin()).abs() + (h * a.cos()).abs()
}

/// Position (0 = start point, 1 = end point) of the point `(x, y)` (box coordinates) on a linear gradient line.
pub fn linear_t(angle_deg: f32, w: f32, h: f32, x: f32, y: f32) -> f32 {
    let a = angle_deg.to_radians();
    let len = gradient_line_len(angle_deg, w, h);
    if len <= 0.0 {
        return 0.0;
    }
    let (dx, dy) = (a.sin(), -a.cos());
    ((x - w / 2.0) * dx + (y - h / 2.0) * dy) / len + 0.5
}

/// `radial-gradient(circle, ...)`: centred, `farthest-corner`, so 1.0 is the distance to the farthest corner.
pub fn radial_t(w: f32, h: f32, x: f32, y: f32) -> f32 {
    let r = ((w / 2.0).powi(2) + (h / 2.0).powi(2)).sqrt();
    if r <= 0.0 {
        return 0.0;
    }
    ((x - w / 2.0).hypot(y - h / 2.0)) / r
}

/// `radial-gradient(ellipse, ...)` (7TV's `shape: "ellipse"`, e.g. "Heavy Rain"): centred, `farthest-corner`; the
/// ellipse keeps the box's aspect ratio, so its radii are the half sides times sqrt 2.
pub fn ellipse_t(w: f32, h: f32, x: f32, y: f32) -> f32 {
    let (rx, ry) = (w / 2.0 * std::f32::consts::SQRT_2, h / 2.0 * std::f32::consts::SQRT_2);
    if rx <= 0.0 || ry <= 0.0 {
        return 0.0;
    }
    (((x - w / 2.0) / rx).powi(2) + ((y - h / 2.0) / ry).powi(2)).sqrt()
}

/// CSS colour-stop fix-up: a stop positioned before an earlier one is moved up to it.
pub fn fix_stops(stops: &[(f32, Pm)]) -> Vec<(f32, Pm)> {
    let mut out: Vec<(f32, Pm)> = Vec::with_capacity(stops.len());
    let mut max = f32::NEG_INFINITY;
    for (p, c) in stops {
        let p = p.max(max);
        max = p;
        out.push((p, *c));
    }
    out
}

fn lerp(a: Pm, b: Pm, t: f32) -> Pm {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t, a[3] + (b[3] - a[3]) * t]
}

/// The (premultiplied) colour of fixed-up `stops` at gradient position `t`. A repeating gradient tiles the
/// span between its first and last stop.
pub fn color_at(stops: &[(f32, Pm)], t: f32, repeat: bool) -> Pm {
    let (Some(first), Some(last)) = (stops.first(), stops.last()) else { return [0.0; 4] };
    let mut t = t;
    if repeat {
        let len = last.0 - first.0;
        if len > 1e-6 {
            t = first.0 + (t - first.0).rem_euclid(len);
        } else {
            return last.1;
        }
    }
    if t < first.0 {
        return first.1;
    }
    for w in stops.windows(2) {
        if t < w[1].0 {
            let span = w[1].0 - w[0].0;
            if span <= 1e-9 {
                continue;
            }
            return lerp(w[0].1, w[1].1, (t - w[0].0) / span);
        }
    }
    last.1
}

// ---- the paint as layers --------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum Underlay {
    Linear { angle: f32, repeat: bool, stops: Vec<(f32, Pm)> },
    Radial { repeat: bool, ellipse: bool, stops: Vec<(f32, Pm)> },
    Solid(Pm),
}

/// `paintStyle`'s underlay: the gradient of the paint's function, else its solid colour.
pub fn underlay_of(p: &SevenTvPaint) -> Option<Underlay> {
    let stops: Vec<(f32, Pm)> = fix_stops(&p.stops.iter().map(|s| (s.at, premul(s.color))).collect::<Vec<_>>());
    if p.function == "LINEAR_GRADIENT" && !stops.is_empty() {
        return Some(Underlay::Linear { angle: p.angle, repeat: p.repeat, stops });
    }
    if p.function == "RADIAL_GRADIENT" && !stops.is_empty() {
        return Some(Underlay::Radial { repeat: p.repeat, ellipse: p.shape == "ellipse", stops });
    }
    p.color.map(|c| Underlay::Solid(premul(c)))
}

impl Underlay {
    fn at(&self, w: f32, h: f32, x: f32, y: f32) -> Pm {
        match self {
            Underlay::Linear { angle, repeat, stops } => color_at(stops, linear_t(*angle, w, h, x, y), *repeat),
            Underlay::Radial { repeat, ellipse, stops } => color_at(stops, if *ellipse { ellipse_t(w, h, x, y) } else { radial_t(w, h, x, y) }, *repeat),
            Underlay::Solid(c) => *c,
        }
    }
}

/// Everything about a paint that decides how a name looks (the cache key part).
#[derive(Clone, Debug)]
pub struct PaintSpec {
    pub id: String,
    pub underlay: Option<Underlay>,
    pub image_url: Option<String>,
    pub shadows: Vec<(f32, f32, f32, Pm)>,
}

impl PaintSpec {
    /// None for a paint that would draw nothing (no colour, gradient or image): the name keeps its colour.
    pub fn from_paint(p: &SevenTvPaint) -> Option<PaintSpec> {
        let underlay = underlay_of(p);
        if underlay.is_none() && p.image_url.is_none() {
            return None;
        }
        Some(PaintSpec {
            id: p.id.clone(),
            underlay,
            image_url: p.image_url.clone(),
            shadows: p.shadows.iter().map(|s| (s.x, s.y, s.radius, premul(s.color))).collect(),
        })
    }
}

/// Room the `filter: drop-shadow()` chain needs around the box: (left, top, right, bottom) in CSS px. Each shadow
/// is a shifted, blurred (3 sigma = 3 x radius) copy of everything before it.
pub fn shadow_extents(shadows: &[(f32, f32, f32, Pm)]) -> (f32, f32, f32, f32) {
    let (mut l, mut t, mut r, mut b) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for (dx, dy, radius, _) in shadows {
        let g = (radius.max(0.0) * 3.0).ceil();
        let (nl, nt, nr, nb) = ((l - dx + g).max(l), (t - dy + g).max(t), (r + dx + g).max(r), (b + dy + g).max(b));
        l = nl;
        t = nt;
        r = nr;
        b = nb;
    }
    (l, t, r, b)
}

/// Normalised 1-D Gaussian kernel (radius = ceil(3 sigma)).
pub fn gaussian_kernel(sigma: f32) -> Vec<f32> {
    if sigma < 0.05 {
        return vec![1.0];
    }
    let r = (sigma * 3.0).ceil() as i32;
    let mut k: Vec<f32> = (-r..=r).map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp()).collect();
    let s: f32 = k.iter().sum();
    k.iter_mut().for_each(|v| *v /= s);
    k
}

/// Separable Gaussian blur of a single `w` x `h` plane (zero outside).
pub fn blur_plane(src: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    let k = gaussian_kernel(sigma);
    if k.len() == 1 {
        return src.to_vec();
    }
    let r = (k.len() / 2) as i32;
    let mut tmp = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (i, kv) in k.iter().enumerate() {
                let xx = x as i32 + i as i32 - r;
                if xx >= 0 && (xx as usize) < w {
                    acc += src[y * w + xx as usize] * kv;
                }
            }
            tmp[y * w + x] = acc;
        }
    }
    let mut out = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (i, kv) in k.iter().enumerate() {
                let yy = y as i32 + i as i32 - r;
                if yy >= 0 && (yy as usize) < h {
                    acc += tmp[yy as usize * w + x] * kv;
                }
            }
            out[y * w + x] = acc;
        }
    }
    out
}

/// `filter: drop-shadow(dx dy radius color)` applied to a premultiplied RGBA layer (device px values).
pub fn drop_shadow(layer: &mut [Pm], w: usize, h: usize, dx: i32, dy: i32, sigma: f32, color: Pm) {
    let alpha: Vec<f32> = layer.iter().map(|p| p[3]).collect();
    let blurred = blur_plane(&alpha, w, h, sigma);
    for y in 0..h {
        for x in 0..w {
            // the shadow at (x, y) is the blurred alpha at (x - dx, y - dy)
            let (sx, sy) = (x as i32 - dx, y as i32 - dy);
            if sx < 0 || sy < 0 || sx >= w as i32 || sy >= h as i32 {
                continue;
            }
            let a = blurred[sy as usize * w + sx as usize];
            if a <= 0.0 {
                continue;
            }
            let p = &mut layer[y * w + x];
            let inv = 1.0 - p[3];
            for c in 0..4 {
                p[c] += color[c] * a * inv;
            }
        }
    }
}

// ---- the image layer --------------------------------------------------------------------------------------

/// A decoded paint image: straight-alpha BGRA pixels.
#[derive(Clone)]
pub struct PaintImage {
    pub w: u32,
    pub h: u32,
    pub bgra: Arc<Vec<u8>>,
}

/// The image layer as the 7TV extension styles it (`background-size: 100% 100%`), resampled to the box in
/// device px.
struct CoverImage {
    w: u32,
    h: u32,
    px: Vec<Pm>,
}

fn cover(img: &PaintImage, box_w_dev: f32, box_h_dev: f32) -> Option<CoverImage> {
    if img.w == 0 || img.h == 0 || box_w_dev < 1.0 || box_h_dev < 1.0 {
        return None;
    }
    // the 7TV extension sizes the image layer `100% 100%`: stretched over the whole box, aspect not kept
    let (tw, th) = (box_w_dev.round().max(1.0) as u32, box_h_dev.round().max(1.0) as u32);
    // premultiplied RGBA for filtering
    let mut rgba = Vec::with_capacity(img.bgra.len());
    for p in img.bgra.chunks_exact(4) {
        let a = p[3] as u32;
        rgba.extend_from_slice(&[((p[2] as u32 * a + 127) / 255) as u8, ((p[1] as u32 * a + 127) / 255) as u8, ((p[0] as u32 * a + 127) / 255) as u8, p[3]]);
    }
    let src = RgbaImage::from_raw(img.w, img.h, rgba)?;
    let scaled = if (tw, th) == (img.w, img.h) { src } else { image::imageops::resize(&src, tw, th, image::imageops::FilterType::CatmullRom) };
    let px = scaled.pixels().map(|p| [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0, p[3] as f32 / 255.0]).collect();
    Some(CoverImage { w: tw, h: th, px })
}

// ---- rendering ---------------------------------------------------------------------------------------------

/// Direct2D's grayscale coverage is a bit heavier than Skia's text mask (measured against Chromium on the same
/// names): thin the partial coverage slightly.
const MASK_GAMMA: f32 = 1.8;

pub struct NameReq<'a> {
    pub text: &'a str,
    /// CSS px
    pub size: f32,
    pub weight: u16,
    pub scale: f32,
    /// the name's box: its advance width and the extents above / below the baseline (CSS px)
    pub box_w: f32,
    pub above: f32,
    pub below: f32,
    pub paint: &'a PaintSpec,
    /// A piece of a longer painted span (a /me message): (CSS px from the span's paint box to this piece's text
    /// origin, the box's width). CSS paints an inline span that wraps as one strip with its line pieces laid end
    /// to end (`box-decoration-break: slice`), so each word shows its own slice of one gradient. None: the paint
    /// box is the text's own (`box_w`).
    pub strip: Option<(f32, f32)>,
    /// The paint image's frame to draw (animated paints) and how to get its pixels (only called on a cache miss).
    pub image: Option<(usize, &'a dyn Fn() -> Option<PaintImage>)>,
}

pub struct Rendered {
    pub w: u32,
    pub h: u32,
    /// straight-alpha BGRA
    pub bgra: Vec<u8>,
    /// device px from the image's left / top edge to the text origin / the baseline
    pub origin_x: i32,
    pub baseline_y: i32,
    /// DirectWrite's advance width of the text (CSS px), for comparison with GPUI's own shaping
    pub dwrite_w: f32,
}

pub fn render(req: &NameReq) -> Option<Rendered> {
    let scale = req.scale.max(0.5);
    let (el, et, er, eb) = shadow_extents(&req.paint.shadows);
    // glyphs overhang their advance box a little (and AA fringes)
    let (pl, pt, pr, pb) = ((el + 3.0).ceil(), (et + 3.0).ceil(), (er + 3.0).ceil(), (eb + 3.0).ceil());
    let origin_x = (pl * scale).round() as i32;
    let baseline_y = ((pt + req.above) * scale).round() as i32;
    let w = (((pl + req.box_w + pr) * scale).ceil() as i32).max(1) as usize;
    let h = (((pt + req.above + req.below + pb) * scale).ceil() as i32).max(1) as usize;
    if w * h > 4_000_000 {
        return None;
    }
    let mask = cached_mask(req.text, req.size * scale, req.weight, w as u32, h as u32, origin_x as f32, baseline_y as f32)?;

    // the box the paint is evaluated in, in CSS px relative to its top-left; device px (x + .5) maps into it
    let box_top_dev = baseline_y as f32 - req.above * scale;
    let (sx, bw) = req.strip.unwrap_or((0.0, req.box_w));
    let bh = req.above + req.below;
    let cov = req.image.and_then(|(_, get)| get()).and_then(|i| cover(&i, bw * scale, bh * scale));
    let mut layer: Vec<Pm> = vec![[0.0; 4]; w * h];
    for y in 0..h {
        for x in 0..w {
            let m = (mask.0[y * w + x] as f32 / 255.0).powf(MASK_GAMMA);
            if m <= 0.0 {
                continue;
            }
            let (bx_dev, by_dev) = (x as f32 - origin_x as f32 + sx * scale, y as f32 - box_top_dev);
            let (cx, cy) = ((bx_dev + 0.5) / scale, (by_dev + 0.5) / scale);
            let mut c: Pm = req.paint.underlay.as_ref().map(|u| u.at(bw, bh, cx, cy)).unwrap_or([0.0; 4]);
            if let Some(ci) = &cov {
                let (ix, iy) = (bx_dev.floor() as i32, by_dev.floor() as i32);
                if ix >= 0 && iy >= 0 && (ix as u32) < ci.w && (iy as u32) < ci.h {
                    let p = ci.px[iy as usize * ci.w as usize + ix as usize];
                    let inv = 1.0 - p[3];
                    c = [p[0] + c[0] * inv, p[1] + c[1] * inv, p[2] + c[2] * inv, p[3] + c[3] * inv];
                }
            }
            layer[y * w + x] = [c[0] * m, c[1] * m, c[2] * m, c[3] * m];
        }
    }
    for (dx, dy, radius, color) in &req.paint.shadows {
        drop_shadow(&mut layer, w, h, (dx * scale).round() as i32, (dy * scale).round() as i32, radius * scale, *color);
    }
    let mut bgra = Vec::with_capacity(w * h * 4);
    for p in &layer {
        let a = p[3].clamp(0.0, 1.0);
        if a <= 0.0 {
            bgra.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            let q = |v: f32| ((v / a).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            bgra.extend_from_slice(&[q(p[2]), q(p[1]), q(p[0]), (a * 255.0 + 0.5) as u8]);
        }
    }
    Some(Rendered { w: w as u32, h: h as u32, bgra, origin_x, baseline_y, dwrite_w: mask.1 / scale })
}

type MaskKey = (String, u32, u16, u32, u32, i32, i32);

thread_local! {
    static MASKS: RefCell<std::collections::HashMap<MaskKey, Rc<(Vec<u8>, f32)>>> = RefCell::new(Default::default());
}

/// `glyph_mask` memoised: an animated paint re-renders a name per frame but its glyphs never change.
fn cached_mask(text: &str, size_px: f32, weight: u16, w: u32, h: u32, ox: f32, by: f32) -> Option<Rc<(Vec<u8>, f32)>> {
    let key: MaskKey = (text.to_string(), (size_px * 100.0) as u32, weight, w, h, ox as i32, by as i32);
    if let Some(m) = MASKS.with(|c| c.borrow().get(&key).cloned()) {
        return Some(m);
    }
    let m = Rc::new(glyph_mask(text, size_px, weight, w, h, ox, by)?);
    MASKS.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() >= 64 {
            c.clear();
        }
        c.insert(key, m.clone());
    });
    Some(m)
}

/// White glyphs' coverage (0..255) on a `w` x `h` canvas plus DirectWrite's advance width in CSS px
/// (in device px).
#[cfg(windows)]
fn glyph_mask(text: &str, size_px: f32, weight: u16, w: u32, h: u32, origin_x: f32, baseline_y: f32) -> Option<(Vec<u8>, f32)> {
    use windows::core::w as wstr;
    use windows::Win32::Graphics::Direct2D::Common::*;
    use windows::Win32::Graphics::Direct2D::*;
    use windows::Win32::Graphics::DirectWrite::*;
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
    use windows::Win32::Graphics::Imaging::*;
    use windows::Win32::System::Com::*;

    struct Fx {
        d2d: ID2D1Factory,
        dw: IDWriteFactory,
        wic: IWICImagingFactory,
    }
    thread_local! {
        static FX: RefCell<Option<Option<Fx>>> = const { RefCell::new(None) };
    }
    FX.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            let make = || -> windows::core::Result<Fx> {
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                    Ok(Fx {
                        d2d: D2D1CreateFactory::<ID2D1Factory>(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?,
                        dw: DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED)?,
                        wic: CoCreateInstance::<_, IWICImagingFactory>(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?,
                    })
                }
            };
            *slot = Some(make().ok());
        }
        let fx = slot.as_ref()?.as_ref()?;
        let run = || -> windows::core::Result<(Vec<u8>, f32)> {
            unsafe {
                let bmp = fx.wic.CreateBitmap(w, h, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnDemand)?;
                let props = D2D1_RENDER_TARGET_PROPERTIES {
                    r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                    pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    usage: D2D1_RENDER_TARGET_USAGE_NONE,
                    minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
                };
                let rt = fx.d2d.CreateWicBitmapRenderTarget(&bmp, &props)?;
                rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
                let fmt = fx.dw.CreateTextFormat(wstr!("Segoe UI"), None, DWRITE_FONT_WEIGHT(weight as i32), DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size_px, wstr!("en-us"))?;
                fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                let wide: Vec<u16> = text.encode_utf16().collect();
                let layout = fx.dw.CreateTextLayout(&wide, &fmt, 10000.0, 10000.0)?;
                let mut tm = DWRITE_TEXT_METRICS::default();
                layout.GetMetrics(&mut tm)?;
                let mut lm = [DWRITE_LINE_METRICS::default(); 4];
                let mut n = 0u32;
                let _ = layout.GetLineMetrics(Some(&mut lm), &mut n);
                let base = lm[0].baseline;
                let brush = rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }, None)?;
                rt.BeginDraw();
                rt.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
                rt.DrawTextLayout(D2D_POINT_2F { x: origin_x, y: baseline_y - base }, &layout, &brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
                rt.EndDraw(None, None)?;
                let lock = bmp.Lock(&WICRect { X: 0, Y: 0, Width: w as i32, Height: h as i32 }, WICBitmapLockRead.0 as u32)?;
                let stride = lock.GetStride()? as usize;
                let mut len = 0u32;
                let mut ptr = std::ptr::null_mut();
                lock.GetDataPointer(&mut len, &mut ptr)?;
                let data = std::slice::from_raw_parts(ptr, len as usize);
                let mut out = vec![0u8; (w * h) as usize];
                for y in 0..h as usize {
                    for x in 0..w as usize {
                        out[y * w as usize + x] = data[y * stride + x * 4 + 3];
                    }
                }
                Ok((out, tm.widthIncludingTrailingWhitespace))
            }
        };
        run().ok()
    })
}

#[cfg(not(windows))]
fn glyph_mask(_: &str, _: f32, _: u16, _: u32, _: u32, _: f32, _: f32) -> Option<(Vec<u8>, f32)> {
    None
}

// ---- cache -----------------------------------------------------------------------------------------------

/// Straight-alpha BGRA pixels as a GPUI image.
pub fn to_render_image(r: &Rendered) -> Option<Arc<RenderImage>> {
    let img = RgbaImage::from_raw(r.w, r.h, r.bgra.clone())?;
    Some(Arc::new(RenderImage::new(vec![Frame::from_parts(img, 0, 0, Delay::from_numer_denom_ms(0, 1))])))
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    text: String,
    paint: String,
    size: u32,
    scale: u32,
    weight: u16,
    above: i32,
    below: i32,
    box_w: u32,
    strip: Option<(i32, u32)>,
    image: Option<usize>,
}

/// A rendered name ready to paint.
#[derive(Clone)]
pub struct NameImage {
    pub image: Arc<RenderImage>,
    pub w: u32,
    pub h: u32,
    pub origin_x: i32,
    pub baseline_y: i32,
}

/// Rendered names kept (an animated paint keeps one per visited frame): by count and by pixel bytes.
const CACHE_MAX: usize = 400;
const CACHE_MAX_BYTES: usize = 12 << 20;

struct Cache {
    map: std::collections::HashMap<Key, Option<NameImage>>,
    order: VecDeque<Key>,
    /// evicted images whose atlas tiles the next paint frees
    dropped: Vec<Arc<RenderImage>>,
    bytes: usize,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache { map: Default::default(), order: VecDeque::new(), dropped: Vec::new(), bytes: 0 });
}

/// The painted name, from the cache or freshly rendered. None when rendering is not possible (the caller draws
/// the plain per-glyph fallback).
pub fn name_image(req: &NameReq) -> Option<NameImage> {
    let key = Key {
        text: req.text.to_string(),
        paint: req.paint.id.clone(),
        size: (req.size * 100.0) as u32,
        scale: (req.scale * 100.0) as u32,
        weight: req.weight,
        above: (req.above * 64.0) as i32,
        below: (req.below * 64.0) as i32,
        box_w: (req.box_w * 64.0) as u32,
        strip: req.strip.map(|(x, w)| ((x * 64.0) as i32, (w * 64.0) as u32)),
        image: req.image.map(|(f, _)| f),
    };
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if let Some(v) = c.map.get(&key) {
            let v = v.clone();
            if let Some(pos) = c.order.iter().position(|k| *k == key) {
                let k = c.order.remove(pos).unwrap();
                c.order.push_back(k);
            }
            return v;
        }
        let ni = render(req).and_then(|r| {
            if std::env::var_os("AGU_LOG_PAINT").is_some() {
                eprintln!("[paint] {:?} dwrite_w={:.2} box_w={:.2} img={}x{}", req.text, r.dwrite_w, req.box_w, r.w, r.h);
            }
            to_render_image(&r).map(|image| NameImage { image, w: r.w, h: r.h, origin_x: r.origin_x, baseline_y: r.baseline_y })
        });
        c.bytes += ni.as_ref().map(|n| (n.w * n.h * 4) as usize).unwrap_or(0);
        c.map.insert(key.clone(), ni.clone());
        c.order.push_back(key);
        while c.order.len() > CACHE_MAX || (c.bytes > CACHE_MAX_BYTES && c.order.len() > 1) {
            if let Some(old) = c.order.pop_front() {
                if let Some(Some(v)) = c.map.remove(&old) {
                    c.bytes = c.bytes.saturating_sub((v.w * v.h * 4) as usize);
                    c.dropped.push(v.image);
                }
            }
        }
        ni
    })
}

/// Images evicted from the cache since the last call (for `Window::drop_image`).
pub fn take_dropped() -> Vec<Arc<RenderImage>> {
    CACHE.with(|c| std::mem::take(&mut c.borrow_mut().dropped))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stops(v: &[(f32, [f32; 4])]) -> Vec<(f32, Pm)> {
        v.to_vec()
    }

    #[test]
    fn gradient_line_length_matches_css() {
        // 0deg / 180deg: the height; 90deg: the width; 45deg on a square: the diagonal
        assert!((gradient_line_len(0.0, 100.0, 20.0) - 20.0).abs() < 1e-4);
        assert!((gradient_line_len(90.0, 100.0, 20.0) - 100.0).abs() < 1e-4);
        assert!((gradient_line_len(180.0, 100.0, 20.0) - 20.0).abs() < 1e-4);
        assert!((gradient_line_len(45.0, 10.0, 10.0) - 14.142136).abs() < 1e-3);
        // CSS example: 100x20 box at 45deg -> (100 + 20) * sin 45
        assert!((gradient_line_len(45.0, 100.0, 20.0) - 120.0 * 0.70710678).abs() < 1e-3);
    }

    #[test]
    fn linear_t_runs_from_corner_to_corner() {
        // 90deg: left edge 0, right edge 1, middle .5
        assert!((linear_t(90.0, 100.0, 20.0, 0.0, 5.0)).abs() < 1e-5);
        assert!((linear_t(90.0, 100.0, 20.0, 100.0, 5.0) - 1.0).abs() < 1e-5);
        assert!((linear_t(90.0, 100.0, 20.0, 50.0, 0.0) - 0.5).abs() < 1e-5);
        // 180deg: top 0, bottom 1
        assert!((linear_t(180.0, 100.0, 20.0, 30.0, 0.0)).abs() < 1e-5);
        assert!((linear_t(180.0, 100.0, 20.0, 30.0, 20.0) - 1.0).abs() < 1e-5);
        // 45deg: the bottom-left corner is the start, the top-right the end ("magic corners")
        assert!((linear_t(45.0, 100.0, 20.0, 0.0, 20.0)).abs() < 1e-5);
        assert!((linear_t(45.0, 100.0, 20.0, 100.0, 0.0) - 1.0).abs() < 1e-5);
        // 0deg: bottom 0, top 1
        assert!((linear_t(0.0, 100.0, 20.0, 10.0, 20.0)).abs() < 1e-5);
        assert!((linear_t(0.0, 100.0, 20.0, 10.0, 0.0) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn radial_is_farthest_corner() {
        assert!(radial_t(100.0, 20.0, 50.0, 10.0).abs() < 1e-6);
        assert!((radial_t(100.0, 20.0, 0.0, 0.0) - 1.0).abs() < 1e-5);
        assert!((radial_t(100.0, 20.0, 100.0, 20.0) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn ellipse_is_farthest_corner_with_the_box_aspect() {
        assert!(ellipse_t(100.0, 20.0, 50.0, 10.0).abs() < 1e-6);
        // every corner is at 1.0, the side midpoints at 1/sqrt 2
        assert!((ellipse_t(100.0, 20.0, 0.0, 0.0) - 1.0).abs() < 1e-5);
        assert!((ellipse_t(100.0, 20.0, 100.0, 20.0) - 1.0).abs() < 1e-5);
        assert!((ellipse_t(100.0, 20.0, 100.0, 10.0) - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-5);
        assert!((ellipse_t(100.0, 20.0, 50.0, 0.0) - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-5);
    }

    #[test]
    fn stops_are_fixed_up_and_looked_up() {
        let red = [1.0, 0.0, 0.0, 1.0];
        let blue = [0.0, 0.0, 1.0, 1.0];
        let s = fix_stops(&stops(&[(0.5, red), (0.2, blue)]));
        assert_eq!(s[1].0, 0.5, "an earlier position is raised to the previous stop");
        // hard stop: at the boundary the later colour wins
        assert_eq!(color_at(&s, 0.5, false), blue);
        assert_eq!(color_at(&s, 0.49, false), red);
        // before the first / after the last stop the end colours are held
        assert_eq!(color_at(&s, 0.0, false), red);
        assert_eq!(color_at(&s, 3.0, false), blue);
        // interpolation is linear
        let g = stops(&[(0.0, red), (1.0, blue)]);
        let mid = color_at(&g, 0.25, false);
        assert!((mid[0] - 0.75).abs() < 1e-5 && (mid[2] - 0.25).abs() < 1e-5);
    }

    #[test]
    fn repeating_tiles_first_to_last_stop() {
        let red = [1.0, 0.0, 0.0, 1.0];
        let blue = [0.0, 0.0, 1.0, 1.0];
        let g = stops(&[(0.0, red), (0.35, blue)]);
        let a = color_at(&g, 0.1, true);
        let b = color_at(&g, 0.1 + 0.35, true);
        let c = color_at(&g, 0.1 + 0.7, true);
        for i in 0..4 {
            assert!((a[i] - b[i]).abs() < 1e-4 && (a[i] - c[i]).abs() < 1e-4);
        }
        // not at 0: the pattern starts at the first stop
        let h = stops(&[(0.2, red), (0.4, blue)]);
        let x = color_at(&h, 0.3, true);
        let y = color_at(&h, 0.5, true);
        for i in 0..4 {
            assert!((x[i] - y[i]).abs() < 1e-4);
        }
    }

    #[test]
    fn shadow_extents_stack() {
        let c = [0.0; 4];
        assert_eq!(shadow_extents(&[]), (0.0, 0.0, 0.0, 0.0));
        // 2px blur -> 6px reach; a +2/+3 offset shifts the reach right / down
        let e = shadow_extents(&[(2.0, 3.0, 2.0, c)]);
        assert_eq!(e, (4.0, 3.0, 8.0, 9.0));
        // a second shadow reaches out of the first one's reach
        let e2 = shadow_extents(&[(0.0, 0.0, 1.0, c), (0.0, 0.0, 1.0, c)]);
        assert_eq!(e2, (6.0, 6.0, 6.0, 6.0));
    }

    #[test]
    fn blur_keeps_energy_and_spreads() {
        let mut p = vec![0.0f32; 21 * 21];
        p[10 * 21 + 10] = 1.0;
        let b = blur_plane(&p, 21, 21, 2.0);
        let sum: f32 = b.iter().sum();
        assert!((sum - 1.0).abs() < 1e-3);
        assert!(b[10 * 21 + 10] < 0.2 && b[10 * 21 + 10] > b[10 * 21 + 12]);
    }

    #[test]
    fn drop_shadow_goes_under_the_layer() {
        let (w, h) = (9, 9);
        let mut l = vec![[0.0f32; 4]; w * h];
        l[4 * w + 4] = [1.0, 0.0, 0.0, 1.0];
        drop_shadow(&mut l, w, h, 2, 0, 0.0, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(l[4 * w + 4], [1.0, 0.0, 0.0, 1.0], "the layer stays on top");
        assert_eq!(l[4 * w + 6], [0.0, 0.0, 1.0, 1.0], "the shadow lands dx to the right");
    }

    #[test]
    fn paint_spec_from_css_function() {
        use crate::chat::twitch::cosmetics::PaintStop;
        let mut p = SevenTvPaint { function: "LINEAR_GRADIENT".into(), angle: 90.0, stops: vec![PaintStop { at: 0.0, color: 0xFF0000FF }, PaintStop { at: 1.0, color: 0x0000FFFF }], ..Default::default() };
        assert!(matches!(PaintSpec::from_paint(&p).unwrap().underlay, Some(Underlay::Linear { .. })));
        p.function = "RADIAL_GRADIENT".into();
        assert!(matches!(PaintSpec::from_paint(&p).unwrap().underlay, Some(Underlay::Radial { .. })));
        let solid = SevenTvPaint { function: "URL".into(), color: Some(0x00FF00FF), ..Default::default() };
        assert!(matches!(PaintSpec::from_paint(&solid).unwrap().underlay, Some(Underlay::Solid(_))));
        assert!(PaintSpec::from_paint(&SevenTvPaint::default()).is_none());
    }
}
