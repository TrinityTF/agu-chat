//! Smoke run of the backend against a COPY of the app data (never the real `%APPDATA%\Agu Chat`):
//!
//!   AGU_DATA_DIR=<temp copy of settings.json layout.json window.json secrets.bin "Local State">
//!   cargo run -p agu-chat-core --example smoke [seconds]
//!   (AGU_IRC_URL=ws://127.0.0.1:6680 points the chat at native/bench/fake-irc.mjs instead of Twitch)
//!
//! It prints the accounts state (flags, login name, scope count: never a token), a few settings, then
//! subscribes `twitch:a_g_u__` and counts frame types for ~20 s, and finally calls read-only api routes.
//! The only real channel it will ever join is a_g_u__.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agu_chat_core::protocol::*;
use agu_chat_core::{default_data_dir, Backend};
use serde_json::{json, Value};

const CHANNEL: &str = "a_g_u__";

fn main() {
    // A run on a copy must never rotate the refresh token of the real install (see twitch/auth.rs).
    if std::env::var_os("AGU_AUTH_READONLY").is_none() {
        std::env::set_var("AGU_AUTH_READONLY", "1");
    }
    let dir = default_data_dir();
    let real = std::env::var_os("APPDATA").map(|a| std::path::PathBuf::from(a).join("Agu Chat"));
    let canon = |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    if std::env::var_os("AGU_DATA_DIR").is_none() || real.as_deref().map(|r| canon(r) == canon(&dir)).unwrap_or(false) {
        eprintln!("refusing to run: set AGU_DATA_DIR to a temp COPY of the app data (never %APPDATA%\\Agu Chat)");
        std::process::exit(2);
    }
    let seconds: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(20);
    let fake = std::env::var("AGU_IRC_URL").ok().filter(|v| !v.is_empty());
    println!("data dir: {}", dir.display());
    println!("chat endpoint: {}", fake.as_deref().unwrap_or("real Twitch"));

    let backend = Arc::new(Backend::start(dir));
    let wait = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();

    println!("app_info: {:?}", backend.app_info());
    println!("updater: {:?}", backend.updater_get().status);

    // accounts_get: connected flags and the login name, no tokens
    let accounts = wait.block_on(backend.accounts_get());
    for p in Platform::ALL {
        let a = &accounts[&p];
        println!(
            "account {:<8} connected={} unsupported={} user={} scopes={} pending={} error={:?}",
            p.as_str(),
            a.connected,
            a.unsupported,
            a.user.as_ref().map(|u| u.login.as_str()).unwrap_or("-"),
            a.scopes.as_ref().map(|s| s.len()).unwrap_or(0),
            a.pending.is_some(),
            a.error
        );
    }
    println!("secrets.bin keys: {:?}", backend.store().secrets.keys());

    let s = backend.settings_get();
    println!(
        "settings: updateChannel={} fontSize={} emoteSize={} sevenTv={} bttv={} ffz={} modButtons={} highlightRules={} ignoredUsers={} hotkeys={}",
        s.update_channel,
        s.font_size,
        s.emote_size,
        s.seven_tv,
        s.bttv,
        s.ffz,
        s.mod_buttons.len(),
        s.highlight_rules.len(),
        s.ignored_users.len(),
        s.hotkeys.len()
    );
    println!("layout: {}", backend.layout_get().map(|l| format!("{} tab(s), active {}", l.tabs.len(), l.active)).unwrap_or_else(|| "none".into()));

    // subscribe and count frames
    let counts: Arc<Mutex<BTreeMap<String, usize>>> = Arc::new(Mutex::new(BTreeMap::new()));
    let notes: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let (c2, n2) = (counts.clone(), notes.clone());
    let off = backend.on_frame(Arc::new(move |_sub, frame: &Frame| {
        let ty = frame["type"].as_str().unwrap_or("?").to_string();
        *c2.lock().unwrap().entry(ty.clone()).or_insert(0) += 1;
        let mut n = n2.lock().unwrap();
        match ty.as_str() {
            "conn" => n.push(format!("conn state={} detail={:?} live={} canSend={} channelId={}", frame["state"], frame["detail"], frame["live"], frame["canSend"], frame["channelId"])),
            "esub" => n.push(format!("esub {}", frame["esub"])),
            "chatlog" => n.push(format!("chatlog messages={} modfeed={} pinned={} esub={}", frame["messages"].as_array().map(|a| a.len()).unwrap_or(0), frame["modfeed"].as_array().map(|a| a.len()).unwrap_or(0), !frame["pinned"].is_null(), frame["esub"])),
            _ => {}
        }
    }));
    let source = SourceRef { platform: Platform::Twitch, channel: CHANNEL.into() };
    let sub = wait.block_on(backend.subscribe(source.clone())).expect("subscribe");
    println!("subscribed {} as {}", CHANNEL, &sub[..8]);

    // read-only api routes while it runs
    wait.block_on(async {
        tokio::time::sleep(Duration::from_secs(3)).await;
    });
    let routes: [(&str, &str); 6] = [
        ("GET", "/api/chat/badges"),
        ("GET", "/api/chat/twitch-emotes"),
        ("GET", "/api/mod/caps"),
        ("GET", "/api/chat/settings"),
        ("GET", "/api/chatters?q=a"),
        ("GET", "/api/nope"),
    ];
    for (method, path) in routes {
        let res = wait.block_on(backend.api(source.clone(), path.to_string(), ApiInit { method: Some(method.into()), body: None }));
        println!("api {method} {path} -> {} {}", res.status, summarize(&res.body));
    }

    wait.block_on(async {
        tokio::time::sleep(Duration::from_secs(seconds.saturating_sub(3))).await;
    });
    // SMOKE_DEVICE=1: start the Twitch device-code flow to prove it answers with a prompt, then cancel it.
    // The code is never entered anywhere, so nothing is logged in.
    if std::env::var("SMOKE_DEVICE").map(|v| v == "1").unwrap_or(false) {
        match wait.block_on(backend.accounts_login(Platform::Twitch, json!({}))) {
            Ok(p) => println!("device prompt: userCode.len={} verificationUri={} expiresAt={}", p["userCode"].as_str().map(str::len).unwrap_or(0), p["verificationUri"], p["expiresAt"]),
            Err(e) => println!("device login failed: {e}"),
        }
        let st = wait.block_on(backend.accounts_get());
        println!("pending after login: {}", st[&Platform::Twitch].pending.is_some());
        wait.block_on(backend.accounts_cancel(Platform::Twitch));
        let st = wait.block_on(backend.accounts_get());
        println!("pending after cancel: {}", st[&Platform::Twitch].pending.is_some());
    }
    println!("--- frames after {seconds}s ---");
    for (ty, n) in counts.lock().unwrap().iter() {
        println!("  {ty}: {n}");
    }
    for line in notes.lock().unwrap().iter().take(12) {
        println!("  note: {line}");
    }
    wait.block_on(backend.unsubscribe(sub));
    off();
    backend.shutdown();
    println!("done");
}

/// A short, secret-free description of a JSON body: its shape and sizes, never its text.
fn summarize(v: &Value) -> String {
    match v {
        Value::Object(o) => {
            let parts: Vec<String> = o
                .iter()
                .take(12)
                .map(|(k, v)| match v {
                    Value::Array(a) => format!("{k}[{}]", a.len()),
                    Value::String(s) if k == "error" => format!("{k}={s:?}"),
                    Value::String(s) => format!("{k}:str({})", s.len()),
                    Value::Bool(b) => format!("{k}={b}"),
                    Value::Number(n) => format!("{k}={n}"),
                    Value::Null => format!("{k}=null"),
                    Value::Object(_) => format!("{k}{{}}"),
                })
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        other => json!(other).to_string().chars().take(80).collect(),
    }
}
