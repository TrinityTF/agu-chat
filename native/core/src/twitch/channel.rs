//! Port of src/main/twitch/channel.js: turns whatever the user typed into a Twitch login.
use crate::util::{js_trim, slice_utf16};

// twitch.tv paths that are not channels.
const NOT_CHANNELS: [&str; 10] = ["videos", "directory", "downloads", "settings", "p", "jobs", "store", "turbo", "subscriptions", "wallet"];

/// `/^[a-z0-9_]{1,25}$/`
fn is_login(s: &str) -> bool {
    (1..=25).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// `/^(?:https?:\/\/)?(?:www\.|m\.|go\.)?twitch\.tv\/(?:popout\/|moderator\/|embed\/)?([^/?#\s]+)/i`
fn login_from_url(v: &str) -> Option<String> {
    let lower = v.to_lowercase();
    let mut rest = lower.as_str();
    let mut off = 0usize;
    for p in ["https://", "http://"] {
        if let Some(r) = rest.strip_prefix(p) {
            off += p.len();
            rest = r;
            break;
        }
    }
    for p in ["www.", "m.", "go."] {
        if let Some(r) = rest.strip_prefix(p) {
            off += p.len();
            rest = r;
            break;
        }
    }
    let r = rest.strip_prefix("twitch.tv/")?;
    off += "twitch.tv/".len();
    let mut rest = r;
    for p in ["popout/", "moderator/", "embed/"] {
        if let Some(r) = rest.strip_prefix(p) {
            off += p.len();
            rest = r;
            break;
        }
    }
    let _ = off;
    let end = rest.find(|c: char| c == '/' || c == '?' || c == '#' || c.is_whitespace()).unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    Some(rest[..end].to_string())
}

/// "login", "#login", "@login", "twitch.tv/login", "https://www.twitch.tv/popout/login/chat" -> "login".
/// Errs with the message for the user.
pub fn normalize_channel(input: &str) -> Result<String, String> {
    let mut v = js_trim(input).to_string();
    if v.is_empty() {
        return Err("Enter a Twitch channel name.".into());
    }
    if let Some(l) = login_from_url(&v) {
        v = l;
    }
    let v = v.trim_start_matches(['#', '@']).to_lowercase();
    if !is_login(&v) || NOT_CHANNELS.contains(&v.as_str()) {
        return Err(format!("\"{}\" is not a Twitch channel name (letters, digits and underscores, up to 25).", slice_utf16(js_trim(input), 40)));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes() {
        assert_eq!(normalize_channel("A_G_U__").unwrap(), "a_g_u__");
        assert_eq!(normalize_channel("#a_g_u__").unwrap(), "a_g_u__");
        assert_eq!(normalize_channel("@Foo").unwrap(), "foo");
        assert_eq!(normalize_channel("https://www.twitch.tv/popout/Foo/chat").unwrap(), "foo");
        assert_eq!(normalize_channel("twitch.tv/foo?x=1").unwrap(), "foo");
        assert!(normalize_channel("").is_err());
        assert!(normalize_channel("twitch.tv/videos").is_err());
        assert!(normalize_channel("has space").is_err());
        assert!(normalize_channel(&"a".repeat(26)).is_err());
    }
}
