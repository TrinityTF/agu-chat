//! One preview request, straight to the provider (never through api.agu.lol): Helix for Twitch clips (with the
//! logged-in user's token; no login, no card), the official keyless oEmbed endpoints of YouTube, X and Streamable,
//! and the post page's OpenGraph tags for Instagram (a plain GET with the app's own user agent: no login, no
//! cookies). `client` carries Agu Chat's honest user agent.
use std::time::Duration;

use serde_json::Value;

use super::{parse, LinkTarget, Preview, Provider};
use crate::images::{classify_status, Fail};

/// Each request (headers and body) gives up after this.
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// An Instagram page is only read up to its `</head>` (the OpenGraph tags), and never past this many bytes.
const PAGE_MAX: usize = 1_500_000;

pub async fn fetch(t: &LinkTarget, client: &reqwest::Client) -> Result<Preview, Fail> {
    match tokio::time::timeout(TIMEOUT, fetch_inner(t, client)).await {
        Ok(r) => r,
        Err(_) => Err(Fail::Transient),
    }
}

async fn fetch_inner(t: &LinkTarget, client: &reqwest::Client) -> Result<Preview, Fail> {
    let enc = |s: &str| url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
    match t.provider {
        Provider::TwitchClip => {
            let v = crate::twitch::helix::helix_get("/clips", crate::q! {"id" => t.id.as_str()}).await.map_err(|e| match e.status {
                // 401 = not logged in (ours) or a token Twitch would not refresh: try again later, a login may come
                0 | 401 => Fail::Transient,
                s => classify_status(s),
            })?;
            parse::twitch_clips(&v).ok_or(Fail::Permanent)
        }
        Provider::YouTube => parse::youtube_oembed(&get_json(client, &format!("https://www.youtube.com/oembed?url={}&format=json", enc(&t.url))).await?).ok_or(Fail::Permanent),
        // publish.twitter.com answers with a redirect to publish.x.com: ask the latter directly
        Provider::X => parse::x_oembed(&get_json(client, &format!("https://publish.x.com/oembed?url={}&omit_script=true&dnt=true", enc(&t.url))).await?).ok_or(Fail::Permanent),
        Provider::Streamable => parse::streamable_oembed(&get_json(client, &format!("https://api.streamable.com/oembed.json?url={}", enc(&t.url))).await?).ok_or(Fail::Permanent),
        Provider::Instagram => parse::instagram_page(&get_head(client, &t.url).await?).ok_or(Fail::Permanent),
    }
}

async fn send(client: &reqwest::Client, url: &str) -> Result<reqwest::Response, Fail> {
    let res = client.get(url).send().await.map_err(|_| Fail::Transient)?;
    let status = res.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(classify_status(status));
    }
    Ok(res)
}

async fn get_json(client: &reqwest::Client, url: &str) -> Result<Value, Fail> {
    let body = send(client, url).await?.text().await.map_err(|_| Fail::Transient)?;
    serde_json::from_str(&body).map_err(|_| Fail::Permanent)
}

/// The page up to and including `</head>`.
async fn get_head(client: &reqwest::Client, url: &str) -> Result<String, Fail> {
    let mut res = send(client, url).await?;
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(|_| Fail::Transient)? {
        let from = buf.len().saturating_sub(8);
        buf.extend_from_slice(&chunk);
        if let Some(i) = find(&buf[from..], b"</head>") {
            buf.truncate(from + i + 7);
            break;
        }
        if buf.len() >= PAGE_MAX {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link_preview::match_url;

    /// Live, read-only requests to the real endpoints (`cargo test -- --ignored live_providers`).
    #[tokio::test]
    #[ignore]
    async fn live_providers() {
        let client = reqwest::Client::builder().user_agent(crate::backend::user_agent("test")).build().unwrap();
        for u in ["https://youtu.be/dQw4w9WgXcQ", "https://x.com/jack/status/20", "https://streamable.com/moo", "https://www.instagram.com/p/BsOGulcndj-/"] {
            let t = match_url(u).unwrap();
            let p = fetch(&t, &client).await;
            println!("{u} -> {p:?}");
            assert!(p.is_ok(), "{u}");
        }
        // no Twitch login in a test: no card, asked again later
        let t = match_url("https://clips.twitch.tv/AwkwardHelplessSalamanderSwiftRage").unwrap();
        assert_eq!(fetch(&t, &client).await, Err(Fail::Transient));
        // a post that does not exist: nothing to show
        let t = match_url("https://www.instagram.com/p/C3pJqH0s5cZ/").unwrap();
        assert_eq!(fetch(&t, &client).await, Err(Fail::Permanent));
    }
}
