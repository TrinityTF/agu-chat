//! The link preview card under a message (Chatterino-style): a thumbnail on the left at a fixed size, a bold title
//! (one or two lines, cut off with an ellipsis) and a muted line under it. A post on X has no picture: its card
//! shows the X mark instead. The card takes no space until its preview is in; then the row is measured again
//! (`ChatPane::drain_events`), and `measure` decides the card's height, which the render reuses, so the two agree.
use std::cell::Cell;
use std::rc::Rc;

use agu_chat_core::link_preview::{match_url, LinkTarget, Preview, Provider};
use gpui::{
    point, px, size, App, Bounds, ContentMask, Element, ElementId, FontWeight, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, SharedString,
    Style, Window,
};

use super::images::ImagesHandle;
use super::inline::{plain_text_width, Act, InlineContent, Item};
use crate::theme;

/// The card is never wider than this, however wide the chat is.
pub const CARD_MAX_W: f32 = 440.0;
pub const THUMB_W: f32 = 89.0;
pub const THUMB_H: f32 = 50.0;
/// the provider mark of a card without a picture
pub const MARK: f32 = 16.0;
pub const PAD: f32 = 5.0;
pub const GAP: f32 = 8.0;
pub const BORDER: f32 = 1.0;
/// space above and below the card inside the row
pub const MARGIN_T: f32 = 3.0;
pub const MARGIN_B: f32 = 2.0;

/// The previewable link of a message: the first one in it.
#[derive(Clone, Debug)]
pub struct LinkCard {
    /// the link as posted (what a click opens)
    pub href: String,
    pub target: LinkTarget,
    /// filled in by the row's measure once the preview is in; the render draws exactly this
    pub layout: Rc<Cell<Option<CardLayout>>>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CardLayout {
    pub w: f32,
    pub h: f32,
    pub title_lines: u8,
}

impl CardLayout {
    /// The height the card adds to its row.
    pub fn row_h(&self) -> f32 {
        MARGIN_T + self.h + MARGIN_B
    }
}

/// Title and subtitle text sizes and line heights for a chat font size.
pub fn fonts(base: f32) -> (f32, f32, f32, f32) {
    let title = (base * 0.95).round();
    let sub = (base * 0.85).round();
    (title, (title * 1.3).round(), sub, (sub * 1.35).round())
}

/// The first link in a message flow that one of the providers can preview.
pub fn first_link(content: &InlineContent) -> Option<LinkCard> {
    content.items.iter().find_map(|it| match it {
        Item::Text(t) => match &t.act {
            Some(Act::Url(u)) => match_url(u).map(|target| LinkCard { href: u.to_string(), target, layout: Rc::new(Cell::new(None)) }),
            _ => None,
        },
        _ => None,
    })
}

/// The card's size inside a row whose content is `avail_w` wide.
pub fn measure(p: &Preview, avail_w: f32, base: f32, window: &Window) -> CardLayout {
    let w = avail_w.min(CARD_MAX_W).max(120.0);
    let (title_px, title_lh, _, sub_lh) = fonts(base);
    let lead_w = if p.thumb.is_some() { THUMB_W } else { MARK };
    let text_w = (w - 2.0 * (BORDER + PAD) - lead_w - GAP).max(20.0);
    let one_line = plain_text_width(window, &p.title, title_px, FontWeight::SEMIBOLD) <= text_w;
    let title_lines: u8 = if one_line { 1 } else { 2 };
    let text_h = title_lines as f32 * title_lh + if p.subtitle.is_empty() { 0.0 } else { sub_lh };
    let lead_h = if p.thumb.is_some() { THUMB_H } else { MARK };
    CardLayout { w, h: text_h.max(lead_h) + 2.0 * (BORDER + PAD), title_lines }
}

pub fn provider_icon(p: Provider) -> &'static str {
    match p {
        Provider::TwitchClip => crate::icons::TWITCH,
        Provider::YouTube => crate::icons::YOUTUBE,
        Provider::X => crate::icons::PLATFORM_X,
        Provider::Streamable | Provider::Instagram => crate::icons::LINK,
    }
}

/// A thumbnail drawn from the shared image cache into a fixed box, cropped to fill it (`object-fit: cover`), on a
/// placeholder while it loads.
pub struct Thumb {
    pub url: SharedString,
    pub w: f32,
    pub h: f32,
}

impl IntoElement for Thumb {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Thumb {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }
    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size = size(px(self.w).into(), px(self.h).into());
        style.flex_shrink = 0.0;
        (window.request_layout(style, [], cx), ())
    }
    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut Window, _: &mut App) {}
    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut App) {
        window.paint_quad(gpui::fill(bounds, super::ui_util::token(theme::RAISED_2)).corner_radii(px(3.)));
        let imgs = cx.global::<ImagesHandle>().clone();
        // decoded big enough to be cropped from either side without being scaled up
        let side = self.w.max(self.h) * 2.0;
        let Some(img) = imgs.0.get(&self.url, side, side, window.scale_factor()) else { return };
        let (nw, nh) = (img.w as f32, img.h as f32);
        if nw <= 0.0 || nh <= 0.0 {
            return;
        }
        let s = (self.w / nw).max(self.h / nh);
        let (w, h) = (nw * s, nh * s);
        let r = Bounds::new(point(bounds.origin.x + px((self.w - w) / 2.0), bounds.origin.y + px((self.h - h) / 2.0)), size(px(w), px(h)));
        let idx = imgs.0.frame(&img, window, cx);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            let _ = window.paint_image(r, gpui::Corners::default(), img.gpu.clone(), idx, false);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::inline::TextItem;
    use gpui::Hsla;

    #[test]
    fn the_first_previewable_link_wins() {
        let text = |t: &str| Item::Text(TextItem::new(t.to_string(), 13.0, Hsla::default()));
        let link = |u: &str| Item::Text(TextItem::new(u.to_string(), 13.0, Hsla::default()).act(Act::Url(u.to_string().into()), false));
        let c = InlineContent::with_spans(
            vec![text("look "), link("https://example.com/a"), text(" and "), link("https://youtu.be/dQw4w9WgXcQ"), link("https://x.com/jack/status/20")],
            13.0,
            1.45,
            vec![],
        );
        let l = first_link(&c).unwrap();
        assert_eq!(l.target.key, "youtube:dQw4w9WgXcQ");
        assert_eq!(l.href, "https://youtu.be/dQw4w9WgXcQ");
        let none = InlineContent::with_spans(vec![text("https://youtu.be/dQw4w9WgXcQ"), link("https://twitch.tv/esfandtv")], 13.0, 1.45, vec![]);
        assert!(first_link(&none).is_none(), "plain text is not a link, a channel page has no card");
    }

    #[test]
    fn fonts_scale_with_the_chat_font() {
        assert_eq!(fonts(13.0), (12.0, 16.0, 11.0, 15.0));
        let (t, tl, s, sl) = fonts(20.0);
        assert!(t > 12.0 && tl > t && s < t && sl > s);
    }
}
