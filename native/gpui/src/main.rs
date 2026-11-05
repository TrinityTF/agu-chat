#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Agu Chat native prototype, GPUI frontend. Networking, parsing, tokenizing, emote maps and image
//! decoding all come from `agu-chat-core`; this file is only the GPUI view layer.
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agu_chat_core::chat::{Change, Channel, Chat, RowData};
use agu_chat_core::images::{Decoded, ImageCache, State};
use agu_chat_core::irc::Kind;
use agu_chat_core::net::Wake;
use agu_chat_core::tokens::{self, Atom};
use agu_chat_core::{logf, sys};
use futures::StreamExt;
use gpui::{
    actions, div, img, list, prelude::*, px, relative, rgb, size, AnyElement, AnyView, App, Application, Bounds,
    ClipboardItem, Context, FocusHandle, ImageSource, KeyBinding, ListAlignment, ListOffset, ListState, MouseButton,
    MouseMoveEvent, MouseUpEvent, Render, RenderImage, Rgba, SharedString, Task, TitlebarOptions, Window,
    WindowBounds, WindowOptions,
};

actions!(agu, [ToggleStats]);

const C_APP: u32 = 0x0b0b10;
const C_PANEL: u32 = 0x111117;
const C_ALT: u32 = 0x15151c;
const C_RAISED: u32 = 0x16161d;
const C_LINE: u32 = 0x26262f;
const C_ACCENT: u32 = 0x9147ff;
const C_ACCENT_SOFT: u32 = 0xa970ff;
const C_TEXT: u32 = 0xe4e4e7;
const C_MUTED: u32 = 0xa1a1aa;
const C_FAINT: u32 = 0x71717a;

const EMOTE_H: f32 = 24.0;
const BADGE_H: f32 = 18.0;

fn rgb3(c: [u8; 3]) -> Rgba {
    rgb(((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32)
}

/// `top` over `base` at `alpha`, as a solid color (rows only get one background fill).
fn blend(base: u32, top: u32, alpha: f32) -> Rgba {
    let ch = |s: u32| {
        let (b, t) = (((base >> s) & 0xff) as f32, ((top >> s) & 0xff) as f32);
        (b + (t - b) * alpha) as u32
    };
    rgb((ch(16) << 16) | (ch(8) << 8) | ch(0))
}

// ------------------------------------------------------------------------------------------ images

/// The GPUI half of a decoded image: one `RenderImage` (BGRA) per frame.
struct GImg {
    frames: Vec<Arc<RenderImage>>,
}

type Imgs = ImageCache<GImg>;

/// RGBA from the shared decoder -> GPUI's BGRA `RenderImage`s. Returns the handle and CPU bytes held.
fn upload(_url: &str, d: &Decoded) -> (GImg, u64) {
    let mut frames = Vec::with_capacity(d.frames.len());
    for (rgba, _) in &d.frames {
        let mut buf = rgba.clone();
        for px in buf.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        let Some(ib) = image::RgbaImage::from_raw(d.w, d.h, buf) else { continue };
        frames.push(Arc::new(RenderImage::new(vec![image::Frame::new(ib)])));
    }
    let bytes = frames.len() as u64 * d.w as u64 * d.h as u64 * 4;
    (GImg { frames }, bytes)
}

// ------------------------------------------------------------------------------------------ row cache

enum GAtom {
    Time(SharedString),
    Badge { url: String },
    Word { text: SharedString, color: Rgba },
    Emote { name: SharedString, url: String, tip: SharedString },
}

struct RowAtoms {
    gen: u32,
    atoms: Vec<GAtom>,
}

/// Per-row GPUI data: the tokenized message, rebuilt only when the emote/badge maps change.
#[derive(Default)]
struct RowCache {
    atoms: Option<Rc<RowAtoms>>,
}

impl RowData for RowCache {}

fn build_atoms(
    msg: &agu_chat_core::irc::ChatMsg,
    ch: &agu_chat_core::maps::Maps,
    gl: &agu_chat_core::maps::Maps,
    tz: i32,
    gen: u32,
) -> RowAtoms {
    let mut text_col = rgb(match msg.kind {
        Kind::Chat => C_TEXT,
        Kind::System => C_ACCENT_SOFT,
        Kind::Notice => C_MUTED,
    });
    let mut out = Vec::new();
    for a in tokens::tokenize(msg, ch, gl, tz) {
        out.push(match a {
            Atom::Time(t) => GAtom::Time(t.into()),
            Atom::Badge { url, .. } => GAtom::Badge { url },
            Atom::Name { text, rgb } => {
                let c = rgb3(rgb);
                if msg.action {
                    text_col = c;
                }
                GAtom::Word { text: text.into(), color: c }
            }
            Atom::Word(w) => GAtom::Word { text: w.into(), color: text_col },
            Atom::Emote { name, url, provider } => {
                let tip = format!("{name} ({provider})");
                GAtom::Emote { name: name.into(), url, tip: tip.into() }
            }
        });
    }
    RowAtoms { gen, atoms: out }
}

// ------------------------------------------------------------------------------------------ model

/// Everything the list item closures need; shared with the root view through an `Rc<RefCell>`.
struct Model {
    chat: Chat<RowCache>,
    imgs: Imgs,
    tz: i32,
    // frame stats
    renders: u32,
    item_ns: u64,
}

/// Single pending "repaint when the next animated frame is due" timer, shared by all rows.
#[derive(Default)]
struct AnimSched {
    deadline: Cell<Option<Instant>>,
    task: RefCell<Option<Task<()>>>,
}

impl AnimSched {
    fn schedule(self: &Rc<Self>, ms: u32, window: &mut Window, cx: &mut App) {
        let ms = ms.clamp(20, 1000) as u64;
        let now = Instant::now();
        let when = now + Duration::from_millis(ms);
        if let Some(d) = self.deadline.get() {
            if d > now && d <= when {
                return; // an earlier wake-up is already pending; it will re-ask after rendering
            }
        }
        self.deadline.set(Some(when));
        let view = window.current_view();
        let me = self.clone();
        let task = window.spawn(cx, async move |cx| {
            cx.background_executor().timer(Duration::from_millis(ms)).await;
            me.deadline.set(None);
            cx.update(|_, cx| cx.notify(view)).ok();
        });
        *self.task.borrow_mut() = Some(task);
    }
}

struct SplitView {
    list: ListState,
    /// True while the list is scrolled away from the bottom (set by the list's scroll handler).
    scrolled: Rc<Cell<bool>>,
    seen_total: u64,
    frac: f32,
}

struct Tip(SharedString);

impl Render for Tip {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(8.))
            .py(px(4.))
            .bg(rgb(C_RAISED))
            .border_1()
            .border_color(rgb(C_LINE))
            .rounded(px(4.))
            .text_size(px(12.))
            .text_color(rgb(C_TEXT))
            .child(self.0.clone())
    }
}

struct ChatApp {
    model: Rc<RefCell<Model>>,
    splits: Vec<SplitView>,
    sched: Rc<AnimSched>,
    focus: FocusHandle,
    show_stats: bool,
    stats_task: Option<Task<()>>,
    drag: Option<usize>,
    // stats
    fps: f32,
    fps_t: Instant,
    mem: (f32, f32),
    mem_t: Instant,
    row_ms: f32,
    _bridge: Task<()>,
}

impl ChatApp {
    fn new(channels: Vec<String>, url: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let runtime = agu_chat_core::make_runtime();

        // Background threads -> UI: a coalesced "something happened" signal on an async channel.
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();
        let pending = Arc::new(AtomicBool::new(false));
        let wake: Wake = {
            let pending = pending.clone();
            Arc::new(move || {
                if !pending.swap(true, Ordering::AcqRel) {
                    let _ = tx.unbounded_send(());
                }
            })
        };

        let chat = Chat::start(channels, url, runtime, wake.clone());
        let imgs = ImageCache::new(chat.rt.clone(), chat.client.clone(), wake);
        let n = chat.channels.len();
        let model = Rc::new(RefCell::new(Model { chat, imgs, tz: sys::tz_offset_min(), renders: 0, item_ns: 0 }));

        let m2 = model.clone();
        let bridge = cx.spawn(async move |this, cx| {
            while rx.next().await.is_some() {
                // Let a frame's worth of events pile up, then apply them in one go.
                cx.background_executor().timer(Duration::from_millis(16)).await;
                loop {
                    pending.store(false, Ordering::Release);
                    let more = this
                        .update(cx, |app, cx| {
                            let more = app.pump(&m2);
                            cx.notify();
                            more
                        })
                        .unwrap_or(false);
                    if !more {
                        break;
                    }
                    cx.background_executor().timer(Duration::from_millis(1)).await;
                }
            }
        });

        let splits = (0..n)
            .map(|_| {
                let list = ListState::new(0, ListAlignment::Bottom, px(240.));
                let scrolled = Rc::new(Cell::new(false));
                let s2 = scrolled.clone();
                list.set_scroll_handler(move |ev, _w, _cx| s2.set(ev.is_scrolled));
                SplitView { list, scrolled, seen_total: 0, frac: 1.0 / n as f32 }
            })
            .collect();

        let focus = cx.focus_handle();
        window.focus(&focus);
        ChatApp {
            model,
            splits,
            sched: Rc::new(AnimSched::default()),
            focus,
            show_stats: false,
            stats_task: None,
            drag: None,
            fps: 0.0,
            fps_t: Instant::now(),
            mem: (0.0, 0.0),
            mem_t: Instant::now() - Duration::from_secs(10),
            row_ms: 0.0,
            _bridge: bridge,
        }
    }

    /// Apply queued network events and decoded images; mirror row changes into the list states.
    fn pump(&mut self, model: &Rc<RefCell<Model>>) -> bool {
        let mut m = model.borrow_mut();
        let (changes, more_events) = m.chat.drain(2000);
        for c in changes {
            match c {
                Change::Appended { ch, n } => {
                    let l = &self.splits[ch].list;
                    let c = l.item_count();
                    l.splice(c..c, n);
                }
                Change::Trimmed { ch, n, .. } => self.splits[ch].list.splice(0..n, 0),
                Change::Prepended { ch, n } => self.splits[ch].list.splice(0..0, n),
                Change::Restyled { .. } | Change::Meta => {}
            }
        }
        let more_imgs = m.imgs.pump(upload);
        more_events || more_imgs
    }

    fn toggle_stats(&mut self, _: &ToggleStats, _w: &mut Window, cx: &mut Context<Self>) {
        self.show_stats = !self.show_stats;
        self.stats_task = if self.show_stats {
            Some(cx.spawn(async move |this, cx| loop {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }))
        } else {
            None
        };
        cx.notify();
    }

    fn on_move(&mut self, ev: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(i) = self.drag else { return };
        if ev.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            return;
        }
        let w: f32 = window.viewport_size().width.into();
        let x: f32 = ev.position.x.into();
        let left: f32 = self.splits[..i - 1].iter().map(|s| s.frac).sum();
        let pair = self.splits[i - 1].frac + self.splits[i].frac;
        let want = (x / w - left).clamp(0.1, pair - 0.1);
        self.splits[i - 1].frac = want;
        self.splits[i].frac = pair - want;
        cx.notify();
    }

    fn on_up(&mut self, _: &MouseUpEvent, _w: &mut Window, _cx: &mut Context<Self>) {
        self.drag = None;
    }

    fn split_view(&mut self, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let model = self.model.clone();
        let (name, total, connected) = {
            let m = model.borrow();
            let c = &m.chat.channels[i];
            (c.name.clone(), c.total, m.chat.connected)
        };
        let n_splits = self.splits.len();
        let sv = &mut self.splits[i];
        let scrolled = sv.scrolled.get();
        if !scrolled {
            sv.seen_total = total;
        }
        let unseen = total.saturating_sub(sv.seen_total);
        let list_state = sv.list.clone();
        let frac = sv.frac;

        let dot = if connected { 0x4ade80 } else { 0xef4444 };
        let header = div()
            .h(px(24.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .px(px(8.))
            .gap(px(7.))
            .bg(rgb(C_RAISED))
            .border_b_1()
            .border_color(rgb(C_LINE))
            .child(div().size(px(7.)).rounded_full().bg(rgb(dot)))
            .child(div().text_color(rgb(C_TEXT)).child(SharedString::from(name)))
            .child(div().flex_1())
            .child(div().text_size(px(11.)).text_color(rgb(C_MUTED)).child(format!("{total} msgs")));

        let sched = self.sched.clone();
        let item_model = model.clone();
        let rows = list(list_state.clone(), move |ix, window, cx| render_row(&item_model, &sched, i, ix, window, cx))
            .size_full();

        let mut body = div().id(("body", i)).flex_1().min_h_0().relative().bg(rgb(C_PANEL)).child(rows);
        if scrolled && unseen > 0 {
            let txt = format!("{} new message{}", unseen, if unseen == 1 { "" } else { "s" });
            let ls = list_state.clone();
            body = body.child(
                div().absolute().bottom(px(12.)).left_0().right_0().flex().justify_center().child(
                    div()
                        .id(("pill", i))
                        .px(px(12.))
                        .py(px(4.))
                        .rounded(px(12.))
                        .bg(rgb(C_ACCENT))
                        .hover(|s| s.bg(rgb(C_ACCENT_SOFT)))
                        .cursor_pointer()
                        .text_size(px(12.))
                        .text_color(rgb(0xffffff))
                        .child(txt)
                        .on_click(cx.listener(move |this, _, _w, cx| {
                            ls.scroll_to(ListOffset { item_ix: ls.item_count(), offset_in_item: px(0.) });
                            this.splits[i].scrolled.set(false);
                            cx.notify();
                        })),
                ),
            );
        }

        let input = div()
            .h(px(32.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .px(px(6.))
            .bg(rgb(C_RAISED))
            .border_t_1()
            .border_color(rgb(C_LINE))
            .child(
                div()
                    .flex_1()
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .px(px(8.))
                    .rounded(px(3.))
                    .bg(rgb(C_APP))
                    .border_1()
                    .border_color(rgb(C_LINE))
                    .text_color(rgb(C_FAINT))
                    .child("Read-only prototype"),
            );

        let mut col = div()
            .id(("split", i))
            .h_full()
            .w_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(header)
            .child(body)
            .child(input);
        if i > 0 {
            col = col.border_l_1().border_color(rgb(C_LINE));
        }
        let mut wrap = div().id(("splitwrap", i)).relative().h_full().w(relative(frac)).flex_shrink_0().child(col);
        if i > 0 && n_splits > 1 {
            // draggable divider: a thin hit area straddling the boundary, on top of both neighbours
            wrap = wrap.child(
                div()
                    .id(("divider", i))
                    .absolute()
                    .left(px(-3.))
                    .top_0()
                    .bottom_0()
                    .w(px(6.))
                    .cursor_col_resize()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _w, _cx| {
                            this.drag = Some(i);
                        }),
                    ),
            );
        }
        wrap.into_any_element()
    }
}

/// Build one chat row. Called by the list for visible rows (and once to measure overdraw rows).
fn render_row(
    model: &Rc<RefCell<Model>>,
    sched: &Rc<AnimSched>,
    ci: usize,
    ix: usize,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let t0 = Instant::now();
    let mut guard = model.borrow_mut();
    let Model { chat, imgs, tz, item_ns, .. } = &mut *guard;
    let Chat { channels, global, .. } = chat;
    let Channel { rows, maps, gen, .. } = &mut channels[ci];
    let Some(row) = rows.get_mut(ix) else { return div().h(px(1.)).into_any_element() };

    let stale = row.x.atoms.as_ref().map(|a| a.gen != *gen).unwrap_or(true);
    if stale {
        row.x.atoms = Some(Rc::new(build_atoms(&row.msg, maps, global, *tz, *gen)));
    }
    let atoms = row.x.atoms.clone().unwrap();
    let now = imgs.now_ms();
    let freeze = imgs.freeze;
    let mut wake_ms = u32::MAX;

    let msg = &row.msg;
    let special = msg.kind == Kind::System || msg.bar;
    let base = if row.seq % 2 == 0 { C_PANEL } else { C_ALT };
    let bg = if special { blend(base, C_ACCENT, 0.10) } else { rgb(base) };
    let dim = tokens::dim(msg);

    let mut kids: Vec<AnyElement> = Vec::with_capacity(atoms.atoms.len());
    for (k, a) in atoms.atoms.iter().enumerate() {
        match a {
            GAtom::Time(t) => kids.push(
                div().text_size(px(11.)).text_color(rgb(C_FAINT)).mr(px(1.)).child(t.clone()).into_any_element(),
            ),
            GAtom::Badge { url } => match imgs.get(url) {
                State::Failed => {}
                State::Loading => kids.push(div().size(px(BADGE_H)).flex_shrink_0().mr(px(-2.)).into_any_element()),
                State::Ready(im) => kids.push(
                    img(ImageSource::Render(im.gpu.frames[0].clone()))
                        .size(px(BADGE_H))
                        .flex_shrink_0()
                        .mr(px(-2.))
                        .into_any_element(),
                ),
            },
            GAtom::Word { text, color } => kids.push(div().text_color(*color).child(text.clone()).into_any_element()),
            GAtom::Emote { name, url, tip } => match imgs.get(url) {
                State::Failed => kids.push(div().text_color(rgb(C_TEXT)).child(name.clone()).into_any_element()),
                State::Loading => kids.push(div().w(px(EMOTE_H)).h(px(EMOTE_H)).flex_shrink_0().into_any_element()),
                State::Ready(im) => {
                    let (fi, wk) = im.frame_at(now, freeze);
                    wake_ms = wake_ms.min(wk);
                    let w = (im.w as f32 / im.h as f32 * EMOTE_H).clamp(8.0, EMOTE_H * 4.0);
                    let tip = tip.clone();
                    kids.push(
                        div()
                            .id(("e", k))
                            .flex_shrink_0()
                            .py(px(1.))
                            .child(img(ImageSource::Render(im.gpu.frames[fi].clone())).w(px(w)).h(px(EMOTE_H)))
                            .tooltip(move |_w, cx| -> AnyView { cx.new(|_| Tip(tip.clone())).into() })
                            .into_any_element(),
                    );
                }
            },
        }
    }

    let copy = (msg.kind == Kind::Chat).then(|| format!("{}: {}", msg.display, msg.text));
    let mut row_el = div()
        .id(("row", row.seq as usize))
        .relative()
        .w_full()
        .min_h(px(25.))
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap_x(px(4.))
        .py(px(3.))
        .pl(px(if msg.bar { 11. } else { 6. }))
        .pr(px(6.))
        .bg(bg)
        .hover(|s| s.bg(blend(base, 0xffffff, 0.035)))
        .children(kids);
    if special {
        row_el = row_el.child(div().absolute().left_0().top_0().bottom_0().w(px(3.)).bg(rgb(C_ACCENT)));
    }
    if dim < 1.0 {
        row_el = row_el.opacity(dim);
    }
    if let Some(text) = copy {
        row_el = row_el.on_mouse_down(MouseButton::Right, move |_, _w, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        });
    }

    *item_ns += t0.elapsed().as_nanos() as u64;
    drop(guard);
    if wake_ms != u32::MAX {
        sched.schedule(wake_ms, window, cx);
    }
    row_el.into_any_element()
}

impl Render for ChatApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // stats bookkeeping
        {
            let mut m = self.model.borrow_mut();
            m.renders += 1;
            let el = self.fps_t.elapsed().as_secs_f32();
            if el >= 0.5 {
                self.fps = m.renders as f32 / el;
                self.row_ms = m.item_ns as f32 / 1e6 / m.renders.max(1) as f32;
                m.renders = 0;
                m.item_ns = 0;
                self.fps_t = Instant::now();
            }
        }

        let tab = div()
            .h(px(26.))
            .flex_shrink_0()
            .flex()
            .items_end()
            .pl(px(8.))
            .bg(rgb(C_RAISED))
            .border_b_1()
            .border_color(rgb(C_LINE))
            .child(
                div()
                    .h_full()
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .bg(rgb(C_PANEL))
                    .border_b_2()
                    .border_color(rgb(C_ACCENT))
                    .text_color(rgb(C_TEXT))
                    .child("Agu Chat"),
            );

        let n = self.splits.len();
        let mut cols = div().flex_1().min_h_0().w_full().flex().flex_row().bg(rgb(C_APP));
        for i in 0..n {
            cols = cols.child(self.split_view(i, cx));
        }

        let mut root = div()
            .id("root")
            .track_focus(&self.focus)
            .key_context("agu")
            .on_action(cx.listener(Self::toggle_stats))
            .on_mouse_move(cx.listener(Self::on_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_up))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(C_APP))
            .text_color(rgb(C_TEXT))
            .font_family("Segoe UI")
            .text_size(px(13.))
            .line_height(px(19.))
            .child(tab)
            .child(cols);

        if self.show_stats {
            if self.mem_t.elapsed().as_secs_f32() > 0.5 {
                self.mem = sys::mem_mb();
                self.mem_t = Instant::now();
            }
            let m = self.model.borrow();
            let lines = [
                format!("FPS {:.0}   rows {:.2} ms/frame", self.fps, self.row_ms),
                format!("messages {}", m.chat.total_messages()),
                format!("images {} (+{} failed)", m.imgs.loaded, m.imgs.failed),
                format!("decoded {:.1} MB", m.imgs.decoded_bytes as f32 / 1048576.0),
                format!("render images {} ({} frames)", m.imgs.textures, m.imgs.frames),
                format!("working set {:.0} MB", self.mem.0),
            ];
            root = root.child(
                div()
                    .absolute()
                    .top(px(34.))
                    .right(px(10.))
                    .p(px(6.))
                    .bg(gpui::rgba(0x000000be))
                    .font_family("Consolas")
                    .text_size(px(11.))
                    .children(lines.into_iter().map(|l| div().child(l))),
            );
        }
        root
    }
}

fn main() {
    let channels = agu_chat_core::cli_channels();
    let url = agu_chat_core::irc_url();
    logf!("starting: channels={:?} irc={}", channels, url);

    Application::new().run(move |cx: &mut App| {
        cx.bind_keys([KeyBinding::new("f12", ToggleStats, None)]);
        let bounds = Bounds::centered(None, size(px(1100.), px(700.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions { title: Some("Agu Chat (GPUI prototype)".into()), ..Default::default() }),
                window_min_size: Some(size(px(480.), px(300.))),
                ..Default::default()
            },
            move |window, cx| cx.new(|cx| ChatApp::new(channels, url, window, cx)),
        )
        .unwrap();
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}
