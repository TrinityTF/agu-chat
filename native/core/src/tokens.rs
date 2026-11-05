//! Width-independent message atoms. A frontend lays these out however it likes.
use crate::irc::{ChatMsg, Kind};
use crate::maps::Maps;

#[derive(Debug, Clone)]
pub enum Atom {
    Time(String),
    Badge { url: String, title: String },
    /// Display name (with trailing ':' unless /me) and its readable color.
    Name { text: String, rgb: [u8; 3] },
    Word(String),
    /// Third-party or Twitch emote. `provider` is a short label ("7TV", "Twitch", ...).
    Emote { name: String, url: String, provider: &'static str },
}

pub fn fmt_time(ts_ms: i64, tz_min: i32) -> String {
    let m = (ts_ms / 1000 / 60 + tz_min as i64).rem_euclid(24 * 60);
    format!("{:02}:{:02}", m / 60, m % 60)
}

/// Opacity multiplier for dimmed rows (deleted / history).
pub fn dim(msg: &ChatMsg) -> f32 {
    if msg.deleted {
        0.38
    } else if msg.historical {
        0.82
    } else {
        1.0
    }
}

/// Name color: lighten too-dark colors, hash the login to a hue when the user has none.
pub fn name_rgb(msg: &ChatMsg) -> [u8; 3] {
    match msg.color {
        Some([r, g, b]) => {
            let lum = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
            let target = 125.0;
            let k = if lum < target { ((target - lum) / (255.0 - lum)).clamp(0.0, 1.0) } else { 0.0 };
            let f = |c: u8| (c as f32 + (255.0 - c as f32) * k) as u8;
            [f(r), f(g), f(b)]
        }
        None => {
            let mut h: u32 = 2166136261;
            for b in msg.login.bytes() {
                h = (h ^ b as u32).wrapping_mul(16777619);
            }
            hsv((h % 360) as f32 / 360.0, 0.55, 0.95)
        }
    }
}

fn hsv(h: f32, s: f32, v: f32) -> [u8; 3] {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - f * s), v * (1.0 - (1.0 - f) * s));
    let (r, g, b) = match (i as i32).rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    [(r * 255.0 + 0.5) as u8, (g * 255.0 + 0.5) as u8, (b * 255.0 + 0.5) as u8]
}

pub fn tokenize(msg: &ChatMsg, ch: &Maps, gl: &Maps, tz_min: i32) -> Vec<Atom> {
    let mut out = vec![Atom::Time(fmt_time(msg.ts_ms, tz_min))];
    match msg.kind {
        Kind::Chat => {
            for (set, ver) in &msg.badges {
                let key = (set.clone(), ver.clone());
                if let Some((url, title)) = ch.badges.get(&key).or_else(|| gl.badges.get(&key)) {
                    out.push(Atom::Badge { url: url.clone(), title: title.clone() });
                }
            }
            let text = if msg.action { msg.display.clone() } else { format!("{}:", msg.display) };
            out.push(Atom::Name { text, rgb: name_rgb(msg) });

            let chars: Vec<char> = msg.text.chars().collect();
            let mut word = String::new();
            let mut ei = 0;
            let mut i = 0;
            let flush = |out: &mut Vec<Atom>, word: &mut String| {
                if word.is_empty() {
                    return;
                }
                match ch.emotes.get(word.as_str()).or_else(|| gl.emotes.get(word.as_str())) {
                    Some((url, prov, _)) => {
                        out.push(Atom::Emote { name: word.clone(), url: url.clone(), provider: prov.label() })
                    }
                    None => out.push(Atom::Word(word.clone())),
                }
                word.clear();
            };
            while i < chars.len() {
                while ei < msg.emotes.len() && msg.emotes[ei].end < i {
                    ei += 1;
                }
                if ei < msg.emotes.len() && msg.emotes[ei].start == i && msg.emotes[ei].end < chars.len() {
                    flush(&mut out, &mut word);
                    let e = &msg.emotes[ei];
                    let name: String = chars[e.start..=e.end].iter().collect();
                    let url = format!("https://static-cdn.jtvnw.net/emoticons/v2/{}/default/dark/1.0", e.id);
                    out.push(Atom::Emote { name, url, provider: "Twitch" });
                    i = e.end + 1;
                    ei += 1;
                    continue;
                }
                let c = chars[i];
                if c.is_whitespace() {
                    flush(&mut out, &mut word);
                } else {
                    word.push(c);
                }
                i += 1;
            }
            flush(&mut out, &mut word);
        }
        Kind::System | Kind::Notice => {
            for w in msg.text.split_whitespace() {
                out.push(Atom::Word(w.to_string()));
            }
        }
    }
    out
}
