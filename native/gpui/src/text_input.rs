//! GPUI has no text field widget, so this is the one the DOM gave the Electron app for free (`<input>` and
//! `<textarea>`). Adapted from gpui's `examples/input.rs`: an editable field with a caret, selection, clipboard,
//! word jumps, double / triple click, IME composition, and the styles the app uses (`inputCls` in layout/ui.tsx,
//! the tab rename box in layout/NotebookTabs.tsx, the chat search box, and the send box' auto-growing textarea
//! in chat/input/SendBox.tsx).
//!
//! `InputStyle::Area` is the textarea: wrapped text, Shift+Enter inserts a newline, Up / Down move between
//! visual lines, it grows with its content up to ~5 lines and scrolls after that. A parent that owns richer key
//! behaviour (autocomplete, history) intercepts keys with `capture_key_down` on a wrapping element before the
//! field sees them; Tab never reaches the text (it is a `Tab` event only).
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    actions, div, fill, point, prelude::*, px, relative, size, App, AvailableSpace, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyDownEvent, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, ShapedLine, SharedString, Style, TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window, WrappedLine,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::chat::images::ImagesHandle;
use crate::theme::{self, a, c};

actions!(
    agu_text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        Paste,
        Cut,
        Copy,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        DeleteWordLeft,
        DeleteWordRight,
        SelectHome,
        SelectEnd
    ]
);

/// Key bindings the field needs (call once at startup).
pub fn bind_keys(cx: &mut App) {
    use gpui::KeyBinding;
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("TextInput")),
        KeyBinding::new("shift-backspace", Backspace, Some("TextInput")),
        KeyBinding::new("delete", Delete, Some("TextInput")),
        KeyBinding::new("left", Left, Some("TextInput")),
        KeyBinding::new("right", Right, Some("TextInput")),
        KeyBinding::new("shift-left", SelectLeft, Some("TextInput")),
        KeyBinding::new("shift-right", SelectRight, Some("TextInput")),
        KeyBinding::new("ctrl-left", WordLeft, Some("TextInput")),
        KeyBinding::new("ctrl-right", WordRight, Some("TextInput")),
        KeyBinding::new("ctrl-shift-left", SelectWordLeft, Some("TextInput")),
        KeyBinding::new("ctrl-shift-right", SelectWordRight, Some("TextInput")),
        KeyBinding::new("ctrl-backspace", DeleteWordLeft, Some("TextInput")),
        KeyBinding::new("ctrl-delete", DeleteWordRight, Some("TextInput")),
        KeyBinding::new("ctrl-a", SelectAll, Some("TextInput")),
        KeyBinding::new("ctrl-v", Paste, Some("TextInput")),
        KeyBinding::new("shift-insert", Paste, Some("TextInput")),
        KeyBinding::new("ctrl-c", Copy, Some("TextInput")),
        KeyBinding::new("ctrl-insert", Copy, Some("TextInput")),
        KeyBinding::new("ctrl-x", Cut, Some("TextInput")),
        KeyBinding::new("shift-delete", Cut, Some("TextInput")),
        KeyBinding::new("home", Home, Some("TextInput")),
        KeyBinding::new("end", End, Some("TextInput")),
        KeyBinding::new("shift-home", SelectHome, Some("TextInput")),
        KeyBinding::new("shift-end", SelectEnd, Some("TextInput")),
    ]);
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InputStyle {
    /// `inputCls`: w-full rounded border border-line bg-app px-2 py-1 text-[12px] focus:border-accent
    Field,
    /// the tab rename box: w-32 rounded-sm border border-accent bg-app px-1 text-[12px]
    Rename,
    /// settings: `px-1.5 py-0.5 text-[11px]` (a small field beside other controls)
    Small,
    /// chat search box: min-w-0 flex-1 bg-transparent text-xs, no border or outline
    Bare,
    /// `inputCls` of chat/ui.ts: rounded border border-line bg-panel px-2 py-1 text-xs focus:border-accent/60
    Chat,
    /// the send box textarea: min-w-0 flex-1 rounded border border-line bg-panel px-2 py-1.5 text-[13px]
    /// leading-[1.35] focus:border-accent/60 disabled:opacity-60, rows=1, grows to 96px
    Area,
}

pub enum TextInputEvent {
    Changed,
    Enter,
    Escape,
    Blur,
    /// Tab (Area only; the key never types a tab character)
    Tab,
}

impl EventEmitter<TextInputEvent> for TextInput {}

/// Maps a whitespace-delimited token of the text to the URL of an image to draw in its place (None: plain text).
/// Opt-in per input (`set_inline_images`, Area only); the text itself never changes, only how it is drawn.
pub type InlineImageFn = Rc<dyn Fn(&str) -> Option<SharedString>>;

/// One wrapped visual row of a logical line (byte offsets are local to the line).
struct Row {
    start: usize,
    end: usize,
    /// unwrapped x of the row's first glyph
    x0: f32,
    /// y of the row's top within the content
    y: f32,
}

struct LaidLine {
    line: WrappedLine,
    /// byte offset of the line in the whole text
    start: usize,
    rows: Vec<Row>,
}

struct AreaLayout {
    lines: Vec<LaidLine>,
    line_h: f32,
    total_h: f32,
    /// the shaped text: image tokens are runs of placeholder dots, so offsets in `lines` are display offsets
    text: SharedString,
    shown: Vec<atoms::Shown>,
}

impl AreaLayout {
    fn build(lines: smallvec::SmallVec<[WrappedLine; 1]>, line_h: f32, text: SharedString, shown: Vec<atoms::Shown>) -> AreaLayout {
        let mut out = Vec::new();
        let mut start = 0usize;
        let mut y = 0.0f32;
        for line in lines {
            let len = line.len();
            let mut rows = Vec::new();
            let mut row_start = 0usize;
            let mut x0 = 0.0f32;
            let mut ry = y;
            for b in line.wrap_boundaries.iter() {
                let g = &line.unwrapped_layout.runs[b.run_ix].glyphs[b.glyph_ix];
                rows.push(Row { start: row_start, end: g.index, x0, y: ry });
                row_start = g.index;
                x0 = f32::from(g.position.x);
                ry += line_h;
            }
            rows.push(Row { start: row_start, end: len, x0, y: ry });
            y = ry + line_h;
            out.push(LaidLine { line, start, rows });
            start += len + 1;
        }
        AreaLayout { lines: out, line_h, total_h: y, text, shown }
    }

    fn to_display(&self, idx: usize) -> usize {
        atoms::to_display(&self.shown, idx)
    }

    fn to_content(&self, idx: usize) -> usize {
        atoms::to_content(&self.shown, idx)
    }

    fn x_in_row(l: &LaidLine, row: &Row, local: usize) -> f32 {
        f32::from(l.line.unwrapped_layout.x_for_index(local)) - row.x0
    }

    /// (x, y, line index, row index) of a caret at byte offset `idx` of the content (the later row at a wrap point).
    fn caret_at(&self, idx: usize) -> (f32, f32, usize, usize) {
        self.caret_at_display(self.to_display(idx))
    }

    fn caret_at_display(&self, idx: usize) -> (f32, f32, usize, usize) {
        for (li, l) in self.lines.iter().enumerate() {
            if idx >= l.start && idx <= l.start + l.line.len() {
                let local = idx - l.start;
                let ri = l.rows.iter().rposition(|r| r.start <= local).unwrap_or(0);
                let r = &l.rows[ri];
                return (Self::x_in_row(l, r, local), r.y, li, ri);
            }
        }
        let li = self.lines.len().saturating_sub(1);
        (0.0, self.total_h - self.line_h, li, 0)
    }

    /// The content byte offset nearest to a point (never inside an image: the nearer side wins).
    fn index_at(&self, x: f32, y: f32) -> usize {
        self.to_content(self.display_index_at(x, y))
    }

    fn display_index_at(&self, x: f32, y: f32) -> usize {
        let text: &str = &self.text;
        if self.lines.is_empty() {
            return 0;
        }
        let mut target = None;
        'find: for l in &self.lines {
            for (ri, r) in l.rows.iter().enumerate() {
                if y < r.y + self.line_h {
                    target = Some((l, ri));
                    break 'find;
                }
            }
        }
        let (l, ri) = target.unwrap_or_else(|| {
            let l = self.lines.last().unwrap();
            (l, l.rows.len() - 1)
        });
        let r = &l.rows[ri];
        let gx = x + r.x0;
        let mut local = l.line.unwrapped_layout.closest_index_for_x(px(gx)).clamp(r.start, r.end);
        if ri + 1 < l.rows.len() && local >= r.end {
            // far right of a wrapped row: before the wrap, not at the start of the next row
            local = text[l.start..l.start + r.end].grapheme_indices(true).next_back().map(|(i, _)| i).unwrap_or(r.start).max(r.start);
        }
        l.start + local
    }
}

pub struct TextInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    style: InputStyle,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_area: Option<AreaLayout>,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
    /// Area: how far the content is scrolled up (px)
    scroll_y: f32,
    /// Area: the x Up / Down keep while moving through lines of different lengths
    want_x: Option<f32>,
    disabled: bool,
    max_chars: Option<usize>,
    /// password field: the text shows as bullets
    masked: bool,
    /// red border (a value the field's owner would reject)
    invalid: bool,
    /// `font-mono`
    mono: bool,
    /// `text-center` (single line): the text sits in the middle of the field
    centered: bool,
    /// Area: tokens drawn as images (the send box' emotes)
    inline_images: Option<InlineImageFn>,
    /// Where the caret was left by the last edit (cleared by any caret move): a token that ends at or around it is
    /// still being typed and stays text, so a half-typed word never flips into an image.
    typing: Option<usize>,
    /// caret blink (Chromium: the system caret blink time, solid again after every edit or caret move)
    blink_since: std::time::Instant,
    blink_sig: (usize, usize, usize),
    blink_task: Option<gpui::Task<()>>,
    _blur: gpui::Subscription,
}

/// `GetCaretBlinkTime` (ms); the OS says INFINITE (no blinking) with 0xFFFFFFFF.
#[cfg(windows)]
fn caret_blink_ms() -> Option<u128> {
    let t = unsafe { windows::Win32::UI::WindowsAndMessaging::GetCaretBlinkTime() };
    if t == u32::MAX || t == 0 { None } else { Some(t as u128) }
}

/// macOS / Linux: the common 530 ms default.
#[cfg(not(windows))]
fn caret_blink_ms() -> Option<u128> {
    Some(530)
}

impl TextInput {
    /// Whether the caret is in its visible phase now. A change of text or selection since the last paint restarts
    /// the phase (solid while typing), like Chromium's.
    fn caret_on(&mut self) -> bool {
        let sig = (self.selected_range.start, self.selected_range.end, self.content.len());
        if sig != self.blink_sig {
            self.blink_sig = sig;
            self.blink_since = std::time::Instant::now();
        }
        match caret_blink_ms() {
            None => true,
            Some(ms) => (self.blink_since.elapsed().as_millis() / ms) % 2 == 0,
        }
    }

    pub fn new(window: &mut Window, cx: &mut Context<Self>, placeholder: &str, style: InputStyle) -> Self {
        let focus_handle = cx.focus_handle();
        let blur = cx.on_blur(&focus_handle, window, |_, _, cx| cx.emit(TextInputEvent::Blur));
        Self {
            focus_handle,
            content: "".into(),
            placeholder: placeholder.to_string().into(),
            style,
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_area: None,
            last_bounds: None,
            is_selecting: false,
            scroll_y: 0.0,
            want_x: None,
            disabled: false,
            max_chars: None,
            masked: false,
            invalid: false,
            mono: false,
            centered: false,
            inline_images: None,
            typing: None,
            blink_since: std::time::Instant::now(),
            blink_sig: (0, 0, 0),
            blink_task: None,
            _blur: blur,
        }
    }

    pub fn text(&self) -> String {
        self.content.to_string()
    }

    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.content = text.to_string().into();
        self.selected_range = text.len()..text.len();
        self.selection_reversed = false;
        self.marked_range = None;
        self.want_x = None;
        self.typing = None;
        cx.notify();
    }

    /// Draw the tokens `f` knows an image for as that image (Area only). `None` draws plain text again.
    pub fn set_inline_images(&mut self, f: Option<InlineImageFn>, cx: &mut Context<Self>) {
        self.inline_images = f;
        cx.notify();
    }

    /// Treat the caret as placed by the user rather than left by an edit (a recalled message: its last word is no
    /// longer "being typed").
    pub fn settle(&mut self, cx: &mut Context<Self>) {
        self.typing = None;
        cx.notify();
    }

    pub fn set_placeholder(&mut self, placeholder: &str, cx: &mut Context<Self>) {
        if self.placeholder.as_ref() != placeholder {
            self.placeholder = placeholder.to_string().into();
            cx.notify();
        }
    }

    pub fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        if self.disabled != disabled {
            self.disabled = disabled;
            cx.notify();
        }
    }

    /// `maxLength`
    pub fn set_max_chars(&mut self, n: Option<usize>) {
        self.max_chars = n;
    }

    pub fn set_invalid(&mut self, invalid: bool, cx: &mut Context<Self>) {
        if self.invalid != invalid {
            self.invalid = invalid;
            cx.notify();
        }
    }

    pub fn set_masked(&mut self, masked: bool) {
        self.masked = masked;
    }

    pub fn set_mono(&mut self, mono: bool) {
        self.mono = mono;
    }

    pub fn set_centered(&mut self, centered: bool) {
        self.centered = centered;
    }

    pub fn select_all_text(&mut self, cx: &mut Context<Self>) {
        self.selected_range = 0..self.content.len();
        self.selection_reversed = false;
        self.typing = None;
        cx.notify();
    }

    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus_handle);
    }

    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus_handle.is_focused(window)
    }

    /// Byte offset of the caret (the moving end of the selection).
    pub fn caret(&self) -> usize {
        self.cursor_offset()
    }

    pub fn set_caret(&mut self, pos: usize, cx: &mut Context<Self>) {
        let pos = pos.min(self.content.len());
        self.typing = None;
        let pos = self.snap_near(pos);
        self.selected_range = pos..pos;
        self.selection_reversed = false;
        self.want_x = None;
        cx.notify();
    }

    /// Text with the selection replaced (an emote picked from the picker); the caret lands after it.
    pub fn insert_at_caret(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text_in_range(None, text, window, cx);
    }

    /// Replace everything (autocomplete, history) and put the caret at `caret`. Emits Changed.
    pub fn replace_all(&mut self, text: &str, caret: usize, cx: &mut Context<Self>) {
        self.set_text(text, cx);
        let caret = caret.min(self.content.len());
        self.selected_range = caret..caret;
        self.typing = Some(caret);
        cx.emit(TextInputEvent::Changed);
    }

    fn is_area(&self) -> bool {
        self.style == InputStyle::Area
    }

    /// The tokens that are images right now (as far as editing is concerned: a still-loading one counts too).
    fn atoms(&self) -> Vec<atoms::Atom> {
        match &self.inline_images {
            Some(f) if self.is_area() && !self.masked && !self.content.is_empty() => atoms::find(&self.content, &**f, self.typing, self.marked_range.as_ref()),
            _ => Vec::new(),
        }
    }

    /// The word being typed stays text (no image) while the caret stays in it: Backspace / Left through a
    /// half-typed emote name. Elsewhere it is done and may show as its image.
    fn leave_typing(&mut self, offset: usize) {
        if !self.typing.is_some_and(|p| atoms::in_word(&self.content, p, offset)) {
            self.typing = None;
        }
    }

    /// `offset`, moved out of an image to its nearer side.
    fn snap_near(&self, offset: usize) -> usize {
        atoms::snap_near(&self.atoms(), offset)
    }

    // ---- movement ---------------------------------------------------------------------------------------

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.prev_word(self.cursor_offset()), cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.next_word(self.cursor_offset()), cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.prev_word(self.cursor_offset()), cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_word(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    /// Start of the visual row the caret is on (the whole text for single-line).
    fn row_start(&self, off: usize) -> usize {
        if let Some(area) = self.last_area.as_ref().filter(|_| self.is_area()) {
            let (_, _, li, ri) = area.caret_at(off);
            let l = &area.lines[li];
            return area.to_content(l.start + l.rows[ri].start);
        }
        0
    }

    fn row_end(&self, off: usize) -> usize {
        if let Some(area) = self.last_area.as_ref().filter(|_| self.is_area()) {
            let (_, _, li, ri) = area.caret_at(off);
            let l = &area.lines[li];
            let e = area.to_content(l.start + l.rows[ri].end);
            // before a wrap the caret stays on this row
            return if ri + 1 < l.rows.len() { self.previous_boundary(e).max(area.to_content(l.start + l.rows[ri].start)) } else { e };
        }
        self.content.len()
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.row_start(self.cursor_offset()), cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.row_end(self.cursor_offset()), cx);
    }

    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.row_start(self.cursor_offset()), cx);
    }

    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.row_end(self.cursor_offset()), cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete_word_left(&mut self, _: &DeleteWordLeft, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.prev_word(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete_word_right(&mut self, _: &DeleteWordRight, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_word(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    // ---- mouse ------------------------------------------------------------------------------------------

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        window.focus(&self.focus_handle);
        let at = self.index_for_mouse_position(event.position);
        match event.click_count {
            2 => {
                // double click: the word under the pointer
                let (mut s, mut e) = self.word_at(at);
                // an image is one word, whatever the text inside it looks like (`[emote:1:LUL]`)
                let (w0, w1) = (s, e);
                for a in self.atoms().iter().filter(|a| a.range.start < w1 && w0 < a.range.end) {
                    s = s.min(a.range.start);
                    e = e.max(a.range.end);
                }
                self.typing = None;
                self.selected_range = s..e;
                self.selection_reversed = false;
                self.is_selecting = false;
                cx.notify();
            }
            n if n >= 3 => {
                // triple click: the whole line (box)
                let (s, e) = (self.line_start(at), self.line_end(at));
                self.selected_range = s..e;
                self.selection_reversed = false;
                self.is_selecting = false;
                cx.notify();
            }
            _ => {
                self.is_selecting = true;
                self.want_x = None;
                if event.modifiers.shift {
                    self.select_to(at, cx);
                } else {
                    self.move_to(at, cx)
                }
            }
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        let ks = &event.keystroke;
        if ks.modifiers.control || ks.modifiers.alt {
            return;
        }
        match ks.key.as_str() {
            "enter" => {
                if self.is_area() && ks.modifiers.shift {
                    // Shift+Enter: a new line
                    self.insert_newline(cx);
                } else {
                    cx.emit(TextInputEvent::Enter);
                }
                cx.stop_propagation();
            }
            "escape" => {
                cx.emit(TextInputEvent::Escape);
                cx.stop_propagation();
            }
            "tab" if self.is_area() => {
                cx.emit(TextInputEvent::Tab);
                cx.stop_propagation();
            }
            "up" | "down" if self.is_area() => {
                self.move_vertical(ks.key == "down", ks.modifiers.shift, cx);
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    fn insert_newline(&mut self, cx: &mut Context<Self>) {
        let range = self.selected_range.clone();
        let mut s = self.content.to_string();
        if let Some(max) = self.max_chars {
            let have = s.chars().count() - s[range.clone()].chars().count();
            if have + 1 > max {
                return;
            }
        }
        s.replace_range(range.clone(), "\n");
        self.content = s.into();
        let p = range.start + 1;
        self.selected_range = p..p;
        self.selection_reversed = false;
        self.marked_range = None;
        cx.emit(TextInputEvent::Changed);
        cx.notify();
    }

    /// Up / Down in the textarea: one visual row; at the first / last row the caret goes to the start / end.
    fn move_vertical(&mut self, down: bool, extend: bool, cx: &mut Context<Self>) {
        let Some(area) = self.last_area.as_ref() else { return };
        let off = self.cursor_offset();
        let (cx_, cy, _, _) = area.caret_at(off);
        let x = self.want_x.unwrap_or(cx_);
        let ty = if down { cy + area.line_h * 1.5 } else { cy - area.line_h * 0.5 };
        let to = if ty < 0.0 {
            0
        } else if ty >= area.total_h {
            self.content.len()
        } else {
            area.index_at(x, ty)
        };
        if extend {
            self.select_to(to, cx);
        } else {
            self.move_to(to, cx);
        }
        self.want_x = Some(x);
    }

    // ---- clipboard --------------------------------------------------------------------------------------

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let t = if self.is_area() { text.replace("\r\n", "\n").replace('\r', "\n") } else { text.replace(['\r', '\n'], " ") };
            self.replace_text_in_range(None, &t, window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.selected_range.clone()].to_string()));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.selected_range.clone()].to_string()));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    // ---- selection helpers ------------------------------------------------------------------------------

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = self.snap_near(offset);
        self.leave_typing(offset);
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.want_x = None;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let Some(bounds) = self.last_bounds.as_ref() else { return 0 };
        if self.is_area() {
            let Some(area) = self.last_area.as_ref() else { return 0 };
            return area.index_at(f32::from(position.x - bounds.left()), f32::from(position.y - bounds.top()) + self.scroll_y);
        }
        let Some(line) = self.last_layout.as_ref() else { return 0 };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        let i = line.closest_index_for_x(position.x - bounds.left());
        if self.masked {
            return self.content.char_indices().nth(i / 3).map(|(b, _)| b).unwrap_or(self.content.len());
        }
        i
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = self.snap_near(offset);
        self.leave_typing(offset);
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    /// One grapheme back, or the whole image when the caret is right after (or in) one.
    fn previous_boundary(&self, offset: usize) -> usize {
        let g = self.content.grapheme_indices(true).rev().find_map(|(idx, _)| (idx < offset).then_some(idx)).unwrap_or(0);
        atoms::step_left(&self.atoms(), offset, g)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        let g = self.content.grapheme_indices(true).find_map(|(idx, _)| (idx > offset).then_some(idx)).unwrap_or(self.content.len());
        atoms::step_right(&self.atoms(), offset, g)
    }

    /// Ctrl+Left: skip spaces, then the word before them.
    fn prev_word(&self, offset: usize) -> usize {
        word_start_before(&self.content, offset)
    }

    fn next_word(&self, offset: usize) -> usize {
        word_end_after(&self.content, offset)
    }

    fn word_at(&self, offset: usize) -> (usize, usize) {
        let s: &str = &self.content;
        for (i, w) in s.split_word_bound_indices() {
            if offset >= i && offset <= i + w.len() && !w.trim().is_empty() {
                return (i, i + w.len());
            }
        }
        (offset, offset)
    }

    fn line_start(&self, offset: usize) -> usize {
        self.content[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0)
    }

    fn line_end(&self, offset: usize) -> usize {
        self.content[offset..].find('\n').map(|i| offset + i).unwrap_or(self.content.len())
    }

    /// What `text` may add: whatever fits under `maxLength` once `range` is gone.
    fn clip_to_max(&self, range: &Range<usize>, text: &str) -> String {
        match self.max_chars {
            None => text.to_string(),
            Some(max) => {
                let kept = self.content.chars().count() - self.content[range.clone()].chars().count();
                text.chars().take(max.saturating_sub(kept)).collect()
            }
        }
    }
}

/// Start of the word before `offset` (Ctrl+Left): spaces are skipped, then the run of non-spaces.
pub fn word_start_before(s: &str, offset: usize) -> usize {
    let head = &s[..offset.min(s.len())];
    let trimmed = head.trim_end();
    let word_len = trimmed.chars().rev().take_while(|ch| !ch.is_whitespace()).map(|ch| ch.len_utf8()).sum::<usize>();
    trimmed.len() - word_len
}

/// End of the word after `offset` (Ctrl+Right): the rest of the current word, then the spaces after it.
pub fn word_end_after(s: &str, offset: usize) -> usize {
    let tail = &s[offset.min(s.len())..];
    let word_len = tail.chars().take_while(|ch| !ch.is_whitespace()).map(|ch| ch.len_utf8()).sum::<usize>();
    let rest = &tail[word_len..];
    let space_len = rest.chars().take_while(|ch| ch.is_whitespace()).map(|ch| ch.len_utf8()).sum::<usize>();
    offset + word_len + space_len
}

impl EntityInputHandler for TextInput {
    fn text_for_range(&mut self, range_utf16: Range<usize>, actual_range: &mut Option<Range<usize>>, _window: &mut Window, _cx: &mut Context<Self>) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(&mut self, _ignore_disabled_input: bool, _window: &mut Window, _cx: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: self.range_to_utf16(&self.selected_range), reversed: self.selection_reversed })
    }

    fn marked_text_range(&self, _window: &mut Window, _cx: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range.as_ref().map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(&mut self, range_utf16: Option<Range<usize>>, new_text: &str, _: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        let range = range_utf16.as_ref().map(|r| self.range_from_utf16(r)).or(self.marked_range.clone()).unwrap_or(self.selected_range.clone());
        let new_text = self.clip_to_max(&range, new_text);
        self.content = (self.content[0..range.start].to_owned() + &new_text + &self.content[range.end..]).into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.selection_reversed = false;
        self.want_x = None;
        self.typing = Some(self.selected_range.end);
        self.marked_range.take();
        cx.emit(TextInputEvent::Changed);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(&mut self, range_utf16: Option<Range<usize>>, new_text: &str, new_selected_range_utf16: Option<Range<usize>>, _window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        let range = range_utf16.as_ref().map(|r| self.range_from_utf16(r)).or(self.marked_range.clone()).unwrap_or(self.selected_range.clone());
        let new_text = self.clip_to_max(&range, new_text);
        self.content = (self.content[0..range.start].to_owned() + &new_text + &self.content[range.end..]).into();
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .map(|new_range| (new_range.start + range.start).min(self.content.len())..(new_range.end + range.end).min(self.content.len()))
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.typing = Some(self.selected_range.end);
        self.want_x = None;
        cx.emit(TextInputEvent::Changed);
        cx.notify();
    }

    fn bounds_for_range(&mut self, range_utf16: Range<usize>, bounds: Bounds<Pixels>, _window: &mut Window, _cx: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        if let Some(area) = self.last_area.as_ref().filter(|_| self.is_area()) {
            let (x0, y0, ..) = area.caret_at(range.start);
            let (x1, ..) = area.caret_at(range.end);
            let top = bounds.top() + px(y0 - self.scroll_y);
            return Some(Bounds::from_corners(point(bounds.left() + px(x0), top), point(bounds.left() + px(x1.max(x0 + 1.0)), top + px(area.line_h))));
        }
        let last_layout = self.last_layout.as_ref()?;
        Some(Bounds::from_corners(
            point(bounds.left() + last_layout.x_for_index(range.start), bounds.top()),
            point(bounds.left() + last_layout.x_for_index(range.end), bounds.bottom()),
        ))
    }

    fn character_index_for_point(&mut self, point: gpui::Point<Pixels>, _window: &mut Window, _cx: &mut Context<Self>) -> Option<usize> {
        let bounds = self.last_bounds?;
        bounds.localize(&point)?;
        Some(self.offset_to_utf16(self.index_for_mouse_position(point)))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    /// `text-center`: how far the text is shifted right
    dx: Pixels,
    line: Option<ShapedLine>,
    area: Option<AreaLayout>,
    scroll_y: f32,
    cursor: Option<gpui::PaintQuad>,
    selection: Vec<gpui::PaintQuad>,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// The runs for `len` bytes of text in one colour, the composition range underlined.
fn make_runs(len: usize, marked: Option<&Range<usize>>, font: &gpui::Font, color: gpui::Hsla) -> Vec<TextRun> {
    let run = TextRun { len, font: font.clone(), color, background_color: None, underline: None, strikethrough: None };
    match marked {
        Some(m) if m.end <= len => vec![
            TextRun { len: m.start, ..run.clone() },
            TextRun { len: m.end - m.start, underline: Some(UnderlineStyle { color: Some(color), thickness: px(1.0), wavy: false }), ..run.clone() },
            TextRun { len: len - m.end, ..run },
        ]
        .into_iter()
        .filter(|r| r.len > 0)
        .collect(),
        _ => vec![run],
    }
}

/// Like `make_runs`, with the `hidden` display ranges (the placeholder dots of an image) in a transparent colour.
fn make_runs_hiding(len: usize, marked: Option<Range<usize>>, hidden: &[Range<usize>], font: &gpui::Font, color: gpui::Hsla) -> Vec<TextRun> {
    let mut cuts = vec![0, len];
    for r in hidden {
        cuts.extend([r.start, r.end]);
    }
    if let Some(m) = &marked {
        cuts.extend([m.start.min(len), m.end.min(len)]);
    }
    cuts.sort_unstable();
    cuts.dedup();
    cuts.windows(2)
        .map(|w| {
            let (s, e) = (w[0], w[1]);
            let hide = hidden.iter().any(|r| r.start <= s && e <= r.end);
            let under = marked.as_ref().is_some_and(|m| m.start <= s && e <= m.end);
            let color = if hide { gpui::transparent_black() } else { color };
            TextRun { len: e - s, font: font.clone(), color, background_color: None, underline: under.then(|| UnderlineStyle { color: Some(color), thickness: px(1.0), wavy: false }), strikethrough: None }
        })
        .collect()
}

/// Advance of the placeholder char an image is laid out as.
fn dot_width(window: &Window, font: &gpui::Font, font_size: Pixels) -> f32 {
    let run = TextRun { len: 8, font: font.clone(), color: gpui::black(), background_color: None, underline: None, strikethrough: None };
    let line = window.text_system().shape_line(SharedString::from("........"), font_size, &[run], None);
    (f32::from(line.width) / 8.0).max(0.5)
}

/// The image tokens of `input` that are loaded and can be drawn now, sized to the line, each laid out as a run of
/// placeholder dots about as wide as the picture. While one still loads its text is drawn as usual.
fn compute_shown(input: &TextInput, window: &Window, cx: &App, font: &gpui::Font, font_size: Pixels, line_h: f32, avail_w: f32) -> Vec<atoms::Shown> {
    if input.inline_images.is_none() || !input.is_area() || input.masked || input.content.is_empty() {
        return Vec::new();
    }
    let found = input.atoms();
    let Some(imgs) = cx.try_global::<ImagesHandle>().filter(|_| !found.is_empty()) else { return Vec::new() };
    let dot = dot_width(window, font, font_size);
    // wide emotes are capped (and shrink to keep their aspect) so one never fills the row
    let max_w = (line_h * 4.0).min(avail_w).max(1.0);
    let scale = window.scale_factor();
    let items = found
        .into_iter()
        .filter_map(|a| {
            let img = imgs.0.get(&a.url, max_w, line_h, scale)?;
            let (w, h) = atoms::fit_height(img.w as f32, img.h as f32, line_h, max_w);
            Some((a, w, h, ((w / dot).ceil() as usize).max(1)))
        })
        .collect();
    atoms::lay_out(items, max_w)
}

/// Max height of the textarea (`Math.min(el.scrollHeight, 96)`), minus its borders and padding.
const AREA_MAX_H: f32 = 96.0;
const AREA_PAD_Y: f32 = 12.0;
const AREA_BORDER: f32 = 2.0;

/// The textarea's box height: its scrollHeight (padding + lines, rounded like the DOM's integer) up to 96.
fn area_box_height(content_h: f32) -> f32 {
    (AREA_PAD_Y + content_h).round().min(AREA_MAX_H)
}

impl gpui::Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _id: Option<&GlobalElementId>, _inspector_id: Option<&gpui::InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        if self.input.read(cx).is_area() {
            style.size.width = relative(1.).into();
            let ts = window.text_style();
            let font = ts.font();
            let font_size = ts.font_size.to_pixels(window.rem_size());
            let line_h = f32::from(window.line_height());
            let input = self.input.clone();
            let id = window.request_measured_layout(style, move |known, avail, window, cx| {
                let w = known.width.map(f32::from).unwrap_or_else(|| match avail.width {
                    AvailableSpace::Definite(w) => f32::from(w),
                    _ => 10000.0,
                });
                let i = input.read(cx);
                let shown = compute_shown(i, window, cx, &font, font_size, line_h, w);
                let text: SharedString = if i.content.is_empty() {
                    i.placeholder.clone()
                } else if shown.is_empty() {
                    i.content.clone()
                } else {
                    atoms::display_text(&i.content, &shown).into()
                };
                let runs = make_runs(text.len(), None, &font, gpui::black());
                let lines = window.text_system().shape_text(text.clone(), font_size, &runs, Some(px(w)), None);
                let total = lines.map(|l| AreaLayout::build(l, line_h, text, shown).total_h).unwrap_or(line_h);
                let h = (area_box_height(total) - AREA_PAD_Y - AREA_BORDER).max(0.0);
                size(px(w), px(h))
            });
            return (id, ());
        }
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(&mut self, _id: Option<&GlobalElementId>, _inspector_id: Option<&gpui::InspectorElementId>, bounds: Bounds<Pixels>, _request_layout: &mut Self::RequestLayoutState, window: &mut Window, cx: &mut App) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let style = window.text_style();
        let disabled = input.disabled;

        // a masked (password) field shows one bullet (3 UTF-8 bytes) per char, so byte offsets are remapped
        let masked = input.masked && !content.is_empty();
        let map = |i: usize| if masked { content[..i.min(content.len())].chars().count() * 3 } else { i };
        let (display_text, text_color) = if content.is_empty() {
            (input.placeholder.clone(), c(theme::FAINT).into())
        } else if masked {
            (SharedString::from("\u{2022}".repeat(content.chars().count())), style.color)
        } else {
            (content.clone(), style.color)
        };
        let font = style.font();
        let marked = if content.is_empty() || masked { None } else { input.marked_range.as_ref() };
        let font_size = style.font_size.to_pixels(window.rem_size());

        if input.is_area() {
            let line_h = f32::from(window.line_height());
            let shown = compute_shown(input, window, cx, &font, font_size, line_h, f32::from(bounds.size.width));
            let (display_text, runs) = if shown.is_empty() {
                (display_text.clone(), make_runs(display_text.len(), marked, &font, text_color))
            } else {
                let t = atoms::display_text(&content, &shown);
                let hidden: Vec<Range<usize>> = shown.iter().map(|s| s.dstart..s.dstart + s.disp).collect();
                let marked = marked.map(|m| atoms::to_display(&shown, m.start)..atoms::to_display(&shown, m.end));
                let runs = make_runs_hiding(t.len(), marked, &hidden, &font, text_color);
                (SharedString::from(t), runs)
            };
            let lines = window.text_system().shape_text(display_text.clone(), font_size, &runs, Some(bounds.size.width), None).unwrap_or_default();
            let area = AreaLayout::build(lines, line_h, display_text, shown);
            let (cx_, cy, ..) = area.caret_at(if content.is_empty() { 0 } else { cursor });
            // keep the caret inside the visible part once the content is taller than the box
            let view_h = f32::from(bounds.size.height);
            let max_scroll = (area.total_h - view_h).max(0.0);
            let mut scroll = input.scroll_y.clamp(0.0, max_scroll);
            // Bottom first, then top, so the top wins: the one-line box (30px tall, scrollHeight rounding)
            // is ~1.5px shorter than a 17.55px line, and with `if top {..} else if bottom {..}` the scroll
            // flipped between 0 and 1.55 on every frame, which made the text shake while chat animated.
            if cy + line_h > scroll + view_h {
                scroll = cy + line_h - view_h;
            }
            if cy < scroll {
                scroll = cy;
            }
            scroll = scroll.clamp(0.0, max_scroll);

            let mut selection = Vec::new();
            let mut caret = None;
            if !content.is_empty() && !selected_range.is_empty() {
                // the shaped text has the images as runs of dots: select them by display offsets
                let selected_range = area.to_display(selected_range.start)..area.to_display(selected_range.end);
                let shaped_len = area.text.len();
                for l in &area.lines {
                    let l_end = l.start + l.line.len();
                    if selected_range.end < l.start || selected_range.start > l_end {
                        continue;
                    }
                    for (ri, r) in l.rows.iter().enumerate() {
                        let rs = l.start + r.start;
                        let re = l.start + r.end;
                        let s = selected_range.start.max(rs);
                        let e = selected_range.end.min(re);
                        let nl_selected = ri + 1 == l.rows.len() && selected_range.end > l_end && l_end < shaped_len;
                        if s >= e && !nl_selected {
                            continue;
                        }
                        let x0 = AreaLayout::x_in_row(l, r, s.saturating_sub(l.start).min(r.end).max(r.start));
                        let mut x1 = AreaLayout::x_in_row(l, r, e.saturating_sub(l.start).min(r.end).max(r.start));
                        if nl_selected {
                            x1 += 4.0; // the selected line break
                        }
                        selection.push(fill(
                            Bounds::new(point(bounds.left() + px(x0), bounds.top() + px(r.y - scroll)), size(px((x1 - x0).max(0.0)), px(line_h))),
                            a(theme::ACCENT, 0.45),
                        ));
                    }
                }
            } else if !disabled && self.input.update(cx, |i, _| i.caret_on()) {
                caret = Some(fill(Bounds::new(point(bounds.left() + px(cx_), bounds.top() + px(cy - scroll)), size(px(1.), px(line_h))), c(theme::TEXT)));
            }
            return PrepaintState { line: None, area: Some(area), scroll_y: scroll, cursor: caret, selection, dx: px(0.) };
        }

        let runs = make_runs(display_text.len(), marked, &font, text_color);
        let line = window.text_system().shape_line(display_text, font_size, &runs, None);
        let centered = input.centered;
        let dx = if centered { ((bounds.size.width - line.width) / 2.0).max(px(0.)) } else { px(0.) };
        let bounds = Bounds::new(point(bounds.left() + dx, bounds.top()), size(bounds.size.width - dx, bounds.size.height));

        let cursor_pos = line.x_for_index(map(cursor));
        let caret_on = self.input.update(cx, |i, _| i.caret_on());
        let (selection, cursor) = if selected_range.is_empty() && !caret_on {
            (vec![], None)
        } else if selected_range.is_empty() {
            // The caret is the text colour, 1px wide, like Chromium's.
            (vec![], Some(fill(Bounds::new(point(bounds.left() + cursor_pos, bounds.top()), size(px(1.), bounds.bottom() - bounds.top())), c(theme::TEXT))))
        } else {
            (
                vec![fill(
                    Bounds::from_corners(
                        point(bounds.left() + line.x_for_index(map(selected_range.start)), bounds.top()),
                        point(bounds.left() + line.x_for_index(map(selected_range.end)), bounds.bottom()),
                    ),
                    a(theme::ACCENT, 0.45),
                )],
                None,
            )
        };
        PrepaintState { line: Some(line), area: None, scroll_y: 0.0, cursor, selection, dx }
    }

    fn paint(&mut self, _id: Option<&GlobalElementId>, _inspector_id: Option<&gpui::InspectorElementId>, bounds: Bounds<Pixels>, _request_layout: &mut Self::RequestLayoutState, prepaint: &mut Self::PrepaintState, window: &mut Window, cx: &mut App) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(&focus_handle, ElementInputHandler::new(bounds, self.input.clone()), cx);
        let focused = focus_handle.is_focused(window);

        if let Some(area) = prepaint.area.take() {
            // The textarea clips to its padding box.
            let mask = Bounds::new(point(bounds.left(), bounds.top() - px(AREA_PAD_Y / 2.)), size(bounds.size.width, bounds.size.height + px(AREA_PAD_Y)));
            let scroll = prepaint.scroll_y;
            let line_h = px(area.line_h);
            let selection: Vec<gpui::PaintQuad> = std::mem::take(&mut prepaint.selection);
            let cursor = prepaint.cursor.take();
            window.with_content_mask(Some(ContentMask { bounds: mask }), |window| {
                for q in selection {
                    window.paint_quad(q);
                }
                for l in &area.lines {
                    let y = l.rows[0].y - scroll;
                    let _ = l.line.paint(point(bounds.left(), bounds.top() + px(y)), line_h, TextAlign::Left, None, window, cx);
                }
                if !area.shown.is_empty() {
                    if let Some(imgs) = cx.try_global::<ImagesHandle>().cloned() {
                        let scale = window.scale_factor();
                        for s in &area.shown {
                            let Some(img) = imgs.0.get(&s.url, s.max_w, area.line_h, scale) else { continue };
                            let (x, y, ..) = area.caret_at_display(s.dstart);
                            let r = Bounds::new(point(bounds.left() + px(x), bounds.top() + px(y - scroll + (area.line_h - s.h) / 2.0)), size(px(s.w), px(s.h)));
                            let idx = imgs.0.frame(&img, window, cx);
                            let _ = window.paint_image(r, gpui::Corners::all(px(0.)), img.gpu.clone(), idx, false);
                        }
                    }
                }
                if focused {
                    if let Some(cursor) = cursor {
                        window.paint_quad(cursor);
                    }
                }
            });
            self.input.update(cx, |input, _cx| {
                input.last_area = Some(area);
                input.last_bounds = Some(bounds);
                input.scroll_y = scroll;
            });
            return;
        }

        for q in prepaint.selection.drain(..) {
            window.paint_quad(q);
        }
        let line = prepaint.line.take().unwrap();
        let bounds = Bounds::new(point(bounds.left() + prepaint.dx, bounds.top()), size(bounds.size.width - prepaint.dx, bounds.size.height));
        line.paint(bounds.origin, window.line_height(), window, cx).unwrap();

        if focused {
            if let Some(cursor) = prepaint.cursor.take() {
                window.paint_quad(cursor);
            }
        }

        self.input.update(cx, |input, _cx| {
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);
        if focused && self.blink_task.is_none() {
            if let Some(ms) = caret_blink_ms() {
                self.blink_task = Some(cx.spawn_in(window, async move |this, cx| loop {
                    cx.background_executor().timer(std::time::Duration::from_millis(ms as u64)).await;
                    let alive = this
                        .update_in(cx, |t, window, cx| {
                            if t.focus_handle.is_focused(window) {
                                cx.notify();
                                true
                            } else {
                                t.blink_task = None;
                                false
                            }
                        })
                        .unwrap_or(false);
                    if !alive {
                        break;
                    }
                }));
            }
        }
        let base = div()
            .flex()
            .key_context("TextInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(if self.disabled { CursorStyle::Arrow } else { CursorStyle::IBeam })
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_word_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .bg(c(theme::APP))
            .border_1()
            .text_color(c(theme::TEXT))
            .text_size(px(12.))
            .line_height(px(18.))
            .overflow_hidden()
            .when(self.mono, |d| d.font_family("Consolas"));
        let child = div().w_full().min_w_0().child(TextElement { input: cx.entity() });
        match self.style {
            InputStyle::Field => base.w_full().rounded(px(4.)).px(px(8.)).py(px(4.)).border_color(if self.invalid { a(theme::RED_500, 0.7) } else { c(if focused { theme::ACCENT } else { theme::LINE }) }).child(child),
            InputStyle::Small => base.w_full().rounded(px(4.)).px(px(6.)).py(px(2.)).text_size(px(11.)).line_height(px(16.)).border_color(if self.invalid { a(theme::RED_500, 0.7) } else { c(if focused { theme::ACCENT } else { theme::LINE }) }).child(child),
            InputStyle::Chat => base.w_full().bg(c(theme::PANEL)).rounded(px(4.)).px(px(8.)).py(px(4.)).border_color(if focused { a(theme::ACCENT, 0.6) } else { c(theme::LINE).into() }).child(child),
            InputStyle::Bare => base.flex_1().min_w_0().bg(gpui::transparent_black()).border_0().line_height(px(16.)).child(child),
            InputStyle::Rename => base.w(px(128.)).rounded(px(2.)).px(px(4.)).border_color(c(theme::ACCENT)).child(child),
            // `rounded border border-line bg-panel px-2 py-1.5 text-[13px] leading-[1.35]`, focus:border-accent/60
            InputStyle::Area => base
                .flex_1()
                .min_w_0()
                .bg(c(theme::PANEL))
                .rounded(px(4.))
                .px(px(8.))
                .py(px(6.))
                .text_size(px(13.))
                .line_height(px(13. * 1.35))
                .border_color(if focused { a(theme::ACCENT, 0.6) } else { a(theme::LINE, 1.0) })
                .when(self.disabled, |d| d.opacity(0.6))
                .child(child),
        }
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Tokens drawn as inline images: which ranges they are, how the caret moves over them, and how content offsets
/// map to the shaped text, where each image is a run of placeholder dots as wide as the picture.
mod atoms {
    use std::ops::Range;

    use gpui::SharedString;

    /// An emote name is short; skipping longer tokens keeps the per-frame scan to a few lookups.
    const MAX_TOKEN: usize = 80;

    /// A token (byte range of the content) that is drawn as an image.
    #[derive(Clone, Debug, PartialEq)]
    pub struct Atom {
        pub range: Range<usize>,
        pub url: SharedString,
    }

    /// The whitespace-separated tokens `lookup` has an image for, except one still being typed (`typing` is where the
    /// last edit left the caret: a token that ends at it or around it) or touched by the IME composition.
    pub fn find(text: &str, lookup: &dyn Fn(&str) -> Option<SharedString>, typing: Option<usize>, marked: Option<&Range<usize>>) -> Vec<Atom> {
        let mut out = Vec::new();
        let mut start = None;
        for (i, ch) in text.char_indices().chain(std::iter::once((text.len(), ' '))) {
            if !ch.is_whitespace() {
                start.get_or_insert(i);
                continue;
            }
            let Some(s) = start.take() else { continue };
            if i - s > MAX_TOKEN || typing.is_some_and(|p| s < p && p <= i) || marked.is_some_and(|m| m.start < i && s < m.end) {
                continue;
            }
            if let Some(url) = lookup(&text[s..i]) {
                out.push(Atom { range: s..i, url });
            }
        }
        out
    }

    /// Whether `at` is inside, or at the end of, the whitespace-delimited word that ends at or contains `p`.
    pub fn in_word(text: &str, p: usize, at: usize) -> bool {
        let p = p.min(text.len());
        let start = text[..p].rfind(char::is_whitespace).map_or(0, |i| i + text[i..].chars().next().map_or(1, |c| c.len_utf8()));
        let end = text[p..].find(char::is_whitespace).map_or(text.len(), |i| p + i);
        start < at && at <= end
    }

    /// Where Left / Backspace go from `at`: the start of the image the caret is after (or in), else `prev`.
    pub fn step_left(atoms: &[Atom], at: usize, prev: usize) -> usize {
        atoms.iter().find(|a| a.range.start < at && at <= a.range.end).map_or(prev, |a| a.range.start)
    }

    /// Where Right / Delete go from `at`: the end of the image the caret is before (or in), else `next`.
    pub fn step_right(atoms: &[Atom], at: usize, next: usize) -> usize {
        atoms.iter().find(|a| a.range.start <= at && at < a.range.end).map_or(next, |a| a.range.end)
    }

    /// `at`, or the nearer side of the image it is inside.
    pub fn snap_near(atoms: &[Atom], at: usize) -> usize {
        match atoms.iter().find(|a| a.range.start < at && at < a.range.end) {
            Some(a) if at - a.range.start < a.range.end - at => a.range.start,
            Some(a) => a.range.end,
            None => at,
        }
    }

    /// An image that has its size: it takes `disp` placeholder bytes in the shaped text, from `dstart` on.
    #[derive(Clone, Debug, PartialEq)]
    pub struct Shown {
        pub range: Range<usize>,
        pub dstart: usize,
        pub disp: usize,
        pub url: SharedString,
        pub w: f32,
        pub h: f32,
        /// the box the picture was fetched for (the cache keys its decode on it)
        pub max_w: f32,
    }

    /// `items` are (atom, width, height, placeholder bytes) in text order.
    pub fn lay_out(items: Vec<(Atom, f32, f32, usize)>, max_w: f32) -> Vec<Shown> {
        let mut delta = 0isize;
        let mut out = Vec::with_capacity(items.len());
        for (a, w, h, disp) in items {
            let dstart = (a.range.start as isize + delta) as usize;
            delta += disp as isize - a.range.len() as isize;
            out.push(Shown { range: a.range, dstart, disp, url: a.url, w, h, max_w });
        }
        out
    }

    /// The text to shape: each shown token replaced by its placeholder dots (a word char, so a row never breaks inside one).
    pub fn display_text(content: &str, shown: &[Shown]) -> String {
        let mut s = String::with_capacity(content.len());
        let mut at = 0;
        for a in shown {
            s.push_str(&content[at..a.range.start]);
            s.extend(std::iter::repeat('.').take(a.disp));
            at = a.range.end;
        }
        s.push_str(&content[at..]);
        s
    }

    /// Content offset -> shaped-text offset (an offset inside an image, which the caret never is, maps to its start).
    pub fn to_display(shown: &[Shown], at: usize) -> usize {
        let mut delta = 0isize;
        for a in shown {
            if at <= a.range.start {
                break;
            }
            if at < a.range.end {
                return a.dstart;
            }
            delta += a.disp as isize - a.range.len() as isize;
        }
        (at as isize + delta) as usize
    }

    /// Shaped-text offset -> content offset; inside an image it is the nearer side.
    pub fn to_content(shown: &[Shown], at: usize) -> usize {
        let mut delta = 0isize;
        for a in shown {
            if at <= a.dstart {
                break;
            }
            if at < a.dstart + a.disp {
                return if at - a.dstart < a.dstart + a.disp - at { a.range.start } else { a.range.end };
            }
            delta += a.disp as isize - a.range.len() as isize;
        }
        (at as isize - delta) as usize
    }

    /// The size an image is drawn at: `h` tall unless that makes it wider than `max_w`, aspect kept.
    pub fn fit_height(nat_w: f32, nat_h: f32, h: f32, max_w: f32) -> (f32, f32) {
        if nat_w <= 0.0 || nat_h <= 0.0 {
            return (h, h);
        }
        let s = (h / nat_h).min(max_w / nat_w);
        (nat_w * s, nat_h * s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_jumps_skip_spaces_like_a_textarea() {
        let s = "hello  big world";
        assert_eq!(word_start_before(s, s.len()), 11);
        assert_eq!(word_start_before(s, 11), 7);
        assert_eq!(word_start_before(s, 7), 0);
        assert_eq!(word_start_before(s, 0), 0);
        assert_eq!(word_end_after(s, 0), 7);
        assert_eq!(word_end_after(s, 7), 11);
        assert_eq!(word_end_after(s, 11), s.len());
        assert_eq!(word_end_after(s, s.len()), s.len());
        // multi-byte text stays on char boundaries
        let t = "é\u{1f600} ab";
        assert_eq!(word_start_before(t, t.len()), t.len() - 2);
        assert_eq!(word_end_after(t, 0), t.len() - 2);
    }

    #[test]
    fn textarea_height_follows_scroll_height_up_to_96() {
        assert_eq!(area_box_height(13.0 * 1.35), 30.0);
        assert_eq!(area_box_height(13.0 * 1.35 * 2.0), 47.0);
        assert_eq!(area_box_height(13.0 * 1.35 * 3.0), 65.0);
        assert_eq!(area_box_height(13.0 * 1.35 * 10.0), 96.0);
    }
}

#[cfg(test)]
mod atom_tests {
    use super::atoms::*;
    use gpui::SharedString;

    fn lookup(tok: &str) -> Option<SharedString> {
        matches!(tok, "Kappa" | "LUL" | "[emote:1:Hi]").then(|| SharedString::from(format!("u/{tok}")))
    }

    fn ranges(text: &str, typing: Option<usize>, marked: Option<std::ops::Range<usize>>) -> Vec<std::ops::Range<usize>> {
        find(text, &lookup, typing, marked.as_ref()).into_iter().map(|a| a.range).collect()
    }

    #[test]
    fn whole_known_tokens_become_atoms() {
        assert_eq!(ranges("hi Kappa LUL x", None, None), vec![3..8, 9..12]);
        // a token only counts whole and case-sensitively
        assert!(ranges("Kappas kappa Kappa,", None, None).is_empty());
        assert_eq!(ranges("[emote:1:Hi]\nKappa", None, None), vec![0..12, 13..18]);
        assert_eq!(ranges("Kappa", None, None), vec![0..5]);
        assert!(ranges("", None, None).is_empty());
    }

    #[test]
    fn backspace_through_a_typed_emote_name() {
        // "Kappa" with the caret at its end is being typed: the caret moves inside it without it turning into
        // an image again (which snapped the caret back to its end, and Backspace deleted nothing)
        assert!(in_word("hi Kappa", 8, 7));
        assert!(in_word("hi Kappa", 8, 4));
        assert!(!in_word("hi Kappa", 8, 3));
        assert!(!in_word("hi Kappa x", 8, 10));
        assert!(in_word("Kappa", 5, 1));
        assert!(!in_word("", 0, 0));
        let typed = find("hi Kappa", &lookup, Some(8), None);
        assert_eq!(step_left(&typed, 8, 7), 7);
        assert_eq!(snap_near(&typed, 7), 7);
    }

    #[test]
    fn a_token_being_typed_stays_text() {
        // right after typing "Kappa" the caret touches its end
        assert!(ranges("hi Kappa", Some(8), None).is_empty());
        // mid-token edits too
        assert!(ranges("hi Kappa", Some(5), None).is_empty());
        // after the space the autocomplete adds, or before the token, it is an image
        assert_eq!(ranges("hi Kappa ", Some(9), None), vec![3..8]);
        assert_eq!(ranges("hi Kappa", Some(3), None), vec![3..8]);
        // an IME composition inside it keeps it text
        assert!(ranges("Kappa", None, Some(2..4)).is_empty());
        assert_eq!(ranges("Kappa LUL", None, Some(6..9)), vec![0..5]);
    }

    #[test]
    fn caret_steps_over_a_whole_image() {
        let text = "a Kappa b";
        let atoms = find(text, &lookup, None, None);
        // Left / Backspace from right after it, or from inside
        assert_eq!(step_left(&atoms, 7, 6), 2);
        assert_eq!(step_left(&atoms, 5, 4), 2);
        // from after the space: a plain step to the image's end
        assert_eq!(step_left(&atoms, 8, 7), 7);
        // Right / Delete from right before it
        assert_eq!(step_right(&atoms, 2, 3), 7);
        assert_eq!(step_right(&atoms, 0, 1), 1);
        assert_eq!(step_right(&atoms, 7, 8), 8);
        // the deleted range of a Backspace right after the image is the whole token
        assert_eq!(&text[step_left(&atoms, 7, 6)..7], "Kappa");
        assert_eq!(&text[2..step_right(&atoms, 2, 3)], "Kappa");
    }

    #[test]
    fn a_click_inside_an_image_lands_on_the_nearer_side() {
        let atoms = find("a Kappa b", &lookup, None, None);
        assert_eq!(snap_near(&atoms, 3), 2);
        assert_eq!(snap_near(&atoms, 6), 7);
        assert_eq!(snap_near(&atoms, 2), 2);
        assert_eq!(snap_near(&atoms, 7), 7);
        assert_eq!(snap_near(&atoms, 1), 1);
    }

    fn shown(text: &str, disp: usize) -> Vec<Shown> {
        let items = find(text, &lookup, None, None).into_iter().map(|a| (a, 10.0, 10.0, disp)).collect();
        lay_out(items, 40.0)
    }

    #[test]
    fn offsets_map_between_the_content_and_the_shaped_text() {
        // "a Kappa LUL b": Kappa (5 bytes) -> 3 dots, LUL (3 bytes) -> 6 dots
        let text = "a Kappa LUL b";
        let mut s = shown(text, 3);
        s[1].disp = 6;
        s = lay_out(s.into_iter().map(|x| (Atom { range: x.range, url: x.url }, 10.0, 10.0, x.disp)).collect(), 40.0);
        assert_eq!(display_text(text, &s), "a ... ...... b");
        assert_eq!(s[0].dstart, 2);
        assert_eq!(s[1].dstart, 6);
        for (c, d) in [(0, 0), (2, 2), (7, 5), (8, 6), (11, 12), (12, 13), (13, 14)] {
            assert_eq!(to_display(&s, c), d, "to_display({c})");
            assert_eq!(to_content(&s, d), c, "to_content({d})");
        }
        // inside an image: the nearer side
        assert_eq!(to_content(&s, 3), 2);
        assert_eq!(to_content(&s, 4), 7);
        assert_eq!(to_content(&s, 8), 8);
        assert_eq!(to_content(&s, 10), 11);
        assert_eq!(to_display(&s, 4), 2);
    }

    #[test]
    fn no_images_is_the_identity() {
        assert_eq!(to_display(&[], 5), 5);
        assert_eq!(to_content(&[], 5), 5);
        assert_eq!(display_text("abc", &[]), "abc");
    }

    #[test]
    fn images_keep_their_aspect_and_cap_their_width() {
        assert_eq!(fit_height(28.0, 28.0, 17.5, 70.0), (17.5, 17.5));
        let (w, h) = fit_height(112.0, 28.0, 17.5, 70.0);
        assert!((w - 70.0).abs() < 0.01 && (h - 17.5).abs() < 0.01);
        let (w, h) = fit_height(200.0, 28.0, 17.5, 70.0);
        assert!((w - 70.0).abs() < 0.01 && (h - 9.8).abs() < 0.01);
        assert_eq!(fit_height(0.0, 0.0, 17.5, 70.0), (17.5, 17.5));
    }
}
