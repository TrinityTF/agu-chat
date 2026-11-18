//! One image download, streamed: it fails when the server sends nothing for a while (a stall), not after a fixed
//! total time, so a slow CDN that keeps trickling bytes still gets the file through. A generous overall cap stays
//! as the last resort. What arrived before a failure is kept, so the retry can ask for just the rest (`Range`) when
//! the server supports it and the file provably did not change (`If-Range` with its ETag / Last-Modified).
use std::time::Duration;

use reqwest::header::{HeaderMap, ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, ETAG, IF_RANGE, LAST_MODIFIED, RANGE};
use reqwest::StatusCode;

use super::{classify_status, Fail};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// No byte (or no response) for this long fails the attempt.
    pub stall: Duration,
    /// One attempt never takes longer than this, however steady the trickle (also overrides the client's own total
    /// timeout for this request).
    pub overall: Duration,
    /// Bigger bodies are refused.
    pub max_bytes: usize,
}

pub const LIMITS: Limits = Limits { stall: Duration::from_secs(15), overall: Duration::from_secs(120), max_bytes: 40 * 1024 * 1024 };

/// What came in of a body so far, carried from a failed attempt to the next.
#[derive(Default, Debug)]
pub struct Partial {
    pub bytes: Vec<u8>,
    /// The `If-Range` value (a strong ETag or the Last-Modified date) of the response the bytes came from; None = the
    /// server gave nothing to prove the file is the same, so a retry starts over.
    validator: Option<String>,
    /// The server said `Accept-Ranges: bytes`.
    ranges: bool,
}

impl Partial {
    /// Whether the next attempt asks for the rest only.
    pub fn resumable(&self) -> bool {
        !self.bytes.is_empty() && self.ranges && self.validator.is_some()
    }
}

/// The first byte offset of a `Content-Range: bytes <start>-<end>/<total>` header.
pub fn content_range_start(v: &str) -> Option<u64> {
    let r = v.trim().strip_prefix("bytes")?.trim_start();
    r.split('-').next()?.trim().parse().ok()
}

/// The `If-Range` value for a response: a strong ETag, else Last-Modified (weak ETags are not allowed there).
fn validator(h: &HeaderMap) -> Option<String> {
    if let Some(e) = h.get(ETAG).and_then(|v| v.to_str().ok()) {
        if !e.starts_with("W/") {
            return Some(e.to_string());
        }
    }
    h.get(LAST_MODIFIED).and_then(|v| v.to_str().ok()).map(|s| s.to_string())
}

fn net_err(e: reqwest::Error) -> (String, Fail) {
    let kind = e.status().map(|s| classify_status(s.as_u16())).unwrap_or(Fail::Transient);
    (e.to_string(), kind)
}

/// GETs `url` into memory under `limits`, resuming `partial` when it can. On failure `partial` holds what came in.
pub async fn fetch_bytes(client: &reqwest::Client, url: &str, limits: Limits, partial: &mut Partial) -> Result<Vec<u8>, (String, Fail)> {
    let mut req = client.get(url).timeout(limits.overall);
    let resuming = partial.resumable();
    if resuming {
        req = req.header(RANGE, format!("bytes={}-", partial.bytes.len())).header(IF_RANGE, partial.validator.clone().unwrap_or_default());
    }
    let stalled = || (format!("stalled: no data for {} s", limits.stall.as_secs_f32()), Fail::Transient);
    let resp = match tokio::time::timeout(limits.stall, req.send()).await {
        Err(_) => return Err(stalled()),
        Ok(r) => r.map_err(net_err)?,
    };
    if resuming && resp.status() == StatusCode::RANGE_NOT_SATISFIABLE {
        *partial = Partial::default();
        return Err(("range not satisfiable".into(), Fail::Transient));
    }
    let resp = resp.error_for_status().map_err(net_err)?;
    let headers = resp.headers();
    if resp.status() == StatusCode::PARTIAL_CONTENT && resuming {
        let start = headers.get(CONTENT_RANGE).and_then(|v| v.to_str().ok()).and_then(content_range_start);
        if start != Some(partial.bytes.len() as u64) {
            *partial = Partial::default();
            return Err(("unexpected Content-Range".into(), Fail::Transient));
        }
        crate::logf!("image resume at {} bytes {url}", partial.bytes.len());
    } else {
        // a full body (the server ignored the range, or the file changed): start over
        partial.bytes.clear();
        partial.validator = validator(headers);
        partial.ranges = headers.get(ACCEPT_RANGES).and_then(|v| v.to_str().ok()).map(|v| v.eq_ignore_ascii_case("bytes")).unwrap_or(false);
    }
    let len = headers.get(CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<usize>().ok());
    if len.map(|l| partial.bytes.len() + l > limits.max_bytes).unwrap_or(false) {
        *partial = Partial::default();
        return Err(("image too large".into(), Fail::Permanent));
    }
    if let Some(l) = len {
        partial.bytes.reserve(l);
    }
    let mut resp = resp;
    loop {
        match tokio::time::timeout(limits.stall, resp.chunk()).await {
            Err(_) => return Err(stalled()),
            Ok(Err(e)) => return Err(net_err(e)),
            Ok(Ok(None)) => break,
            Ok(Ok(Some(c))) => {
                if partial.bytes.len() + c.len() > limits.max_bytes {
                    *partial = Partial::default();
                    return Err(("image too large".into(), Fail::Permanent));
                }
                partial.bytes.extend_from_slice(&c);
            }
        }
    }
    Ok(std::mem::take(partial).bytes)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    /// How the test server answers one request: the body bytes it has, and how it sends them.
    #[derive(Clone, Copy)]
    pub(crate) enum Mode {
        /// The whole body in `pieces` parts with `gap_ms` between them.
        Trickle { pieces: usize, gap_ms: u64 },
        /// Headers, then the first half, then silence (the connection stays open).
        StallHalf,
        /// Accepts and never answers.
        Silent,
        /// The first request gets half the body then the connection closes; later ones honour `Range`.
        DropThenRange,
    }

    pub(crate) fn body() -> Vec<u8> {
        (0..20_000u32).map(|i| (i % 251) as u8).collect()
    }

    async fn read_head(s: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut b = [0u8; 1024];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = s.read(&mut b).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&b[..n]);
        }
        String::from_utf8_lossy(&buf).to_string()
    }

    /// A tiny HTTP/1.1 server on localhost; returns its base URL and the number of requests seen.
    pub(crate) async fn serve(mode: Mode) -> (String, Arc<AtomicUsize>) {
        serve_with(mode, Arc::new(body())).await
    }

    /// `serve` with a body of the caller's.
    pub(crate) async fn serve_with(mode: Mode, payload: Arc<Vec<u8>>) -> (String, Arc<AtomicUsize>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else { return };
                let n = h.fetch_add(1, Ordering::SeqCst);
                let payload = payload.clone();
                tokio::spawn(async move {
                    let head = read_head(&mut s).await.to_ascii_lowercase();
                    let body = payload.to_vec();
                    let range = head.lines().find_map(|l| l.strip_prefix("range: bytes=")).and_then(|r| r.trim_end_matches('-').trim().parse::<usize>().ok());
                    let base = "HTTP/1.1 200 OK\r\nContent-Type: image/webp\r\nAccept-Ranges: bytes\r\nETag: \"v1\"\r\nConnection: close\r\n";
                    match mode {
                        Mode::Silent => {
                            tokio::time::sleep(Duration::from_secs(30)).await;
                        }
                        Mode::Trickle { pieces, gap_ms } => {
                            let _ = s.write_all(format!("{base}Content-Length: {}\r\n\r\n", body.len()).as_bytes()).await;
                            for c in body.chunks(body.len().div_ceil(pieces)) {
                                tokio::time::sleep(Duration::from_millis(gap_ms)).await;
                                let _ = s.write_all(c).await;
                                let _ = s.flush().await;
                            }
                        }
                        Mode::StallHalf => {
                            let _ = s.write_all(format!("{base}Content-Length: {}\r\n\r\n", body.len()).as_bytes()).await;
                            let _ = s.write_all(&body[..body.len() / 2]).await;
                            let _ = s.flush().await;
                            tokio::time::sleep(Duration::from_secs(30)).await;
                        }
                        Mode::DropThenRange => match range {
                            Some(from) if n > 0 && head.contains("if-range: \"v1\"") => {
                                let rest = &body[from..];
                                let _ = s
                                    .write_all(format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {from}-{}/{}\r\nContent-Length: {}\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n", body.len() - 1, body.len(), rest.len()).as_bytes())
                                    .await;
                                let _ = s.write_all(rest).await;
                            }
                            _ => {
                                let _ = s.write_all(format!("{base}Content-Length: {}\r\n\r\n", body.len()).as_bytes()).await;
                                let _ = s.write_all(&body[..body.len() / 2]).await;
                                let _ = s.flush().await;
                                // closes early: the client sees a short body
                            }
                        },
                    }
                });
            }
        });
        (format!("http://{addr}/emote.webp"), hits)
    }

    fn limits(stall_ms: u64) -> Limits {
        Limits { stall: Duration::from_millis(stall_ms), overall: Duration::from_secs(10), max_bytes: 1 << 20 }
    }

    #[test]
    fn content_range_parsing() {
        assert_eq!(content_range_start("bytes 100-199/200"), Some(100));
        assert_eq!(content_range_start("bytes 0-0/1"), Some(0));
        assert_eq!(content_range_start("items 1-2/3"), None);
    }

    #[tokio::test]
    async fn a_slow_but_steady_download_finishes_past_the_clients_own_timeout() {
        let (url, _) = serve(Mode::Trickle { pieces: 10, gap_ms: 120 }).await;
        // the shared client gives up after 400 ms in total; the body takes about 1.2 s but never stops for 400 ms
        let client = reqwest::Client::builder().timeout(Duration::from_millis(400)).build().unwrap();
        let mut p = Partial::default();
        let got = fetch_bytes(&client, &url, limits(800), &mut p).await.expect("a trickle is not a stall");
        assert_eq!(got, body());
    }

    #[tokio::test]
    async fn a_stalled_body_fails_after_the_stall_time_and_keeps_what_came() {
        let (url, _) = serve(Mode::StallHalf).await;
        let mut p = Partial::default();
        let t0 = std::time::Instant::now();
        let e = fetch_bytes(&reqwest::Client::new(), &url, limits(300), &mut p).await.unwrap_err();
        assert_eq!(e.1, Fail::Transient);
        assert!(e.0.contains("stalled"), "{}", e.0);
        assert!(t0.elapsed() < Duration::from_secs(3));
        assert_eq!(p.bytes.len(), body().len() / 2);
        assert!(p.resumable());
    }

    #[tokio::test]
    async fn a_server_that_never_answers_is_a_stall() {
        let (url, _) = serve(Mode::Silent).await;
        let mut p = Partial::default();
        let e = fetch_bytes(&reqwest::Client::new(), &url, limits(300), &mut p).await.unwrap_err();
        assert_eq!(e.1, Fail::Transient);
        assert!(p.bytes.is_empty() && !p.resumable());
    }

    #[tokio::test]
    async fn a_retry_after_a_cut_download_asks_for_the_rest_only() {
        let (url, hits) = serve(Mode::DropThenRange).await;
        let client = reqwest::Client::new();
        let mut p = Partial::default();
        let e = fetch_bytes(&client, &url, limits(2_000), &mut p).await.unwrap_err();
        assert_eq!(e.1, Fail::Transient);
        assert_eq!(p.bytes.len(), body().len() / 2);
        let got = fetch_bytes(&client, &url, limits(2_000), &mut p).await.expect("the rest via Range");
        assert_eq!(got, body());
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert!(p.bytes.is_empty());
    }

    #[tokio::test]
    async fn a_body_over_the_limit_is_refused_for_good() {
        let (url, _) = serve(Mode::Trickle { pieces: 1, gap_ms: 0 }).await;
        let mut p = Partial::default();
        let l = Limits { max_bytes: 1000, ..limits(1_000) };
        assert_eq!(fetch_bytes(&reqwest::Client::new(), &url, l, &mut p).await.unwrap_err().1, Fail::Permanent);
    }
}
