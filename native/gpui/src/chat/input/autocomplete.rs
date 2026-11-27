//! The two autocomplete dropdowns. The matching and ranking behind them is `agu_chat_model::input::autocomplete`.
use gpui::{div, prelude::*, px, relative, AnyElement, ElementId, FontWeight, MouseButton, MouseDownEvent, SharedString};

pub use agu_chat_model::input::autocomplete::*;
use crate::chat::pane_render::ImageBox;
use crate::theme::{self, a, c};

// ---- dropdowns --------------------------------------------------------------------------------------

/// `AutocompleteDropdown`: absolute bottom-full left-0 z-30 mb-1 max-h-56 w-72, rows px-2 py-1 text-xs.
/// `on_pick` runs on mouse down (the input must not lose focus before the pick).
pub fn autocomplete_dropdown(state: &AutoState, scroll: &gpui::ScrollHandle, on_pick: impl Fn(usize, &mut gpui::Window, &mut gpui::App) + Clone + 'static) -> AnyElement {
    let is_cmd = state.mode == AutoMode::Cmd;
    let mut panel = div()
        .id("auto-dd")
        .max_h(px(224.))
        .overflow_y_scroll()
        .track_scroll(scroll)
        .py(px(4.))
        .text_size(px(12.))
        .line_height(px(16.));
    for (i, m) in state.matches.iter().enumerate() {
        let sel = i == state.idx;
        let pick = on_pick.clone();
        let mut row = div()
            .id(ElementId::Integer(i as u64))
            .flex()
            .w_full()
            .items_center()
            .gap(px(8.))
            .px(px(8.))
            .py(px(4.))
            .text_color(if sel { c(theme::TEXT) } else if m.dim { c(theme::FAINT) } else { c(theme::MUTED) })
            .when(sel, |d| d.bg(a(theme::ACCENT, 0.25)))
            .when(!sel, |d| d.hover(|s| s.bg(c(theme::RAISED_2))))
            .on_mouse_down(MouseButton::Left, move |_: &MouseDownEvent, w, cx| {
                pick(i, w, cx);
                cx.stop_propagation();
            });
        if is_cmd {
            row = row
                .child(div().flex_none().font_family(theme::MONO_CMD).font_weight(FontWeight::SEMIBOLD).text_color(c(theme::ACCENT_SOFT)).child(SharedString::from(format!("/{}", m.name))))
                .child(div().min_w_0().flex_1().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(c(theme::FAINT)).child(SharedString::from(m.label.clone())));
            if m.dim {
                row = row.child(div().flex_none().text_size(px(10.)).text_color(c(theme::AMBER_500)).child("(not available)"));
            }
        } else {
            row = row
                .child(if m.img.is_empty() {
                    div().size(px(20.)).flex_none().flex().items_center().justify_center().text_color(c(theme::FAINT)).child("@").into_any_element()
                } else {
                    ImageBox { url: m.img.clone().into(), side: 20.0, radius: 0.0, fallback: None }.into_any_element()
                })
                .child(div().min_w_0().flex_1().overflow_hidden().whitespace_nowrap().text_ellipsis().child(SharedString::from(m.name.clone())))
                .child(div().flex_none().text_size(px(10.)).text_color(c(theme::FAINT)).child(SharedString::from(m.label.clone())));
        }
        panel = panel.child(row);
    }
    div()
        .absolute()
        .bottom(relative(1.))
        .left_0()
        .mb(px(4.))
        .w(px(288.))
        .occlude()
        .rounded(px(4.))
        .border_1()
        .border_color(c(theme::LINE))
        .bg(c(theme::RAISED))
        .shadow_2xl()
        .child(panel)
        .into_any_element()
}

/// `MentionDropdown`: like the above, w-56, an "@" then the name.
pub fn mention_dropdown(state: &MentionState, scroll: &gpui::ScrollHandle, on_pick: impl Fn(usize, &mut gpui::Window, &mut gpui::App) + Clone + 'static) -> AnyElement {
    let mut panel = div()
        .id("mention-dd")
        .max_h(px(224.))
        .overflow_y_scroll()
        .track_scroll(scroll)
        .py(px(4.))
        .text_size(px(12.))
        .line_height(px(16.));
    for (i, name) in state.matches.iter().enumerate() {
        let sel = i == state.idx;
        let pick = on_pick.clone();
        panel = panel.child(
            div()
                .id(ElementId::Integer(i as u64))
                .flex()
                .w_full()
                .items_center()
                .gap(px(8.))
                .px(px(8.))
                .py(px(4.))
                .text_color(if sel { c(theme::TEXT) } else { c(theme::MUTED) })
                .when(sel, |d| d.bg(a(theme::ACCENT, 0.25)))
                .when(!sel, |d| d.hover(|s| s.bg(c(theme::RAISED_2))))
                .on_mouse_down(MouseButton::Left, move |_: &MouseDownEvent, w, cx| {
                    pick(i, w, cx);
                    cx.stop_propagation();
                })
                .child(div().flex_none().text_color(c(theme::FAINT)).child("@"))
                .child(div().min_w_0().flex_1().overflow_hidden().whitespace_nowrap().text_ellipsis().child(SharedString::from(name.clone()))),
        );
    }
    div()
        .absolute()
        .bottom(relative(1.))
        .left_0()
        .mb(px(4.))
        .w(px(224.))
        .occlude()
        .rounded(px(4.))
        .border_1()
        .border_color(c(theme::LINE))
        .bg(c(theme::RAISED))
        .shadow_2xl()
        .child(panel)
        .into_any_element()
}

