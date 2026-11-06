//! Port of src/renderer/layout/ui.tsx: small shared widgets for the layout (modal, popup menu, buttons,
//! toggles, input class). The text field lives in `crate::text_input`.
use std::rc::Rc;

use gpui::{
    anchored, deferred, div, point, prelude::*, px, AnyElement, App, Bounds, ClipboardItem, ClickEvent, Context, Corner, Edges, ElementId,
    MouseButton, MouseDownEvent, Pixels, Point, Render, SharedString, Window,
};

use crate::icons;
use crate::layout::app::AguApp;
use crate::theme::{self, a, c};

// ---- tooltips (the `title` attribute) ------------------------------------------------

/// What the browser shows for `title="..."`.
pub struct Tip(pub SharedString);

impl Render for Tip {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .font_family(theme::FONT)
            .text_size(px(12.))
            .line_height(px(16.))
            .text_color(c(theme::TEXT))
            .bg(c(theme::RAISED))
            .border_1()
            .border_color(c(theme::LINE))
            .px(px(6.))
            .py(px(2.))
            .child(self.0.clone())
    }
}

pub fn tip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> gpui::AnyView + 'static {
    let text: SharedString = text.into();
    move |_, cx| cx.new(|_| Tip(text.clone())).into()
}

// ---- Modal ------------------------------------------------------------------------

pub type CloseFn = Rc<dyn Fn(&mut Window, &mut App)>;

/// Centered modal. Esc (handled by the window's key handler) and a click on the backdrop close it.
pub fn modal(title: &str, width: f32, on_close: CloseFn, body: impl IntoElement, footer: Option<AnyElement>) -> AnyElement {
    let close_backdrop = on_close.clone();
    let close_btn = on_close.clone();
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(a(theme::BLACK, 0.6))
        .p(px(16.))
        .occlude()
        .on_mouse_down(MouseButton::Left, move |_: &MouseDownEvent, w, cx| close_backdrop(w, cx))
        .child(
            div()
                .flex()
                .flex_col()
                .max_h_full()
                .w(px(width))
                .max_w_full()
                .overflow_hidden()
                .rounded(px(8.))
                .border_1()
                .border_color(c(theme::LINE))
                .bg(c(theme::RAISED))
                .shadow_2xl()
                .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .border_b_1()
                        .border_color(c(theme::LINE))
                        .px(px(12.))
                        .py(px(8.))
                        .child(div().text_size(px(13.)).font_weight(gpui::FontWeight::SEMIBOLD).child(title.to_string()))
                        .child(
                            div()
                                .id("modal-close")
                                .rounded(px(4.))
                                .p(px(4.))
                                .text_color(c(theme::MUTED))
                                .hover(|s| s.bg(c(theme::RAISED_2)).text_color(c(theme::TEXT)))
                                .on_click(move |_: &ClickEvent, w, cx| close_btn(w, cx))
                                .child(icons::icon(icons::X, 14.)),
                        ),
                )
                .child(div().min_h_0().flex_1().child(body))
                .children(footer.map(|f| {
                    div().flex().items_center().justify_end().gap(px(8.)).border_t_1().border_color(c(theme::LINE)).px(px(12.)).py(px(8.)).child(f)
                })),
        )
        .into_any_element()
}

// ---- PopupMenu --------------------------------------------------------------------

pub type MenuAction = Rc<dyn Fn(&mut AguApp, &mut Window, &mut Context<AguApp>)>;

pub struct MenuItem {
    pub label: String,
    pub hint: Option<String>,
    pub danger: bool,
    pub disabled: bool,
    pub active: bool,
    pub action: MenuAction,
}

pub enum MenuEntry {
    Item(MenuItem),
    Heading(String),
    Sep,
}

impl MenuItem {
    pub fn new(label: impl Into<String>, action: impl Fn(&mut AguApp, &mut Window, &mut Context<AguApp>) + 'static) -> Self {
        MenuItem { label: label.into(), hint: None, danger: false, disabled: false, active: false, action: Rc::new(action) }
    }
    pub fn hint(mut self, h: Option<String>) -> Self {
        self.hint = h;
        self
    }
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }
    pub fn disabled(mut self, d: bool) -> Self {
        self.disabled = d;
        self
    }
    pub fn active(mut self, a: bool) -> Self {
        self.active = a;
        self
    }
}

/// An open popup menu anchored at a screen point (the `anchor` bounds are the opener, so pressing it
/// again is left to its own click handler to toggle the menu shut).
pub struct MenuState {
    pub key: String,
    pub pos: Point<Pixels>,
    pub align_right: bool,
    pub entries: Vec<MenuEntry>,
    pub anchor: Option<Bounds<Pixels>>,
}

/// A popup menu anchored at a screen rectangle (drawn on top of everything, so split borders never clip it).
pub fn popup_menu(menu: &MenuState, cx: &mut Context<AguApp>) -> AnyElement {
    let anchor_bounds = menu.anchor;
    let mut panel = div()
        .id("popup-menu")
        .occlude()
        .min_w(px(170.))
        .max_h(gpui::relative(0.7))
        .overflow_y_scroll()
        .rounded(px(4.))
        .border_1()
        .border_color(c(theme::LINE))
        .bg(c(theme::RAISED))
        .py(px(4.))
        .text_size(px(12.))
        .shadow_xl()
        .on_mouse_down_out(cx.listener(move |this, ev: &MouseDownEvent, _w, cx| {
            if anchor_bounds.map(|b| b.contains(&ev.position)).unwrap_or(false) {
                return;
            }
            this.close_menu(cx);
        }))
        .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| cx.stop_propagation());

    for (i, entry) in menu.entries.iter().enumerate() {
        match entry {
            MenuEntry::Sep => {
                panel = panel.child(div().my(px(4.)).h(px(1.)).bg(c(theme::LINE)));
            }
            MenuEntry::Heading(h) => {
                panel = panel.child(
                    div()
                        .px(px(12.))
                        .pb(px(2.))
                        .pt(px(4.))
                        .text_size(px(10.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(c(theme::FAINT))
                        .truncate()
                        .child(crate::chat::inline::tracked(&h.to_uppercase())),
                );
            }
            MenuEntry::Item(it) => {
                let color = if it.danger {
                    c(theme::RED_400)
                } else if it.active {
                    c(theme::ACCENT_SOFT)
                } else {
                    c(theme::TEXT)
                };
                let action = it.action.clone();
                let mut row = div()
                    .id(ElementId::Integer(i as u64))
                    .flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap(px(16.))
                    .px(px(12.))
                    .py(px(4.))
                    .text_color(color)
                    .child(div().min_w_0().truncate().when(it.active, |d| d.font_weight(gpui::FontWeight::SEMIBOLD)).child(it.label.clone()))
                    .children(it.hint.clone().map(|h| div().text_size(px(11.)).text_color(c(theme::FAINT)).child(h)));
                if it.disabled {
                    row = row.opacity(0.4);
                } else {
                    row = row.hover(|s| s.bg(c(theme::RAISED_2))).on_click(cx.listener(move |this, _: &ClickEvent, w, cx| {
                        this.close_menu(cx);
                        action(this, w, cx);
                    }));
                }
                panel = panel.child(row);
            }
        }
    }

    let corner = if menu.align_right { Corner::TopRight } else { Corner::TopLeft };
    deferred(anchored().position(menu.pos).anchor(corner).snap_to_window_with_margin(Edges::all(px(4.))).child(panel)).with_priority(2).into_any_element()
}

// ---- Btn ---------------------------------------------------------------------------

#[derive(Default, Clone, Copy)]
pub struct BtnOpts {
    pub primary: bool,
    pub danger: bool,
    pub disabled: bool,
    pub small: bool,
}

impl BtnOpts {
    pub fn primary() -> Self {
        BtnOpts { primary: true, ..Default::default() }
    }
    pub fn danger() -> Self {
        BtnOpts { danger: true, ..Default::default() }
    }
}

/// `Btn`: rounded border px-3 py-1 text-[12px] font-medium (small: px-2 py-0.5 text-[11px]). The caller adds `.on_click`.
pub fn btn(id: impl Into<ElementId>, label: impl Into<SharedString>, o: BtnOpts) -> gpui::Stateful<gpui::Div> {
    let mut b = div()
        .id(id)
        .flex_none()
        .rounded(px(4.))
        .border_1()
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_size(px(if o.small { 11. } else { 12. }))
        .line_height(px(if o.small { 16.5 } else { 18. }))
        .when(o.small, |d| d.px(px(8.)).py(px(2.)))
        .when(!o.small, |d| d.px(px(12.)).py(px(4.)));
    b = if o.primary {
        b.bg(c(theme::ACCENT)).text_color(c(theme::WHITE)).border_color(gpui::transparent_black()).hover(|s| s.bg(c(theme::ACCENT_STRONG)))
    } else if o.danger {
        b.border_color(a(theme::RED_500, 0.4)).text_color(c(theme::RED_300)).hover(|s| s.bg(a(theme::RED_500, 0.1)))
    } else {
        b.border_color(c(theme::LINE)).text_color(c(theme::TEXT)).hover(|s| s.bg(c(theme::RAISED_2)))
    };
    if o.disabled {
        b = b.opacity(0.4);
    }
    b.child(label.into())
}

// ---- Toggle ------------------------------------------------------------------------

/// `Toggle`: a 28x16 switch (h-4 w-7), knob 12px.
pub fn toggle(id: impl Into<ElementId>, checked: bool, disabled: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .relative()
        .flex_none()
        .w(px(28.))
        .h(px(16.))
        .rounded_full()
        .bg(if checked { c(theme::ACCENT) } else { c(theme::SCROLLBAR_THUMB) })
        .when(disabled, |d| d.opacity(0.4))
        .child(div().absolute().top(px(2.)).left(px(if checked { 14. } else { 2. })).size(px(12.)).rounded_full().bg(c(theme::WHITE)))
}

/// `copyText`
pub fn copy_text(cx: &mut App, text: &str) {
    cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
}

pub fn pt(x: Pixels, y: Pixels) -> Point<Pixels> {
    point(x, y)
}
