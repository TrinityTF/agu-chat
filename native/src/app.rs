//! The egui application: splits, message layout cache, painting, stats overlay.
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, CursorIcon, FontId, Galley, Id, Rect, ScrollArea, Sense, Stroke, Ui,
    UiBuilder,
};

use crate::images::{ImageCache, State};
use crate::irc::{ChatMsg, Kind};
use crate::net::{Event, Net, Provider};
use crate::{logf, sys};

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

const MAX_ROWS: usize = 1000;
const FONT: f32 = 13.0;
const EMOTE_H: f32 = 24.0;
const BADGE_H: f32 = 18.0;
const GAP: f32 = 4.0;

// ---------------------------------------------------------------- emote / badge maps

#[derive(Default)]
struct Maps {
    /// name -> (url, provider, priority)
    emotes: HashMap<String, (String, Provider, u8)>,
    /// (set, version) -> (url, title)
    badges: HashMap<(String, String), (String, String)>,
}

fn prio(provider: Provider, channel: bool) -> u8 {
    let base = match provider {
        Provider::SevenTv => 3,
        Provider::Bttv => 2,
        Provider::Ffz => 1,
    };
    if channel {
        base + 3
    } else {
        base
    }
}

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

struct Row {
    msg: ChatMsg,
    seq: u64,
    layout: Option<Layout>,
    h: f32,
}

struct LayoutCtx<'a> {
    painter: &'a egui::Painter,
    ch: &'a Maps,
    gl: &'a Maps,
    tz_min: i32,
    gen: u32,
}

fn fmt_time(ts_ms: i64, tz_min: i32) -> String {
    let m = ((ts_ms / 1000 / 60 + tz_min as i64).rem_euclid(24 * 60)) as i64;
    format!("{:02}:{:02}", m / 60, m % 60)
}

fn name_color(msg: &ChatMsg) -> Color32 {
    match msg.color {
        Some([r, g, b]) => {
            let lum = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
            let target = 125.0;
            let k = if lum < target { ((target - lum) / (255.0 - lum)).clamp(0.0, 1.0) } else { 0.0 };
            let f = |c: u8| (c as f32 + (255.0 - c as f32) * k) as u8;
            Color32::from_rgb(f(r), f(g), f(b))
        }
        None => {
            let mut h: u32 = 2166136261;
            for b in msg.login.bytes() {
                h = (h ^ b as u32).wrapping_mul(16777619);
            }
            egui::ecolor::Hsva::new((h % 360) as f32 / 360.0, 0.55, 0.95, 1.0).into()
        }
    }
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
    fn image(&mut self, imgs: &mut ImageCache, url: &str, tip: String, target_h: f32, square: bool) -> bool {
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

fn layout_row(c: &LayoutCtx, msg: &ChatMsg, width: f32, imgs: &mut ImageCache) -> Layout {
    let pad = 6.0;
    let x0 = pad + if msg.bar { 5.0 } else { 0.0 };
    let avail = (width - x0 - pad).max(40.0);
    let mut b = Builder { items: Vec::new(), line: 0, x: 0.0, avail, assumed: Vec::new() };
    let p = c.painter;

    let dim = if msg.deleted { 0.38 } else if msg.historical { 0.82 } else { 1.0 };
    let tint = |col: Color32| col.gamma_multiply(dim);

    // timestamp
    let ts = p.layout_no_wrap(fmt_time(msg.ts_ms, c.tz_min), FontId::proportional(11.0), tint(C_FAINT));
    let (w, h) = (ts.size().x, ts.size().y);
    b.push(w, h, ItemKind::Text(ts, tint(C_FAINT)), GAP + 1.0);

    match msg.kind {
        Kind::Chat => {
            for (set, ver) in &msg.badges {
                let found = c.ch.badges.get(&(set.clone(), ver.clone())).or_else(|| c.gl.badges.get(&(set.clone(), ver.clone())));
                if let Some((url, title)) = found {
                    b.image(imgs, url, title.clone(), BADGE_H, true);
                }
            }
            let ncol = name_color(msg);
            let name = if msg.action { msg.display.clone() } else { format!("{}:", msg.display) };
            b.word(p, &name, tint(ncol));
            let text_col = if msg.action { tint(ncol) } else { tint(C_TEXT) };

            let chars: Vec<char> = msg.text.chars().collect();
            let mut word = String::new();
            let mut ei = 0;
            let mut i = 0;
            let flush = |b: &mut Builder, word: &mut String, imgs: &mut ImageCache| {
                if word.is_empty() {
                    return;
                }
                let found = c.ch.emotes.get(word.as_str()).or_else(|| c.gl.emotes.get(word.as_str()));
                let mut done = false;
                if let Some((url, prov, _)) = found {
                    done = b.image(imgs, url, format!("{}\n{}", word, prov.label()), EMOTE_H, false);
                }
                if !done {
                    b.word(p, word, text_col);
                }
                word.clear();
            };
            while i < chars.len() {
                while ei < msg.emotes.len() && msg.emotes[ei].end < i {
                    ei += 1;
                }
                if ei < msg.emotes.len() && msg.emotes[ei].start == i && msg.emotes[ei].end < chars.len() {
                    flush(&mut b, &mut word, imgs);
                    let e = &msg.emotes[ei];
                    let name: String = chars[e.start..=e.end].iter().collect();
                    let url = format!("https://static-cdn.jtvnw.net/emoticons/v2/{}/default/dark/1.0", e.id);
                    if !b.image(imgs, &url, format!("{name}\nTwitch"), EMOTE_H, false) {
                        b.word(p, &name, text_col);
                    }
                    i = e.end + 1;
                    ei += 1;
                    continue;
                }
                let ch = chars[i];
                if ch.is_whitespace() {
                    flush(&mut b, &mut word, imgs);
                } else {
                    word.push(ch);
                }
                i += 1;
            }
            flush(&mut b, &mut word, imgs);
        }
        Kind::System | Kind::Notice => {
            let col = if msg.kind == Kind::System { tint(C_ACCENT_SOFT) } else { tint(C_MUTED) };
            for w in msg.text.split_whitespace() {
                b.word(p, w, col);
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

// ---------------------------------------------------------------- split state

struct Split {
    name: String,
    room_id: Option<String>,
    rows: VecDeque<Row>,
    maps: Maps,
    gen: u32,
    total: u64,
    seq: u64,
    width: f32,
    at_bottom: bool,
    seen_total: u64,
    last_offset: f32,
    removed_h: f32,
    jump: bool,
    emotes_started: bool,
}

impl Split {
    fn new(name: &str) -> Self {
        Split {
            name: name.to_string(),
            room_id: None,
            rows: VecDeque::new(),
            maps: Maps::default(),
            gen: 0,
            total: 0,
            seq: 0,
            width: 400.0,
            at_bottom: true,
            seen_total: 0,
            last_offset: 0.0,
            removed_h: 0.0,
            jump: false,
            emotes_started: false,
        }
    }

    fn push(&mut self, msg: ChatMsg) {
        self.seq += 1;
        self.total += 1;
        self.rows.push_back(Row { msg, seq: self.seq, layout: None, h: 22.0 });
        while self.rows.len() > MAX_ROWS {
            if let Some(r) = self.rows.pop_front() {
                if !self.at_bottom {
                    self.removed_h += r.h;
                }
            }
        }
    }

    fn mark_deleted(&mut self, f: impl Fn(&ChatMsg) -> bool) {
        for r in self.rows.iter_mut() {
            if r.msg.kind == Kind::Chat && !r.msg.deleted && f(&r.msg) {
                r.msg.deleted = true;
                r.layout = None;
            }
        }
    }
}

// ---------------------------------------------------------------- app

pub struct App {
    rx: Receiver<Event>,
    net: Net,
    rt: tokio::runtime::Handle,
    _runtime: Arc<tokio::runtime::Runtime>,
    splits: Vec<Split>,
    fracs: Vec<f32>,
    global: Maps,
    imgs: ImageCache,
    connected: bool,
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

        let client = reqwest::Client::builder()
            .user_agent(crate::net::UA)
            .timeout(Duration::from_secs(20))
            .build()
            .expect("http client");
        let (tx, rx) = crossbeam_channel::unbounded();
        let net = Net { client: client.clone(), tx, ctx: ctx.clone() };
        let rt = runtime.handle().clone();
        let bench = std::env::var("AGU_IRC_URL").is_ok();

        {
            let n = net.clone();
            rt.spawn(async move { n.load_globals().await });
            for c in &channels {
                let (n, c) = (net.clone(), c.clone());
                rt.spawn(async move { n.load_channel_badges(&c).await });
            }
            rt.spawn(crate::net::irc_task(net.clone(), url, channels.clone(), bench));
        }

        let n = channels.len();
        App {
            rx,
            net,
            rt: rt.clone(),
            _runtime: runtime,
            splits: channels.iter().map(|c| Split::new(c)).collect(),
            fracs: vec![1.0 / n as f32; n],
            global: Maps::default(),
            imgs: ImageCache::new(rt, client, ctx),
            connected: false,
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

    fn split_mut(&mut self, ch: &str) -> Option<&mut Split> {
        self.splits.iter_mut().find(|s| s.name == ch)
    }

    fn drain(&mut self, ctx: &egui::Context) {
        let mut n = 0;
        while let Ok(e) = self.rx.try_recv() {
            n += 1;
            match e {
                Event::Connected(c) => {
                    self.connected = c;
                    logf!("connection state: {c}");
                }
                Event::Msgs { ch, msgs } => {
                    if let Some(s) = self.split_mut(&ch) {
                        for m in msgs {
                            s.push(m);
                        }
                    }
                }
                Event::Backlog { ch, msgs } => {
                    if let Some(s) = self.split_mut(&ch) {
                        let have: HashSet<String> = s.rows.iter().map(|r| r.msg.id.clone()).collect();
                        let mut add = 0;
                        for m in msgs.into_iter().rev() {
                            if !m.id.is_empty() && have.contains(&m.id) {
                                continue;
                            }
                            s.seq += 1;
                            let seq = s.seq;
                            s.rows.push_front(Row { msg: m, seq, layout: None, h: 22.0 });
                            add += 1;
                        }
                        s.total += add;
                        while s.rows.len() > MAX_ROWS {
                            s.rows.pop_front();
                        }
                    }
                }
                Event::ClearUser { ch, login, secs } => {
                    if let Some(s) = self.split_mut(&ch) {
                        let l = login.to_lowercase();
                        s.mark_deleted(|m| m.login.to_lowercase() == l);
                        let txt = match secs {
                            Some(d) => format!("{login} has been timed out for {d}s."),
                            None => format!("{login} has been permanently banned."),
                        };
                        s.push(ChatMsg::notice(txt, Kind::Notice));
                    }
                }
                Event::ClearAll(ch) => {
                    if let Some(s) = self.split_mut(&ch) {
                        s.push(ChatMsg::notice("Chat has been cleared by a moderator.".into(), Kind::Notice));
                    }
                }
                Event::ClearMsg { ch, id } => {
                    if let Some(s) = self.split_mut(&ch) {
                        s.mark_deleted(|m| m.id == id);
                    }
                }
                Event::Notice { ch, text } => {
                    if let Some(s) = self.split_mut(&ch) {
                        s.push(ChatMsg::notice(text, Kind::Notice));
                    }
                }
                Event::RoomState { ch, room_id } => {
                    let net = self.net.clone();
                    let rt = self.rt.clone();
                    if let Some(s) = self.split_mut(&ch) {
                        if s.room_id.is_none() {
                            s.room_id = Some(room_id.clone());
                        }
                        if !s.emotes_started {
                            s.emotes_started = true;
                            logf!("room-id for {ch}: {room_id}");
                            rt.spawn(async move { net.load_channel_emotes(&ch, &room_id).await });
                        }
                    }
                }
                Event::Emotes { ch, provider, list } => {
                    let count = list.len();
                    let (maps, channel) = match &ch {
                        Some(c) => match self.splits.iter_mut().find(|s| &s.name == c) {
                            Some(s) => {
                                s.gen += 1;
                                (&mut s.maps, true)
                            }
                            None => continue,
                        },
                        None => {
                            for s in self.splits.iter_mut() {
                                s.gen += 1;
                            }
                            (&mut self.global, false)
                        }
                    };
                    let pr = prio(provider, channel);
                    for e in list {
                        let better = maps.emotes.get(&e.name).map(|x| pr > x.2).unwrap_or(true);
                        if better {
                            maps.emotes.insert(e.name, (e.url, provider, pr));
                        }
                    }
                    logf!("emotes {} {:?}: {count}", ch.as_deref().unwrap_or("global"), provider);
                }
                Event::Badges { ch, list } => {
                    let maps = match &ch {
                        Some(c) => match self.splits.iter_mut().find(|s| &s.name == c) {
                            Some(s) => {
                                s.gen += 1;
                                &mut s.maps
                            }
                            None => continue,
                        },
                        None => {
                            for s in self.splits.iter_mut() {
                                s.gen += 1;
                            }
                            &mut self.global
                        }
                    };
                    for b in list {
                        maps.badges.insert((b.set, b.version), (b.url, b.title));
                    }
                }
            }
            if n >= 3000 {
                ctx.request_repaint();
                break;
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let t0 = Instant::now();
        let ctx = ui.ctx().clone();
        self.drain(&ctx);
        if self.imgs.pump() {
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
            let total: u64 = self.splits.iter().map(|s| s.total).sum();
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
        let n = self.splits.len();
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
            let connected = self.connected;
            let tz = self.tz_min;
            let (splits, global, imgs) = (&mut self.splits, &self.global, &mut self.imgs);
            show_split(&mut splits[i], i, &mut child, rect, global, imgs, connected, tz, next_wake);
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
    split: &mut Split,
    idx: usize,
    ui: &mut Ui,
    rect: Rect,
    global: &Maps,
    imgs: &mut ImageCache,
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
        let g = p.layout_no_wrap(split.name.clone(), FontId::proportional(FONT), C_TEXT);
        p.galley(pos2(header.min.x + 22.0, header.center().y - g.size().y / 2.0), g, C_TEXT);
        let g = p.layout_no_wrap(format!("{} msgs", split.total), FontId::proportional(11.0), C_MUTED);
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
    if split.removed_h > 0.0 && !split.at_bottom {
        area = area.vertical_scroll_offset((split.last_offset - split.removed_h).max(0.0));
    }
    split.removed_h = 0.0;
    let jump = std::mem::take(&mut split.jump);
    let gen = split.gen;
    let rows_len = split.rows.len();

    let out = {
        let Split { rows, maps, width, .. } = &mut *split;
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
                let near = y + row.h >= viewport.min.y - 120.0 && y <= viewport.max.y + 120.0;
                let tail = ri + 80 >= n;
                if near || (tail && row.layout.is_none()) {
                    let stale = match &row.layout {
                        None => true,
                        Some(l) => {
                            (l.width - w).abs() > 0.5
                                || l.gen != gen
                                || l.assumed.iter().any(|u| matches!(imgs.peek(u), Some(State::Ready(_)) | Some(State::Failed)))
                        }
                    };
                    if stale {
                        let l = layout_row(&lc, &row.msg, w, imgs);
                        row.h = l.height;
                        row.layout = Some(l);
                    }
                }
                let top = y;
                y += row.h;
                if top > viewport.max.y || top + row.h < viewport.min.y {
                    continue;
                }
                let Some(l) = &row.layout else { continue };
                let rrect = Rect::from_min_size(origin + vec2(0.0, top), vec2(w, row.h));
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
                                    img.tex.id(),
                                    r,
                                    img.uv(fi),
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
    split.last_offset = out.state.clamped_offset().y;
    split.at_bottom = at_bottom;
    if at_bottom {
        split.seen_total = split.total;
    }
    let _ = rows_len;

    // "N new messages" pill
    let unseen = split.total.saturating_sub(split.seen_total);
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
            split.jump = true;
            ui.ctx().request_repaint();
        }
    }
}
