//! Updates from GitHub Releases (TrinityTF/agu-chat), with two channels:
//!   beta   = every release: prereleases (any `-suffix` version, or any 0.x version) AND full releases
//!   stable = full releases only (>= 1.0.0, no suffix)
//! Rules and the behaviour behind them: docs/GPUI-PORT.md "Installer and updates".
//!
//! This file does three things: pick the release for the channel (`pick_update`, `Version`, pure code), run the
//! state machine around it (`Updater`), and download and install it on request. The download and install step
//! sits behind the `Installer` trait.
//!
//! Release scheme:
//!   tag      `v<version>`                              e.g. `v0.1.0-beta`, `v0.1.1`, `v1.0.0`
//!   assets   `Agu-Chat-Setup-<version>-x64.exe` and `<same>.sha256`
//!   Releases of the old Electron app (tags `v0.0.1-beta.N`, installers `Agu-Chat-Setup-<ver>.exe` without `-x64`,
//!   plus `latest.yml`) are never offered: they lack the exact installer asset, and versions below 0.1.0 are
//!   skipped outright (`MIN_VERSION`).
//! `NativeInstaller` downloads the installer asset, checks its size and sha256, and `install` runs it silently
//! (`/S /R`) and quits the app. `NoInstaller` (a found update stays "available") is for builds without one.
use std::cmp::Ordering;
use std::path::PathBuf;
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
const RELEASES_URL: &str = "https://api.github.com/repos/TrinityTF/agu-chat/releases";
/// The feed pages through up to this many pages of 100 releases (the old Electron app's many beta releases
/// are still in the list, below the new ones).
const FEED_PAGES: u32 = 3;
/// Tag prefix of releases.
pub const TAG_PREFIX: &str = "v";
/// The first release of this app. Anything older is the Electron app and is never an update.
const MIN_VERSION: (u64, u64, u64) = (0, 1, 0);

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

    /// A `-suffix` version. (Before 1.0.0 every release counts as a prerelease too: see `Release::is_pre`.)
    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }

    fn at_least_min(&self) -> bool {
        (self.major, self.minor, self.patch) >= MIN_VERSION
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

/// A version that counts as a prerelease for the channels: it has a `-suffix`, or its major is 0.
fn counts_as_pre(version: &str) -> bool {
    is_prerelease(version) || Version::parse(version).map(|v| v.major == 0).unwrap_or(false)
}

/// Stable only means "full releases" once the app itself is on a full-release line. Before 1.0.0 every release
/// is a prerelease, so a stable user on 0.x would never be offered anything; while the CURRENT version's major
/// is 0, stable behaves like beta. From 1.0.0 on the channels differ as documented.
fn stable_filters(channel: &str, current: &str) -> bool {
    channel == "stable" && Version::parse(current).map(|v| v.major > 0).unwrap_or(true)
}

/// Would an update to `version` be allowed on `channel`, for an app running `current`? (stable never takes a
/// prerelease, except while `current` is still a 0.x version)
pub fn allowed_on_channel(channel: &str, current: &str, version: &str) -> bool {
    !stable_filters(channel, current) || !counts_as_pre(version)
}

/// `Agu-Chat-Setup-<version>-x64.exe`: the one installer asset the updater downloads.
#[cfg(not(target_os = "macos"))]
pub fn installer_asset_name(version: &str) -> String {
    format!("Agu-Chat-Setup-{version}-x64.exe")
}

/// macOS: the zipped app bundle (`Agu-Chat-<version>-mac.zip`); only releases that carry it are offered.
#[cfg(target_os = "macos")]
pub fn installer_asset_name(version: &str) -> String {
    format!("Agu-Chat-{version}-mac.zip")
}

/// A file attached to a release.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    /// GitHub's `sha256:<hex>` digest of the file, when it reports one
    pub digest: Option<String>,
}

/// One GitHub release.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Release {
    /// the tag without the leading `v`
    pub version: String,
    pub prerelease: bool,
    pub draft: bool,
    /// the release notes (GitHub markdown)
    pub notes: String,
    pub url: String,
    pub assets: Vec<Asset>,
}

impl Release {
    pub fn installer(&self) -> Option<&Asset> {
        let want = installer_asset_name(&self.version);
        self.assets.iter().find(|a| a.name == want)
    }
}

/// GitHub's releases list -> the releases in it. Old Electron releases (`v0.0.1-beta.9`: below 0.1.0 and without
/// the `-x64` installer), other tags and releases that do not (yet) carry their installer are dropped: a
/// half-uploaded release is never offered.
pub fn parse_releases(list: &Value) -> Vec<Release> {
    list.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    let version = r["tag_name"].as_str()?.strip_prefix(TAG_PREFIX)?.to_string();
                    if !Version::parse(&version)?.at_least_min() {
                        return None;
                    }
                    let assets = r["assets"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| {
                                    Some(Asset {
                                        name: x["name"].as_str()?.to_string(),
                                        url: x["browser_download_url"].as_str()?.to_string(),
                                        size: x["size"].as_u64().unwrap_or(0),
                                        digest: x["digest"].as_str().map(String::from),
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let rel = Release {
                        version,
                        prerelease: r["prerelease"].as_bool().unwrap_or(false),
                        draft: r["draft"].as_bool().unwrap_or(false),
                        notes: r["body"].as_str().unwrap_or("").to_string(),
                        url: r["html_url"].as_str().unwrap_or("").to_string(),
                        assets,
                    };
                    rel.installer().is_some().then_some(rel)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The release to move to, or None. beta: the newest of all releases; stable: the newest full release (but see
/// `stable_filters`: while the running version is 0.x, stable takes prereleases too). Never older than (or equal
/// to) the running version.
pub fn pick_update(releases: &[Release], current: &str, channel: &str) -> Option<Release> {
    let running = Version::parse(current)?;
    releases
        .iter()
        .filter(|r| !r.draft)
        .filter(|r| !stable_filters(channel, current) || (!r.prerelease && !counts_as_pre(&r.version)))
        .filter_map(|r| Version::parse(&r.version).map(|v| (v, r)))
        .filter(|(v, _)| *v > running)
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

/// The releases endpoint. `AGU_RELEASES_URL` points it at a local fake for tests, and only when it is a
/// loopback address (an environment variable must not be able to redirect real updates).
fn releases_url() -> String {
    match std::env::var("AGU_RELEASES_URL") {
        Ok(u) if u.starts_with("http://127.0.0.1") || u.starts_with("http://localhost") => u,
        _ => RELEASES_URL.to_string(),
    }
}

impl ReleaseFeed for GithubFeed {
    fn releases(&self) -> BoxFuture<'static, Result<Vec<Release>, FeedError>> {
        Box::pin(async {
            let client = reqwest::Client::new();
            let mut out: Vec<Release> = Vec::new();
            for page in 1..=FEED_PAGES {
                let res = client
                    .get(format!("{}?per_page=100&page={page}", releases_url()))
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
                let n = list.as_array().map(|a| a.len()).unwrap_or(0);
                out.extend(parse_releases(&list));
                if n < 100 {
                    break;
                }
            }
            Ok(out)
        })
    }
}

pub enum InstallError {
    /// there is no installer for this build yet: the update stays "available"
    Unsupported,
    Failed(String),
}

/// Downloading a release and installing it.
pub trait Installer: Send + Sync {
    fn supported(&self) -> bool;
    /// `progress` takes 0..=1
    fn download(&self, release: &Release, progress: Arc<dyn Fn(f32) + Send + Sync>) -> BoxFuture<'static, Result<(), InstallError>>;
    /// Quits into the downloaded update ("Restart to update").
    fn install(&self) -> BoxFuture<'static, Result<(), String>>;
    /// The app is quitting with a downloaded update: install it without starting the new version.
    fn install_on_exit(&self) {}
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

/// Called by `NativeInstaller::install` once the installer runs: the UI quits the app (flushing its files first).
pub type QuitHook = Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>;

/// The update of the native app, from the release's assets (see the module docs and docs/GPUI-PORT.md). The download
/// is size- and sha256-checked before it counts as ready.
///   Windows: `Agu-Chat-Setup-<version>-x64.exe`, run silently (`/S`, plus `/R` to start the new version).
///   macOS:   `Agu-Chat-<version>-mac.zip`, unpacked and signature-checked when downloaded; installing swaps the
///            running `Agu Chat.app` for the new one (a small shell script that waits for this process to exit).
pub struct NativeInstaller {
    dir: PathBuf,
    client: reqwest::Client,
    /// the verified installer of the last finished download
    ready: Arc<Mutex<Option<PathBuf>>>,
    quit: QuitHook,
    log: Arc<Logger>,
}

impl NativeInstaller {
    pub fn new(dir: PathBuf, quit: QuitHook, log: Arc<Logger>) -> Self {
        NativeInstaller { dir, client: reqwest::Client::new(), ready: Arc::new(Mutex::new(None)), quit, log }
    }
}

/// `sha256:<hex>` -> hex
fn digest_hex(d: &str) -> Option<String> {
    let h = d.strip_prefix("sha256:")?.trim().to_lowercase();
    (h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())).then_some(h)
}

/// First word of a `<hex>  <name>` sidecar file (what `sha256sum` and build-installer.ps1 write).
fn sidecar_hex(text: &str) -> Option<String> {
    let h = text.split_whitespace().next()?.to_lowercase();
    (h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())).then_some(h)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Installer for NativeInstaller {
    fn supported(&self) -> bool {
        cfg!(any(windows, target_os = "macos"))
    }

    fn download(&self, release: &Release, progress: Arc<dyn Fn(f32) + Send + Sync>) -> BoxFuture<'static, Result<(), InstallError>> {
        let (dir, client, ready, log, release) = (self.dir.clone(), self.client.clone(), self.ready.clone(), self.log.clone(), release.clone());
        Box::pin(async move {
            use futures_util::StreamExt;
            use sha2::{Digest, Sha256};
            use std::io::Write;
            let fail = |m: String| InstallError::Failed(m);
            let asset = release.installer().ok_or_else(|| fail("This release has no installer for this app.".into()))?.clone();
            // The name goes into a path: only the exact asset name we build ourselves is accepted.
            if asset.name != installer_asset_name(&release.version) || asset.name.contains(['/', '\\']) {
                return Err(fail("Unexpected installer name.".into()));
            }
            *ready.lock().unwrap() = None;
            std::fs::create_dir_all(&dir).map_err(|e| fail(e.to_string()))?;
            // older downloads are leftovers
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
                }
            }

            // the expected hash: GitHub's own digest, else the .sha256 file next to the installer
            let mut expected = asset.digest.as_deref().and_then(digest_hex);
            if expected.is_none() {
                if let Some(side) = release.assets.iter().find(|a| a.name == format!("{}.sha256", asset.name)) {
                    if let Ok(r) = client.get(&side.url).header("User-Agent", "AguChat-updater").timeout(Duration::from_secs(30)).send().await {
                        if let Ok(t) = r.text().await {
                            expected = sidecar_hex(&t);
                        }
                    }
                }
            }
            if expected.is_none() {
                log.log("[updater] no sha256 published for the installer; checking the size only");
            }

            let part = dir.join(format!("{}.part", asset.name));
            let res = client.get(&asset.url).header("User-Agent", "AguChat-updater").send().await.and_then(|r| r.error_for_status()).map_err(|e| fail(e.to_string()))?;
            let total = res.content_length().unwrap_or(asset.size).max(1);
            let mut file = std::fs::File::create(&part).map_err(|e| fail(e.to_string()))?;
            let mut hasher = Sha256::new();
            let mut got: u64 = 0;
            let mut stream = res.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| fail(e.to_string()))?;
                hasher.update(&chunk);
                file.write_all(&chunk).map_err(|e| fail(e.to_string()))?;
                got += chunk.len() as u64;
                progress((got as f32 / total as f32).min(1.0));
            }
            drop(file);
            if (asset.size != 0 && got != asset.size) || got == 0 {
                let _ = std::fs::remove_file(&part);
                return Err(fail("The download is incomplete. Try again.".into()));
            }
            if let Some(want) = expected {
                if hex(&hasher.finalize()) != want {
                    let _ = std::fs::remove_file(&part);
                    return Err(fail("The downloaded update did not match its checksum. Try again later.".into()));
                }
            }
            let done = dir.join(&asset.name);
            std::fs::rename(&part, &done).map_err(|e| fail(e.to_string()))?;
            #[cfg(target_os = "macos")]
            let done = mac::unpack(&done, &dir).map_err(fail)?;
            *ready.lock().unwrap() = Some(done);
            Ok(())
        })
    }

    fn install(&self) -> BoxFuture<'static, Result<(), String>> {
        let (ready, quit, log) = (self.ready.clone(), self.quit.clone(), self.log.clone());
        Box::pin(async move {
            match take_and_launch(&ready, true, &log) {
                Ok(()) => {
                    log.log("[updater] update started, quitting");
                    let hook = quit.lock().unwrap().clone();
                    match hook {
                        Some(h) => h(),
                        None => std::process::exit(0),
                    }
                    Ok(())
                }
                // macOS: an app that cannot replace itself (not in a folder it may write to, or started straight
                // from the zip) is updated by hand from the release page instead
                #[cfg(target_os = "macos")]
                Err(e) => {
                    log.log(&format!("[updater] macOS: {e}; opening the release page"));
                    crate::backend::shell_open(RELEASES_PAGE);
                    Ok(())
                }
                #[cfg(not(target_os = "macos"))]
                Err(e) => Err(e),
            }
        })
    }

    fn install_on_exit(&self) {
        match take_and_launch(&self.ready, false, &self.log) {
            Ok(()) => self.log.log("[updater] installing the downloaded update on exit"),
            Err(e) => self.log.log(&format!("[updater] update not installed on exit: {e}")),
        }
    }
}

#[cfg(target_os = "macos")]
const RELEASES_PAGE: &str = "https://github.com/TrinityTF/agu-chat/releases/latest";

/// Starts the ready update once: the slot is emptied, and put back if starting fails.
fn take_and_launch(ready: &Mutex<Option<PathBuf>>, relaunch: bool, log: &Logger) -> Result<(), String> {
    let path = ready.lock().unwrap().take().ok_or_else(|| "No update is ready to install.".to_string())?;
    match launch_update(&path, relaunch, log) {
        Ok(()) => Ok(()),
        Err(e) => {
            *ready.lock().unwrap() = Some(path);
            Err(e)
        }
    }
}

#[cfg(windows)]
fn launch_update(path: &std::path::Path, relaunch: bool, _log: &Logger) -> Result<(), String> {
    if !path.is_file() {
        return Err("The downloaded update is gone. Check for updates again.".into());
    }
    // Silent install; /R starts the new copy afterwards. The installer waits for this process to let go of
    // "Agu Chat.exe", so the app only has to quit right after starting it.
    let mut cmd = std::process::Command::new(path);
    cmd.arg("/S");
    if relaunch {
        cmd.arg("/R");
    }
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0000_0008 | 0x0000_0200); // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
    cmd.spawn().map_err(|e| format!("Could not start the installer: {e}"))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn launch_update(new_app: &std::path::Path, relaunch: bool, log: &Logger) -> Result<(), String> {
    mac::launch(new_app, relaunch, log)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn launch_update(_: &std::path::Path, _: bool, _: &Logger) -> Result<(), String> {
    Err("Installing updates is not available on this system yet.".into())
}

/// `.../Agu Chat.app/Contents/MacOS/Agu Chat` -> `.../Agu Chat.app` (None when the program is not inside an app bundle).
pub fn app_bundle_of(exe: &std::path::Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    (macos.file_name()? == "MacOS" && contents.file_name()? == "Contents" && app.extension()? == "app").then(|| app.to_path_buf())
}

/// Swaps the app bundle once the app has quit: `$0 <pid> <new .app> <installed .app> <1 = open it afterwards>`.
/// The new bundle is copied next to the old one first, so the swap itself is two renames in the same folder, and
/// the old bundle is put back if the second rename fails. Waits at most 30 s for the app to exit.
pub const MAC_SWAP_SCRIPT: &str = r#"pid="$1"; new="$2"; app="$3"; relaunch="$4"
i=0
while kill -0 "$pid" 2>/dev/null && [ "$i" -lt 300 ]; do sleep 0.1; i=$((i+1)); done
tmp="$app.updating"; old="$app.old"
rm -rf "$tmp" "$old"
ditto "$new" "$tmp" || { rm -rf "$tmp"; exit 1; }
xattr -dr com.apple.quarantine "$tmp" 2>/dev/null
if mv "$app" "$old"; then
  if mv "$tmp" "$app"; then rm -rf "$old"; else mv "$old" "$app"; fi
fi
rm -rf "$tmp" "$new"
if [ "$relaunch" = 1 ]; then open "$app"; fi
"#;

#[cfg(target_os = "macos")]
mod mac {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    use crate::log::Logger;

    /// Unpacks the downloaded zip into `<dir>/staged` and checks the app inside (its program is there and its
    /// signature is intact). The zip is removed; the unpacked `Agu Chat.app` is what gets installed.
    pub fn unpack(zip: &Path, dir: &Path) -> Result<PathBuf, String> {
        let staged = dir.join("staged");
        let _ = std::fs::remove_dir_all(&staged);
        let ok = Command::new("/usr/bin/ditto").args(["-x", "-k"]).arg(zip).arg(&staged).status().map(|s| s.success()).unwrap_or(false);
        if !ok {
            return Err("Could not unpack the update.".into());
        }
        let app = staged.join("Agu Chat.app");
        if !app.join("Contents/MacOS/Agu Chat").is_file() {
            return Err("The update has no Agu Chat app inside.".into());
        }
        let signed = Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(&app)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !signed {
            return Err("The update's signature did not check out.".into());
        }
        let _ = std::fs::remove_file(zip);
        Ok(app)
    }

    /// Can this process create files in `dir`? (The installed app's folder; false for an app started from a
    /// read-only place, such as the copy macOS runs when the app was opened straight from Downloads.)
    fn writable(dir: &Path) -> bool {
        let probe = dir.join(format!(".agu-chat-update-{}", std::process::id()));
        let ok = std::fs::write(&probe, b"").is_ok();
        let _ = std::fs::remove_file(&probe);
        ok
    }

    pub fn launch(new_app: &Path, relaunch: bool, log: &Logger) -> Result<(), String> {
        if !new_app.join("Contents/MacOS/Agu Chat").is_file() {
            return Err("The downloaded update is gone. Check for updates again.".into());
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let app = super::app_bundle_of(&exe).ok_or("Agu Chat is not running from its app bundle")?;
        let folder = app.parent().ok_or("Agu Chat's folder is unknown")?;
        if !writable(folder) {
            return Err(format!("cannot write to {}", folder.display()));
        }
        use std::os::unix::process::CommandExt;
        Command::new("/bin/sh")
            .arg("-c")
            .arg(super::MAC_SWAP_SCRIPT)
            .arg("agu-chat-update")
            .arg(std::process::id().to_string())
            .arg(new_app)
            .arg(&app)
            .arg(if relaunch { "1" } else { "0" })
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0) // not taken down with the app
            .spawn()
            .map_err(|e| format!("Could not start the update: {e}"))?;
        log.log(&format!("[updater] macOS: replacing {} after exit", app.display()));
        Ok(())
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
                let pending = ["downloading", "ready"].contains(&g.state.status.as_str()) && !g.state.version.as_deref().map(|v| allowed_on_channel(next, &self.current_version, v)).unwrap_or(true);
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
                if !allowed_on_channel(&self.channel(), &self.current_version, &rel.version) {
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
                if !allowed_on_channel(&self.channel(), &self.current_version, &rel.version) {
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

    /// The app is quitting: a downloaded update is installed now (without starting it), like "Restart to update"
    /// minus the restart.
    pub fn install_on_exit(&self) {
        let ready = self.inner.lock().unwrap().enabled && self.get_state().status == "ready";
        if ready {
            self.installer.install_on_exit();
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
        Release { version: v.into(), prerelease: pre, ..Default::default() }
    }

    /// A GitHub releases list: new releases (with and without their installer) among old Electron ones.
    fn github_list() -> Value {
        let asset = |name: &str, size: u64| serde_json::json!({ "name": name, "browser_download_url": format!("https://example.invalid/{name}"), "size": size });
        serde_json::json!([
            // old Electron releases: no -x64 installer, version below 0.1.0
            { "tag_name": "v0.0.1-beta.40", "prerelease": true, "draft": false, "body": "electron", "html_url": "u1",
              "assets": [asset("Agu-Chat-Setup-0.0.1-beta.40.exe", 5), asset("latest.yml", 1)] },
            { "tag_name": "v0.0.1", "prerelease": false, "draft": false, "body": "electron stable", "html_url": "u7", "assets": [] },
            // even with an -x64 installer, a version below 0.1.0 is never offered
            { "tag_name": "v0.0.9", "prerelease": false, "draft": false, "body": "", "html_url": "u8", "assets": [asset(&installer_asset_name("0.0.9"), 9)] },
            { "tag_name": "v0.1.0-beta", "prerelease": true, "draft": false, "body": "- first beta", "html_url": "u2",
              "assets": [asset(&installer_asset_name("0.1.0-beta"), 9), asset(&format!("{}.sha256", installer_asset_name("0.1.0-beta")), 80)] },
            { "tag_name": "v0.1.1", "prerelease": true, "draft": false, "body": "", "html_url": "u3",
              "assets": [asset(&installer_asset_name("0.1.1"), 9)] },
            // no installer attached (yet): never offered
            { "tag_name": "v0.2.0", "prerelease": true, "draft": false, "body": "", "html_url": "u4", "assets": [asset("notes.txt", 1)] },
            // the Electron-style installer name on a new-looking tag: not ours
            { "tag_name": "v0.3.0", "prerelease": true, "draft": false, "body": "", "html_url": "u5", "assets": [asset("Agu-Chat-Setup-0.3.0.exe", 1)] },
            { "tag_name": "vnope", "prerelease": false, "draft": false, "body": "", "html_url": "u6", "assets": [asset(&installer_asset_name("nope"), 1)] },
            { "tag_name": "native-v0.4.0", "prerelease": false, "draft": false, "body": "", "html_url": "u9", "assets": [asset(&installer_asset_name("0.4.0"), 1)] }
        ])
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
        assert!(v("0.1.0-beta") < v("0.1.0") && v("0.1.0") < v("0.1.1") && v("0.1.1") < v("0.2.0") && v("0.2.0") < v("1.0.0"));
    }

    #[test]
    fn the_channel_decides_which_release_counts() {
        // on a 1.x install: stable takes full releases only
        let all = vec![rel("1.0.0-beta", true), rel("1.0.0-beta.2", true), rel("1.0.0", false), rel("1.1.0-beta", true)];
        assert_eq!(pick_update(&all, "1.0.0-beta", "beta").unwrap().version, "1.1.0-beta");
        assert_eq!(pick_update(&all, "0.9.0", "stable").unwrap().version, "1.1.0-beta"); // a 0.x install: stable acts like beta
        assert_eq!(pick_update(&all, "1.0.0-beta", "stable").unwrap().version, "1.0.0");
        // never a downgrade, never the same version
        assert!(pick_update(&all, "1.0.0", "stable").is_none());
        assert!(pick_update(&all, "1.1.0-beta", "beta").is_none());
        // drafts never count
        let mut d = rel("9.9.9", false);
        d.draft = true;
        assert!(pick_update(&[d], "1.0.0", "beta").is_none());
        assert!(allowed_on_channel("beta", "1.0.0", "1.1.0-beta"));
        assert!(!allowed_on_channel("stable", "1.0.0", "1.1.0-beta"));
        assert!(allowed_on_channel("stable", "1.0.0", "1.1.0"));
    }

    #[test]
    fn stable_acts_like_beta_while_the_running_version_is_0x() {
        // every 0.x release is a prerelease, so stable users on 0.x would otherwise never update
        let all = vec![rel("0.1.1", true), rel("0.2.0", true), rel("1.0.0", false)];
        assert_eq!(pick_update(&all[..2], "0.1.0-beta", "stable").unwrap().version, "0.2.0");
        assert_eq!(pick_update(&all, "0.1.0-beta", "stable").unwrap().version, "1.0.0");
        assert!(allowed_on_channel("stable", "0.1.0-beta", "0.2.0"));
        // from 1.0.0 on stable is full releases only again
        let later = vec![rel("1.0.1-beta", true), rel("1.0.1", false), rel("1.1.0-beta", true)];
        assert_eq!(pick_update(&later, "1.0.0", "stable").unwrap().version, "1.0.1");
        assert!(pick_update(&later[..1], "1.0.0", "stable").is_none());
    }

    #[test]
    fn only_current_releases_with_their_installer_are_read() {
        let rels = parse_releases(&github_list());
        let versions: Vec<_> = rels.iter().map(|r| r.version.as_str()).collect();
        assert_eq!(versions, vec!["0.1.0-beta", "0.1.1"]);
        assert_eq!(rels[0].installer().unwrap().name, installer_asset_name("0.1.0-beta"));
        assert!(rels[0].prerelease);
        // the old Electron updater's files are never ours
        assert!(rels.iter().all(|r| r.assets.iter().all(|a| a.name != "latest.yml")));
        #[cfg(not(target_os = "macos"))]
        assert_eq!(installer_asset_name("1.2.3"), "Agu-Chat-Setup-1.2.3-x64.exe");
        #[cfg(target_os = "macos")]
        assert_eq!(installer_asset_name("1.2.3"), "Agu-Chat-1.2.3-mac.zip");
    }

    #[test]
    fn channels_over_a_mixed_feed() {
        let rels = parse_releases(&github_list());
        // anyone below the first release is offered the newest one
        assert_eq!(pick_update(&rels, "0.0.9", "beta").unwrap().version, "0.1.1");
        assert_eq!(pick_update(&rels[..1], "0.0.9", "stable").unwrap().version, "0.1.0-beta");
        assert!(pick_update(&rels[..1], "0.1.0-beta", "beta").is_none());
        assert_eq!(pick_update(&rels, "0.1.0-beta", "stable").unwrap().version, "0.1.1");
        assert!(pick_update(&rels, "0.1.1", "beta").is_none());
    }

    #[test]
    fn checksums_are_read_from_either_place() {
        let h = "a".repeat(64);
        assert_eq!(digest_hex(&format!("sha256:{h}")), Some(h.clone()));
        assert_eq!(digest_hex("sha1:abc"), None);
        assert_eq!(sidecar_hex(&format!("{}  Agu-Chat-Setup-1-x64.exe
", h.to_uppercase())), Some(h));
        assert_eq!(sidecar_hex("not a hash"), None);
    }

    #[test]
    fn friendly_errors() {
        assert!(friendly_error("error sending request", Some(0)).contains("internet"));
        assert!(friendly_error("HTTP 403", Some(403)).contains("limiting"));
        assert_eq!(friendly_error("boom
second line", None), "boom");
    }

    /// One-shot HTTP server for the download test: answers every GET with `body`.
    fn serve(body: Vec<u8>) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut s = stream;
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf);
                let _ = write!(s, "HTTP/1.1 200 OK{0}Content-Length: {1}{0}Connection: close{0}{0}", "
", body.len());
                let _ = s.write_all(&body);
            }
        });
        format!("http://{addr}/x")
    }

    #[tokio::test]
    async fn the_installer_is_downloaded_and_verified() {
        use sha2::{Digest, Sha256};
        let dir = std::env::temp_dir().join(format!("agu-upd-{}", crate::util::uuid_v4()));
        let log = Arc::new(Logger::new(&dir, true));
        #[cfg(not(target_os = "macos"))]
        let body = b"MZ pretend installer bytes".to_vec();
        #[cfg(target_os = "macos")]
        let body = mac_test_zip(&dir);
        let url = serve(body.clone());
        let good: String = Sha256::digest(&body).iter().map(|b| format!("{b:02x}")).collect();
        let mk = |digest: Option<String>, size: u64| Release {
            version: "0.1.0".into(),
            assets: vec![Asset { name: installer_asset_name("0.1.0"), url: url.clone(), size, digest }],
            ..Default::default()
        };
        let inst = NativeInstaller::new(dir.join("updates"), Arc::new(Mutex::new(None)), log);
        let seen = Arc::new(Mutex::new(0.0f32));
        let s2 = seen.clone();
        let progress: Arc<dyn Fn(f32) + Send + Sync> = Arc::new(move |p| *s2.lock().unwrap() = p);

        // right size and hash: ready, and the file is there under its final name
        assert!(inst.download(&mk(Some(format!("sha256:{good}")), body.len() as u64), progress.clone()).await.is_ok());
        assert_eq!(*seen.lock().unwrap(), 1.0);
        let done = dir.join("updates").join(installer_asset_name("0.1.0"));
        #[cfg(not(target_os = "macos"))]
        assert_eq!(std::fs::read(&done).unwrap(), body);
        // macOS: the zip is unpacked and its signed app is what is ready
        #[cfg(target_os = "macos")]
        {
            let app = dir.join("updates/staged/Agu Chat.app");
            assert_eq!(inst.ready.lock().unwrap().clone(), Some(app.clone()));
            assert_eq!(std::fs::read_to_string(app.join("Contents/MacOS/Agu Chat")).unwrap(), "#!/bin/sh\n");
            assert!(!done.exists());
        }
        // wrong hash: refused, nothing left behind
        let bad = format!("sha256:{}", "0".repeat(64));
        assert!(matches!(inst.download(&mk(Some(bad), body.len() as u64), progress.clone()).await, Err(InstallError::Failed(m)) if m.contains("checksum")));
        assert!(!done.exists());
        assert!(inst.ready.lock().unwrap().is_none());
        // wrong size: refused
        assert!(matches!(inst.download(&mk(None, 3), progress.clone()).await, Err(InstallError::Failed(m)) if m.contains("incomplete")));
        // no installer asset at all
        assert!(inst.download(&Release { version: "0.1.0".into(), ..Default::default() }, progress).await.is_err());
        // nothing verified is ready: install refuses (macOS install only opens the release page)
        #[cfg(not(target_os = "macos"))]
        assert!(inst.install().await.is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A zipped, ad-hoc signed `Agu Chat.app` like the release's (bundle.sh), built with the system tools.
    #[cfg(target_os = "macos")]
    fn mac_test_zip(dir: &std::path::Path) -> Vec<u8> {
        let src = dir.join("zipsrc");
        let app = src.join("Agu Chat.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::write(app.join("Contents/MacOS/Agu Chat"), "#!/bin/sh\n").unwrap();
        std::fs::write(
            app.join("Contents/Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleExecutable</key><string>Agu Chat</string><key>CFBundleIdentifier</key><string>lol.agu.chat.test</string></dict></plist>"#,
        )
        .unwrap();
        let run = |cmd: &str, args: &[&std::ffi::OsStr]| assert!(std::process::Command::new(cmd).args(args).status().unwrap().success(), "{cmd} failed");
        run("/usr/bin/codesign", &["--force".as_ref(), "--deep".as_ref(), "--sign".as_ref(), "-".as_ref(), app.as_os_str()]);
        let zip = src.join("app.zip");
        run("/usr/bin/ditto", &["-c".as_ref(), "-k".as_ref(), "--keepParent".as_ref(), app.as_os_str(), zip.as_os_str()]);
        std::fs::read(&zip).unwrap()
    }

    #[test]
    fn the_app_bundle_is_found_from_its_program() {
        let exe = std::path::Path::new("/Applications/Agu Chat.app/Contents/MacOS/Agu Chat");
        assert_eq!(app_bundle_of(exe), Some(PathBuf::from("/Applications/Agu Chat.app")));
        assert_eq!(app_bundle_of(std::path::Path::new("/Users/me/dev/target/release/agu-chat-gpui")), None);
        assert_eq!(app_bundle_of(std::path::Path::new("/Applications/Agu Chat/Contents/MacOS/Agu Chat")), None);
    }

    /// The real swap script on a fake installed app: the new bundle takes its place, nothing is left beside it.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_swap_script_replaces_the_app_bundle() {
        let root = std::env::temp_dir().join(format!("agu-swap-{}", crate::util::uuid_v4()));
        let make = |app: &std::path::Path, text: &str| {
            std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
            std::fs::write(app.join("Contents/MacOS/Agu Chat"), text).unwrap();
        };
        let installed = root.join("Applications/Agu Chat.app");
        let staged = root.join("updates/staged/Agu Chat.app");
        make(&installed, "old");
        make(&staged, "new");
        // a pid that has already exited, so the script does not wait
        let mut done = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let pid = done.id();
        done.wait().unwrap();
        let st = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(MAC_SWAP_SCRIPT)
            .arg("agu-chat-update")
            .arg(pid.to_string())
            .arg(&staged)
            .arg(&installed)
            .arg("0")
            .status()
            .unwrap();
        assert!(st.success());
        assert_eq!(std::fs::read_to_string(installed.join("Contents/MacOS/Agu Chat")).unwrap(), "new");
        let mut left: Vec<String> = std::fs::read_dir(root.join("Applications")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, vec!["Agu Chat.app".to_string()]);
        assert!(!staged.exists());
        std::fs::remove_dir_all(&root).ok();
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
