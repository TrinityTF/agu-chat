//! Link previews: the small card under a chat message that links to a Twitch clip, a YouTube video, a post on X,
//! a Streamable video or an Instagram post.
//!
//! Only these providers are ever asked, and only at their own official, keyless endpoints (Helix for Twitch clips,
//! with the logged-in user's token), so a link a chatter posts never makes the app contact an arbitrary site.
//!
//! - `match_url` turns a link into a `LinkTarget` (provider + id + the canonical URL), or None;
//! - `parse` reads each provider's answer into a `Preview`;
//! - `cache::PreviewCache` is the shared, bounded cache (LRU, de-duped, failures remembered, at most 4 in flight);
//! - `fetch::fetch` does one request.
pub mod cache;
pub mod fetch;
pub mod parse;

use url::Url;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Provider {
    TwitchClip,
    YouTube,
    X,
    Streamable,
    Instagram,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Provider::TwitchClip => "Twitch",
            Provider::YouTube => "YouTube",
            Provider::X => "X",
            Provider::Streamable => "Streamable",
            Provider::Instagram => "Instagram",
        }
    }
}

/// A link a preview can be made for.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LinkTarget {
    pub provider: Provider,
    /// the clip slug / video id / post id / shortcode
    pub id: String,
    /// The cache key: the same video linked in different ways shares one entry ("youtube:dQw4w9WgXcQ").
    pub key: String,
    /// The canonical URL the provider is asked about.
    pub url: String,
}

/// What the card shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
    pub provider: Provider,
    pub title: String,
    pub subtitle: String,
    /// thumbnail image URL (https), None for a text-only card (X)
    pub thumb: Option<String>,
}

fn target(provider: Provider, id: &str, url: String) -> Option<LinkTarget> {
    let prefix = match provider {
        Provider::TwitchClip => "twitch-clip",
        Provider::YouTube => "youtube",
        Provider::X => "x",
        Provider::Streamable => "streamable",
        Provider::Instagram => "instagram",
    };
    Some(LinkTarget { provider, id: id.to_string(), key: format!("{prefix}:{id}"), url })
}

fn all(s: &str, ok: impl Fn(char) -> bool) -> bool {
    !s.is_empty() && s.chars().all(ok)
}

fn id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn clip_slug(s: &str) -> bool {
    (3..=100).contains(&s.len()) && all(s, id_char)
}

fn youtube_id(s: &str) -> bool {
    s.len() == 11 && all(s, id_char)
}

/// Pages on streamable.com that are not videos.
const STREAMABLE_RESERVED: &[&str] = &[
    "login", "signup", "upload", "pricing", "blog", "about", "terms", "privacy", "settings", "documentation", "support", "careers", "clipper", "videos",
    "help", "enterprise", "download", "plans", "contact", "embed", "o", "e", "s",
];

/// The preview target of a link, or None for anything that is not a link to one of the providers' items.
pub fn match_url(href: &str) -> Option<LinkTarget> {
    let href = href.trim();
    let lower = href.to_ascii_lowercase();
    let full = if lower.starts_with("http://") || lower.starts_with("https://") { href.to_string() } else { format!("https://{href}") };
    let u = Url::parse(&full).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    let mut host = u.host_str()?.to_ascii_lowercase();
    for p in ["www.", "m.", "mobile."] {
        if let Some(h) = host.strip_prefix(p) {
            host = h.to_string();
            break;
        }
    }
    let segs: Vec<&str> = u.path_segments().map(|s| s.filter(|x| !x.is_empty()).collect()).unwrap_or_default();
    let query = |k: &str| u.query_pairs().find(|(q, _)| q == k).map(|(_, v)| v.into_owned());
    match host.as_str() {
        "clips.twitch.tv" => {
            let slug = match segs.as_slice() {
                ["embed", ..] => query("clip")?,
                [slug, ..] => slug.to_string(),
                _ => return None,
            };
            clip_slug(&slug).then_some(())?;
            target(Provider::TwitchClip, &slug, format!("https://clips.twitch.tv/{slug}"))
        }
        "twitch.tv" => match segs.as_slice() {
            [_channel, "clip", slug, ..] if clip_slug(slug) => target(Provider::TwitchClip, slug, format!("https://clips.twitch.tv/{slug}")),
            _ => None,
        },
        "youtube.com" => {
            let id = match segs.as_slice() {
                ["watch"] => query("v")?,
                ["shorts" | "live", id, ..] => id.to_string(),
                _ => return None,
            };
            youtube_id(&id).then_some(())?;
            target(Provider::YouTube, &id, format!("https://www.youtube.com/watch?v={id}"))
        }
        "youtu.be" => match segs.as_slice() {
            [id] if youtube_id(id) => target(Provider::YouTube, id, format!("https://www.youtube.com/watch?v={id}")),
            _ => None,
        },
        "x.com" | "twitter.com" => {
            let (user, id) = match segs.as_slice() {
                ["i", "web", "status", id, ..] => ("i", *id),
                [user, "status" | "statuses", id, ..] => (*user, *id),
                _ => return None,
            };
            if !all(id, |c| c.is_ascii_digit()) || id.len() > 25 || !all(user, |c| c.is_ascii_alphanumeric() || c == '_') || user.len() > 15 {
                return None;
            }
            target(Provider::X, id, format!("https://twitter.com/{user}/status/{id}"))
        }
        "streamable.com" => match segs.as_slice() {
            [id] if (3..=16).contains(&id.len()) && all(id, |c| c.is_ascii_alphanumeric()) && !STREAMABLE_RESERVED.contains(&id.to_ascii_lowercase().as_str()) => {
                target(Provider::Streamable, id, format!("https://streamable.com/{id}"))
            }
            _ => None,
        },
        "instagram.com" => {
            let (kind, code) = match segs.as_slice() {
                [kind @ ("p" | "reel" | "reels" | "tv"), code, ..] => (*kind, *code),
                [_user, kind @ ("p" | "reel"), code, ..] => (*kind, *code),
                _ => return None,
            };
            if !(5..=64).contains(&code.len()) || !all(code, id_char) {
                return None;
            }
            let kind = if kind == "reels" { "reel" } else { kind };
            target(Provider::Instagram, code, format!("https://www.instagram.com/{kind}/{code}/"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(u: &str) -> Option<String> {
        match_url(u).map(|t| t.key)
    }

    #[test]
    fn twitch_clip_links() {
        for u in [
            "https://clips.twitch.tv/AwkwardHelplessSalamanderSwiftRage",
            "clips.twitch.tv/AwkwardHelplessSalamanderSwiftRage",
            "https://clips.twitch.tv/AwkwardHelplessSalamanderSwiftRage?tt_medium=x",
            "https://www.twitch.tv/chewiemelodies/clip/AwkwardHelplessSalamanderSwiftRage",
            "https://twitch.tv/chewiemelodies/clip/AwkwardHelplessSalamanderSwiftRage?filter=clips",
            "https://m.twitch.tv/chewiemelodies/clip/AwkwardHelplessSalamanderSwiftRage",
            "https://clips.twitch.tv/embed?clip=AwkwardHelplessSalamanderSwiftRage&parent=x",
        ] {
            assert_eq!(key(u).as_deref(), Some("twitch-clip:AwkwardHelplessSalamanderSwiftRage"), "{u}");
        }
        let t = match_url("https://m.twitch.tv/a/clip/Some-Slug_1").unwrap();
        assert_eq!(t.url, "https://clips.twitch.tv/Some-Slug_1");
        assert_eq!(t.provider, Provider::TwitchClip);
        for u in ["https://twitch.tv/esfandtv", "https://www.twitch.tv/esfandtv/videos", "https://twitch.tv/esfandtv/clip/", "https://clips.twitch.tv/", "https://twitch.tv/videos/123", "https://clips.twitch.tv/a%20b"] {
            assert_eq!(key(u), None, "{u}");
        }
    }

    #[test]
    fn youtube_links() {
        for u in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtube.com/watch?v=dQw4w9WgXcQ&t=42s",
            "https://m.youtube.com/watch?v=dQw4w9WgXcQ",
            "youtube.com/watch?feature=share&v=dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ?si=abc",
            "https://www.youtube.com/shorts/dQw4w9WgXcQ",
            "https://youtube.com/live/dQw4w9WgXcQ?feature=share",
        ] {
            let t = match_url(u).unwrap_or_else(|| panic!("{u}"));
            assert_eq!(t.key, "youtube:dQw4w9WgXcQ", "{u}");
            assert_eq!(t.url, "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        }
        for u in [
            "https://www.youtube.com/@channel",
            "https://www.youtube.com/channel/UC1234567890",
            "https://www.youtube.com/watch?v=short",
            "https://www.youtube.com/playlist?list=PL123",
            "https://youtu.be/",
            "https://music.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com/",
        ] {
            assert_eq!(key(u), None, "{u}");
        }
    }

    #[test]
    fn x_links() {
        for u in [
            "https://x.com/jack/status/20",
            "https://twitter.com/jack/status/20",
            "https://www.twitter.com/jack/status/20?s=20",
            "https://mobile.twitter.com/jack/status/20",
            "https://x.com/jack/status/20/photo/1",
            "x.com/jack/status/20",
        ] {
            let t = match_url(u).unwrap_or_else(|| panic!("{u}"));
            assert_eq!(t.key, "x:20", "{u}");
            assert_eq!(t.url, "https://twitter.com/jack/status/20");
        }
        for u in ["https://x.com/jack", "https://x.com/jack/status/abc", "https://x.com/home", "https://twitter.com/i/lists/123", "https://x.com/jack/status/"] {
            assert_eq!(key(u), None, "{u}");
        }
    }

    #[test]
    fn streamable_links() {
        let t = match_url("https://streamable.com/moo").unwrap();
        assert_eq!((t.key.as_str(), t.url.as_str()), ("streamable:moo", "https://streamable.com/moo"));
        assert_eq!(key("streamable.com/e4uf5?src=player"), Some("streamable:e4uf5".into()));
        for u in ["https://streamable.com/", "https://streamable.com/login", "https://streamable.com/e/moo", "https://streamable.com/a", "https://streamable.com/blog/post"] {
            assert_eq!(key(u), None, "{u}");
        }
    }

    #[test]
    fn instagram_links() {
        for (u, url) in [
            ("https://www.instagram.com/p/BsOGulcndj-/", "https://www.instagram.com/p/BsOGulcndj-/"),
            ("https://instagram.com/p/BsOGulcndj-", "https://www.instagram.com/p/BsOGulcndj-/"),
            ("https://www.instagram.com/p/BsOGulcndj-/?igsh=abc", "https://www.instagram.com/p/BsOGulcndj-/"),
            ("https://www.instagram.com/reel/BsOGulcndj-/", "https://www.instagram.com/reel/BsOGulcndj-/"),
            ("https://www.instagram.com/reels/BsOGulcndj-/", "https://www.instagram.com/reel/BsOGulcndj-/"),
            ("https://www.instagram.com/tv/BsOGulcndj-/", "https://www.instagram.com/tv/BsOGulcndj-/"),
            ("https://www.instagram.com/world_record_egg/p/BsOGulcndj-/", "https://www.instagram.com/p/BsOGulcndj-/"),
        ] {
            let t = match_url(u).unwrap_or_else(|| panic!("{u}"));
            assert_eq!(t.key, "instagram:BsOGulcndj-", "{u}");
            assert_eq!(t.url, url, "{u}");
        }
        for u in ["https://www.instagram.com/instagram/", "https://www.instagram.com/", "https://www.instagram.com/explore/tags/cats/", "https://www.instagram.com/stories/someone/123/"] {
            assert_eq!(key(u), None, "{u}");
        }
    }

    #[test]
    fn other_sites_never_match() {
        for u in ["https://example.com/watch?v=dQw4w9WgXcQ", "https://evil.com/clips.twitch.tv/abc", "https://youtube.com.evil.com/watch?v=dQw4w9WgXcQ", "ftp://youtu.be/dQw4w9WgXcQ", "https://kick.com/abc/clips/clip_1", "not a url"] {
            assert_eq!(key(u), None, "{u}");
        }
    }
}
