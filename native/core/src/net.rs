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


// ---------- temporary vs. permanent failures ----------

/// Whether an HTTP error status is a bad moment on the server's side (try again soon) rather than an answer.
/// 408 / 425 / 429 and every 5xx are temporary; any other status (404, 403, 400, ...) means what it says.
pub fn is_transient_status(status: u16) -> bool {
    matches!(status, 408 | 425 | 429 | 500..=599)
}

/// Whether a platform module's error text describes a temporary failure. The modules' errors are strings of two
/// shapes: what `fetch` / `text` / `json` above produced (the network, a deadline, a cut-off body: all temporary),
/// or "<what> returned|answered <status>" / "<what> <status>" (decided by the status). Anything else is a
/// statement about the channel or the page ("no video id", "a page this doesn't recognise") and is not.
pub fn is_transient_failure(msg: &str) -> bool {
    let m = msg.trim();
    if m.is_empty() || m == "fetch failed" || m == "This operation was aborted" || m.starts_with("Unexpected token in JSON") {
        return true;
    }
    failure_status(m).map(is_transient_status).unwrap_or(false)
}

/// The HTTP status at the end of a module's error text ("live chat poll returned 503", "... token (429)").
pub fn failure_status(msg: &str) -> Option<u16> {
    let last = msg.trim().rsplit(|c: char| c.is_whitespace()).next().unwrap_or("").trim_matches(|c: char| matches!(c, '(' | ')' | '.' | ':'));
    match last.parse::<u16>() {
        Ok(status) if last.len() == 3 && (100..=599).contains(&status) => Some(status),
        _ => None,
    }
}

/// Waits before reconnect attempts after a temporary failure: quick at first, then settling at the last step.
pub const RECONNECT_STEPS_MS: [u64; 4] = [2_000, 5_000, 10_000, 30_000];

/// The wait before retrying after `failures` temporary failures in a row (1 = the first), `jitter` in 0..1 spreading
/// it by +-20% so sources that failed together don't retry in lockstep.
pub fn reconnect_delay_ms(failures: u32, jitter: f64) -> u64 {
    let i = (failures.max(1) as usize - 1).min(RECONNECT_STEPS_MS.len() - 1);
    (RECONNECT_STEPS_MS[i] as f64 * (0.8 + 0.4 * jitter.clamp(0.0, 1.0))).round() as u64
}

#[cfg(test)]
mod retry_tests {
    use super::*;

    #[test]
    fn statuses_that_retry() {
        for s in [408, 425, 429, 500, 502, 503, 504, 599] {
            assert!(is_transient_status(s), "{s} should retry");
        }
        for s in [200, 204, 301, 400, 401, 403, 404, 410, 451] {
            assert!(!is_transient_status(s), "{s} should not retry");
        }
    }

    #[test]
    fn module_errors_are_classified() {
        // network, deadline and cut-off bodies
        assert!(is_transient_failure("fetch failed"));
        assert!(is_transient_failure("This operation was aborted"));
        assert!(is_transient_failure("Unexpected token in JSON: EOF while parsing a value at line 1 column 0"));
        assert!(is_transient_failure(""));
        // the status shapes the YouTube, Kick and Rumble modules write
        assert!(is_transient_failure("live chat poll returned 503"));
        assert!(is_transient_failure("YouTube answered 500"));
        assert!(is_transient_failure("Kick answered 429"));
        assert!(is_transient_failure("rumble channel 502"));
        assert!(is_transient_failure("rumble embedJS 504"));
        assert!(is_transient_failure("could not get an X guest token (503)"));
        assert!(!is_transient_failure("live chat poll returned 403"));
        assert!(!is_transient_failure("YouTube answered 404"));
        assert!(!is_transient_failure("Kick answered 403"));
        assert!(!is_transient_failure("rumble channel 410"));
        // statements about the channel or the page
        assert!(!is_transient_failure("rumble embedJS: no video id"));
        assert!(!is_transient_failure("Rumble returned a page this doesn't recognise"));
        assert!(!is_transient_failure("Kick returned a channel without a chat room"));
        assert!(!is_transient_failure("No such YouTube channel."));
    }

    #[test]
    fn status_at_the_end_of_an_error() {
        assert_eq!(failure_status("live chat poll returned 503"), Some(503));
        assert_eq!(failure_status("could not get an X guest token (429)"), Some(429));
        assert_eq!(failure_status("rumble embedJS: no video id"), None);
        assert_eq!(failure_status("line 1 column 12"), None);
        assert_eq!(failure_status("error 1234"), None);
    }

    #[test]
    fn reconnect_backoff_steps_and_caps() {
        assert_eq!(reconnect_delay_ms(0, 0.5), 2_000);
        assert_eq!(reconnect_delay_ms(1, 0.5), 2_000);
        assert_eq!(reconnect_delay_ms(2, 0.5), 5_000);
        assert_eq!(reconnect_delay_ms(3, 0.5), 10_000);
        assert_eq!(reconnect_delay_ms(4, 0.5), 30_000);
        assert_eq!(reconnect_delay_ms(50, 0.5), 30_000);
        // jitter stays within +-20%
        assert_eq!(reconnect_delay_ms(1, 0.0), 1_600);
        assert_eq!(reconnect_delay_ms(1, 1.0), 2_400);
        assert_eq!(reconnect_delay_ms(9, 1.0), 36_000);
        assert_eq!(reconnect_delay_ms(9, 7.0), 36_000);
    }
}
