//! Port of src/renderer/icons.tsx and platformIcons.tsx.
//! Every icon is an SVG file in assets/icons/ (generated from the TSX, same geometry and stroke widths),
//! drawn by GPUI's `svg()`. Like the React icons they have no colour of their own: they take the surrounding
//! text colour (`currentColor`), so set `.text_color(..)` on the icon or any parent.
use agu_chat_core::protocol::Platform;
use gpui::{prelude::*, px, svg, Rgba, Svg};

use crate::theme;

macro_rules! icons {
    ($($name:ident => $file:literal),* $(,)?) => {
        $(pub const $name: &str = concat!("icons/", $file, ".svg");)*
    };
}

icons! {
    GRIP => "grip", TRASH => "trash", COPY => "copy", REPLY => "reply", PIN => "pin", DOTS => "dots",
    MINUS_CIRCLE => "minus-circle", EYE_OFF => "eye-off", SEARCH => "search", X => "x", COLUMNS => "columns",
    POPOUT => "popout", CHEVRON => "chevron", UPLOAD => "upload", LINK => "link", GEAR => "gear", SMILE => "smile",
    WARNING => "warning", CAMERA => "camera", SPEAKER => "speaker", SHIELD => "shield", SCREEN => "screen",
    BOT => "bot", MIC => "mic", GLOBE => "globe", FLAME => "flame", TROPHY => "trophy", TREND => "trend",
    CLOCK => "clock", MUSIC => "music", PENCIL => "pencil", CHAT => "chat", BELL => "bell", LIST => "list",
    TAG => "tag", GIFT => "gift", FISH => "fish", IMAGE => "image", TEXT => "text", COINS => "coins",
    SWORD => "sword", FLAG => "flag", CHECK => "check", WIN_MIN => "win-min", WIN_MAX => "win-max",
    WIN_RESTORE => "win-restore", STAR => "star", CROWN => "crown",
    TWITCH => "platform-twitch", KICK => "platform-kick", YOUTUBE => "platform-youtube", RUMBLE => "platform-rumble",
    PLATFORM_X => "platform-x",
}

/// An icon at `size` px square (`className="h-3.5 w-3.5"` = `icon(X, 14.0)`), in the inherited text colour.
pub fn icon(path: &'static str, size: f32) -> Svg {
    svg().path(path).size(px(size)).flex_none()
}

/// `PlatformIcon`: a platform's mark in its brand colour (`colored`) or the inherited colour.
pub fn platform_icon(platform: Platform, size: f32, colored: bool) -> Svg {
    let path = match platform {
        Platform::Twitch => TWITCH,
        Platform::Kick => KICK,
        Platform::Youtube => YOUTUBE,
        Platform::Rumble => RUMBLE,
        Platform::X => PLATFORM_X,
    };
    let s = icon(path, size);
    if colored {
        s.text_color(Rgba::from(theme::c(theme::platform_color(platform))))
    } else {
        s
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
