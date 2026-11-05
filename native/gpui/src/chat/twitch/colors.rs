//! Port of src/renderer/chat/twitch/colors.ts: fallback name colours and the "readable colors" lift.
use crate::chat::types::ChatUser;

const NAME_COLORS: [&str; 15] = [
    "#FF0000", "#0000FF", "#008000", "#B22222", "#FF7F50", "#9ACD32", "#FF4500", "#2E8B57", "#DAA520", "#D2691E", "#5F9EA0", "#1E90FF", "#FF69B4", "#8A2BE2", "#00FF7F",
];

fn hash_login(login: &str) -> usize {
    // charCodeAt sums UTF-16 code units
    login.encode_utf16().map(|u| u as usize).sum()
}

pub fn fallback_color(login: &str) -> &'static str {
    NAME_COLORS[hash_login(login) % NAME_COLORS.len()]
}

pub fn hex_to_rgb(hex: &str) -> (u8, u8, u8) {
    let h = hex.replace('#', "");
    let n: String = if h.len() == 3 { h.chars().flat_map(|c| [c, c]).collect() } else { h };
    let num = u32::from_str_radix(&n, 16).unwrap_or(0);
    ((num >> 16) as u8, (num >> 8) as u8, num as u8)
}

fn rel_luminance(hex: &str) -> f32 {
    let (r, g, b) = hex_to_rgb(hex);
    let f = |v: u8| {
        let s = v as f32 / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b)
}

fn hex_to_hsl(hex: &str) -> (f32, f32, f32) {
    let (r0, g0, b0) = hex_to_rgb(hex);
    let (r, g, b) = (r0 as f32 / 255.0, g0 as f32 / 255.0, b0 as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let (mut h, mut s) = (0.0, 0.0);
    let l = (max + min) / 2.0;
    if max != min {
        let d = max - min;
        s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
        h = if max == r {
            (g - b) / d + if g < b { 6.0 } else { 0.0 }
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        h /= 6.0;
    }
    (h * 360.0, s, l)
}

fn hsl_to_hex(h: f32, s: f32, l: f32) -> String {
    let hue = h / 360.0;
    let hue2rgb = |p: f32, q: f32, t: f32| {
        let mut tt = t;
        if tt < 0.0 {
            tt += 1.0;
        }
        if tt > 1.0 {
            tt -= 1.0;
        }
        if tt < 1.0 / 6.0 {
            p + (q - p) * 6.0 * tt
        } else if tt < 0.5 {
            q
        } else if tt < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - tt) * 6.0
        } else {
            p
        }
    };
    let (r, g, b) = if s == 0.0 {
        (l, l, l)
    } else {
        let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
        let p = 2.0 * l - q;
        (hue2rgb(p, q, hue + 1.0 / 3.0), hue2rgb(p, q, hue), hue2rgb(p, q, hue - 1.0 / 3.0))
    };
    let t = |v: f32| (v * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", t(r), t(g), t(b))
}

pub fn readable_color(hex: &str) -> String {
    if rel_luminance(hex) >= 0.3 {
        return hex.to_string();
    }
    let (h, s, l) = hex_to_hsl(hex);
    hsl_to_hex(h, s, l.max(0.65))
}

pub fn name_color(user: &ChatUser, readable: bool) -> String {
    let base = if user.color.is_empty() { fallback_color(&user.login).to_string() } else { user.color.clone() };
    if readable { readable_color(&base) } else { base }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_colours_are_lifted_light_ones_kept() {
        assert_eq!(readable_color("#ffffff"), "#ffffff");
        let lifted = readable_color("#0000ff");
        assert_ne!(lifted, "#0000ff");
        assert!(rel_luminance(&lifted) > 0.1);
    }

    #[test]
    fn fallback_is_stable() {
        assert_eq!(fallback_color("a"), NAME_COLORS[97 % 15]);
    }
}
