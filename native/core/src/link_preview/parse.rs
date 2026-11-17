//! Each provider's answer read into a `Preview`. None = nothing worth a card (no such item, or no title).
use serde_json::Value;

use super::{Preview, Provider};

/// The whitespace of a title collapsed (captions and tweets carry line breaks), trimmed.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn str_of<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").trim()
}

/// A thumbnail URL as https (Streamable hands out protocol-relative ones), or None when it is not a web URL.
fn https(u: &str) -> Option<String> {
    let u = u.trim();
    let full = if let Some(rest) = u.strip_prefix("//") { format!("https://{rest}") } else { u.to_string() };
    (full.starts_with("https://") || full.starts_with("http://")).then_some(full)
}

/// `1234` -> "1.2K", `1500000` -> "1.5M".
pub fn compact_count(n: u64) -> String {
    let f = |v: f64, s: &str| {
        let t = format!("{v:.1}");
        format!("{}{s}", t.strip_suffix(".0").unwrap_or(&t))
    };
    if n >= 1_000_000_000 {
        f(n as f64 / 1e9, "B")
    } else if n >= 1_000_000 {
        f(n as f64 / 1e6, "M")
    } else if n >= 1_000 {
        f(n as f64 / 1e3, "K")
    } else {
        n.to_string()
    }
}

/// Seconds as "0:58" / "1:02:03".
pub fn duration(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Helix `GET /clips?id=` -> the clip's card: its title, "Clipped by X · channel · 0:30 · 1.2K views".
pub fn twitch_clips(v: &Value) -> Option<Preview> {
    let c = v.get("data")?.as_array()?.first()?;
    let title = one_line(str_of(c, "title"));
    if title.is_empty() {
        return None;
    }
    let mut sub: Vec<String> = Vec::new();
    let creator = str_of(c, "creator_name");
    if !creator.is_empty() {
        sub.push(format!("Clipped by {creator}"));
    }
    let channel = str_of(c, "broadcaster_name");
    if !channel.is_empty() {
        sub.push(channel.to_string());
    }
    if let Some(d) = c.get("duration").and_then(|d| d.as_f64()).filter(|d| *d > 0.0) {
        sub.push(duration(d));
    }
    if let Some(n) = c.get("view_count").and_then(|d| d.as_u64()) {
        sub.push(format!("{} {}", compact_count(n), if n == 1 { "view" } else { "views" }));
    }
    Some(Preview { provider: Provider::TwitchClip, title, subtitle: sub.join(" · "), thumb: https(str_of(c, "thumbnail_url")) })
}

/// YouTube oEmbed -> title, channel name, thumbnail.
pub fn youtube_oembed(v: &Value) -> Option<Preview> {
    let title = one_line(str_of(v, "title"));
    if title.is_empty() {
        return None;
    }
    Some(Preview { provider: Provider::YouTube, title, subtitle: one_line(str_of(v, "author_name")), thumb: https(str_of(v, "thumbnail_url")) })
}

/// Streamable oEmbed -> title (often empty: then "Streamable video"), thumbnail.
pub fn streamable_oembed(v: &Value) -> Option<Preview> {
    if v.get("type").is_none() && v.get("html").is_none() {
        return None;
    }
    let title = one_line(str_of(v, "title"));
    Some(Preview {
        provider: Provider::Streamable,
        title: if title.is_empty() { "Streamable video".into() } else { title },
        subtitle: "Streamable".into(),
        thumb: https(str_of(v, "thumbnail_url")),
    })
}

/// X oEmbed -> the post's text (out of its blockquote HTML) and who posted it; no image.
pub fn x_oembed(v: &Value) -> Option<Preview> {
    let text = x_html_text(str_of(v, "html"));
    let name = one_line(str_of(v, "author_name"));
    let handle = str_of(v, "author_url").trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string();
    if text.is_empty() && name.is_empty() {
        return None;
    }
    let subtitle = match (name.is_empty(), handle.is_empty()) {
        (false, false) => format!("{name} @{handle}"),
        (false, true) => name.clone(),
        (true, false) => format!("@{handle}"),
        _ => String::new(),
    };
    let title = if text.is_empty() { format!("Post by {name}") } else { text };
    Some(Preview { provider: Provider::X, title, subtitle, thumb: None })
}

/// The plain text of the post in an X oEmbed blockquote: the first `<p>`, tags stripped, entities decoded, line
/// breaks as spaces, the trailing `pic.twitter.com/...` media links dropped.
pub fn x_html_text(html: &str) -> String {
    let body = match html.find("<p") {
        Some(start) => {
            let rest = &html[start..];
            let open_end = rest.find('>').map(|i| i + 1).unwrap_or(rest.len());
            let inner = &rest[open_end..];
            &inner[..inner.find("</p>").unwrap_or(inner.len())]
        }
        None => "",
    };
    let with_breaks = body.replace("<br>", " ").replace("<br/>", " ").replace("<br />", " ");
    let text = decode_entities(&strip_tags(&with_breaks));
    one_line(&text).split(' ').filter(|w| !w.starts_with("pic.twitter.com/") && !w.starts_with("pic.x.com/")).collect::<Vec<_>>().join(" ")
}

/// Instagram post page -> OpenGraph: og:title is `<name> on Instagram: "<caption>"`, og:image the picture.
pub fn instagram_page(html: &str) -> Option<Preview> {
    let og = og_tags(html);
    let get = |k: &str| og.iter().find(|(p, _)| p == k).map(|(_, v)| v.clone()).unwrap_or_default();
    let raw_title = one_line(&get("og:title"));
    if raw_title.is_empty() {
        return None; // a login wall / missing post: no OpenGraph tags
    }
    let (who, caption) = match raw_title.split_once(" on Instagram: ") {
        Some((who, rest)) => (who.trim().to_string(), rest.trim().trim_matches(|c| c == '"' || c == '\u{201c}' || c == '\u{201d}').trim().to_string()),
        None => (String::new(), raw_title.clone()),
    };
    let (title, subtitle) = match (caption.is_empty(), who.is_empty()) {
        (false, false) => (caption, format!("{who} · Instagram")),
        (false, true) => (caption, "Instagram".to_string()),
        (true, false) => (format!("Post by {who}"), "Instagram".to_string()),
        (true, true) => return None,
    };
    Some(Preview { provider: Provider::Instagram, title, subtitle, thumb: https(&get("og:image")) })
}

/// Every `<meta property="og:..." content="...">` of a page, in order, with the content's entities decoded.
pub fn og_tags(html: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find("<meta") {
        let after = &rest[i + 5..];
        let end = after.find('>').unwrap_or(after.len());
        let tag = &after[..end];
        rest = &after[end..];
        let prop = attr(tag, "property").or_else(|| attr(tag, "name"));
        if let (Some(p), Some(c)) = (prop, attr(tag, "content")) {
            if p.starts_with("og:") {
                out.push((p, decode_entities(&c)));
            }
        }
    }
    out
}

/// The value of `name="..."` (or single-quoted) inside one tag.
fn attr(tag: &str, name: &str) -> Option<String> {
    let mut from = 0;
    while let Some(off) = tag[from..].find(name) {
        let at = from + off;
        from = at + name.len();
        // a whole attribute name: preceded by whitespace
        if at > 0 && !tag[..at].ends_with(char::is_whitespace) {
            continue;
        }
        let after = tag[from..].trim_start();
        let Some(after) = after.strip_prefix('=') else { continue };
        let after = after.trim_start();
        let q = after.chars().next()?;
        if q != '"' && q != '\'' {
            continue;
        }
        let body = &after[1..];
        return body.find(q).map(|e| body[..e].to_string());
    }
    None
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// HTML entities: the named ones these pages use and every numeric one.
pub fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let semi = tail.bytes().take(12).position(|b| b == b';');
        let decoded = semi.and_then(|e| {
            let name = &tail[1..e];
            let ch = if let Some(num) = name.strip_prefix('#') {
                let n = if let Some(h) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) { u32::from_str_radix(h, 16).ok() } else { num.parse().ok() };
                n.and_then(char::from_u32).map(String::from)
            } else {
                match name {
                    "amp" => Some("&"),
                    "lt" => Some("<"),
                    "gt" => Some(">"),
                    "quot" => Some("\""),
                    "apos" => Some("'"),
                    "nbsp" => Some(" "),
                    "mdash" => Some("\u{2014}"),
                    "ndash" => Some("\u{2013}"),
                    "hellip" => Some("\u{2026}"),
                    "lsquo" => Some("\u{2018}"),
                    "rsquo" => Some("\u{2019}"),
                    "ldquo" => Some("\u{201c}"),
                    "rdquo" => Some("\u{201d}"),
                    "bull" => Some("\u{2022}"),
                    "middot" => Some("\u{b7}"),
                    _ => None,
                }
                .map(String::from)
            };
            ch.map(|c| (c, e + 1))
        });
        match decoded {
            Some((c, len)) => {
                out.push_str(&c);
                rest = &tail[len..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn twitch_clip_from_helix() {
        // the shape of Helix's Get Clips answer (the example in Twitch's API reference)
        let p = twitch_clips(&json(include_str!("fixtures/twitch_clips.json"))).unwrap();
        assert_eq!(p.title, "babymetal");
        assert_eq!(p.subtitle, "Clipped by BlackNova03 · ChewieMelodies · 1:00 · 10 views");
        assert_eq!(p.thumb.as_deref(), Some("https://clips-media-assets.twitch.tv/157589949-preview-480x272.jpg"));
        assert_eq!(p.provider, Provider::TwitchClip);
        // a clip that is gone: empty data
        assert_eq!(twitch_clips(&json(r#"{"data":[],"pagination":{}}"#)), None);
    }

    #[test]
    fn youtube_from_oembed() {
        let p = youtube_oembed(&json(include_str!("fixtures/youtube_oembed.json"))).unwrap();
        assert_eq!(p.title, "Rick Astley - Never Gonna Give You Up (Official Video) (4K Remaster)");
        assert_eq!(p.subtitle, "Rick Astley");
        assert_eq!(p.thumb.as_deref(), Some("https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg"));
        assert_eq!(youtube_oembed(&json("{}")), None);
    }

    #[test]
    fn streamable_from_oembed() {
        let p = streamable_oembed(&json(include_str!("fixtures/streamable_oembed.json"))).unwrap();
        assert_eq!(p.title, "\"Please don't eat me!\"");
        assert_eq!(p.subtitle, "Streamable");
        assert!(p.thumb.as_deref().unwrap().starts_with("https://cdn-cf-west.streamable.com/image/"));
        // no title, and a protocol-relative thumbnail
        let p = streamable_oembed(&json(include_str!("fixtures/streamable_oembed_untitled.json"))).unwrap();
        assert_eq!(p.title, "Streamable video");
        assert!(p.thumb.as_deref().unwrap().starts_with("https://cdn-cf-west.streamable.com/image/dnd1.jpg?"));
    }

    #[test]
    fn x_from_oembed() {
        let p = x_oembed(&json(include_str!("fixtures/x_oembed.json"))).unwrap();
        assert_eq!(p.title, "If only Bradley's arm was longer. Best photo ever. #oscars");
        assert_eq!(p.subtitle, "The Ellen Show @TheEllenShow");
        assert_eq!(p.thumb, None);
    }

    #[test]
    fn x_html_to_text() {
        let html = "<blockquote class=\"twitter-tweet\"><p lang=\"en\" dir=\"ltr\">just setting up my twttr</p>&mdash; jack (@jack) <a href=\"https://x.com/jack/status/20?ref_src=twsrc%5Etfw\">March 21, 2006</a></blockquote>\n\n";
        assert_eq!(x_html_text(html), "just setting up my twttr");
        let html = "<blockquote class=\"twitter-tweet\"><p lang=\"en\" dir=\"ltr\">Four more years. <a href=\"http://t.co/bAJE6Vom\">pic.twitter.com/bAJE6Vom</a></p>&mdash; Barack Obama</blockquote>";
        assert_eq!(x_html_text(html), "Four more years.");
        let html = "<blockquote><p dir=\"ltr\">line one<br>line &amp; two &lt;3 &#x1f525; &#8212; <a href=\"https://t.co/x\">t.co/x</a></p></blockquote>";
        assert_eq!(x_html_text(html), "line one line & two <3 \u{1f525} \u{2014} t.co/x");
        assert_eq!(x_html_text("no paragraph"), "");
    }

    #[test]
    fn instagram_from_open_graph() {
        let p = instagram_page(include_str!("fixtures/instagram_post_head.html")).unwrap();
        assert!(p.title.starts_with("Let\u{2019}s set a world record together and get the most liked post on Instagram."), "{}", p.title);
        assert!(p.title.ends_with("#LikeTheEgg #EggSoldiers #EggGang"), "{}", p.title);
        assert_eq!(p.subtitle, "Just An Egg \u{1f95a} · Instagram");
        let thumb = p.thumb.unwrap();
        assert!(thumb.starts_with("https://scontent.cdninstagram.com/") && !thumb.contains("&amp;"), "{thumb}");
        // a post that does not exist (or a login wall): no OpenGraph title, no card
        assert_eq!(instagram_page(include_str!("fixtures/instagram_missing_head.html")), None);
    }

    #[test]
    fn entities_and_attributes() {
        assert_eq!(decode_entities("a &amp; b &quot;c&quot; &#39;d&#39; &#x2022; &unknown; & e"), "a & b \"c\" 'd' \u{2022} &unknown; & e");
        assert_eq!(og_tags("<meta content='x' property='og:title'><meta name=\"description\" content=\"y\"><meta property=\"og:image\" content=\"a?b=1&amp;c=2\" />"), vec![
            ("og:title".to_string(), "x".to_string()),
            ("og:image".to_string(), "a?b=1&c=2".to_string()),
        ]);
    }

    #[test]
    fn counts_and_durations() {
        assert_eq!(compact_count(10), "10");
        assert_eq!(compact_count(1234), "1.2K");
        assert_eq!(compact_count(12_000), "12K");
        assert_eq!(compact_count(1_500_000), "1.5M");
        assert_eq!(duration(58.4), "0:58");
        assert_eq!(duration(60.0), "1:00");
        assert_eq!(duration(3723.0), "1:02:03");
    }
}
