//! Port of src/renderer/icons.tsx and platformIcons.tsx.
//! Every icon is an SVG file in assets/icons/ (generated from the TSX, same geometry and stroke widths),
//! painted as a GPUI monochrome sprite. Like the React icons they have no colour of their own: they take the
//! surrounding text colour (`currentColor`), resolved at paint time, so a parent's `hover(text_color)` recolours
//! them exactly as it does in CSS. `.color(..)` overrides it (the platform brand colours).
use agu_chat_core::protocol::Platform;
use gpui::{
    App, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, Pixels, SharedString, Style, TransformationMatrix,
    Window, px, size,
};

use crate::theme;

macro_rules! icons {
    ($($name:ident => $file:literal),* $(,)?) => {
        $(pub const $name: &str = concat!("icons/", $file, ".svg");)*
    };
}

icons! {
    GRIP => "grip", TRASH => "trash", COPY => "copy", REPLY => "reply", PIN => "pin", DOTS => "dots",
    DOTS_H => "dots-h", MINUS_CIRCLE => "minus-circle", EYE_OFF => "eye-off", SEARCH => "search", X => "x", COLUMNS => "columns",
    POPOUT => "popout", CHEVRON => "chevron", UPLOAD => "upload", LINK => "link", GEAR => "gear", SMILE => "smile",
    WARNING => "warning", CAMERA => "camera", SPEAKER => "speaker", SHIELD => "shield", SCREEN => "screen",
    BOT => "bot", MIC => "mic", GLOBE => "globe", FLAME => "flame", TROPHY => "trophy", TREND => "trend",
    CLOCK => "clock", MUSIC => "music", PENCIL => "pencil", CHAT => "chat", BELL => "bell", LIST => "list",
    TAG => "tag", GIFT => "gift", FISH => "fish", IMAGE => "image", TEXT => "text", COINS => "coins",
    SWORD => "sword", FLAG => "flag", CHECK => "check", WIN_MIN => "win-min", WIN_MAX => "win-max",
    WIN_RESTORE => "win-restore", STAR => "star", CROWN => "crown",
    BITS_10000 => "bits-10000", BITS_5000 => "bits-5000", BITS_1000 => "bits-1000", BITS_100 => "bits-100", BITS_0 => "bits-0",
    CHANNEL_POINTS => "channel-points",
    TWITCH => "platform-twitch", KICK => "platform-kick", YOUTUBE => "platform-youtube", RUMBLE => "platform-rumble",
    PLATFORM_X => "platform-x",
}

/// An icon element: a square of `size` px (`className="h-3.5 w-3.5"` = `icon(X, 14.0)`).
pub struct Icon {
    path: &'static str,
    size: f32,
    color: Option<Hsla>,
}

pub fn icon(path: &'static str, size: f32) -> Icon {
    Icon { path, size, color: None }
}

impl Icon {
    /// A fixed colour instead of the inherited text colour.
    pub fn color(mut self, color: impl Into<Hsla>) -> Self {
        self.color = Some(color.into());
        self
    }
}

impl IntoElement for Icon {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Icon {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _id: Option<&GlobalElementId>, _inspector_id: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size = size(px(self.size).into(), px(self.size).into());
        style.flex_shrink = 0.0;
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(&mut self, _id: Option<&GlobalElementId>, _inspector_id: Option<&InspectorElementId>, _bounds: Bounds<Pixels>, _state: &mut (), _window: &mut Window, _cx: &mut App) {}

    fn paint(&mut self, _id: Option<&GlobalElementId>, _inspector_id: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _rl: &mut (), _pp: &mut (), window: &mut Window, cx: &mut App) {
        let color = self.color.unwrap_or_else(|| window.text_style().color);
        let _ = window.paint_svg(bounds, SharedString::from(self.path), TransformationMatrix::unit(), color, cx);
    }
}

/// `PlatformIcon`: a platform's mark in its brand colour (`colored`) or the inherited colour.
pub fn platform_icon(platform: Platform, size: f32, colored: bool) -> Icon {
    let path = match platform {
        Platform::Twitch => TWITCH,
        Platform::Kick => KICK,
        Platform::Youtube => YOUTUBE,
        Platform::Rumble => RUMBLE,
        Platform::X => PLATFORM_X,
    };
    let i = icon(path, size);
    if colored {
        i.color(theme::c(theme::platform_color(platform)))
    } else {
        i
    }
}

pub fn platform_label(p: Platform) -> &'static str {
    match p {
        Platform::Twitch => "Twitch",
        Platform::Kick => "Kick",
        Platform::Youtube => "YouTube",
        Platform::Rumble => "Rumble",
        Platform::X => "X",
    }
}
