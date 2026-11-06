//! Controls the settings dialog needs that the DOM gave for free: `<select>`, `<input type="range">`,
//! `<input type="checkbox">`. Port of the Tailwind classes used in layout/SettingsDialog.tsx
//! (`selectCls`, `accent-[var(--color-accent)]`). State lives in `SettingsDialog`; these are only the looks.
use std::cell::Cell;
use std::rc::Rc;

use gpui::{canvas, div, prelude::*, px, AnyElement, Bounds, ElementId, Pixels, SharedString, Stateful};

use crate::icons;
use crate::theme::{self, a, c};

pub fn eid(s: impl Into<String>) -> ElementId {
    ElementId::Name(SharedString::from(s.into()))
}

/// Text of a given size with Tailwind's default `line-height: 1.5` (preflight sets it on `html`).
pub fn txt(size: f32) -> gpui::Div {
    div().text_size(px(size)).line_height(px(size * 1.5))
}

/// A `<select>`'s closed look: border, bg-app, the current label and the arrow. `small`: `px-1.5` (else `px-2`).
pub fn select_button(id: ElementId, label: &str, width: f32, small: bool, open: bool) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_between()
        .gap(px(6.))
        .w(px(width))
        .rounded(px(4.))
        .border_1()
        .border_color(c(if open { theme::ACCENT } else { theme::LINE }))
        .bg(c(theme::APP))
        .px(px(if small { 6. } else { 8. }))
        .py(px(4.))
        .text_size(px(12.))
        .line_height(px(18.))
        .text_color(c(theme::TEXT))
        .child(div().min_w_0().truncate().child(label.to_string()))
        .child(div().flex_none().child(icons::icon(icons::CHEVRON, 10.).color(c(theme::MUTED))))
}

/// A drop-down list under its button.
pub fn select_list(width: f32, rows: Vec<AnyElement>) -> AnyElement {
    gpui::deferred(
        div()
            .absolute()
            .top_full()
            .left_0()
            .mt(px(2.))
            .min_w(px(width))
            .rounded(px(4.))
            .border_1()
            .border_color(c(theme::LINE))
            .bg(c(theme::RAISED))
            .py(px(4.))
            .shadow_xl()
            .occlude()
            .children(rows),
    )
    .with_priority(3)
    .into_any_element()
}

pub fn select_row(id: ElementId, label: &str, active: bool) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .px(px(8.))
        .py(px(3.))
        .text_size(px(12.))
        .line_height(px(18.))
        .text_color(c(if active { theme::ACCENT_SOFT } else { theme::TEXT }))
        .hover(|s| s.bg(c(theme::RAISED_2)))
        .child(label.to_string())
}

/// `<input type="checkbox" className="h-3 w-3 accent-...">`
pub fn checkbox(checked: bool) -> gpui::Div {
    div()
        .flex_none()
        .size(px(12.))
        .rounded(px(2.))
        .border_1()
        .border_color(c(if checked { theme::ACCENT } else { theme::FAINT }))
        .bg(if checked { c(theme::ACCENT) } else { a(theme::BLACK, 0.0) })
        .flex()
        .items_center()
        .justify_center()
        .when(checked, |d| d.child(icons::icon(icons::CHECK, 9.).color(c(theme::WHITE))))
}

/// `<input type="range" className="w-32 accent-...">`: the track, the filled part and the thumb. The caller makes
/// it interactive (it records `bounds` so a drag can turn the pointer position into a value).
pub fn slider(id: ElementId, frac: f32, bounds: Rc<Cell<Bounds<Pixels>>>) -> Stateful<gpui::Div> {
    let w = 128.0;
    let thumb = 14.0;
    let x = (w - thumb) * frac.clamp(0.0, 1.0);
    div()
        .id(id)
        .relative()
        .flex_none()
        .w(px(w))
        .h(px(16.))
        .cursor_pointer()
        .child(canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {}).absolute().top_0().left_0().size_full())
        .child(div().absolute().top(px(6.)).left_0().w_full().h(px(4.)).rounded_full().bg(c(theme::SCROLLBAR_THUMB_HOVER)))
        .child(div().absolute().top(px(6.)).left_0().w(px(x + thumb / 2.)).h(px(4.)).rounded_full().bg(c(theme::ACCENT)))
        .child(div().absolute().top(px(1.)).left(px(x)).size(px(thumb)).rounded_full().bg(c(theme::ACCENT)).border_1().border_color(a(theme::BLACK, 0.2)))
}
