//! The inline text flow a chat row is made of. GPUI has no inline layout of text mixed with images, so this is
//! one custom element that does what the browser did for the Electron rows: break a sequence of text words,
//! emotes, badges, icons, pills and buttons into lines of a given width (CSS `white-space: normal`,
//! `overflow-wrap: break-word`), align everything to a shared baseline with CSS' half-leading and
//! `vertical-align: middle` rules, and paint it. Interactive parts (links, mod buttons, emote tooltips) get
//! hitboxes, so hover and click work like the DOM's.
//!
//! Layout is cached per width (and per image-size epoch), shaped text is kept between frames, so a visible row
//! costs a few quads, glyph runs and image sprites per frame.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{
    point, px, relative, size, AnyElement, App, Bounds, CursorStyle, DispatchPhase, Element, ElementId, FontWeight, GlobalElementId, Hitbox, HitboxBehavior,
    Hsla, InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Point, ShapedLine, SharedString, Size, Style,
    StrikethroughStyle, TextRun, TransformationMatrix, Window,
};

use super::images::{fit_down, ImagesHandle};
use super::paint_text::{self, NameReq, PaintImage, PaintSpec};
use super::twitch::parts::split_keep_ws;
use super::PxF;
use crate::theme;

/// Measured (pixel diff against the Electron app): GPUI's DirectWrite glyph baseline lands this many px lower than
/// Chromium's for the same layout baseline, so text is painted this much higher.
pub const TEXT_DY: f32 = -1.0;

/// Segoe UI's x-height / em, for `vertical-align: middle`.
pub const X_HEIGHT: f32 = 0.5;

#[derive(Clone)]
pub enum Act {
    Url(SharedString),
    Tag(u32),
}

/// An emote tooltip (emoteTip.tsx).
#[derive(Clone)]
pub struct Tip {
    pub img: SharedString,
    pub name: SharedString,
    pub label: SharedString,
}

#[derive(Clone, Copy, PartialEq)]
pub enum VAlign {
    /// `vertical-align: middle`: box centre at the baseline raised by half the parent's x-height.
    Middle,
    /// centred on the text content box (a flex item with `align-self: center` in a text-height flex line)
    BoxCenter,
    /// box bottom sits `down` px below the baseline
    BottomAt(f32),
}

#[derive(Clone)]
pub struct TextItem {
    pub text: SharedString,
    pub size: f32,
    pub color: Hsla,
    pub weight: FontWeight,
    pub italic: bool,
    pub strike: bool,
    /// per-character colours across the text (7TV paints)
    pub gradient: Option<Rc<dyn Fn(f32) -> Hsla>>,
    /// a 7TV paint drawn as a whole (gradient / image fill, drop-shadows) by `paint_text`; `gradient` is the fallback
    pub paint: Option<Rc<PaintSpec>>,
    pub act: Option<Act>,
    pub underline_on_hover: bool,
}

impl TextItem {
    pub fn new(text: impl Into<SharedString>, size: f32, color: Hsla) -> Self {
        TextItem { text: text.into(), size, color, weight: FontWeight::NORMAL, italic: false, strike: false, gradient: None, paint: None, act: None, underline_on_hover: false }
    }
    pub fn weight(mut self, w: FontWeight) -> Self {
        self.weight = w;
        self
    }
    pub fn strike(mut self, s: bool) -> Self {
        self.strike = s;
        self
    }
    pub fn italic(mut self, s: bool) -> Self {
        self.italic = s;
        self
    }
    pub fn act(mut self, a: Act, underline: bool) -> Self {
        self.act = Some(a);
        self.underline_on_hover = underline;
        self
    }
}

#[derive(Clone)]
pub struct ImageItem {
    pub url: SharedString,
    /// the box a natural-size image is scaled down into
    pub max_w: f32,
    pub max_h: f32,
    /// exactly this size, whatever the natural size (badges)
    pub fixed: bool,
    /// the image sits in a wrapper whose height is always `max_h` (EmotePart's span); a bare <img> is as tall as it draws
    pub wrap: bool,
    pub vmargin: f32,
    pub margin_x: f32,
    pub valign: VAlign,
    pub tip: Option<Rc<Tip>>,
    pub overlays: Vec<SharedString>,
    pub radius: f32,
    pub act: Option<Act>,
}

#[derive(Clone)]
pub struct IconItem {
    pub path: &'static str,
    pub size: f32,
    pub color: Hsla,
    pub valign: VAlign,
    pub margin_x: f32,
}

#[derive(Clone)]
pub struct PillItem {
    pub text: SharedString,
    pub size: f32,
    pub color: Hsla,
    pub weight: FontWeight,
    pub bg: Hsla,
    pub pad_x: f32,
    pub margin_r: f32,
    pub radius: f32,
}

#[derive(Clone)]
pub enum ButtonFace {
    Icon(&'static str, f32),
    Label(SharedString, f32),
}

#[derive(Clone)]
pub struct ButtonItem {
    pub face: ButtonFace,
    pub w: f32,
    pub h: f32,
    pub color: Hsla,
    pub hover_color: Hsla,
    pub hover_bg: Hsla,
    pub down: f32,
    pub radius: f32,
    pub margin_r: f32,
    pub act: Act,
    pub tip: Option<SharedString>,
}

#[derive(Clone)]
pub struct GifItem {
    pub url: SharedString,
}

#[derive(Clone)]
pub enum Item {
    Text(TextItem),
    Image(ImageItem),
    Icon(IconItem),
    Pill(PillItem),
    Button(ButtonItem),
    Gif(GifItem),
    /// fixed horizontal gap
    Gap(f32),
}

pub struct InlineContent {
    pub items: Vec<Item>,
    /// the row's font size (the strut) and unitless line-height
    pub base_size: f32,
    pub line_ratio: f32,
    /// Item ranges painted as ONE span: a 7TV paint on a /me message runs across all its words (and the gaps and
    /// emotes between them) like CSS's background on a wrapping inline span, instead of restarting per word.
    pub spans: Vec<(usize, usize)>,
    cache: RefCell<Option<Laid>>,
    /// (width, epoch, height) of the last measure that did not keep its layout: a row that is only measured
    /// (off screen) needs its height, not the shaped words.
    height_memo: Cell<Option<(f32, u64, f32)>>,
    /// When the layout was last used for painting (ms, `clock_ms`); `sweep_layouts` drops the ones idle for long.
    used_ms: Cell<u64>,
    tracked: Cell<bool>,
}

fn clock_ms() -> u64 {
    static T0: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    T0.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64 + 1
}

/// Layouts to sweep (rows paint a lot less than they exist: only the ones on screen need their shaped words).
const LAYOUT_IDLE_MS: u64 = 2_000;

thread_local! {
    static LAID: RefCell<Vec<std::rc::Weak<InlineContent>>> = const { RefCell::new(Vec::new()) };
    static LAID_ADDS: Cell<u32> = const { Cell::new(0) };
}

/// Remember that `c` holds a layout and, every so often, drop the layouts of rows that have not been painted
/// for a while (they are laid out again when they come back on screen; shaped lines are cached by the text system).
fn track_layout(c: &Rc<InlineContent>) {
    c.used_ms.set(clock_ms());
    if !c.tracked.replace(true) {
        LAID.with(|l| l.borrow_mut().push(Rc::downgrade(c)));
        let n = LAID_ADDS.with(|a| {
            a.set(a.get().wrapping_add(1));
            a.get()
        });
        if n % 64 == 0 {
            let now = clock_ms();
            LAID.with(|l| {
                l.borrow_mut().retain(|w| match w.upgrade() {
                    None => false,
                    Some(c) if now.saturating_sub(c.used_ms.get()) > LAYOUT_IDLE_MS => {
                        *c.cache.borrow_mut() = None;
                        c.tracked.set(false);
                        false
                    }
                    Some(_) => true,
                });
            });
        }
    }
}

impl InlineContent {
    pub fn new(items: Vec<Item>, base_size: f32, line_ratio: f32) -> Rc<InlineContent> {
        Self::with_spans(items, base_size, line_ratio, Vec::new())
    }

    pub fn with_spans(items: Vec<Item>, base_size: f32, line_ratio: f32, spans: Vec<(usize, usize)>) -> Rc<InlineContent> {
        Rc::new(InlineContent { items, base_size, line_ratio, spans, cache: RefCell::new(None), height_memo: Cell::new(None), used_ms: Cell::new(0), tracked: Cell::new(false) })
    }

    /// Every image URL the flow draws (rows re-layout when one of them finishes loading).
    pub fn urls(&self) -> Vec<String> {
        let mut v = Vec::new();
        for it in &self.items {
            match it {
                Item::Image(i) => {
                    v.push(i.url.to_string());
                    v.extend(i.overlays.iter().map(|o| o.to_string()));
                }
                Item::Gif(g) => v.push(g.url.to_string()),
                Item::Text(t) => {
                    if let Some(u) = t.paint.as_ref().and_then(|p| p.image_url.as_ref()) {
                        v.push(u.clone());
                    }
                }
                _ => {}
            }
        }
        v
    }

    /// The flow's height at a width (the DOM block's content height), cached like the element's own layout.
    pub fn height_at(&self, width: f32, window: &Window, cx: &App) -> f32 {
        let imgs = cx.global::<ImagesHandle>().clone();
        let epoch = epoch_of(cx);
        if let Some(l) = self.cache.borrow().as_ref().filter(|l| (l.width - width).abs() < 0.01 && l.epoch == epoch) {
            return l.height;
        }
        if let Some((w, e, h)) = self.height_memo.get() {
            if (w - width).abs() < 0.01 && e == epoch {
                return h;
            }
        }
        let h = layout(self, width, window, &imgs, epoch).height;
        self.height_memo.set(Some((width, epoch, h)));
        h
    }

    /// Forget the cached layout (an image size became known).
    pub fn invalidate(&self) {
        *self.cache.borrow_mut() = None;
        self.height_memo.set(None);
    }
}

// ---- layout -----------------------------------------------------------------------------------

#[derive(Clone)]
enum AtomKind {
    Word { shaped: ShapedLine, item: usize, size: f32, hl: f32, asc: f32 },
    Space,
    Box { item: usize },
    Block { item: usize },
}

#[derive(Clone)]
struct Atom {
    kind: AtomKind,
    w: f32,
    /// extents above / below the baseline
    above: f32,
    below: f32,
    /// a break opportunity right before this atom even without whitespace
    break_before: bool,
    /// for Box/Block: size actually drawn
    dims: (f32, f32),
}

struct LineInfo {
    y: f32,
    baseline: f32,
    height: f32,
}

#[derive(Clone)]
struct Placed {
    atom: Atom,
    x: f32,
    line: usize,
}

struct Laid {
    width: f32,
    /// the widest line (for shrink-to-fit flows)
    used_w: f32,
    epoch: u64,
    height: f32,
    lines: Vec<LineInfo>,
    placed: Vec<Placed>,
}

fn text_font(weight: FontWeight, italic: bool) -> gpui::Font {
    let mut f = gpui::font(theme::FONT);
    f.weight = weight;
    if italic {
        f.style = gpui::FontStyle::Italic;
    }
    f
}

fn shape(window: &Window, t: &TextItem, text: &str, char_off: usize, total_chars: usize) -> ShapedLine {
    let font = text_font(t.weight, t.italic);
    let strike = t.strike.then(|| StrikethroughStyle { thickness: px(1.), color: Some(t.color) });
    let runs: Vec<TextRun> = match &t.gradient {
        Some(g) => text
            .char_indices()
            .enumerate()
            .map(|(i, (_, c))| TextRun {
                len: c.len_utf8(),
                font: font.clone(),
                color: g((char_off + i) as f32 / total_chars.max(1) as f32 + 0.5 / total_chars.max(1) as f32),
                background_color: None,
                underline: None,
                strikethrough: strike,
            })
            .collect(),
        None => vec![TextRun { len: text.len(), font, color: t.color, background_color: None, underline: None, strikethrough: strike }],
    };
    window.text_system().shape_line(SharedString::from(text.to_string()), px(t.size), &runs, None)
}

/// Blink's line metrics for a font size: ascent and descent are rounded to whole pixels, the leading
/// (line-height - content) is split with the half rounded DOWN above the text (`FontHeight::AddLeading`), the
/// rest goes below. Returns (above baseline, below baseline).
fn floor64(v: f32) -> f32 {
    (v * 64.0).floor() / 64.0
}

fn blink_above_below(asc: f32, desc: f32, line_h: f32) -> (f32, f32) {
    let line_h = floor64(line_h);
    let (a, d) = (asc.round(), desc.round());
    let leading = line_h - (a + d);
    let above = a + (leading / 2.0).floor();
    (above, line_h - above)
}

fn word_atom(window: &Window, items: &[Item], item: usize, text: &str, char_off: usize, ratio: f32) -> Atom {
    let Item::Text(t) = &items[item] else { unreachable!() };
    let total = t.text.chars().count();
    let shaped = shape(window, t, text, char_off, total);
    let (asc, desc) = (shaped.ascent.f(), shaped.descent.f());
    let hl = (ratio * t.size - (asc + desc)) / 2.0;
    let (above, below) = blink_above_below(asc, desc, ratio * t.size);
    Atom { w: shaped.width.f(), above, below, kind: AtomKind::Word { shaped, item, size: t.size, hl, asc }, break_before: false, dims: (0.0, 0.0) }
}

fn space_atom(window: &Window, t: &TextItem, ratio: f32) -> Atom {
    let shaped = shape(window, t, " ", 0, 1);
    let (asc, desc) = (shaped.ascent.f(), shaped.descent.f());
    let (above, below) = blink_above_below(asc, desc, ratio * t.size);
    Atom { w: shaped.width.f(), above, below, kind: AtomKind::Space, break_before: false, dims: (0.0, 0.0) }
}

/// Centre of a box relative to the baseline (negative = above), per vertical-align rule.
fn box_center(v: VAlign, h: f32, base_size: f32, ratio: f32, window: &Window) -> f32 {
    match v {
        VAlign::Middle => -X_HEIGHT * base_size / 2.0,
        VAlign::BoxCenter => {
            let (asc, desc) = base_metrics(window, base_size);
            let (above, below) = blink_above_below(asc, desc, ratio * base_size);
            // centre of [baseline-above, baseline+below]
            (below - above) / 2.0
        }
        VAlign::BottomAt(down) => down - h / 2.0,
    }
}

fn base_metrics(window: &Window, size: f32) -> (f32, f32) {
    let font = text_font(FontWeight::NORMAL, false);
    let s = window.text_system().shape_line(SharedString::from("x"), px(size), &[TextRun { len: 1, font, color: Hsla::default(), background_color: None, underline: None, strikethrough: None }], None);
    (s.ascent.f(), s.descent.f())
}

fn image_dims(i: &ImageItem, imgs: &ImagesHandle, scale: f32) -> (f32, f32) {
    if i.fixed {
        return (i.max_w, i.max_h);
    }
    match imgs.0.dims(&i.url, i.max_w, i.max_h, scale) {
        Some((w, h)) => fit_down(w as f32, h as f32, i.max_w, i.max_h),
        // size unknown until it loads: a square box of the emote height
        None => (i.max_h, i.max_h),
    }
}

fn box_atom(window: &Window, items: &[Item], item: usize, content: &InlineContent, imgs: &ImagesHandle, max_w: f32) -> Atom {
    let (bs, ratio) = (content.base_size, content.line_ratio);
    match &items[item] {
        Item::Image(i) => {
            let (w, h) = image_dims(i, imgs, window.scale_factor());
            let mh = if i.wrap { i.max_h } else { h } + 2.0 * i.vmargin;
            let c = box_center(i.valign, mh, bs, ratio, window);
            Atom { w: w + 2.0 * i.margin_x, above: mh / 2.0 - c, below: mh / 2.0 + c, kind: AtomKind::Box { item }, break_before: true, dims: (w, h) }
        }
        Item::Icon(i) => {
            let c = box_center(i.valign, i.size, bs, ratio, window);
            Atom { w: i.size + 2.0 * i.margin_x, above: i.size / 2.0 - c, below: i.size / 2.0 + c, kind: AtomKind::Box { item }, break_before: false, dims: (i.size, i.size) }
        }
        Item::Pill(p) => {
            let shaped = shape(window, &TextItem { text: p.text.clone(), size: p.size, color: p.color, weight: p.weight, italic: false, strike: false, gradient: None, paint: None, act: None, underline_on_hover: false }, &p.text, 0, 1);
            let (asc, desc) = (shaped.ascent.f(), shaped.descent.f());
            Atom { w: shaped.width.f() + 2.0 * p.pad_x + p.margin_r, above: asc.round(), below: desc.round(), kind: AtomKind::Box { item }, break_before: false, dims: (shaped.width.f() + 2.0 * p.pad_x, asc + desc) }
        }
        Item::Button(b) => {
            let c = box_center(VAlign::BottomAt(b.down), b.h, bs, ratio, window);
            Atom { w: b.w + b.margin_r, above: b.h / 2.0 - c, below: b.h / 2.0 + c, kind: AtomKind::Box { item }, break_before: true, dims: (b.w, b.h) }
        }
        Item::Gap(w) => Atom { w: *w, above: 0.0, below: 0.0, kind: AtomKind::Box { item }, break_before: false, dims: (*w, 0.0) },
        Item::Gif(g) => {
            // `my-1 block max-h-40 max-w-full rounded object-contain`
            let (w, h) = match imgs.0.get(&g.url, max_w.max(240.0), 160.0, window.scale_factor()) {
                Some(img) => fit_down(img.w as f32, img.h as f32, max_w, 160.0),
                None => (0.0, 0.0),
            };
            Atom { w: max_w, above: h + 4.0, below: 4.0, kind: AtomKind::Block { item }, break_before: true, dims: (w, h) }
        }
        Item::Text(_) => unreachable!(),
    }
}

fn layout(content: &InlineContent, width: f32, window: &Window, imgs: &ImagesHandle, epoch: u64) -> Laid {
    let items = &content.items;
    let ratio = content.line_ratio;
    let (sa, sd) = base_metrics(window, content.base_size);
    let (strut_above, strut_below) = blink_above_below(sa, sd, ratio * content.base_size);

    // 1. atoms
    let mut atoms: Vec<Atom> = Vec::new();
    let mut last_was_space = true; // leading whitespace is dropped
    for (idx, it) in items.iter().enumerate() {
        match it {
            Item::Text(t) => {
                let mut off = 0usize;
                let mut first = true;
                for tok in split_keep_ws(&t.text) {
                    let n = tok.chars().count();
                    if tok.chars().all(|c| c.is_whitespace()) {
                        if !last_was_space {
                            atoms.push(space_atom(window, t, ratio));
                            last_was_space = true;
                        }
                    } else {
                        let mut a = word_atom(window, items, idx, tok, off, ratio);
                        a.break_before = first && false;
                        atoms.push(a);
                        last_was_space = false;
                    }
                    off += n;
                    first = false;
                }
            }
            other => {
                let a = box_atom(window, items, idx, content, imgs, width);
                // a Gap is not content: it doesn't end a collapsed-space run
                if !matches!(other, Item::Gap(_)) {
                    last_was_space = false;
                }
                atoms.push(a);
            }
        }
    }

    // 2. lines
    let max_w = width.max(1.0);
    let mut lines: Vec<(Vec<(Atom, f32)>, f32)> = vec![(Vec::new(), 0.0)];
    let mut pending_space: Option<Atom> = None;
    let mut i = 0;
    while i < atoms.len() {
        // a run: atoms with no break opportunity between them
        if matches!(atoms[i].kind, AtomKind::Space) {
            if pending_space.is_none() {
                pending_space = Some(atoms[i].clone());
            }
            i += 1;
            continue;
        }
        let mut j = i + 1;
        if matches!(atoms[i].kind, AtomKind::Word { .. }) {
            while j < atoms.len() && matches!(atoms[j].kind, AtomKind::Word { .. }) && !atoms[j].break_before {
                j += 1;
            }
        } else if matches!(atoms[i].kind, AtomKind::Box { .. }) && matches!(items[match atoms[i].kind { AtomKind::Box { item } => item, _ => 0 }], Item::Gap(_)) {
            // a gap sticks to what follows
            while j < atoms.len() && !matches!(atoms[j].kind, AtomKind::Space) && !atoms[j].break_before {
                j += 1;
            }
        }
        let run: Vec<Atom> = atoms[i..j].to_vec();
        let run_w: f32 = run.iter().map(|a| a.w).sum();
        let is_block = matches!(run[0].kind, AtomKind::Block { .. });
        let cur_empty = lines.last().unwrap().0.is_empty();
        let space_w = pending_space.as_ref().map(|s| s.w).unwrap_or(0.0);
        let cur_x = lines.last().unwrap().1;
        if is_block {
            if !cur_empty {
                lines.push((Vec::new(), 0.0));
            }
            let l = lines.last_mut().unwrap();
            l.0.push((run[0].clone(), 0.0));
            l.1 = max_w;
            lines.push((Vec::new(), 0.0));
        } else if cur_empty || cur_x + space_w + run_w <= max_w + 0.01 {
            let l = lines.last_mut().unwrap();
            let mut x = l.1;
            if !cur_empty {
                if let Some(s) = pending_space.take() {
                    l.0.push((s.clone(), x));
                    x += s.w;
                }
            }
            if cur_empty && run_w > max_w && run.len() == 1 && matches!(run[0].kind, AtomKind::Word { .. }) {
                // overflow-wrap: break-word: a word wider than the line breaks wherever it fits
                let mut rest = run[0].clone();
                loop {
                    let AtomKind::Word { shaped, item, .. } = &rest.kind else { break };
                    let item = *item;
                    let text = shaped.text.to_string();
                    let Item::Text(t) = &items[item] else { break };
                    let mut cut = 0usize;
                    let mut acc = 0.0f32;
                    for (bi, ch) in text.char_indices() {
                        let wch = shape(window, t, &text[bi..bi + ch.len_utf8()], 0, 1).width.f();
                        if acc + wch > max_w && cut > 0 {
                            break;
                        }
                        acc += wch;
                        cut = bi + ch.len_utf8();
                    }
                    if cut >= text.len() {
                        let l = lines.last_mut().unwrap();
                        l.1 = acc;
                        l.0.push((rest.clone(), 0.0));
                        break;
                    }
                    let head = word_atom(window, items, item, &text[..cut], 0, ratio);
                    let tail = word_atom(window, items, item, &text[cut..], 0, ratio);
                    let l = lines.last_mut().unwrap();
                    l.0.push((head, 0.0));
                    l.1 = acc;
                    lines.push((Vec::new(), 0.0));
                    rest = tail;
                }
            } else {
                let l = lines.last_mut().unwrap();
                for a in run {
                    let w = a.w;
                    l.0.push((a, x));
                    x += w;
                }
                l.1 = x;
            }
            pending_space = None;
        } else {
            // does not fit: wrap (the space at the break is dropped)
            pending_space = None;
            lines.push((Vec::new(), 0.0));
            continue; // retry this run on the new line
        }
        i = j;
    }

    // 3. vertical metrics
    let mut out_lines: Vec<LineInfo> = Vec::new();
    let mut placed: Vec<Placed> = Vec::new();
    let mut y = 0.0f32;
    let n_lines = lines.len();
    for (li, (atoms_in_line, _)) in lines.into_iter().enumerate() {
        // an empty trailing line (after a block, or the initial one) takes no height
        if atoms_in_line.is_empty() && (li + 1 == n_lines || li > 0) && !(li == 0 && n_lines == 1) {
            if li + 1 == n_lines {
                continue;
            }
        }
        let (mut above, mut below) = (strut_above, strut_below);
        let mut only_block = !atoms_in_line.is_empty();
        for (a, _) in &atoms_in_line {
            if !matches!(a.kind, AtomKind::Block { .. }) {
                only_block = false;
            }
        }
        if only_block {
            above = 0.0;
            below = 0.0;
        }
        for (a, _) in &atoms_in_line {
            above = above.max(a.above);
            below = below.max(a.below);
        }
        let h = above + below;
        let baseline = y + above;
        let line_idx = out_lines.len();
        out_lines.push(LineInfo { y, baseline, height: h });
        for (a, x) in atoms_in_line {
            placed.push(Placed { atom: a, x, line: line_idx });
        }
        y += h;
    }
    if std::env::var_os("AGU_DEBUG_LAYOUT").is_some() && (out_lines.len() > 1 || (y - 18.85).abs() > 0.01) {
        let desc: Vec<String> = out_lines.iter().map(|l| format!("{:.2}/{:.2}", l.y, l.height)).collect();
        eprintln!("[layout] w={width:.1} h={y:.2} lines={}", desc.join(" "));
    }
    let used_w = placed.iter().map(|p| p.x + p.atom.w).fold(0.0f32, f32::max);
    Laid { width, used_w, epoch, height: y, lines: out_lines, placed }
}

// ---- the element -----------------------------------------------------------------------------

/// Hover tooltip callback: Some((tip, window position)) while an emote is hovered, None when it stops.
pub type OnTip = Rc<dyn Fn(Option<(Rc<Tip>, Point<Pixels>)>, &mut Window, &mut App)>;
/// The clicked part's rectangle (window coordinates) comes with it: a user card opens beside the name.
pub type OnAct = Rc<dyn Fn(&Act, Bounds<Pixels>, &mut Window, &mut App)>;

pub struct InlineFlow {
    content: Rc<InlineContent>,
    on_act: Option<OnAct>,
    on_tip: Option<OnTip>,
    /// shared by every flow of a pane: (tooltip showing, an image was hit by the current mouse move)
    hovering_tip: Rc<Cell<(bool, bool)>>,
    fit: bool,
}

pub fn inline_flow(content: Rc<InlineContent>) -> InlineFlow {
    InlineFlow { content, on_act: None, on_tip: None, hovering_tip: Rc::new(Cell::new((false, false))), fit: false }
}

impl InlineFlow {
    /// Shrink to the content's width (an inline span among flex items) instead of filling the parent.
    pub fn fit(mut self) -> Self {
        self.fit = true;
        self
    }
    pub fn on_act(mut self, f: OnAct) -> Self {
        self.on_act = Some(f);
        self
    }
    pub fn on_tip(mut self, f: OnTip, state: Rc<Cell<(bool, bool)>>) -> Self {
        self.on_tip = Some(f);
        self.hovering_tip = state;
        self
    }
}

impl IntoElement for InlineFlow {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

pub struct Prepaint {
    hitboxes: Vec<(usize, Hitbox)>,
}

fn epoch_of(cx: &App) -> u64 {
    cx.try_global::<ImagesHandle>().map(|h| h.0.stats().0 as u64).unwrap_or(0)
}

impl Element for InlineFlow {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _id: Option<&GlobalElementId>, _ins: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let content = self.content.clone();
        let imgs = cx.global::<ImagesHandle>().clone();
        let mut style = Style::default();
        let fit = self.fit;
        if !fit {
            style.size.width = relative(1.).into();
        }
        let id = window.request_measured_layout(style, move |known, avail, window, cx| {
            let w = known.width.map(|w| w.f()).unwrap_or_else(|| match avail.width {
                gpui::AvailableSpace::Definite(w) => w.f(),
                _ => 100000.0,
            });
            let epoch = epoch_of(cx);
            track_layout(&content);
            let mut c = content.cache.borrow_mut();
            let reuse = matches!(&*c, Some(l) if (l.width - w).abs() < 0.01 && l.epoch == epoch);
            if !reuse {
                *c = Some(layout(&content, w, window, &imgs, epoch));
            }
            let l = c.as_ref().unwrap();
            size(px(if fit { l.used_w.min(w).ceil() } else { w.min(l.width) }), px(l.height))
        });
        (id, ())
    }

    fn prepaint(&mut self, _id: Option<&GlobalElementId>, _ins: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _rl: &mut (), window: &mut Window, cx: &mut App) -> Prepaint {
        let imgs = cx.global::<ImagesHandle>().clone();
        let epoch = epoch_of(cx);
        let w = bounds.size.width.f();
        track_layout(&self.content);
        let mut c = self.content.cache.borrow_mut();
        let reuse = matches!(&*c, Some(l) if (l.width - w).abs() < 0.01 && l.epoch == epoch);
        if !reuse {
            *c = Some(layout(&self.content, w, window, &imgs, epoch));
        }
        let laid = c.as_ref().unwrap();
        let mut hitboxes = Vec::new();
        for (pi, p) in laid.placed.iter().enumerate() {
            let item = match &p.atom.kind {
                AtomKind::Word { item, .. } | AtomKind::Box { item } | AtomKind::Block { item } => *item,
                AtomKind::Space => continue,
            };
            let interactive = match &self.content.items[item] {
                Item::Text(t) => t.act.is_some() && matches!(p.atom.kind, AtomKind::Word { .. }),
                Item::Image(i) => i.tip.is_some() || i.act.is_some(),
                Item::Button(_) => true,
                _ => false,
            };
            if interactive {
                let r = atom_rect(&self.content, laid, p, bounds.origin, w, &imgs, window);
                hitboxes.push((pi, window.insert_hitbox(r, HitboxBehavior::Normal)));
            }
        }
        Prepaint { hitboxes }
    }

    fn paint(&mut self, _id: Option<&GlobalElementId>, _ins: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _rl: &mut (), pp: &mut Prepaint, window: &mut Window, cx: &mut App) {
        let imgs = cx.global::<ImagesHandle>().clone();
        self.content.used_ms.set(clock_ms());
        let c = self.content.cache.borrow();
        let Some(laid) = c.as_ref() else { return };
        let hover_of = |pi: usize, window: &Window| pp.hitboxes.iter().find(|(i, _)| *i == pi).map(|(_, h)| h.is_hovered(window)).unwrap_or(false);
        let origin = bounds.origin;
        let bs = self.content.base_size;
        let strips = span_strips(&self.content.spans, laid);
        for (pi, p) in laid.placed.iter().enumerate() {
            let line = &laid.lines[p.line];
            let x0 = origin.x.f() + p.x;
            match &p.atom.kind {
                AtomKind::Space => {}
                AtomKind::Word { shaped, item, hl, asc, size: sz } => {
                    let line_h = self.content.line_ratio * *sz;
                    let base_abs = (origin.y.f() + line.baseline).round();
                    let top = base_abs - (*hl + *asc) + TEXT_DY;
                    let painted = match &self.content.items[*item] {
                        Item::Text(t) if t.paint.is_some() && (strips.contains_key(&pi) || shaped.text.as_ref() == t.text.as_ref()) => {
                            paint_name(window, cx, &imgs, t, &shaped.text, strips.get(&pi).copied(), p.atom.w, (*asc, shaped.descent.f()), line_h, x0, base_abs)
                        }
                        _ => false,
                    };
                    if !painted {
                        let _ = shaped.paint(point(px(x0), px(top)), px(line_h), window, cx);
                    }
                    if let Item::Text(t) = &self.content.items[*item] {
                        if t.underline_on_hover && hover_of(pi, window) {
                            let y = base_abs + 1.5;
                            window.paint_quad(gpui::fill(Bounds::new(point(px(x0), px(y)), size(px(p.atom.w), px(1.))), t.color));
                        }
                    }
                }
                AtomKind::Box { item } => {
                    let r = atom_rect(&self.content, laid, p, origin, bounds.size.width.f(), &imgs, window);
                    match &self.content.items[*item] {
                        Item::Image(i) => {
                            let draw = |url: &str, r: Bounds<Pixels>, window: &mut Window, cx: &mut App| {
                                if let Some(img) = imgs.0.get(url, i.max_w, i.max_h, window.scale_factor()) {
                                    let idx = imgs.0.frame(&img, window, cx);
                                    let radius = px(i.radius);
                                    let _ = window.paint_image(r, gpui::Corners::all(radius), img.gpu.clone(), idx, false);
                                }
                            };
                            draw(&i.url, r, window, cx);
                            for o in &i.overlays {
                                // same fit as the base so a zero-width overlay keeps sitting on top of it
                                if let Some(img) = imgs.0.get(o, i.max_w, i.max_h, window.scale_factor()) {
                                    let (ow, oh) = fit_down(img.w as f32, img.h as f32, i.max_w, i.max_h);
                                    let c = r.center();
                                    let or = Bounds::new(point(c.x - px(ow / 2.0), c.y - px(oh / 2.0)), size(px(ow), px(oh)));
                                    draw(o, or, window, cx);
                                }
                            }
                        }
                        Item::Icon(i) => {
                            let _ = window.paint_svg(r, SharedString::from(i.path), TransformationMatrix::unit(), i.color, cx);
                        }
                        Item::Pill(pl) => {
                            let mut q = gpui::fill(r, pl.bg);
                            q.corner_radii = gpui::Corners::all(px(pl.radius));
                            window.paint_quad(q);
                            let ti = TextItem { text: pl.text.clone(), size: pl.size, color: pl.color, weight: pl.weight, italic: false, strike: false, gradient: None, paint: None, act: None, underline_on_hover: false };
                            let shaped = shape(window, &ti, &pl.text, 0, 1);
                            let (asc, desc) = (shaped.ascent.f(), shaped.descent.f());
                            let hl = (self.content.line_ratio * pl.size - (asc + desc)) / 2.0;
                            let _ = shaped.paint(point(r.origin.x + px(pl.pad_x), px((origin.y.f() + line.baseline).round() - hl - asc + TEXT_DY)), px(self.content.line_ratio * pl.size), window, cx);
                        }
                        Item::Button(b) => {
                            let hovered = hover_of(pi, window);
                            if hovered {
                                let mut q = gpui::fill(r, b.hover_bg);
                                q.corner_radii = gpui::Corners::all(px(b.radius));
                                window.paint_quad(q);
                            }
                            let col = if hovered { b.hover_color } else { b.color };
                            match &b.face {
                                ButtonFace::Icon(path, s) => {
                                    let ir = Bounds::new(point(r.origin.x + (r.size.width - px(*s)) / 2.0, r.origin.y + (r.size.height - px(*s)) / 2.0), size(px(*s), px(*s)));
                                    let _ = window.paint_svg(ir, SharedString::from(*path), TransformationMatrix::unit(), col, cx);
                                }
                                ButtonFace::Label(text, fs) => {
                                    let ti = TextItem { text: text.clone(), size: *fs, color: col, weight: FontWeight::BOLD, italic: false, strike: false, gradient: None, paint: None, act: None, underline_on_hover: false };
                                    let shaped = shape(window, &ti, text, 0, 1);
                                    // leading-none: centre the glyph box in the button
                                    let (asc, desc) = (shaped.ascent.f(), shaped.descent.f());
                                    let top = r.origin.y + (r.size.height - px(asc + desc)) / 2.0 + px(TEXT_DY);
                                    let left = r.origin.x + (r.size.width - shaped.width) / 2.0;
                                    let _ = shaped.paint(point(left, top), px(asc + desc), window, cx);
                                }
                            }
                        }
                        Item::Gap(_) | Item::Gif(_) | Item::Text(_) => {}
                    }
                }
                AtomKind::Block { item } => {
                    if let Item::Gif(g) = &self.content.items[*item] {
                        let (w, h) = p.atom.dims;
                        if let Some(img) = imgs.0.get(&g.url, p.atom.w.max(240.0), 160.0, window.scale_factor()) {
                            let idx = imgs.0.frame(&img, window, cx);
                            let r = Bounds::new(point(px(x0), px(origin.y.f() + line.y + 4.0)), size(px(w), px(h)));
                            let _ = window.paint_image(r, gpui::Corners::all(px(4.)), img.gpu.clone(), idx, false);
                        }
                    }
                }
            }
        }
        let _ = bs;

        // interaction
        for (pi, hb) in &pp.hitboxes {
            let p = &laid.placed[*pi];
            let item = match &p.atom.kind {
                AtomKind::Word { item, .. } | AtomKind::Box { item } | AtomKind::Block { item } => *item,
                AtomKind::Space => continue,
            };
            let act = match &self.content.items[item] {
                Item::Text(t) => t.act.clone(),
                Item::Image(i) => i.act.clone(),
                Item::Button(b) => Some(b.act.clone()),
                _ => None,
            };
            if act.is_some() {
                window.set_cursor_style(CursorStyle::PointingHand, hb);
            }
            if let (Some(act), Some(cb)) = (act, self.on_act.clone()) {
                let hb = hb.clone();
                let rect = atom_rect(&self.content, laid, p, origin, bounds.size.width.f(), &imgs, window);
                window.on_mouse_event(move |ev: &MouseDownEvent, phase, window, cx| {
                    if phase == DispatchPhase::Bubble && ev.button == MouseButton::Left && hb.is_hovered(window) {
                        cb(&act, rect, window, cx);
                        cx.stop_propagation();
                    }
                });
            }
        }
        if let Some(on_tip) = self.on_tip.clone() {
            let tips: Vec<(Hitbox, Rc<Tip>)> = pp
                .hitboxes
                .iter()
                .filter_map(|(pi, hb)| {
                    let p = &laid.placed[*pi];
                    let AtomKind::Box { item } = &p.atom.kind else { return None };
                    match &self.content.items[*item] {
                        Item::Image(i) => i.tip.clone().map(|t| (hb.clone(), t)),
                        _ => None,
                    }
                })
                .collect();
            // Every row's flow registers this, sharing one state. A row that is not hovered must not clear a tip
            // another row just set in the same event (it used to: the tip then showed only when the hovered row's
            // handler happened to run last). So each move first resets "hit" (capture runs before any bubble),
            // a hovered image sets it, and the tip is cleared only after the event, if nothing was hit.
            if !tips.is_empty() || self.hovering_tip.get().0 {
                let state = self.hovering_tip.clone();
                window.on_mouse_event(move |ev: &MouseMoveEvent, phase, window, cx| {
                    if phase == DispatchPhase::Capture {
                        state.set((state.get().0, false));
                        return;
                    }
                    if let Some((_, t)) = tips.iter().find(|(h, _)| h.is_hovered(window)) {
                        state.set((true, true));
                        on_tip(Some((t.clone(), ev.position)), window, cx);
                    } else if state.get().0 {
                        let (state, on_tip) = (state.clone(), on_tip.clone());
                        window.defer(cx, move |window, cx| {
                            if state.get() == (true, false) {
                                state.set((false, false));
                                on_tip(None, window, cx);
                            }
                        });
                    }
                });
            }
        }
    }
}

/// Where each word of a painted span sits on the span's strip: placed index -> (offset of its text origin, strip
/// width). The strip is the span's line pieces (first to last word / box of the span on each line) laid end to
/// end, like Blink's background on a wrapping inline (`box-decoration-break: slice`).
fn span_strips(spans: &[(usize, usize)], laid: &Laid) -> std::collections::HashMap<usize, (f32, f32)> {
    let mut out = std::collections::HashMap::new();
    for &(a, b) in spans {
        let in_span = |p: &Placed| match &p.atom.kind {
            AtomKind::Word { item, .. } | AtomKind::Box { item } => (a..b).contains(item),
            _ => false,
        };
        // per line, in order: (line, left, right)
        let mut pieces: Vec<(usize, f32, f32)> = Vec::new();
        for p in laid.placed.iter().filter(|p| in_span(p)) {
            match pieces.last_mut() {
                Some(l) if l.0 == p.line => {
                    l.1 = l.1.min(p.x);
                    l.2 = l.2.max(p.x + p.atom.w);
                }
                _ => pieces.push((p.line, p.x, p.x + p.atom.w)),
            }
        }
        let total: f32 = pieces.iter().map(|l| l.2 - l.1).sum();
        let mut before = 0.0;
        for (line, left, right) in &pieces {
            for (pi, p) in laid.placed.iter().enumerate() {
                if p.line == *line && matches!(p.atom.kind, AtomKind::Word { .. }) && in_span(p) {
                    out.insert(pi, (before + p.x - left, total));
                }
            }
            before += right - left;
        }
    }
    out
}

/// Draws a 7TV-painted name (`paint_text`) with its text origin at `x0` and its baseline on pixel row `base_abs`.
/// False when it could not be rendered (the caller draws the plain glyphs).
#[allow(clippy::too_many_arguments)]
fn paint_name(window: &mut Window, cx: &mut App, imgs: &ImagesHandle, t: &TextItem, text: &str, strip: Option<(f32, f32)>, width: f32, (asc, desc): (f32, f32), line_h: f32, x0: f32, base_abs: f32) -> bool {
    let Some(spec) = t.paint.as_ref() else { return false };
    let scale = window.scale_factor();
    let (above, below) = blink_above_below(asc, desc, line_h);
    // the paint image, first frame only
    // the paint image: the frame of the global animation clock (first frame when animations are off)
    let gimg = spec.image_url.as_ref().and_then(|u| imgs.0.get(u, 1024.0, 1024.0, 1.0));
    let frame = gimg.as_ref().map(|g| imgs.0.frame(g, window, cx)).unwrap_or(0);
    let getter = || {
        let g = gimg.as_ref()?;
        let idx = frame.min(g.gpu.frame_count().saturating_sub(1));
        let sz = g.gpu.size(idx);
        g.gpu.as_bytes(idx).map(|b| PaintImage { w: sz.width.0 as u32, h: sz.height.0 as u32, bgra: std::sync::Arc::new(b.to_vec()) })
    };
    let req = NameReq { text, size: t.size, weight: t.weight.0 as u16, scale, box_w: width, above, below, paint: spec, strip, image: gimg.as_ref().map(|_| (frame, &getter as &dyn Fn() -> Option<PaintImage>)) };
    let ni = paint_text::name_image(&req);
    for d in paint_text::take_dropped() {
        window.drop_image(d).ok();
    }
    let Some(ni) = ni else { return false };
    let left = (x0 * scale).round() - ni.origin_x as f32;
    let top = (base_abs * scale).round() - ni.baseline_y as f32;
    // a hair inside the device-pixel grid: GPUI floors the origin and ceils the size
    let r = Bounds::new(point(px((left + 0.01) / scale), px((top + 0.01) / scale)), size(px((ni.w as f32 - 0.02) / scale), px((ni.h as f32 - 0.02) / scale)));
    window.paint_image(r, gpui::Corners::all(px(0.)), ni.image.clone(), 0, false).is_ok()
}

/// The painted rectangle of a placed atom, in window coordinates. A box's centre follows from the extents
/// stored at layout (margin box from `-above` to `+below` around the baseline).
fn atom_rect(content: &InlineContent, laid: &Laid, p: &Placed, origin: Point<Pixels>, _w: f32, _imgs: &ImagesHandle, _window: &Window) -> Bounds<Pixels> {
    let line = &laid.lines[p.line];
    let x = origin.x.f() + p.x;
    let base_abs = (origin.y.f() + line.baseline).round();
    match &p.atom.kind {
        AtomKind::Word { asc, hl, size: sz, .. } => {
            let top = base_abs - (*hl + *asc);
            Bounds::new(point(px(x), px(top)), size(px(p.atom.w), px(content.line_ratio * *sz)))
        }
        AtomKind::Box { item } => {
            let (w, h) = p.atom.dims;
            match &content.items[*item] {
                // the background spans the content area: ascent above the baseline, descent below
                Item::Pill(_) => Bounds::new(point(px(x), px(base_abs - p.atom.above)), size(px(w), px(h))),
                it => {
                    let mx = match it {
                        Item::Image(i) => i.margin_x,
                        Item::Icon(i) => i.margin_x,
                        _ => 0.0,
                    };
                    let cy = base_abs + (p.atom.below - p.atom.above) / 2.0;
                    Bounds::new(point(px(x + mx), px(cy - h / 2.0)), size(px(w), px(h)))
                }
            }
        }
        AtomKind::Block { .. } => {
            let (w, h) = p.atom.dims;
            Bounds::new(point(px(x), px(origin.y.f() + line.y + 4.0)), size(px(w), px(h)))
        }
        AtomKind::Space => Bounds::default(),
    }
}

#[allow(dead_code)]
fn _unused(_: AnyElement, _: Size<Pixels>) {}

/// `tracking-wide`: letter-spacing 0.025em. GPUI's text runs have no letter-spacing, so the glyphs are shaped one by
/// one and each is moved over by the spacing so far (fractional pixels, like the browser's). Font, size, colour and
/// line height come from the surrounding element's text style.
pub fn tracked(s: &str) -> Tracked {
    Tracked(s.to_string())
}

pub struct Tracked(String);

pub struct TrackedLayout {
    glyphs: Vec<(ShapedLine, f32)>,
    line_h: Pixels,
}

pub const TRACKING_WIDE_EM: f32 = 0.025;

impl IntoElement for Tracked {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Tracked {
    type RequestLayoutState = TrackedLayout;
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }
    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, TrackedLayout) {
        let ts = window.text_style();
        let fs = ts.font_size.to_pixels(window.rem_size());
        let ls = f32::from(fs) * TRACKING_WIDE_EM;
        let mut glyphs = Vec::new();
        let mut x = 0.0f32;
        for ch in self.0.chars() {
            let t: SharedString = ch.to_string().into();
            let run = TextRun { len: t.len(), font: ts.font(), color: ts.color, background_color: None, underline: None, strikethrough: None };
            let line = window.text_system().shape_line(t, fs, &[run], None);
            let w = f32::from(line.width);
            glyphs.push((line, x));
            x += w + ls;
        }
        let line_h = window.line_height();
        let mut style = Style::default();
        style.size = size(px(x).into(), line_h.into());
        style.flex_shrink = 0.0;
        (window.request_layout(style, [], cx), TrackedLayout { glyphs, line_h })
    }
    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut TrackedLayout, _: &mut Window, _: &mut App) {}
    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, l: &mut TrackedLayout, _: &mut (), window: &mut Window, cx: &mut App) {
        for (line, x) in &l.glyphs {
            let _ = line.paint(point(bounds.origin.x + px(*x), bounds.origin.y), l.line_h, window, cx);
        }
    }
}
