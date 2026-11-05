//! Port of src/main/relay.js: the in-process "local relay": ref-counted live sources plus the api router.
//! See docs/ARCHITECTURE.md ("Sources", "The api router").
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use tokio::task::JoinHandle;

use crate::platforms::{Ctx, Emit, Modules, Source};
use crate::protocol::{ApiInit, ApiResult, Frame, SourceRef, SubId};
use crate::util::uuid_v4;

pub const LINGER_MS: u64 = 30_000;

/// `(subId, frame) => void`
pub type SendFn = Arc<dyn Fn(&SubId, &Frame) + Send + Sync>;

struct Entry {
    source: Arc<dyn Source>,
    subs: Arc<Mutex<HashSet<SubId>>>,
    linger: Option<JoinHandle<()>>,
}

struct Inner {
    sources: HashMap<String, Entry>,
    subs: HashMap<SubId, String>, // subId -> key (JS also kept the owning window: there is one UI here)
}

pub struct SourceManager {
    modules: Arc<Modules>,
    ctx: Arc<Ctx>,
    send: SendFn,
    inner: Arc<Mutex<Inner>>,
}

impl SourceManager {
    pub fn new(modules: Arc<Modules>, ctx: Arc<Ctx>, send: SendFn) -> Self {
        SourceManager { modules, ctx, send, inner: Arc::new(Mutex::new(Inner { sources: HashMap::new(), subs: HashMap::new() })) }
    }

    /// `platform:normalized-channel`. Errs with the message to show the user.
    pub fn key_of(&self, r: &SourceRef) -> Result<String, String> {
        let m = self.modules.get(&r.platform).ok_or_else(|| format!("Unknown platform: {}", r.platform.as_str()))?;
        Ok(format!("{}:{}", r.platform.as_str(), m.normalize_channel(&r.channel)?))
    }

    /// Must run inside the tokio runtime (sources spawn tasks).
    pub fn subscribe(&self, r: &SourceRef) -> Result<SubId, String> {
        let key = self.key_of(r)?;
        let sub_id = uuid_v4();
        let snapshot_source;
        {
            let mut inner = self.inner.lock().unwrap();
            if !inner.sources.contains_key(&key) {
                let channel = key[r.platform.as_str().len() + 1..].to_string();
                let subs: Arc<Mutex<HashSet<SubId>>> = Arc::new(Mutex::new(HashSet::new()));
                let (subs2, send) = (subs.clone(), self.send.clone());
                let emit: Emit = Arc::new(move |frame| {
                    let ids: Vec<SubId> = subs2.lock().unwrap().iter().cloned().collect();
                    for id in ids {
                        send(&id, &frame);
                    }
                });
                let m = self.modules.get(&r.platform).ok_or("unknown platform")?;
                let src = m.create_source(channel, emit, self.ctx.clone());
                inner.sources.insert(key.clone(), Entry { source: src.clone(), subs, linger: None });
                // Started before the new subscriber is added, as in JS: it gets the state from the snapshot below.
                src.start();
            }
            let entry = inner.sources.get_mut(&key).unwrap();
            if let Some(h) = entry.linger.take() {
                h.abort();
            }
            entry.subs.lock().unwrap().insert(sub_id.clone());
            snapshot_source = entry.source.clone();
            inner.subs.insert(sub_id.clone(), key);
        }
        // Replay the current state so a late subscriber sees what everyone else sees.
        for frame in snapshot_source.snapshot() {
            (self.send)(&sub_id, &frame);
        }
        Ok(sub_id)
    }

    pub fn unsubscribe(&self, sub_id: &str) {
        let mut inner = self.inner.lock().unwrap();
        let Some(key) = inner.subs.remove(sub_id) else { return };
        let Some(entry) = inner.sources.get_mut(&key) else { return };
        let empty = {
            let mut subs = entry.subs.lock().unwrap();
            subs.remove(sub_id);
            subs.is_empty()
        };
        if empty {
            let (inner_arc, ctx) = (self.inner.clone(), self.ctx.clone());
            entry.linger = Some(tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(LINGER_MS)).await;
                let stopped = {
                    let mut inner = inner_arc.lock().unwrap();
                    match inner.sources.get(&key) {
                        Some(e) if e.subs.lock().unwrap().is_empty() => inner.sources.remove(&key).map(|e| e.source),
                        _ => None,
                    }
                };
                if let Some(src) = stopped {
                    src.stop();
                    let _ = &ctx; // JS logged a failed stop; stop() cannot fail here
                }
            }));
        }
    }

    pub fn stop_all(&self) {
        let entries: Vec<Entry> = {
            let mut inner = self.inner.lock().unwrap();
            inner.subs.clear();
            inner.sources.drain().map(|(_, e)| e).collect()
        };
        for e in entries {
            if let Some(h) = e.linger {
                h.abort();
            }
            e.source.stop();
        }
    }

    /// For tests and diagnostics: how many live upstream sources there are.
    pub fn source_count(&self) -> usize {
        self.inner.lock().unwrap().sources.len()
    }
}

// ---------------------------------------------------------------------------------------------------------
// api router
// ---------------------------------------------------------------------------------------------------------

/// What a route handler gets (`{source, method, query, body, ctx}`).
pub struct RouteReq {
    pub source: SourceRef,
    pub method: String,
    /// URLSearchParams, in order
    pub query: Vec<(String, String)>,
    /// parsed JSON, or Null
    pub body: Value,
    pub ctx: Arc<Ctx>,
}

impl RouteReq {
    /// `query.get(name)`: the first value.
    pub fn query_get(&self, name: &str) -> Option<&str> {
        self.query.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct RouteRes {
    pub status: u16,
    pub body: Value,
}

/// A handler's escape hatch: something thrown that is not an HTTP answer. The router answers it with
/// `{status, body: {error}}` (500 unless the error carried a status).
#[derive(Debug, Clone)]
pub struct RouteError {
    pub status: u16,
    pub message: String,
}

pub type Handler = Arc<dyn Fn(RouteReq) -> BoxFuture<'static, Result<RouteRes, RouteError>> + Send + Sync>;

/// Routes "METHOD /path" to the platform modules' handlers.
pub struct Api {
    table: HashMap<String, Handler>,
    ctx: Arc<Ctx>,
}

impl Api {
    pub fn new(modules: &Modules, ctx: Arc<Ctx>) -> Self {
        let mut table = HashMap::new();
        for m in modules.values() {
            for (route, handler) in m.routes() {
                table.insert(route, handler);
            }
        }
        Api { table, ctx }
    }

    pub fn call(&self, source: SourceRef, path: &str, init: ApiInit) -> BoxFuture<'static, ApiResult> {
        let method = init.method.as_deref().unwrap_or("GET").to_uppercase();
        let url = url::Url::parse("http://local").and_then(|base| base.join(path));
        let url = match url {
            Ok(u) => u,
            Err(e) => {
                let err = format!("invalid path: {e}");
                return Box::pin(async move { ApiResult { status: 500, body: json!({"error": err}) } });
            }
        };
        let pathname = url.path().to_string();
        let handler = self.table.get(&format!("{method} {pathname}")).or_else(|| self.table.get(&format!("* {pathname}"))).cloned();
        let Some(handler) = handler else {
            return Box::pin(async { ApiResult { status: 404, body: json!({"error": "not found"}) } });
        };
        let mut body = init.body.unwrap_or(Value::Null);
        if let Value::String(s) = &body {
            if let Ok(parsed) = serde_json::from_str::<Value>(s) {
                body = parsed;
            }
        }
        let req = RouteReq {
            source,
            method: method.clone(),
            query: url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect(),
            body,
            ctx: self.ctx.clone(),
        };
        let ctx = self.ctx.clone();
        Box::pin(async move {
            match handler(req).await {
                Ok(res) => ApiResult { status: res.status, body: res.body },
                Err(err) => {
                    ctx.log(&format!("[api] {method} {pathname} {}", err.message));
                    ApiResult { status: if err.status == 0 { 500 } else { err.status }, body: json!({"error": err.message}) }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::Logger;
    use crate::platforms::PlatformModule;
    use crate::protocol::Platform;
    use crate::store::Store;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fake {
        started: Arc<AtomicUsize>,
        stopped: Arc<AtomicUsize>,
    }
    struct FakeSource {
        emit: Emit,
        started: Arc<AtomicUsize>,
        stopped: Arc<AtomicUsize>,
    }
    impl Source for FakeSource {
        fn start(self: Arc<Self>) {
            self.started.fetch_add(1, Ordering::SeqCst);
            (self.emit)(json!({"type": "conn", "state": "open"}));
        }
        fn stop(&self) {
            self.stopped.fetch_add(1, Ordering::SeqCst);
        }
        fn snapshot(&self) -> Vec<Frame> {
            vec![json!({"type": "snap"})]
        }
    }
    impl PlatformModule for Fake {
        fn platform(&self) -> Platform {
            Platform::Twitch
        }
        fn normalize_channel(&self, i: &str) -> Result<String, String> {
            if i.is_empty() { Err("empty".into()) } else { Ok(i.to_lowercase()) }
        }
        fn create_source(&self, _c: String, emit: Emit, _ctx: Arc<Ctx>) -> Arc<dyn Source> {
            Arc::new(FakeSource { emit, started: self.started.clone(), stopped: self.stopped.clone() })
        }
        fn routes(&self) -> Vec<(String, Handler)> {
            let ok: Handler = Arc::new(|r: RouteReq| Box::pin(async move { Ok(RouteRes { status: 200, body: json!({"m": r.method, "q": r.query_get("a"), "b": r.body}) }) }));
            let boom: Handler = Arc::new(|_| Box::pin(async { Err(RouteError { status: 0, message: "kaboom".into() }) }));
            vec![("POST /x".into(), ok.clone()), ("* /any".into(), ok), ("GET /boom".into(), boom)]
        }
    }

    fn ctx() -> Arc<Ctx> {
        let dir = std::env::temp_dir().join(format!("agu-relay-{}", crate::util::uuid_v4()));
        let log = Arc::new(Logger::new(&dir, true));
        let st = Store::open(dir, log.clone());
        Arc::new(Ctx::new(st.settings.clone(), st.secrets.clone(), log, "t".into()))
    }

    #[tokio::test(start_paused = true)]
    async fn ref_counting_linger_and_snapshot_replay() {
        let (started, stopped) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let mut mods = Modules::new();
        mods.insert(Platform::Twitch, Arc::new(Fake { started: started.clone(), stopped: stopped.clone() }));
        let got: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let g2 = got.clone();
        let m = SourceManager::new(Arc::new(mods), ctx(), Arc::new(move |id, f| g2.lock().unwrap().push((id.clone(), f["type"].as_str().unwrap().to_string()))));
        let r = SourceRef { platform: Platform::Twitch, channel: "Foo".into() };
        let a = m.subscribe(&r).unwrap();
        let b = m.subscribe(&r).unwrap();
        assert_eq!(started.load(Ordering::SeqCst), 1);
        assert_eq!(m.source_count(), 1);
        // start() runs before the first subscriber is added (as in JS), so both get only the snapshot
        let log = got.lock().unwrap().clone();
        assert_eq!(log.iter().filter(|(i, _)| *i == a).map(|(_, t)| t.as_str()).collect::<Vec<_>>(), vec!["snap"]);
        assert_eq!(log.iter().filter(|(i, _)| *i == b).map(|(_, t)| t.as_str()).collect::<Vec<_>>(), vec!["snap"]);
        assert!(m.subscribe(&SourceRef { platform: Platform::Twitch, channel: "".into() }).is_err());
        m.unsubscribe(&a);
        m.unsubscribe(&b);
        tokio::time::sleep(Duration::from_millis(LINGER_MS - 1000)).await;
        assert_eq!(stopped.load(Ordering::SeqCst), 0);
        let c = m.subscribe(&r).unwrap(); // back within the linger window: same upstream
        assert_eq!(started.load(Ordering::SeqCst), 1);
        m.unsubscribe(&c);
        tokio::time::sleep(Duration::from_millis(LINGER_MS + 100)).await;
        assert_eq!(stopped.load(Ordering::SeqCst), 1);
        assert_eq!(m.source_count(), 0);
    }

    #[tokio::test]
    async fn router_table_fallback_404_and_500() {
        let mut mods = Modules::new();
        mods.insert(Platform::Twitch, Arc::new(Fake { started: Arc::new(AtomicUsize::new(0)), stopped: Arc::new(AtomicUsize::new(0)) }));
        let api = Api::new(&mods, ctx());
        let r = SourceRef { platform: Platform::Twitch, channel: "foo".into() };
        let res = api.call(r.clone(), "/x?a=1", ApiInit { method: Some("post".into()), body: Some(json!("{\"k\":2}")) }).await;
        assert_eq!((res.status, res.body), (200, json!({"m": "POST", "q": "1", "b": {"k": 2}})));
        assert_eq!(api.call(r.clone(), "/any", ApiInit::default()).await.status, 200);
        let nf = api.call(r.clone(), "/x", ApiInit::default()).await; // GET /x has no handler
        assert_eq!((nf.status, nf.body), (404, json!({"error": "not found"})));
        let e = api.call(r, "/boom", ApiInit::default()).await;
        assert_eq!((e.status, e.body), (500, json!({"error": "kaboom"})));
    }
}
