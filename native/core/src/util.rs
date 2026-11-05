//! Rust-only helpers standing in for things Node and the JS runtime give the Electron main process for free:
//! `Date`, `crypto.randomUUID`, `String.prototype.trim/slice`, `EventEmitter`, `setTimeout/setInterval`.
//! There is no JS counterpart file.
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::task::JoinHandle;

/// `Date.now()`
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `crypto.randomUUID()` (v4).
pub fn uuid_v4() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: Vec<String> = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", h[0..4].concat(), h[4..6].concat(), h[6..8].concat(), h[8..10].concat(), h[10..16].concat())
}

/// Days since 1970-01-01 -> (year, month, day). Howard Hinnant's civil_from_days.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `new Date(ms).toISOString()`
pub fn iso_ms(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3_600_000,
        rem / 60_000 % 60,
        rem / 1000 % 60,
        rem % 1000
    )
}

/// `Date.parse(s)` for the RFC 3339 / ISO 8601 strings Twitch sends ("2026-01-02T03:04:05.678Z", "...+01:00").
pub fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let num = |a: usize, z: usize| s.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let mut ms = days_from_civil(y, mo as u32, d as u32) * 86_400_000;
    let mut i = 10;
    if i < b.len() && (b[i] == b'T' || b[i] == b't' || b[i] == b' ') {
        let (h, mi) = (num(i + 1, i + 3)?, num(i + 4, i + 6)?);
        i += 6;
        let mut sec = 0;
        let mut frac = 0;
        if i < b.len() && b[i] == b':' {
            sec = num(i + 1, i + 3)?;
            i += 3;
            if i < b.len() && b[i] == b'.' {
                let start = i + 1;
                let mut j = start;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                let digits = &s[start..j];
                if digits.is_empty() {
                    return None;
                }
                let three: String = digits.chars().chain("00".chars()).take(3).collect();
                frac = three.parse::<i64>().ok()?;
                i = j;
            }
        }
        ms += h * 3_600_000 + mi * 60_000 + sec * 1000 + frac;
        if i < b.len() {
            match b[i] {
                b'Z' | b'z' => {}
                b'+' | b'-' => {
                    let sign = if b[i] == b'+' { 1 } else { -1 };
                    let oh = num(i + 1, i + 3)?;
                    let om = if s.get(i + 3..i + 4) == Some(":") { num(i + 4, i + 6)? } else { num(i + 3, i + 5).unwrap_or(0) };
                    ms -= sign * (oh * 3_600_000 + om * 60_000);
                }
                _ => return None,
            }
        }
    }
    Some(ms)
}

/// JS `String.prototype.trim`: Unicode whitespace plus U+FEFF, but not U+0085 (which Rust's `trim` strips).
pub fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}')
}

/// JS `s.slice(0, max)`, which counts UTF-16 code units. A cut inside a surrogate pair drops the whole
/// character (JS would keep a lone surrogate, which Rust strings cannot hold).
pub fn slice_utf16(s: &str, max: usize) -> String {
    let mut units = 0;
    let mut out = String::new();
    for c in s.chars() {
        units += c.len_utf16();
        if units > max {
            break;
        }
        out.push(c);
    }
    out
}

/// JS `s.length`
pub fn len_utf16(s: &str) -> usize {
    s.encode_utf16().count()
}

/// JS truthiness of a JSON value (`if (v)`).
pub fn truthy(v: &serde_json::Value) -> bool {
    use serde_json::Value::*;
    match v {
        Null => false,
        Bool(b) => *b,
        Number(n) => n.as_f64().map(|f| f != 0.0 && !f.is_nan()).unwrap_or(true),
        String(s) => !s.is_empty(),
        Array(_) | Object(_) => true,
    }
}

/// JS `String(v)` for a JSON value.
pub fn js_string(v: &serde_json::Value) -> String {
    use serde_json::Value::*;
    match v {
        Null => "null".into(),
        Bool(b) => b.to_string(),
        Number(n) => n.to_string(),
        String(s) => s.clone(),
        Array(a) => a.iter().map(|x| if x.is_null() { std::string::String::new() } else { js_string(x) }).collect::<Vec<_>>().join(","),
        Object(_) => "[object Object]".into(),
    }
}

/// JS `Number(v)` for a JSON value; `None` stands for `undefined` (NaN).
pub fn js_to_number(v: Option<&serde_json::Value>) -> f64 {
    use serde_json::Value::*;
    match v {
        None => f64::NAN,
        Some(Null) => 0.0,
        Some(Bool(b)) => *b as u8 as f64,
        Some(Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(String(s)) => js_number(s),
        Some(_) => f64::NAN,
    }
}

/// `String(v || "")` for an optional JSON value.
pub fn js_str_or_empty(v: Option<&serde_json::Value>) -> String {
    match v {
        Some(x) if truthy(x) => js_string(x),
        _ => String::new(),
    }
}

/// JS `Number(s)` for a string (trim, "" is 0, otherwise a decimal float or NaN).
pub fn js_number(s: &str) -> f64 {
    let t = js_trim(s);
    if t.is_empty() {
        return 0.0;
    }
    t.parse::<f64>().ok().filter(|f| f.is_finite() || t.contains("inf") || t.contains("Inf")).unwrap_or(f64::NAN)
}

/// `Number(s) || fallback` where the number is meant to be a whole count (timestamps, bits, durations):
/// zero and NaN give `fallback`, a fraction is kept only by truncation.
pub fn js_int_or(s: &str, fallback: i64) -> i64 {
    let n = js_number(s);
    if n == 0.0 || n.is_nan() {
        fallback
    } else {
        n as i64
    }
}

// ---------------------------------------------------------------------------------------------------------
// EventEmitter
// ---------------------------------------------------------------------------------------------------------

/// A list of callbacks (`EventEmitter` with one event). `emit` copies the list first, so a callback may
/// add or remove listeners.
pub struct Listeners<T: ?Sized> {
    next: AtomicU64,
    list: Mutex<Vec<(u64, Arc<dyn Fn(&T) + Send + Sync>)>>,
}

impl<T: ?Sized> Default for Listeners<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: ?Sized> Listeners<T> {
    pub const fn new() -> Self {
        Listeners { next: AtomicU64::new(1), list: Mutex::new(Vec::new()) }
    }
    /// `events.on(...)`; the id is what `off` takes.
    pub fn on(&self, cb: Arc<dyn Fn(&T) + Send + Sync>) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.list.lock().unwrap().push((id, cb));
        id
    }
    pub fn off(&self, id: u64) {
        self.list.lock().unwrap().retain(|(i, _)| *i != id);
    }
    pub fn emit(&self, v: &T) {
        let cbs: Vec<_> = self.list.lock().unwrap().iter().map(|(_, c)| c.clone()).collect();
        for cb in cbs {
            cb(v);
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Timers
// ---------------------------------------------------------------------------------------------------------

/// `setTimeout` / `setInterval` with `clearTimeout`: one slot. Setting replaces (and cancels) what was there.
/// The callback runs as its own task, so a callback that re-arms or clears its own timer is not cut short.
#[derive(Default)]
pub struct Timer {
    cancel: Option<Arc<AtomicBool>>,
    handle: Option<JoinHandle<()>>,
}

impl Timer {
    pub const fn new() -> Self {
        Timer { cancel: None, handle: None }
    }

    pub fn clear(&mut self) {
        if let Some(c) = self.cancel.take() {
            c.store(true, Ordering::SeqCst);
        }
        if let Some(h) = self.handle.take() {
            h.abort();
        }
    }

    /// Runs `fut` once after `d`.
    pub fn set<F: Future<Output = ()> + Send + 'static>(&mut self, d: Duration, fut: F) {
        self.clear();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        self.handle = Some(tokio::spawn(async move {
            tokio::time::sleep(d).await;
            if !flag.load(Ordering::SeqCst) {
                tokio::spawn(fut);
            }
        }));
        self.cancel = Some(cancel);
    }

    /// Runs `f()` every `d` (the first run after `d`).
    pub fn every<F: Fn() -> Fut + Send + Sync + 'static, Fut: Future<Output = ()> + Send + 'static>(&mut self, d: Duration, f: F) {
        self.clear();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        self.handle = Some(tokio::spawn(async move {
            let mut iv = tokio::time::interval_at(tokio::time::Instant::now() + d, d);
            loop {
                iv.tick().await;
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                tokio::spawn(f());
            }
        }));
        self.cancel = Some(cancel);
    }

    pub fn is_set(&self) -> bool {
        self.cancel.as_ref().map(|c| !c.load(Ordering::SeqCst)).unwrap_or(false)
    }
}

/// A single-threaded event loop owning state `S`: jobs posted from anywhere run one at a time, in order,
/// like callbacks on Node's event loop. IrcHub uses it so its socket handlers, timers and `add/remove` calls
/// never race and never need a lock held across a listener callback.
pub struct Mailbox<S: Send + 'static> {
    tx: UnboundedSender<Box<dyn FnOnce(&mut S) + Send>>,
}

impl<S: Send + 'static> Clone for Mailbox<S> {
    fn clone(&self) -> Self {
        Mailbox { tx: self.tx.clone() }
    }
}

impl<S: Send + 'static> Mailbox<S> {
    /// Starts the loop on the current tokio runtime. `init` gets a handle to the mailbox itself.
    pub fn spawn(init: impl FnOnce(Mailbox<S>) -> S) -> Self {
        let (tx, mut rx) = unbounded_channel::<Box<dyn FnOnce(&mut S) + Send>>();
        let mb = Mailbox { tx };
        let mut state = init(mb.clone());
        tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                job(&mut state);
            }
        });
        mb
    }

    pub fn post(&self, f: impl FnOnce(&mut S) + Send + 'static) {
        let _ = self.tx.send(Box::new(f));
    }
}

/// A `Timer` whose callback runs inside a `Mailbox`. Cancelling is checked again when the job runs, so a
/// timer cleared by an earlier job in the queue never fires.
#[derive(Default)]
pub struct MailTimer {
    cancel: Option<Arc<AtomicBool>>,
    handle: Option<JoinHandle<()>>,
}

impl MailTimer {
    pub const fn new() -> Self {
        MailTimer { cancel: None, handle: None }
    }
    pub fn clear(&mut self) {
        if let Some(c) = self.cancel.take() {
            c.store(true, Ordering::SeqCst);
        }
        if let Some(h) = self.handle.take() {
            h.abort();
        }
    }
    pub fn set<S: Send + 'static>(&mut self, mb: &Mailbox<S>, d: Duration, f: impl FnOnce(&mut S) + Send + 'static) {
        self.clear();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let mb = mb.clone();
        self.handle = Some(tokio::spawn(async move {
            tokio::time::sleep(d).await;
            mb.post(move |s| {
                if !flag.load(Ordering::SeqCst) {
                    f(s)
                }
            });
        }));
        self.cancel = Some(cancel);
    }
    pub fn every<S: Send + 'static>(&mut self, mb: &Mailbox<S>, d: Duration, f: impl Fn(&mut S) + Send + Sync + 'static) {
        self.clear();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let mb = mb.clone();
        let f = Arc::new(f);
        self.handle = Some(tokio::spawn(async move {
            let mut iv = tokio::time::interval_at(tokio::time::Instant::now() + d, d);
            loop {
                iv.tick().await;
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                let (flag, f) = (flag.clone(), f.clone());
                mb.post(move |s| {
                    if !flag.load(Ordering::SeqCst) {
                        f(s)
                    }
                });
            }
        }));
        self.cancel = Some(cancel);
    }
    pub fn is_set(&self) -> bool {
        self.cancel.as_ref().map(|c| !c.load(Ordering::SeqCst)).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_and_parse_round_trip() {
        assert_eq!(iso_ms(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_ms(1_767_225_600_123), "2026-01-01T00:00:00.123Z");
        assert_eq!(parse_rfc3339_ms("2026-01-01T00:00:00.123Z"), Some(1_767_225_600_123));
        assert_eq!(parse_rfc3339_ms("2026-01-01T01:00:00+01:00"), Some(1_767_225_600_000));
        assert_eq!(parse_rfc3339_ms("2026-01-01T00:00:00.5Z"), Some(1_767_225_600_500));
        assert_eq!(parse_rfc3339_ms("nope"), None);
    }

    #[test]
    fn js_helpers() {
        assert_eq!(js_trim("\u{feff} a \u{85}"), "a \u{85}");
        assert_eq!(slice_utf16("ab😀c", 3), "ab");
        assert_eq!(slice_utf16("ab😀c", 4), "ab😀");
        assert_eq!(js_int_or("", 7), 7);
        assert_eq!(js_int_or("abc", 7), 7);
        assert_eq!(js_int_or("1700000000123", 7), 1_700_000_000_123);
        assert_eq!(uuid_v4().len(), 36);
    }
}
