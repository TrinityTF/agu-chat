//! The words of the Settings screens that are not layout: the updater's status line, a stored sound file's shown name,
//! the new-issue link. Shared by the desktop dialog and the Android screen so both say the same thing.
use agu_chat_core::protocol::UpdaterState;

pub const REPO_URL: &str = "https://github.com/TrinityTF/agu-chat";
/// What a command mod button may contain.
pub const TEMPLATE_HELP: &str = "{user} login, {name} display name, {user-id}, {msg-id}, {channel}, {msg} message text";
/// The names of `MOD_BUTTON_ICONS`, in the same order.
pub const MOD_ICON_LABELS: [&str; 10] = ["Ban", "Clock", "Trash", "Shield", "Warning", "Flag", "Hidden", "Chat", "Pin", "Check"];

/// Shown name of a stored sound file: drop the "-<hash>" suffix `Sounds::import` adds.
pub fn sound_label(file: &str) -> String {
    if let Some((stem, ext)) = file.rsplit_once('.') {
        if let Some((base, hash)) = stem.rsplit_once('-') {
            if hash.len() == 8 && hash.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
                return format!("{base}.{ext}");
            }
        }
    }
    file.to_string()
}

/// GitHub's new-issue page with the app version and system already in the body (nothing personal: no names,
/// no channels).
pub fn new_issue_url(version: &str, os: &str) -> String {
    let os = match os {
        "windows" => "Windows",
        "macos" => "macOS",
        "android" => "Android",
        o => o,
    };
    let body = format!(
        "**What happened**\n\n\n**What you expected**\n\n\n**Steps to reproduce**\n1. \n\n**Platform / chat** (Twitch, Kick, YouTube, Rumble, X)\n\n\n---\nAgu Chat {version} on {os}\n"
    );
    let enc: String = body.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    format!("{REPO_URL}/issues/new?body={enc}")
}

/// Progress as a percentage. The backend reports 0..1 (updater.js); the dev mock reports 0..100. The JS dialog
/// printed `Math.round(progress)` and so showed 0-1% for a real download; this shows the real percentage.
pub fn percent_of(p: f32) -> f32 {
    if p <= 1.0 {
        p * 100.0
    } else {
        p
    }
}

/// `updaterText`
pub fn updater_text(u: &UpdaterState) -> String {
    let v = |d: &str| u.version.clone().unwrap_or_else(|| d.to_string());
    match u.status.as_str() {
        "idle" => "Not checked yet.".into(),
        "dev" => "Updates are off in development builds.".into(),
        "checking" => "Checking for updates…".into(),
        "none" => "You are on the latest version.".into(),
        "available" => format!("Version {} is available.", v("?")),
        "downloading" => format!("Downloading {}… {}%", v("the update"), percent_of(u.progress).round() as i64),
        "ready" => format!("Version {} is ready to install.", v("?")),
        "error" => format!("Update check failed{}", u.error.as_ref().map(|e| format!(": {e}")).unwrap_or_else(|| ".".into())),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(status: &str) -> UpdaterState {
        UpdaterState { status: status.into(), channel: "beta".into(), version: Some("0.1.0".into()), notes: vec![], progress: 0.0, error: None }
    }

    #[test]
    fn updater_text_matches_the_desktop_dialog() {
        assert_eq!(updater_text(&st("none")), "You are on the latest version.");
        assert_eq!(updater_text(&st("checking")), "Checking for updates…");
        assert_eq!(updater_text(&st("available")), "Version 0.1.0 is available.");
        let mut d = st("downloading");
        d.progress = 0.6;
        assert_eq!(updater_text(&d), "Downloading 0.1.0… 60%");
        let mut e = st("error");
        assert_eq!(updater_text(&e), "Update check failed.");
        e.error = Some("Could not reach GitHub.".into());
        assert_eq!(updater_text(&e), "Update check failed: Could not reach GitHub.");
    }

    #[test]
    fn the_hash_suffix_is_not_shown() {
        assert_eq!(sound_label("My Ping-a9993e36.mp3"), "My Ping.mp3");
        assert_eq!(sound_label("plain.wav"), "plain.wav");
    }

    #[test]
    fn the_issue_link_names_the_system() {
        assert!(new_issue_url("0.3.11", "android").contains("on%20Android"));
    }
}
