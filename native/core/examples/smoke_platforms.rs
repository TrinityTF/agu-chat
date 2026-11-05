//! Read-only smoke run of the Kick / YouTube / Rumble / X modules against a COPY of the app data (never the
//! real `%APPDATA%\Agu Chat`):
//!
//!   AGU_DATA_DIR=<empty or temp dir> cargo run -p agu-chat-core --example smoke_platforms -- [seconds] [platform:channel ...]
//!
//! With no sources given it uses the defaults below (public channels; pass your own with
//! `kick:name youtube:@handle rumble:v7abcd x:name`). It subscribes one source per entry, counts the frame types
//! each emits for ~20 s and prints a secret-free summary (conn states, chatlog sizes, emote/badge counts, the
//! shape of the last chat row), then calls a few read-only routes. It never logs in, sends, or moderates:
//! the two write routes are only called while logged out, where they answer before touching the network.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agu_chat_core::protocol::*;
use agu_chat_core::{default_data_dir, Backend};
use serde_json::{json, Value};

const DEFAULTS: [&str; 5] = ["kick:fabicolo99", "youtube:@lofigirl", "rumble:v7hwjsi", "rumble:c/AlexJonesNetworkLive", "x:nasa"];

fn platform(s: &str) -> Platform {
    match s {
        "kick" => Platform::Kick,
        "youtube" => Platform::Youtube,
        "rumble" => Platform::Rumble,
        "x" => Platform::X,
        other => panic!("platform {other} is not part of this smoke run"),
    }
}

#[derive(Default)]
struct Stats {
    counts: BTreeMap<String, usize>,
    notes: Vec<String>,
    last_row: Option<Value>,
}

fn main() {
    if std::env::var_os("AGU_AUTH_READONLY").is_none() {
        std::env::set_var("AGU_AUTH_READONLY", "1");
    }
    let dir = default_data_dir();
    let real = std::env::var_os("APPDATA").map(|a| std::path::PathBuf::from(a).join("Agu Chat"));
    let canon = |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    if std::env::var_os("AGU_DATA_DIR").is_none() || real.as_deref().map(|r| canon(r) == canon(&dir)).unwrap_or(false) {
        eprintln!("refusing to run: set AGU_DATA_DIR to a temp dir (never %APPDATA%\\Agu Chat)");
        std::process::exit(2);
    }
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let seconds: u64 = if args.first().map(|a| a.parse::<u64>().is_ok()).unwrap_or(false) { args.remove(0).parse().unwrap() } else { 20 };
    if args.is_empty() {
        args = DEFAULTS.iter().map(|s| s.to_string()).collect();
    }

    let backend = Arc::new(Backend::start(dir));
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();

    let stats: Arc<Mutex<BTreeMap<String, Stats>>> = Arc::new(Mutex::new(BTreeMap::new()));
    let labels: Arc<Mutex<BTreeMap<String, String>>> = Arc::new(Mutex::new(BTreeMap::new()));
    let (s2, l2) = (stats.clone(), labels.clone());
    let off = backend.on_frame(Arc::new(move |sub, frame: &Frame| {
        let Some(label) = l2.lock().unwrap().get(sub).cloned() else { return };
        let mut all = s2.lock().unwrap();
        let st = all.entry(label).or_default();
        let ty = frame["type"].as_str().unwrap_or("?").to_string();
        *st.counts.entry(ty.clone()).or_insert(0) += 1;
        let n = |k: &str| frame[k].as_array().map(|a| a.len()).unwrap_or(0);
        let objn = |k: &str| frame[k].as_object().map(|a| a.len()).unwrap_or(0);
        match ty.as_str() {
            "conn" => st.notes.push(format!("conn state={} detail={:?} live={} canSend={} title={}", frame["state"], frame["detail"], frame["live"], frame["canSend"], frame["title"].as_str().map(|t| format!("str({})", t.len())).unwrap_or("-".into()))),
            "chatlog" => st.notes.push(format!("chatlog messages={} emotes={} badges={} live={}", n("messages"), objn("emotes"), if frame["badges"].is_array() { n("badges") } else { objn("badges") }, frame["status"]["live"])),
            "emotes" => st.notes.push(format!("emotes frame: {} entries", objn("emotes"))),
            "badges" => st.notes.push(format!("badges frame: {} entries", objn("badges"))),
            "status" => st.notes.push(format!("status live={} viewers={} error={}", frame["status"]["live"], frame["status"]["viewers"], frame["status"]["error"])),
            "chat" => {
                let row = if frame["message"].is_object() { &frame["message"] } else if frame["msg"].is_object() { &frame["msg"] } else { &frame["messages"][0] };
                st.last_row = Some(row.clone());
            }
            _ => {}
        }
    }));

    let mut subs = Vec::new();
    let mut refs = Vec::new();
    for a in &args {
        let (p, ch) = a.split_once(':').expect("platform:channel");
        let r = SourceRef { platform: platform(p), channel: ch.to_string() };
        match rt.block_on(backend.subscribe(r.clone())) {
            Ok(sub) => {
                println!("subscribed {a}");
                labels.lock().unwrap().insert(sub.clone(), a.clone());
                subs.push(sub);
                refs.push(r);
            }
            Err(e) => println!("subscribe {a} refused: {e}"),
        }
    }

    // read-only routes while it runs
    rt.block_on(async { tokio::time::sleep(Duration::from_secs(4)).await });
    let calls: [(Platform, &str, &str, Value); 6] = [
        (Platform::Rumble, "GET", "/api/rumble/caps", Value::Null),
        // logged out: these answer 403 before any network call
        (Platform::Rumble, "POST", "/api/rumble/send", json!({"text": "x"})),
        (Platform::Rumble, "POST", "/api/rumble/action", json!({"action": "ban", "username": "x"})),
        (Platform::Kick, "GET", "/api/kick/caps", Value::Null),
        (Platform::Youtube, "GET", "/api/youtube/caps", Value::Null),
        (Platform::X, "POST", "/api/x/send", json!({"text": "x"})),
    ];
    for (p, method, path, body) in calls {
        let Some(r) = refs.iter().find(|r| r.platform == p).cloned() else { continue };
        let res = rt.block_on(backend.api(r, path.to_string(), ApiInit { method: Some(method.into()), body: if body.is_null() { None } else { Some(body) } }));
        println!("api {method} {path} -> {} {}", res.status, res.body);
    }
    let accounts = rt.block_on(backend.accounts_get());
    for p in [Platform::Kick, Platform::Youtube, Platform::Rumble, Platform::X] {
        let a = &accounts[&p];
        println!("account {:<8} connected={} unsupported={}", p.as_str(), a.connected, a.unsupported);
    }

    rt.block_on(async { tokio::time::sleep(Duration::from_secs(seconds.saturating_sub(4))).await });
    println!("--- after {seconds}s ---");
    for (label, st) in stats.lock().unwrap().iter() {
        println!("[{label}] frames: {:?}", st.counts);
        for n in st.notes.iter().take(8) {
            println!("    {n}");
        }
        if let Some(row) = &st.last_row {
            // shape only: field names, and the lengths of text fields, never their content
            let shape: Vec<String> = row.as_object().map(|o| o.iter().map(|(k, v)| match v {
                Value::String(s) => format!("{k}:str({})", s.chars().count()),
                Value::Array(a) => format!("{k}[{}]", a.len()),
                Value::Object(_) => format!("{k}{{}}"),
                other => format!("{k}={other}"),
            }).collect()).unwrap_or_default();
            println!("    last chat row: {}", shape.join(", "));
        }
    }
    for s in subs {
        rt.block_on(backend.unsubscribe(s));
    }
    off();
    backend.shutdown();
    println!("done");
}
