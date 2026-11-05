//! Port of src/main/twitch/http.js: one small JSON-over-HTTP helper for every Twitch HTTP call (id.twitch.tv,
//! Helix, recent-messages). Always sends the app's honest User-Agent.
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;

use super::rt::user_agent;
use crate::util::{js_string, slice_utf16, truthy};

#[derive(Debug, Clone)]
pub struct HttpError {
    /// 0 = never reached the server
    pub status: u16,
    pub message: String,
    pub body: Option<Value>,
    /// ours, not Twitch's: routes answer it as-is instead of "Twitch error: ..."
    pub local: bool,
}

impl HttpError {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        HttpError { status, message: message.into(), body: None, local: false }
    }
    /// An error of ours (`e.local = true`).
    pub fn local(status: u16, message: impl Into<String>) -> Self {
        HttpError { local: true, ..HttpError::new(status, message) }
    }
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HttpError {}

#[derive(Default)]
pub struct ReqOpts {
    /// GET when empty
    pub method: &'static str,
    pub headers: Vec<(String, String)>,
    pub json: Option<Value>,
    pub form: Option<Vec<(&'static str, String)>>,
    /// 15 s when 0
    pub timeout_ms: u64,
}

pub fn client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| reqwest::Client::builder().build().expect("http client"))
}

fn root_cause(e: &reqwest::Error) -> String {
    use std::error::Error;
    if e.is_timeout() {
        return "TimeoutError".into();
    }
    let mut cur: &dyn Error = e;
    while let Some(next) = cur.source() {
        cur = next;
    }
    cur.to_string()
}

pub async fn request_json(url: &str, opts: ReqOpts) -> Result<Value, HttpError> {
    let method = reqwest::Method::from_bytes(if opts.method.is_empty() { b"GET" } else { opts.method.as_bytes() }).unwrap_or(reqwest::Method::GET);
    let mut req = client().request(method, url).header("User-Agent", user_agent()).header("Accept", "application/json");
    for (k, v) in &opts.headers {
        req = req.header(k.as_str(), v.as_str());
    }
    if let Some(form) = &opts.form {
        req = req.header("Content-Type", "application/x-www-form-urlencoded").body(url::form_urlencoded::Serializer::new(String::new()).extend_pairs(form.iter().map(|(k, v)| (*k, v.as_str()))).finish());
    } else if let Some(j) = &opts.json {
        req = req.header("Content-Type", "application/json").body(j.to_string());
    }
    req = req.timeout(Duration::from_millis(if opts.timeout_ms == 0 { 15_000 } else { opts.timeout_ms }));
    let res = req.send().await.map_err(|e| HttpError::new(0, format!("Could not reach Twitch ({})", root_cause(&e))))?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    let data = if text.is_empty() { None } else { serde_json::from_str::<Value>(&text).ok() };
    if !status.is_success() {
        let from_body = data.as_ref().and_then(|d| {
            let m = d.get("message").filter(|v| truthy(v)).or_else(|| d.get("error").filter(|v| truthy(v)))?;
            Some(js_string(m))
        });
        let message = from_body.filter(|m| !m.is_empty()).or_else(|| Some(slice_utf16(&text, 200)).filter(|t| !t.is_empty())).unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
        return Err(HttpError { status: status.as_u16(), message, body: data, local: false });
    }
    Ok(if status.as_u16() == 204 { Value::Null } else { data.unwrap_or(Value::Null) })
}
