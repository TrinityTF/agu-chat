//! Port of src/main/updater.js: updates from GitHub Releases (TrinityTF/agu-chat), with two channels:
//!   beta   = GitHub prereleases (0.0.1-beta.N) AND full releases, whichever is newer
//!   stable = full releases only
//! Rules and the behaviour behind them: docs/RELEASING.md.
//!
//! electron-updater did three things for the JS: pick the release for the channel, download it, install it on
//! quit. This port keeps the first as pure code (`pick_update`, `Version`) and the state machine around it as
//! `Updater`; the download and install step sits behind the `Installer` trait.
//! TODO(round 3): the installer format of the native app is decided later. Until a real `Installer` is given
//! (`NoInstaller` says it is unsupported) a found update stays in status "available" with its version and
//! notes, and nothing is downloaded.
use std::cmp::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde_json::Value;

use crate::log::Logger;
use crate::protocol::UpdaterState;
use crate::util::{Listeners, Timer};

const FIRST_CHECK_MS: u64 = 5_000; // shortly after start
const CHECK_EVERY_MS: u64 = 4 * 60 * 60_000; // then every 4 hours
const RETRY_AFTER_ERROR_MS: u64 = 15 * 60_000; // a failed background check is retried sooner than 4h
const RELEASES_URL: &str = "https://api.github.com/repos/TrinityTF/agu-chat/releases?per_page=30";

// ---------------------------------------------------------------------------------------------------------
// versions and the channel rule
// ---------------------------------------------------------------------------------------------------------

/// Semantic version, enough for `0.0.1-beta.3` style tags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Vec<String>,
}

impl Version {
    /// "v1.2.3", "1.2.3-beta.4", "1.2.3+build" -> Version. None when it is not a version.
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim().trim_start_matches('v');
        let s = s.split('+').next().unwrap_or("");
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (s, None),
        };
        let mut it = core.split('.');
        let major = it.next()?.parse().ok()?;
        let minor = it.next()?.parse().ok()?;
        let patch = it.next()?.parse().ok()?;
        if it.next().is_some() {
            return None;
        }
        let pre = match pre {
            Some(p) if !p.is_empty() && p.split('.').all(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')) => p.split('.').map(String::from).collect(),
            Some(_) => return None,
            None => Vec::new(),
        };
        Some(Version { major, minor, patch, pre })
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

impl Ord for Version {
    fn cmp(&self, o: &Self) -> Ordering {
        (self.major, self.minor, self.patch).cmp(&(o.major, o.minor, o.patch)).then_with(|| match (self.pre.is_empty(), o.pre.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater, // a release is newer than its prereleases
            (false, true) => Ordering::Less,
            (false, false) => {
                for (a, b) in self.pre.iter().zip(&o.pre) {
                    let ord = match (a.parse::<u64>(), b.parse::<u64>()) {
                        (Ok(x), Ok(y)) => x.cmp(&y),
                        (Ok(_), Err(_)) => Ordering::Less, // numeric identifiers sort before alphanumeric ones
                        (Err(_), Ok(_)) => Ordering::Greater,
                        (Err(_), Err(_)) => a.cmp(b),
                    };
                    if ord != Ordering::Equal {
                        return ord;
                    }
                }
                self.pre.len().cmp(&o.pre.len())
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

/// `/^\d+\.\d+\.\d+-/.test(version)`
pub fn is_prerelease(version: &str) -> bool {
    Version::parse(version).map(|v| v.is_prerelease()).unwrap_or(false) || {
        // the JS regex only looks at the shape, so keep that for strings Version rejects
        let b = version.as_bytes();
        let mut i = 0;
        let mut dots = 0;
        while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
            if b[i] == b'.' {
                dots += 1;
            }
            i += 1;
        }
        dots == 2 && i < b.len() && b[i] == b'-'
    }
}

fn normalize_channel(channel: &str) -> &'static str {
    if channel == "stable" {
        "stable"
    } else {
        "beta"
    }
}

/// Would an update to `version` be allowed on `channel`? (stable never takes a prerelease)
pub fn allowed_on_channel(channel: &str, version: &str) -> bool {
    channel == "beta" || !is_prerelease(version)
}

/// One GitHub release.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// without a leading "v"
    pub version: String,
    pub prerelease: bool,
    pub draft: bool,
    /// the release notes (GitHub markdown)
    pub notes: String,
    pub url: String,
}

/// The release to move to, or None. beta: the newest of all releases; stable: the newest full release. Never
/// older than (or equal to) the running version (`allowDowngrade = false`).
pub fn pick_update(releases: &[Release], current: &str, channel: &str) -> Option<Release> {
    let current = Version::parse(current)?;
    releases
        .iter()
        .filter(|r| !r.draft)
        .filter(|r| channel != "stable" || (!r.prerelease && !is_prerelease(&r.version)))
        .filter_map(|r| Version::parse(&r.version).map(|v| (v, r)))
        .filter(|(v, _)| *v > current)
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, r)| r.clone())
}

/// Release notes arrive as markdown or HTML; turn them into plain lines.
pub fn notes_to_lines(notes: &str) -> Vec<String> {
    // </p> </li> </h1>.. and <br> end a line; other tags vanish
    let mut text = String::new();
    let mut rest = notes;
    while let Some(i) = rest.find('<') {
        text.push_str(&rest[..i]);
        match rest[i..].find('>') {
            Some(j) => {
                let tag = rest[i + 1..i + j].trim().to_lowercase();
                let ends_line = tag.starts_with("br") || (tag.starts_with('/') && (tag[1..].starts_with('p') || tag[1..].starts_with("li") || (tag[1..].starts_with('h') && tag[2..].chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false))));
                if ends_line {
                    text.push('\n');
                }
                rest = &rest[i + j + 1..];
            }
            None => {
                text.push_str(&rest[i..]);
                rest = "";
            }
        }
    }
    text.push_str(rest);
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .split('\n')
        .map(|line| {
            let t = line.trim_start();
            let t = match t.chars().next() {
                Some('-' | '*' | '\u{2022}') => t[t.chars().next().unwrap().len_utf8()..].trim_start(),
                _ => t,
            };
            t.trim().to_string()
        })
        .filter(|l| !l.is_empty())
        .take(12)
        .collect()
}

fn friendly_error(message: &str, status: Option<u16>) -> String {
    let lower = message.to_lowercase();
    if status == Some(0) || ["enotfound", "etimedout", "econnreset", "econnrefused", "eai_again", "error sending request", "dns", "timed out", "connection"].iter().any(|k| lower.contains(k)) {
        return "Could not reach GitHub. Check your internet connection.".into();
    }
    if status == Some(403) || status == Some(429) || lower.contains("rate limit") || lower.contains("403") {
        return "GitHub is limiting requests right now. Try again later.".into();
    }
    crate::util::slice_utf16(message.lines().next().unwrap_or(""), 200)
}

// ---------------------------------------------------------------------------------------------------------
// the two seams: where releases come from, and what downloads / installs one
// ---------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct FeedError {
    /// HTTP status; 0 = never reached the server
    pub status: u16,
    pub message: String,
}

/// Where the releases come from (GitHub; a test supplies its own).
pub trait ReleaseFeed: Send + Sync {
    fn releases(&self) -> BoxFuture<'static, Result<Vec<Release>, FeedError>>;
}

pub struct GithubFeed;

impl ReleaseFeed for GithubFeed {
    fn releases(&self) -> BoxFuture<'static, Result<Vec<Release>, FeedError>> {
        Box::pin(async {
            let client = reqwest::Client::new();
            let res = client
                .get(RELEASES_URL)
                .header("User-Agent", "AguChat-updater (+https://github.com/TrinityTF/agu-chat)")
                .header("Accept", "application/vnd.github+json")
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .map_err(|e| FeedError { status: 0, message: e.to_string() })?;
            let status = res.status().as_u16();
            if !res.status().is_success() {
                return Err(FeedError { status, message: format!("HTTP {status}") });
            }
            let list: Value = res.json().await.map_err(|e| FeedError { status: 0, message: e.to_string() })?;
            Ok(list
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|r| {
                            Some(Release {
                                version: r["tag_name"].as_str()?.trim_start_matches('v').to_string(),
                                prerelease: r["prerelease"].as_bool().unwrap_or(false),
                                draft: r["draft"].as_bool().unwrap_or(false),
                                notes: r["body"].as_str().unwrap_or("").to_string(),
                                url: r["html_url"].as_str().unwrap_or("").to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default())
        })
    }
}

pub enum InstallError {
    /// there is no installer for this build yet: the update stays "available"
    Unsupported,
    Failed(String),
}

/// Downloading a release and installing it. TODO: the native installer format is decided later.
pub trait Installer: Send + Sync {
    fn supported(&self) -> bool;
    /// `progress` takes 0..=1
    fn download(&self, release: &Release, progress: Arc<dyn Fn(f32) + Send + Sync>) -> BoxFuture<'static, Result<(), InstallError>>;
    /// Quits into the downloaded update ("Restart to update").
    fn install(&self) -> BoxFuture<'static, Result<(), String>>;
}

pub struct NoInstaller;

impl Installer for NoInstaller {
    fn supported(&self) -> bool {
        false
    }
    fn download(&self, _release: &Release, _progress: Arc<dyn Fn(f32) + Send + Sync>) -> BoxFuture<'static, Result<(), InstallError>> {
        Box::pin(async { Err(InstallError::Unsupported) })
    }
    fn install(&self) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async { Err("Installing updates is not available in this build yet.".to_string()) })
    }
}

// ---------------------------------------------------------------------------------------------------------
// the state machine
// ---------------------------------------------------------------------------------------------------------

struct Inner {
    state: UpdaterState,
    enabled: bool, // JS: `autoUpdater` exists (a packaged build)
    in_flight_check: bool,
    recheck_queued: bool,
    download_gen: u64, // bumping it cancels the running download's result
    downloading: bool,
    last_percent: i32,
    retry_timer: Timer,
    first_timer: Timer,
    interval_timer: Timer,
}

pub struct Updater {
    inner: Mutex<Inner>,
    current_version: String,
    packaged: bool,
    feed: Arc<dyn ReleaseFeed>,
    installer: Arc<dyn Installer>,
    log: Arc<Logger>,
    pub events: Listeners<UpdaterState>,
}

impl Updater {
    pub fn new(current_version: String, packaged: bool, feed: Arc<dyn ReleaseFeed>, installer: Arc<dyn Installer>, log: Arc<Logger>) -> Arc<Self> {
        Arc::new(Updater {
            inner: Mutex::new(Inner {
                state: UpdaterState { status: "idle".into(), channel: "beta".into(), version: None, notes: Vec::new(), progress: 0.0, error: None },
                enabled: false,
                in_flight_check: false,
                recheck_queued: false,
                download_gen: 0,
                downloading: false,
                last_percent: -1,
                retry_timer: Timer::new(),
                first_timer: Timer::new(),
                interval_timer: Timer::new(),
            }),
            current_version,
            packaged,
            feed,
            installer,
            log,
            events: Listeners::new(),
        })
    }

    pub fn get_state(&self) -> UpdaterState {
        self.inner.lock().unwrap().state.clone()
    }

    fn set(&self, f: impl FnOnce(&mut UpdaterState)) {
        let st = {
            let mut g = self.inner.lock().unwrap();
            f(&mut g.state);
            g.state.clone()
        };
        self.events.emit(&st);
    }

    fn channel(&self) -> String {
        self.inner.lock().unwrap().state.channel.clone()
    }

    /// Must run inside the tokio runtime when packaged (it schedules the checks).
    pub fn init(self: &Arc<Self>, channel: &str) {
        let ch = normalize_channel(channel);
        self.inner.lock().unwrap().state.channel = ch.to_string();
        if !self.packaged {
            return self.set(|s| s.status = "dev".into());
        }
        let me = self.clone();
        let mut g = self.inner.lock().unwrap();
        g.enabled = true;
        let m1 = me.clone();
        g.first_timer.set(Duration::from_millis(FIRST_CHECK_MS), async move { m1.check() });
        let m2 = me;
        g.interval_timer.every(Duration::from_millis(CHECK_EVERY_MS), move || {
            let m = m2.clone();
            async move { m.check() }
        });
    }

    /// Switch channel (called on every settings change, so unchanged values are a no-op). Re-checks at once.
    pub fn set_channel(self: &Arc<Self>, channel: &str) {
        let next = normalize_channel(channel);
        let (same, enabled, pending_not_allowed, in_flight) = {
            let mut g = self.inner.lock().unwrap();
            if g.state.channel == next {
                (true, g.enabled, false, g.in_flight_check)
            } else {
                g.state.channel = next.to_string();
                let pending = ["downloading", "ready"].contains(&g.state.status.as_str()) && !g.state.version.as_deref().map(|v| allowed_on_channel(next, v)).unwrap_or(true);
                (false, g.enabled, pending, g.in_flight_check)
            }
        };
        if same {
            return;
        }
        if !enabled {
            return self.set(|_| {}); // dev: just remember it
        }
        // A pending prerelease must not be installed once the user asks for stable only.
        if pending_not_allowed {
            self.discard_pending();
            return self.check();
        }
        self.set(|_| {});
        if in_flight {
            self.inner.lock().unwrap().recheck_queued = true; // its result belongs to the old channel; run again afterwards
        } else {
            self.check();
        }
    }

    /// Throw away an update that is not allowed on the current channel (e.g. a beta after switching to stable).
    fn discard_pending(&self) {
        {
            let mut g = self.inner.lock().unwrap();
            g.download_gen += 1; // cancels the running download's result
            g.downloading = false;
        }
        self.set(|s| {
            s.status = "none".into();
            s.version = None;
            s.notes = Vec::new();
            s.progress = 0.0;
            s.error = None;
        });
    }

    fn schedule_retry(self: &Arc<Self>) {
        let me = self.clone();
        self.inner.lock().unwrap().retry_timer.set(Duration::from_millis(RETRY_AFTER_ERROR_MS), async move { me.check() });
    }

    /// Looks for a newer release on the current channel; a found update downloads by itself.
    pub fn check(self: &Arc<Self>) {
        {
            let mut g = self.inner.lock().unwrap();
            if !g.enabled || ["downloading", "ready"].contains(&g.state.status.as_str()) || g.in_flight_check {
                return;
            }
            g.in_flight_check = true;
        }
        let me = self.clone();
        tokio::spawn(async move {
            me.run_check().await;
            let again = {
                let mut g = me.inner.lock().unwrap();
                g.in_flight_check = false;
                std::mem::take(&mut g.recheck_queued)
            };
            if again {
                me.check();
            }
        });
    }

    async fn run_check(self: &Arc<Self>) {
        self.set(|s| {
            s.status = "checking".into();
            s.error = None;
        });
        let channel_at_start = self.channel();
        let found = match self.feed.releases().await {
            Ok(list) => Ok(pick_update(&list, &self.current_version, &channel_at_start)),
            Err(e) => Err(e),
        };
        match found {
            Err(e) => {
                self.log.log(&format!("[updater] error {} {}", e.status, e.message));
                // "No release on this channel yet" is not a failure: the repo may simply have nothing published for it.
                if e.status == 404 {
                    return self.set(|s| {
                        s.status = "none".into();
                        s.version = None;
                        s.notes = Vec::new();
                        s.progress = 0.0;
                        s.error = None;
                    });
                }
                let msg = friendly_error(&e.message, Some(e.status));
                self.set(|s| {
                    s.status = "error".into();
                    s.error = Some(msg);
                });
                self.schedule_retry();
            }
            Ok(None) => self.set(|s| {
                s.status = "none".into();
                s.version = None;
                s.notes = Vec::new();
                s.progress = 0.0;
            }),
            Ok(Some(rel)) => {
                if !allowed_on_channel(&self.channel(), &rel.version) {
                    // Channel changed while this check was running: ignore it, the queued re-check decides.
                    return self.set(|s| {
                        s.status = "none".into();
                        s.version = None;
                        s.notes = Vec::new();
                        s.progress = 0.0;
                    });
                }
                self.on_update_available(rel).await;
            }
        }
    }

    async fn on_update_available(self: &Arc<Self>, rel: Release) {
        let supported = self.installer.supported();
        let (version, notes) = (rel.version.clone(), notes_to_lines(&rel.notes));
        self.set(|s| {
            s.status = if supported { "downloading" } else { "available" }.into();
            s.version = Some(version.clone());
            s.notes = notes;
            s.progress = 0.0;
            s.error = None;
        });
        if !supported {
            return; // TODO: nothing to download until the native installer exists
        }
        let gen = {
            let mut g = self.inner.lock().unwrap();
            g.download_gen += 1;
            g.downloading = true;
            g.last_percent = -1;
            g.download_gen
        };
        let me = self.clone();
        let progress: Arc<dyn Fn(f32) + Send + Sync> = Arc::new(move |p| {
            let percent = (p * 100.0).floor() as i32;
            {
                let mut g = me.inner.lock().unwrap();
                if g.download_gen != gen || percent == g.last_percent {
                    return;
                }
                g.last_percent = percent;
            }
            me.set(|s| {
                s.status = "downloading".into();
                s.progress = percent as f32 / 100.0;
            });
        });
        let res = self.installer.download(&rel, progress).await;
        {
            let mut g = self.inner.lock().unwrap();
            if g.download_gen != gen {
                return; // deliberate cancellation
            }
            g.downloading = false;
        }
        match res {
            Ok(()) => {
                if !allowed_on_channel(&self.channel(), &rel.version) {
                    return self.discard_pending();
                }
                self.set(|s| {
                    s.status = "ready".into();
                    s.version = Some(rel.version.clone());
                    s.progress = 1.0;
                    s.error = None;
                });
            }
            Err(InstallError::Unsupported) => self.set(|s| s.status = "available".into()),
            Err(InstallError::Failed(m)) => {
                self.log.log(&format!("[updater] download failed {m}"));
                let msg = friendly_error(&m, None);
                self.set(|s| {
                    s.status = "error".into();
                    s.error = Some(msg);
                });
                self.schedule_retry();
            }
        }
    }

    /// Restarts the app into the downloaded update ("Restart to update").
    pub fn install(&self) -> BoxFuture<'static, Result<(), String>> {
        let ready = self.inner.lock().unwrap().enabled && self.get_state().status == "ready";
        if !ready {
            return Box::pin(async { Err("No update is ready to install.".to_string()) });
        }
        self.installer.install()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(v: &str, pre: bool) -> Release {
        Release { version: v.into(), prerelease: pre, draft: false, notes: String::new(), url: String::new() }
    }

    #[test]
    fn versions_order_like_semver() {
        let v = |s: &str| Version::parse(s).unwrap();
        assert!(v("0.0.1") > v("0.0.1-beta.9"));
        assert!(v("0.0.1-beta.10") > v("0.0.1-beta.2"));
        assert!(v("0.0.2-beta.1") > v("0.0.1"));
        assert!(v("v1.0.0") > v("0.9.9"));
        assert!(v("1.0.0-beta") > v("1.0.0-alpha.1"));
        assert!(Version::parse("nope").is_none());
    }

    #[test]
    fn the_channel_decides_which_release_counts() {
        let all = vec![rel("0.0.1-beta.1", true), rel("0.0.1-beta.2", true), rel("0.0.1", false), rel("0.0.2-beta.1", true)];
        // beta: whichever is newest, prerelease or not
        assert_eq!(pick_update(&all, "0.0.1-beta.1", "beta").unwrap().version, "0.0.2-beta.1");
        // stable: full releases only
        assert_eq!(pick_update(&all, "0.0.1-beta.1", "stable").unwrap().version, "0.0.1");
        // never a downgrade, never the same version
        assert!(pick_update(&all, "0.0.1", "stable").is_none());
        assert!(pick_update(&all, "0.0.2-beta.1", "beta").is_none());
        // drafts never count
        let mut d = rel("9.9.9", false);
        d.draft = true;
        assert!(pick_update(&[d], "0.0.1", "beta").is_none());
        assert!(allowed_on_channel("beta", "0.0.1-beta.1"));
        assert!(!allowed_on_channel("stable", "0.0.1-beta.1"));
        assert!(allowed_on_channel("stable", "0.0.1"));
    }

    #[test]
    fn notes_become_plain_lines() {
        let n = "<h2>What's new</h2><ul><li>Faster &amp; smaller</li><li>- Fixed it</li></ul>\n* Markdown bullet\n\n";
        assert_eq!(notes_to_lines(n), vec!["What's new", "Faster & smaller", "Fixed it", "Markdown bullet"]);
    }

    struct FakeFeed(Vec<Release>);
    impl ReleaseFeed for FakeFeed {
        fn releases(&self) -> BoxFuture<'static, Result<Vec<Release>, FeedError>> {
            let v = self.0.clone();
            Box::pin(async move { Ok(v) })
        }
    }

    #[tokio::test]
    async fn a_found_update_stays_available_without_an_installer_and_follows_the_channel() {
        let dir = std::env::temp_dir();
        let log = Arc::new(Logger::new(&dir, true));
        let feed = Arc::new(FakeFeed(vec![rel("0.0.1-beta.2", true), rel("0.0.1", false)]));
        let u = Updater::new("0.0.1-beta.1".into(), true, feed, Arc::new(NoInstaller), log);
        u.inner.lock().unwrap().enabled = true;
        u.inner.lock().unwrap().state.channel = "beta".into();
        u.check();
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if u.get_state().status == "available" {
                break;
            }
        }
        let s = u.get_state();
        assert_eq!(s.status, "available");
        assert_eq!(s.version.as_deref(), Some("0.0.1"));
        u.inner.lock().unwrap().state.status = "idle".into();
        u.set_channel("stable");
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if u.get_state().status == "available" && u.get_state().channel == "stable" {
                break;
            }
        }
        assert_eq!(u.get_state().version.as_deref(), Some("0.0.1"));
        assert!(u.install().await.is_err());
    }
}
