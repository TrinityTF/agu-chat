//! The egui application: splits, message layout cache, painting, stats overlay.
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, ColorImage, CursorIcon, FontId, Galley, Id, Rect, ScrollArea, Sense,
    Stroke, TextureHandle, TextureOptions, Ui, UiBuilder,
};

use agu_chat_core::chat::{Change, Chat, Channel, RowData};
use agu_chat_core::images::{ImageCache, Img, State};
use agu_chat_core::irc::{ChatMsg, Kind};
use agu_chat_core::maps::Maps;
use agu_chat_core::tokens::{self, Atom};
use agu_chat_core::{logf, sys};

/// Frontend half of a cached image: one atlas texture holding every frame in a grid (1px gutters).
pub struct Tex {
    tex: TextureHandle,
    cols: u32,
    rows: u32,
}
type Imgs = ImageCache<Tex>;

fn uv(img: &Img<Tex>, i: usize) -> Rect {
    let (cw, ch) = (img.w + 2, img.h + 2);
    let (aw, ah) = (img.gpu.cols * cw, img.gpu.rows * ch);
    let (c, r) = (i as u32 % img.gpu.cols, i as u32 / img.gpu.cols);
    let (x0, y0) = (c * cw + 1, r * ch + 1);
    Rect::from_min_max(
        pos2(x0 as f32 / aw as f32, y0 as f32 / ah as f32),
        pos2((x0 + img.w) as f32 / aw as f32, (y0 + img.h) as f32 / ah as f32),
    )
}

/// Pack all frames of a decoded image into one atlas texture.
fn upload(ctx: &egui::Context, url: &str, d: &agu_chat_core::images::Decoded) -> (Tex, u64) {
    let (w, h) = (d.w as usize, d.h as usize);
    let n = d.frames.len();
    let (cw, ch) = (w + 2, h + 2);
    let cols = (2048 / cw).max(1).min(n);
    let rows = n.div_ceil(cols);
    let (aw, ah) = (cols * cw, rows * ch);
    let mut px = vec![0u8; aw * ah * 4];
    for (i, (rgba, _)) in d.frames.iter().enumerate() {
        let (c, r) = (i % cols, i / cols);
        for y in 0..h {
            let dst = ((r * ch + 1 + y) * aw + c * cw + 1) * 4;
            px[dst..dst + w * 4].copy_from_slice(&rgba[y * w * 4..(y + 1) * w * 4]);
        }
    }
    let ci = ColorImage::from_rgba_unmultiplied([aw, ah], &px);
    let tex = ctx.load_texture(url.to_string(), ci, TextureOptions::LINEAR);
    (Tex { tex, cols: cols as u32, rows: rows as u32 }, (aw * ah * 4) as u64)
}

pub const C_APP: Color32 = Color32::from_rgb(0x0b, 0x0b, 0x10);
pub const C_PANEL: Color32 = Color32::from_rgb(0x11, 0x11, 0x17);
pub const C_ALT: Color32 = Color32::from_rgb(0x15, 0x15, 0x1c);
pub const C_RAISED: Color32 = Color32::from_rgb(0x16, 0x16, 0x1d);
pub const C_LINE: Color32 = Color32::from_rgb(0x26, 0x26, 0x2f);
pub const C_ACCENT: Color32 = Color32::from_rgb(0x91, 0x47, 0xff);
pub const C_ACCENT_SOFT: Color32 = Color32::from_rgb(0xa9, 0x70, 0xff);
pub const C_TEXT: Color32 = Color32::from_rgb(0xe4, 0xe4, 0xe7);
pub const C_MUTED: Color32 = Color32::from_rgb(0xa1, 0xa1, 0xaa);
pub const C_FAINT: Color32 = Color32::from_rgb(0x71, 0x71, 0x7a);

const FONT: f32 = 13.0;
const EMOTE_H: f32 = 24.0;
const BADGE_H: f32 = 18.0;
const GAP: f32 = 4.0;

// ---------------------------------------------------------------- layout

enum ItemKind {
    Text(Arc<Galley>, Color32),
    Img { url: String, tip: String },
}

struct Item {
    line: usize,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    kind: ItemKind,
}

struct Layout {
    width: f32,
    gen: u32,
    height: f32,
    items: Vec<Item>,
    /// Image URLs laid out with a guessed size (still loading).
    assumed: Vec<String>,
}

/// Per-row egui state: the wrapped layout for the current width, kept until something changes.
struct RowLayout {
    layout: Option<Layout>,
    h: f32,
}

impl Default for RowLayout {
    fn default() -> Self {
        RowLayout { layout: None, h: 22.0 }
    }
}

impl RowData for RowLayout {
    fn invalidate(&mut self) {
        self.layout = None;
    }
}


struct LayoutCtx<'a> {
    painter: &'a egui::Painter,
    ch: &'a Maps,
    gl: &'a Maps,
    tz_min: i32,
    gen: u32,
}

fn col(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

struct Builder {
    items: Vec<Item>,
    line: usize,
    x: f32,
    avail: f32,
    assumed: Vec<String>,
}

impl Builder {
    fn push(&mut self, w: f32, h: f32, kind: ItemKind, gap: f32) {
        if self.x > 0.0 && self.x + w > self.avail {
            self.line += 1;
            self.x = 0.0;
        }
        self.items.push(Item { line: self.line, x: self.x, y: 0.0, w, h, kind });
        self.x += w + gap;
    }

    fn word(&mut self, p: &egui::Painter, text: &str, color: Color32) {
        let font = FontId::proportional(FONT);
        let g = p.layout_no_wrap(text.to_string(), font.clone(), color);
        if g.size().x > self.avail {
            let g = p.layout(text.to_string(), font, color, self.avail);
            if self.x > 0.0 {
                self.line += 1;
                self.x = 0.0;
            }
            let (w, h) = (self.avail, g.size().y);
            self.items.push(Item { line: self.line, x: 0.0, y: 0.0, w, h, kind: ItemKind::Text(g, color) });
            self.x = self.avail; // force a break after a wrapped block
        } else {
            let (w, h) = (g.size().x, g.size().y);
            self.push(w, h, ItemKind::Text(g, color), GAP);
        }
    }

    /// Returns false if the image failed to load (caller falls back to text for emotes).
    fn image(&mut self, imgs: &mut Imgs, url: &str, tip: String, target_h: f32, square: bool) -> bool {
        let (w, h) = match imgs.get(url) {
            State::Failed => return false,
            State::Loading => {
                self.assumed.push(url.to_string());
                (target_h, target_h)
            }
            State::Ready(img) => {
                if square {
                    (target_h, target_h)
                } else {
                    ((img.w as f32 / img.h as f32 * target_h).clamp(8.0, target_h * 4.0), target_h)
                }
            }
        };
        self.push(w, h, ItemKind::Img { url: url.to_string(), tip }, if square { 2.0 } else { GAP });
        true
    }
}

fn layout_row(c: &LayoutCtx, msg: &ChatMsg, width: f32, imgs: &mut Imgs) -> Layout {
    let pad = 6.0;
    let x0 = pad + if msg.bar { 5.0 } else { 0.0 };
    let avail = (width - x0 - pad).max(40.0);
    let mut b = Builder { items: Vec::new(), line: 0, x: 0.0, avail, assumed: Vec::new() };
    let p = c.painter;

    let d = tokens::dim(msg);
    let tint = |col: Color32| col.gamma_multiply(d);
    let mut text_col = match msg.kind {
        Kind::Chat => tint(C_TEXT),
        Kind::System => tint(C_ACCENT_SOFT),
        Kind::Notice => tint(C_MUTED),
    };

    for atom in tokens::tokenize(msg, c.ch, c.gl, c.tz_min) {
        match atom {
            Atom::Time(t) => {
                let ts = p.layout_no_wrap(t, FontId::proportional(11.0), tint(C_FAINT));
                let (w, h) = (ts.size().x, ts.size().y);
                b.push(w, h, ItemKind::Text(ts, tint(C_FAINT)), GAP + 1.0);
            }
            Atom::Badge { url, title } => {
                b.image(imgs, &url, title, BADGE_H, true);
            }
            Atom::Name { text, rgb } => {
                let ncol = tint(col(rgb));
                b.word(p, &text, ncol);
                if msg.action {
                    text_col = ncol;
                }
            }
            Atom::Word(w) => b.word(p, &w, text_col),
            Atom::Emote { name, url, provider } => {
                if !b.image(imgs, &url, format!("{name}
{provider}"), EMOTE_H, false) {
                    b.word(p, &name, text_col);
                }
            }
        }
    }

    // vertical placement
    let nlines = b.line + 1;
    let mut lh = vec![19.0f32; nlines];
    for it in &b.items {
        let need = if it.h > 20.0 { it.h + 2.0 } else { it.h.max(19.0) };
        if need > lh[it.line] {
            lh[it.line] = need;
        }
    }
    let mut tops = vec![0.0f32; nlines];
    let mut acc = 3.0;
    for i in 0..nlines {
        tops[i] = acc;
        acc += lh[i];
    }
    for it in &mut b.items {
        it.x += x0;
        it.y = tops[it.line] + (lh[it.line] - it.h) / 2.0;
    }
    Layout { width, gen: c.gen, height: acc + 3.0, items: b.items, assumed: b.assumed }
}

// ---------------------------------------------------------------- split view state

/// egui-side scroll bookkeeping for one channel column.
struct SplitUi {
    width: f32,
    at_bottom: bool,
    seen_total: u64,
    last_offset: f32,
    removed_h: f32,
    jump: bool,
}

impl SplitUi {
    fn new() -> Self {
        SplitUi { width: 400.0, at_bottom: true, seen_total: 0, last_offset: 0.0, removed_h: 0.0, jump: false }
    }
}

// ---------------------------------------------------------------- app

pub struct App {
    chat: Chat<RowLayout>,
    ui: Vec<SplitUi>,
    fracs: Vec<f32>,
    imgs: Imgs,
    show_stats: bool,
    tz_min: i32,
    // stats
    frame_count: u32,
    fps: f32,
    fps_t: Instant,
    ui_ms: f32,
    mem: (f32, f32),
    mem_t: Instant,
}

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let list: [(&str, &str); 5] = [
        ("segoeui", "C:\\Windows\\Fonts\\segoeui.ttf"),
        ("seguisym", "C:\\Windows\\Fonts\\seguisym.ttf"),
        ("seguiemj", "C:\\Windows\\Fonts\\seguiemj.ttf"),
        ("msyh", "C:\\Windows\\Fonts\\msyh.ttc"),
        ("malgun", "C:\\Windows\\Fonts\\malgun.ttf"),
    ];
    let mut names = Vec::new();
    for (name, path) in list {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert(name.to_string(), Arc::new(egui::FontData::from_owned(bytes)));
            names.push(name.to_string());
        } else {
            logf!("font missing: {path}");
        }
    }
    let fam = fonts.families.entry(egui::FontFamily::Proportional).or_default();
    for (i, n) in names.iter().enumerate() {
        fam.insert(i, n.clone());
    }
    ctx.set_fonts(fonts);
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        channels: Vec<String>,
        runtime: Arc<tokio::runtime::Runtime>,
        url: String,
    ) -> Self {
        let ctx = cc.egui_ctx.clone();
        setup_fonts(&ctx);
        let mut v = egui::Visuals::dark();
        v.panel_fill = C_APP;
        v.window_fill = C_RAISED;
        v.extreme_bg_color = C_RAISED;
        v.faint_bg_color = C_ALT;
        v.override_text_color = Some(C_TEXT);
        ctx.set_visuals(v);

        let wake: agu_chat_core::net::Wake = {
            let ctx = ctx.clone();
            Arc::new(move || ctx.request_repaint())
        };
        let n = channels.len();
        let chat = Chat::start(channels, url, runtime, wake.clone());
        let imgs = ImageCache::new(chat.rt.clone(), chat.client.clone(), wake);
        App {
            chat,
            ui: (0..n).map(|_| SplitUi::new()).collect(),
            fracs: vec![1.0 / n as f32; n],
            imgs,
            show_stats: false,
            tz_min: sys::tz_offset_min(),
            frame_count: 0,
            fps: 0.0,
            fps_t: Instant::now(),
            ui_ms: 0.0,
            mem: (0.0, 0.0),
            mem_t: Instant::now() - Duration::from_secs(10),
        }
    }

    fn drain(&mut self, ctx: &egui::Context) {
        let (changes, more) = self.chat.drain(3000);
        for c in changes {
            if let Change::Trimmed { ch, removed, .. } = c {
                if !self.ui[ch].at_bottom {
                    self.ui[ch].removed_h += removed.iter().map(|x| x.h).sum::<f32>();
                }
            }
        }
        if more {
            ctx.request_repaint();
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let t0 = Instant::now();
        let ctx = ui.ctx().clone();
        self.drain(&ctx);
        if self.imgs.pump(|url, d| upload(&ctx, url, d)) {
            ctx.request_repaint();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F12)) {
            self.show_stats = !self.show_stats;
        }

        // tab bar
        egui::Panel::top("tabs").exact_size(26.0).frame(egui::Frame::new().fill(C_RAISED)).show(ui, |ui| {
            let r = ui.max_rect();
            let p = ui.painter();
            let g = p.layout_no_wrap("Agu Chat".into(), FontId::proportional(FONT), C_TEXT);
            let tab = Rect::from_min_size(pos2(r.min.x + 8.0, r.min.y), vec2(g.size().x + 24.0, r.height()));
            p.rect_filled(tab, 0.0, C_PANEL);
            p.galley(pos2(tab.min.x + 12.0, tab.center().y - g.size().y / 2.0), g, C_TEXT);
            p.rect_filled(Rect::from_min_size(pos2(tab.min.x, tab.max.y - 2.0), vec2(tab.width(), 2.0)), 0.0, C_ACCENT);
            p.line_segment([pos2(r.min.x, r.max.y - 0.5), pos2(r.max.x, r.max.y - 0.5)], Stroke::new(1.0, C_LINE));
        });

        let mut next_wake = u32::MAX;
        egui::CentralPanel::default().frame(egui::Frame::new().fill(C_APP)).show(ui, |ui| {
            self.splits_ui(ui, &mut next_wake);
        });

        // stats
        self.frame_count += 1;
        let el = self.fps_t.elapsed().as_secs_f32();
        if el >= 0.5 {
            self.fps = self.frame_count as f32 / el;
            self.frame_count = 0;
            self.fps_t = Instant::now();
        }
        self.ui_ms = self.ui_ms * 0.9 + t0.elapsed().as_secs_f32() * 1000.0 * 0.1;
        if self.show_stats {
            if self.mem_t.elapsed().as_secs_f32() > 0.5 {
                self.mem = sys::mem_mb();
                self.mem_t = Instant::now();
            }
            let total: u64 = self.chat.total_messages();
            let lines = [
                format!("FPS {:.0}   ui {:.2} ms", self.fps, self.ui_ms),
                format!("messages {}", total),
                format!("images {} (+{} failed)", self.imgs.loaded, self.imgs.failed),
                format!("decoded {:.1} MB", self.imgs.decoded_bytes as f32 / 1048576.0),
                format!("textures {} ({} frames)", self.imgs.textures, self.imgs.frames),
                format!("working set {:.0} MB", self.mem.0),
            ];
            egui::Area::new(Id::new("stats"))
                .anchor(Align2::RIGHT_TOP, vec2(-10.0, 34.0))
                .order(egui::Order::Foreground)
                .interactable(false)
                .show(&ctx, |ui| {
                    egui::Frame::new()
                        .fill(Color32::from_black_alpha(190))
                        .inner_margin(egui::Margin::same(6))
                        .show(ui, |ui| {
                            for l in lines {
                                ui.label(egui::RichText::new(l).size(11.0).monospace().color(C_TEXT));
                            }
                        });
                });
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        if next_wake != u32::MAX {
            ctx.request_repaint_after(Duration::from_millis(next_wake.clamp(20, 1000) as u64));
        }
    }
}

impl App {
    fn splits_ui(&mut self, ui: &mut Ui, next_wake: &mut u32) {
        let area = ui.available_rect_before_wrap();
        let n = self.chat.channels.len();
        let sum: f32 = self.fracs.iter().sum();
        let mut x = area.min.x;
        let mut rects = Vec::new();
        for f in &self.fracs {
            let w = area.width() * f / sum;
            rects.push(Rect::from_min_max(pos2(x, area.min.y), pos2(x + w, area.max.y)));
            x += w;
        }
        // dividers
        for j in 0..n.saturating_sub(1) {
            let xx = rects[j].max.x;
            let hit = Rect::from_min_max(pos2(xx - 3.0, area.min.y), pos2(xx + 3.0, area.max.y));
            let r = ui.interact(hit, Id::new(("divider", j)), Sense::drag());
            if r.hovered() || r.dragged() {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            }
            if r.dragged() {
                let dx = r.drag_delta().x / area.width() * sum;
                let (a, b) = (self.fracs[j] + dx, self.fracs[j + 1] - dx);
                let min = 0.1 * sum;
                if a > min && b > min {
                    self.fracs[j] = a;
                    self.fracs[j + 1] = b;
                }
            }
        }
        for i in 0..n {
            let mut rect = rects[i];
            if i + 1 < n {
                rect.max.x -= 1.0;
            }
            let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
            child.set_clip_rect(rect.intersect(ui.clip_rect()));
            let connected = self.chat.connected;
            let tz = self.tz_min;
            let Chat { channels, global, .. } = &mut self.chat;
            show_split(&mut channels[i], &mut self.ui[i], i, &mut child, rect, global, &mut self.imgs, connected, tz, next_wake);
            if i + 1 < n {
                ui.painter().rect_filled(
                    Rect::from_min_max(pos2(rect.max.x, area.min.y), pos2(rect.max.x + 1.0, area.max.y)),
                    0.0,
                    C_LINE,
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn show_split(
    chan: &mut Channel<RowLayout>,
    su: &mut SplitUi,
    idx: usize,
    ui: &mut Ui,
    rect: Rect,
    global: &Maps,
    imgs: &mut Imgs,
    connected: bool,
    tz: i32,
    next_wake: &mut u32,
) {
    let header = Rect::from_min_size(rect.min, vec2(rect.width(), 24.0));
    let input = Rect::from_min_max(pos2(rect.min.x, rect.max.y - 32.0), rect.max);
    let body = Rect::from_min_max(pos2(rect.min.x, header.max.y), pos2(rect.max.x, input.min.y));

    ui.painter().rect_filled(body, 0.0, C_PANEL);

    // header
    {
        let p = ui.painter();
        p.rect_filled(header, 0.0, C_RAISED);
        p.line_segment([pos2(header.min.x, header.max.y - 0.5), pos2(header.max.x, header.max.y - 0.5)], Stroke::new(1.0, C_LINE));
        let dot = if connected { Color32::from_rgb(0x4a, 0xde, 0x80) } else { Color32::from_rgb(0xef, 0x44, 0x44) };
        p.circle_filled(pos2(header.min.x + 12.0, header.center().y), 3.5, dot);
        let g = p.layout_no_wrap(chan.name.clone(), FontId::proportional(FONT), C_TEXT);
        p.galley(pos2(header.min.x + 22.0, header.center().y - g.size().y / 2.0), g, C_TEXT);
        let g = p.layout_no_wrap(format!("{} msgs", chan.total), FontId::proportional(11.0), C_MUTED);
        p.galley(pos2(header.max.x - 8.0 - g.size().x, header.center().y - g.size().y / 2.0), g, C_MUTED);
    }

    // input bar
    {
        ui.painter().rect_filled(input, 0.0, C_RAISED);
        ui.painter().line_segment([pos2(input.min.x, input.min.y + 0.5), pos2(input.max.x, input.min.y + 0.5)], Stroke::new(1.0, C_LINE));
        let mut inner = ui.new_child(UiBuilder::new().max_rect(input.shrink2(vec2(6.0, 4.0))));
        let mut dummy = String::new();
        inner.add_enabled(
            false,
            egui::TextEdit::singleline(&mut dummy).hint_text("Read-only prototype").desired_width(f32::INFINITY),
        );
    }

    // messages
    let mut body_ui = ui.new_child(UiBuilder::new().max_rect(body));
    body_ui.set_clip_rect(body.intersect(ui.clip_rect()));
    let mut area = ScrollArea::vertical()
        .id_salt(("scroll", idx))
        .auto_shrink([false, false])
        .stick_to_bottom(true);
    if su.removed_h > 0.0 && !su.at_bottom {
        area = area.vertical_scroll_offset((su.last_offset - su.removed_h).max(0.0));
    }
    su.removed_h = 0.0;
    let jump = std::mem::take(&mut su.jump);
    let gen = chan.gen;
    
    let out = {
        let Channel { rows, maps, .. } = &mut *chan;
        let width = &mut su.width;
        area.show_viewport(&mut body_ui, |ui, viewport| {
            let w = ui.available_width();
            *width = w;
            let origin = ui.cursor().min;
            let painter = ui.painter().clone();
            let lc = LayoutCtx { painter: &painter, ch: maps, gl: global, tz_min: tz, gen };
            let pointer = ui.input(|i| i.pointer.hover_pos()).filter(|p| ui.clip_rect().contains(*p));
            let right_click = ui.input(|i| i.pointer.secondary_clicked());
            let now = imgs.now_ms();
            let freeze = imgs.freeze;
            let mut y = 0.0f32;
            let n = rows.len();
            let mut copy: Option<String> = None;
            for (ri, row) in rows.iter_mut().enumerate() {
                let near = y + row.x.h >= viewport.min.y - 120.0 && y <= viewport.max.y + 120.0;
                let tail = ri + 80 >= n;
                if near || (tail && row.x.layout.is_none()) {
                    let stale = match &row.x.layout {
                        None => true,
                        Some(l) => {
                            (l.width - w).abs() > 0.5
                                || l.gen != gen
                                || l.assumed.iter().any(|u| matches!(imgs.peek(u), Some(State::Ready(_)) | Some(State::Failed)))
                        }
                    };
                    if stale {
                        let l = layout_row(&lc, &row.msg, w, imgs);
                        row.x.h = l.height;
                        row.x.layout = Some(l);
                    }
                }
                let top = y;
                y += row.x.h;
                if top > viewport.max.y || top + row.x.h < viewport.min.y {
                    continue;
                }
                let Some(l) = &row.x.layout else { continue };
                let rrect = Rect::from_min_size(origin + vec2(0.0, top), vec2(w, row.x.h));
                let bg = if row.seq % 2 == 0 { C_PANEL } else { C_ALT };
                painter.rect_filled(rrect, 0.0, bg);
                if row.msg.kind == Kind::System || row.msg.bar {
                    painter.rect_filled(rrect, 0.0, C_ACCENT.gamma_multiply(0.10));
                    painter.rect_filled(Rect::from_min_size(rrect.min, vec2(3.0, rrect.height())), 0.0, C_ACCENT);
                }
                let hovered_row = pointer.map(|p| rrect.contains(p)).unwrap_or(false);
                if hovered_row {
                    painter.rect_filled(rrect, 0.0, Color32::from_white_alpha(9));
                    if right_click && row.msg.kind == Kind::Chat {
                        copy = Some(format!("{}: {}", row.msg.display, row.msg.text));
                    }
                }
                let alpha = if row.msg.deleted { 0.38 } else { 1.0 };
                for (ii, it) in l.items.iter().enumerate() {
                    let pos = rrect.min + vec2(it.x, it.y);
                    match &it.kind {
                        ItemKind::Text(g, col) => {
                            painter.galley(pos, g.clone(), *col);
                        }
                        ItemKind::Img { url, tip } => {
                            let r = Rect::from_min_size(pos, vec2(it.w, it.h));
                            if let Some(State::Ready(img)) = imgs.peek(url) {
                                let (fi, wake) = img.frame_at(now, freeze);
                                if wake < *next_wake {
                                    *next_wake = wake;
                                }
                                painter.image(
                                    img.gpu.tex.id(),
                                    r,
                                    uv(img, fi),
                                    Color32::WHITE.gamma_multiply(alpha),
                                );
                            }
                            if hovered_row && pointer.map(|p| r.contains(p)).unwrap_or(false) {
                                let resp = ui.interact(r, Id::new((idx, row.seq, ii)), Sense::hover());
                                resp.on_hover_text(tip.clone());
                            }
                        }
                    }
                }
            }
            if let Some(t) = copy {
                ui.ctx().copy_text(t);
            }
            ui.allocate_space(vec2(w, y));
            if jump {
                ui.scroll_to_cursor(Some(Align::BOTTOM));
            }
        })
    };

    let at_bottom = out.state.clamped_offset().y + out.inner_rect.height() >= out.content_size.y - 3.0;
    su.last_offset = out.state.clamped_offset().y;
    su.at_bottom = at_bottom;
    if at_bottom {
        su.seen_total = chan.total;
    }

    // "N new messages" pill
    let unseen = chan.total.saturating_sub(su.seen_total);
    if !at_bottom && unseen > 0 {
        let txt = format!("{} new message{}", unseen, if unseen == 1 { "" } else { "s" });
        let g = ui.painter().layout_no_wrap(txt, FontId::proportional(12.0), Color32::WHITE);
        let size = g.size() + vec2(24.0, 8.0);
        let r = Rect::from_center_size(pos2(body.center().x, body.max.y - 24.0), size);
        let resp = ui.interact(r, Id::new(("pill", idx)), Sense::click());
        let col = if resp.hovered() { C_ACCENT_SOFT } else { C_ACCENT };
        ui.painter().rect_filled(r, 12.0, col);
        ui.painter().galley(r.center() - g.size() / 2.0, g, Color32::WHITE);
        if resp.clicked() {
            su.jump = true;
            ui.ctx().request_repaint();
        }
    }
}
