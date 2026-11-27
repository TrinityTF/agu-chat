//! The page a channel's stream plays on (the desktop's "Stream link", the phone's too).
use agu_chat_core::protocol::{Platform, SourceRef};

/// The page a source's stream plays on, for "Stream link"; None for a whisper conversation (no stream) or a channel
/// that does not parse. Built from the same parsers the platforms normalise channels with.
pub fn stream_url(s: &SourceRef) -> Option<String> {
    use agu_chat_core::{kick::vendor::kick::parse_kick_source, rumble::vendor::rumble::parse_rumble_source, youtube::vendor::youtube::parse_youtube_source};
    let ch = s.channel.trim();
    match s.platform {
        Platform::Twitch => {
            if s.platform == Platform::Twitch && agu_chat_core::twitch::whisper::login_of(&s.channel).is_some() {
                return None;
            }
            let login = if ch.to_lowercase().contains("twitch.tv/") { ch.split(['?', '#']).next().unwrap_or("").trim_end_matches('/').rsplit('/').next().unwrap_or("") } else { ch };
            let login = login.trim_start_matches(['#', '@']).to_lowercase();
            let ok = (1..=25).contains(&login.len()) && login.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
            ok.then(|| format!("https://www.twitch.tv/{login}"))
        }
        Platform::Kick => parse_kick_source(ch).map(|slug| format!("https://kick.com/{slug}")),
        Platform::Youtube => parse_youtube_source(ch).map(|y| match y.kind {
            "video" => format!("https://www.youtube.com/watch?v={}", y.id),
            "channel" => format!("https://www.youtube.com/channel/{}/live", y.id),
            _ => format!("https://www.youtube.com/{}/live", y.id), // a handle: "@name"
        }),
        Platform::Rumble => parse_rumble_source(ch).map(|r| match r.space {
            Some(space) => format!("https://rumble.com/{space}/{}", r.id),
            None => format!("https://rumble.com/embed/{}/", r.id), // a video id
        }),
        Platform::X => agu_chat_core::x::normalize_channel(ch).ok().map(|h| match h.strip_prefix("broadcast:") {
            Some(id) => format!("https://x.com/i/broadcasts/{id}"),
            None => format!("https://x.com/{h}"),
        }),
    }
}
