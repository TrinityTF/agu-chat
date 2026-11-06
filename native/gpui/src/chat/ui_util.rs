//! Port of src/renderer/chat/ui.ts (`fmtTime`) plus the colour helpers the rows share.
//! `toLocaleTimeString([], {hour: "2-digit", minute: "2-digit"[, second: "2-digit"]})` follows the user's Windows
//! locale; here the 12h / 24h choice is read from `LOCALE_STIMEFORMAT` of the user locale.
use std::sync::OnceLock;

use chrono::{Local, TimeZone, Timelike};
use gpui::{Hsla, Rgba};

use crate::theme;

fn uses_12h() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| {
        #[cfg(windows)]
        {
            use windows::core::PCWSTR;
            use windows::Win32::Globalization::{GetLocaleInfoEx, LOCALE_STIMEFORMAT};
            let mut buf = [0u16; 80];
            let n = unsafe { GetLocaleInfoEx(PCWSTR::null(), LOCALE_STIMEFORMAT, Some(&mut buf)) };
            if n > 0 {
                let s = String::from_utf16_lossy(&buf[..(n as usize).saturating_sub(1)]);
                return s.contains("tt") || s.contains('t');
            }
        }
        false
    })
}

pub fn fmt_time(ts_ms: i64, seconds: bool) -> String {
    let Some(dt) = Local.timestamp_millis_opt(ts_ms).single() else { return String::new() };
    let (h, m, s) = (dt.hour(), dt.minute(), dt.second());
    if uses_12h() {
        let h12 = if h % 12 == 0 { 12 } else { h % 12 };
        let ap = if h < 12 { "AM" } else { "PM" };
        if seconds { format!("{h12:02}:{m:02}:{s:02} {ap}") } else { format!("{h12:02}:{m:02} {ap}") }
    } else if seconds {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{h:02}:{m:02}")
    }
}

/// "#rrggbb" / "#rgb" -> Hsla (a bad value is the muted text colour).
pub fn hex(color: &str) -> Hsla {
    let (r, g, b) = crate::chat::twitch::colors::hex_to_rgb(color);
    Rgba { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a: 1.0 }.into()
}

pub fn token(hexv: u32) -> Hsla {
    theme::c(hexv).into()
}

pub fn token_a(hexv: u32, alpha: f32) -> Hsla {
    theme::a(hexv, alpha).into()
}

pub fn with_alpha(c: Hsla, a: f32) -> Hsla {
    Hsla { a: c.a * a, ..c }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_has_the_shape_of_the_locale() {
        let t = fmt_time(1_700_000_000_000, false);
        assert!(t.len() >= 5);
        assert!(fmt_time(1_700_000_000_000, true).len() > t.len());
    }
}

/// `btnPrimary` / `btnGhost` / `btnDanger` of chat/ui.ts: `rounded px-2.5 py-1 text-xs`, the layout's `BtnOpts` pick which.
pub fn cbtn(id: impl Into<gpui::ElementId>, label: impl Into<gpui::SharedString>, o: crate::layout::ui::BtnOpts) -> gpui::Stateful<gpui::Div> {
    use gpui::{div, prelude::*, px, FontWeight};
    use crate::theme::{self, a, c};
    let b = div().id(id).flex_none().rounded(px(4.)).px(px(10.)).py(px(4.)).text_size(px(12.)).line_height(px(16.));
    let b = if o.primary {
        b.bg(c(theme::ACCENT)).font_weight(FontWeight::SEMIBOLD).text_color(c(theme::WHITE)).when(!o.disabled, |d| d.hover(|s| s.bg(c(theme::ACCENT_STRONG))))
    } else if o.danger {
        b.border_1().border_color(a(theme::RED_500, 0.4)).bg(a(theme::RED_500, 0.1)).font_weight(FontWeight::SEMIBOLD).text_color(c(theme::RED_300)).when(!o.disabled, |d| d.hover(|s| s.bg(a(theme::RED_500, 0.2))))
    } else {
        b.border_1().border_color(c(theme::LINE)).text_color(c(theme::MUTED)).when(!o.disabled, |d| d.hover(|s| s.border_color(a(theme::WHITE, 0.25)).text_color(c(theme::TEXT))))
    };
    b.when(o.disabled, |d| d.opacity(0.4)).child(label.into())
}
