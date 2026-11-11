//! Port of src/renderer/index.css (the `@theme` tokens and the few global rules).
//! Use these constants, never raw hex, exactly like the CSS uses `var(--color-*)`.
use gpui::Rgba;

pub const APP: u32 = 0x0b0b10; // window background
pub const PANEL: u32 = 0x111117; // split / chat background
pub const PANEL_ALT: u32 = 0x15151c; // alternating message rows
pub const RAISED: u32 = 0x16161d; // headers, inputs, popovers
pub const RAISED_2: u32 = 0x1c1c24; // hover
pub const LINE: u32 = 0x26262f; // 1px borders between splits
pub const ACCENT: u32 = 0x9147ff; // Twitch / agu purple
pub const ACCENT_SOFT: u32 = 0xa970ff;
pub const ACCENT_STRONG: u32 = 0x9333ea;
pub const TEXT: u32 = 0xe4e4e7;
pub const MUTED: u32 = 0xa1a1aa;
pub const FAINT: u32 = 0x71717a;
pub const KICK: u32 = 0x53fc18;
pub const YOUTUBE: u32 = 0xff0033;
pub const RUMBLE: u32 = 0x85c742;
pub const X: u32 = 0xffffff;
/// --color-mention: rgba(145, 71, 255, 0.18)
pub const MENTION: (u32, f32) = (0x9147ff, 0.18);

// Raw values the CSS hard-codes outside the @theme block.
pub const SCROLLBAR_THUMB: u32 = 0x2c2c36; // ::-webkit-scrollbar-thumb, also the Toggle's off track
pub const SCROLLBAR_THUMB_HOVER: u32 = 0x3a3a46;

// Tailwind v4 palette values (sRGB) used by the layout classes.
pub const RED_300: u32 = 0xffa2a2;
pub const RED_400: u32 = 0xff6467;
pub const RED_500: u32 = 0xfb2c36;
pub const RED_600: u32 = 0xe7000b;
pub const AMBER_400: u32 = 0xffb900;
pub const EMERALD_500: u32 = 0x00bc7d;
pub const ZINC_500: u32 = 0x71717b;
pub const ORANGE_500: u32 = 0xff6900;
pub const WHITE: u32 = 0xffffff;
pub const BLACK: u32 = 0x000000;

/// The font Chromium's `system-ui` resolves to on Windows.
#[cfg(target_os = "macos")]
pub const FONT: &str = ".SystemUIFont";
#[cfg(not(target_os = "macos"))]
pub const FONT: &str = "Segoe UI";
/// Monospace for patterns and code-like text (Consolas on Windows, Menlo on macOS).
#[cfg(target_os = "macos")]
pub const MONO: &str = "Menlo";
#[cfg(not(target_os = "macos"))]
pub const MONO: &str = "Consolas";
/// The slash-command name in the autocomplete list (Cascadia Mono on Windows).
#[cfg(target_os = "macos")]
pub const MONO_CMD: &str = "Menlo";
#[cfg(not(target_os = "macos"))]
pub const MONO_CMD: &str = "Cascadia Mono";

/// `var(--color-x)`
pub fn c(hex: u32) -> Rgba {
    a(hex, 1.0)
}

/// `bg-[var(--color-x)]/NN`: a token at an opacity.
pub fn a(hex: u32, alpha: f32) -> Rgba {
    Rgba { r: ((hex >> 16) & 0xff) as f32 / 255.0, g: ((hex >> 8) & 0xff) as f32 / 255.0, b: (hex & 0xff) as f32 / 255.0, a: alpha }
}

pub fn mention() -> Rgba {
    a(MENTION.0, MENTION.1)
}

/// Platform brand colours (platformIcons.tsx PLATFORM_COLOR).
pub fn platform_color(p: agu_chat_core::protocol::Platform) -> u32 {
    use agu_chat_core::protocol::Platform::*;
    match p {
        Twitch => 0xa970ff,
        Kick => 0x53fc18,
        Youtube => 0xff0033,
        Rumble => 0x85c742,
        X => 0xffffff,
    }
}

// More Tailwind v4 palette values (sRGB) the chat rows use.
pub const PURPLE_200: u32 = 0xe9d4ff;
pub const PURPLE_300: u32 = 0xdab2ff;
pub const PURPLE_500: u32 = 0xad46ff;
pub const ORANGE_200: u32 = 0xffd6a7;
pub const ORANGE_300: u32 = 0xffb86a;
pub const AMBER_200: u32 = 0xfee685;
pub const AMBER_300: u32 = 0xffd230;
pub const AMBER_500: u32 = 0xfe9a00;
pub const GREEN_300: u32 = 0x7bf1a8;
pub const GREEN_500: u32 = 0x00c950;
pub const SKY_300: u32 = 0x74d4ff;
pub const SKY_400: u32 = 0x00bcff;
pub const SLATE_500: u32 = 0x62748e;
pub const SLATE_600: u32 = 0x45556c;
pub const ROSE_500: u32 = 0xff2056;
pub const YELLOW_500: u32 = 0xefb100;
pub const LIME_500: u32 = 0x7ccf00;
