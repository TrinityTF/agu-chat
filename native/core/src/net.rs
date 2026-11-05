//! Rust-only helpers standing in for what Node gives the Electron main process for free in the Kick, YouTube,
//! Rumble and X modules: the global `fetch` (with the `AbortController` + `setTimeout` deadline each of them
//! wrapped around it), `AbortSignal` teardown (here: `Jobs`, tasks that `stop()` aborts) and a few
//! optional-chaining helpers for `serde_json::Value`. There is no JS counterpart file.
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde_json::Value;
use tokio::task::AbortHandle;

use crate::util::{js_string, js_to_number, truthy};

#[derive(Debug, Clone)]
pub struct FetchError {
    pub message: String,
    /// the deadline hit (`AbortError` / `TimeoutError` in undici)
    pub timed_out: bool,
    /// the connection was refused / reset / never made (`ECONNRESET`, `ECONNREFUSED`, `ENOTFOUND`, ...)
    pub refused: bool,
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FetchError {}

#[derive(Default, Clone)]
pub struct FetchOpts {
    /// GET when empty
    pub method: &'static str,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    /// deadline for the response HEADERS (like the JS `setTimeout(() => ctl.abort())` cleared once `fetch`
    /// resolves); 0 = none (an SSE stream)
    pub timeout_ms: u64,
    /// `redirect: "manual"`: a 3xx is returned as is
    pub manual_redirect: bool,
}

impl FetchOpts {
    pub fn new(timeout_ms: u64) -> Self {
        FetchOpts { timeout_ms, ..Default::default() }
    }
    pub fn header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.to_string(), v.to_string()));
        self
    }
    pub fn method(mut self, m: &'static str) -> Self {
        self.method = m;
        self
    }
    pub fn body(mut self, b: String) -> Self {
        self.body = Some(b);
        self
    }
}

pub fn client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| reqwest::Client::builder().build().expect("http client"))
}

fn client_manual() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().expect("http client"))
}

fn classify(e: &reqwest::Error) -> FetchError {
    use std::error::Error;
    if e.is_timeout() {
        return FetchError { message: "This operation was aborted".into(), timed_out: true, refused: false };
    }
    let mut refused = e.is_connect();
    let mut cur: &dyn Error = e;
    loop {
        if let Some(io) = cur.downcast_ref::<std::io::Error>() {
            use std::io::ErrorKind::*;
            if matches!(io.kind(), ConnectionReset | ConnectionRefused | ConnectionAborted | NotFound | TimedOut | BrokenPipe | UnexpectedEof) {
                refused = true;
            }
        }
        match cur.source() {
            Some(next) => cur = next,
            None => break,
        }
    }
    FetchError { message: "fetch failed".into(), timed_out: false, refused }
}

/// `fetch(url, opts)`. The caller's User-Agent is always passed in `opts.headers` (never a browser one).
pub async fn fetch(url: &str, opts: FetchOpts) -> Result<reqwest::Response, FetchError> {
    let method = reqwest::Method::from_bytes(if opts.method.is_empty() { b"GET" } else { opts.method.as_bytes() }).unwrap_or(reqwest::Method::GET);
    let cl = if opts.manual_redirect { client_manual() } else { client() };
    let mut req = cl.request(method, url);
    for (k, v) in &opts.headers {
        req = req.header(k.as_str(), v.as_str());
    }
    if let Some(b) = opts.body {
        req = req.body(b);
    }
    let send = req.send();
    let res = if opts.timeout_ms > 0 {
        match tokio::time::timeout(Duration::from_millis(opts.timeout_ms), send).await {
            Ok(r) => r,
            Err(_) => return Err(FetchError { message: "This operation was aborted".into(), timed_out: true, refused: false }),
        }
    } else {
        send.await
    };
    res.map_err(|e| classify(&e))
}

/// `await r.text()`. Unlike JS (whose deadline stops once the headers are in) a body read gets its own 60 s
/// bound, so a server that stalls mid-page cannot hang a source forever.
pub async fn text(res: reqwest::Response) -> Result<String, FetchError> {
    match tokio::time::timeout(Duration::from_secs(60), res.text()).await {
        Ok(Ok(t)) => Ok(t),
        Ok(Err(e)) => Err(classify(&e)),
        Err(_) => Err(FetchError { message: "This operation was aborted".into(), timed_out: true, refused: false }),
    }
}

/// `await r.json()` (null on a body that is not JSON would be a throw in JS; here it is an error string).
pub async fn json(res: reqwest::Response) -> Result<Value, FetchError> {
    let t = text(res).await?;
    serde_json::from_str::<Value>(&t).map_err(|e| FetchError { message: format!("Unexpected token in JSON: {e}"), timed_out: false, refused: false })
}

/// Tasks a source owns; `stop()` aborts them all (the `AbortController` of the JS modules). A task aborted
/// while awaiting drops its in-flight request.
#[derive(Default)]
pub struct Jobs(Mutex<Vec<AbortHandle>>);

impl Jobs {
    pub fn new() -> Self {
        Jobs(Mutex::new(Vec::new()))
    }
    pub fn spawn<F: std::future::Future<Output = ()> + Send + 'static>(&self, fut: F) {
        let h = tokio::spawn(fut);
        let mut l = self.0.lock().unwrap();
        l.retain(|a| !a.is_finished());
        l.push(h.abort_handle());
    }
    pub fn abort_all(&self) {
        for a in self.0.lock().unwrap().drain(..) {
            a.abort();
        }
    }
}

// ---------- optional-chaining helpers for JSON ----------

/// `Number(v) || 0` as a JSON number (whole numbers stay integers so they print like JS: `3`, not `3.0`).
pub fn num_or0(v: &Value) -> Value {
    let n = js_to_number(Some(v));
    if n.is_nan() || n == 0.0 {
        return Value::from(0);
    }
    f64_value(n)
}

pub fn f64_value(n: f64) -> Value {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 9.0e15 {
        Value::from(n as i64)
    } else {
        serde_json::Number::from_f64(n).map(Value::Number).unwrap_or(Value::Null)
    }
}

/// `String(v ?? "")`
pub fn str_nullish(v: &Value) -> String {
    if v.is_null() {
        String::new()
    } else {
        js_string(v)
    }
}

/// `a || b` for JSON values (JS truthiness).
pub fn or<'a>(a: &'a Value, b: &'a Value) -> &'a Value {
    if truthy(a) {
        a
    } else {
        b
    }
}

