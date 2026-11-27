//! Highlight alerts on the phone: the same rules as the desktop's `chat/alerts.rs` (one alert per message however
//! many tabs show it, one sound per burst), but the taskbar flash is a short vibration. This only decides; the
//! app plays the sound (`alerts.ping` gives the built-in ping) and vibrates.
use std::collections::HashMap;
use std::time::{Duration, Instant};

use agu_chat_core::protocol::Settings;

const SOUND_GAP: Duration = Duration::from_millis(400);
const FLASH_GAP: Duration = Duration::from_millis(1000);
const SEEN_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Wants {
    pub sound: bool,
    pub sound_file: String,
    pub flash: bool,
}

/// What the app should do for one alert.
#[derive(Clone, Debug, PartialEq)]
pub struct Fire {
    /// (custom file name or "" for the ping, volume 0..100)
    pub sound: Option<(String, u8)>,
    pub vibrate: bool,
}

#[derive(Default)]
pub struct Alerter {
    seen: HashMap<String, Instant>,
    last_sound: Option<Instant>,
    last_flash: Option<Instant>,
}

impl Alerter {
    /// `key` identifies the message across tabs; `active` is whether the app is on screen.
    pub fn once(&mut self, key: &str, wants: &Wants, rs: &Settings, active: bool, now: Instant) -> Option<Fire> {
        if !wants.sound && !wants.flash {
            return None;
        }
        if self.seen.contains_key(key) {
            return None;
        }
        self.seen.insert(key.to_string(), now);
        if self.seen.len() > 2000 {
            self.seen.retain(|_, t| now.duration_since(*t) <= SEEN_TTL);
            if self.seen.len() > 1500 {
                let mut keys: Vec<(String, Instant)> = self.seen.iter().map(|(k, t)| (k.clone(), *t)).collect();
                keys.sort_by_key(|(_, t)| *t);
                let drop = self.seen.len() - 1500;
                for (k, _) in keys.into_iter().take(drop) {
                    self.seen.remove(&k);
                }
            }
        }
        let since = |l: Option<Instant>| l.map(|t| now.duration_since(t)).unwrap_or(Duration::MAX);
        let mut fire = Fire { sound: None, vibrate: false };
        if wants.sound && (rs.highlight_sound_focused || !active) && since(self.last_sound) >= SOUND_GAP {
            self.last_sound = Some(now);
            if rs.highlight_volume > 0 {
                let file = if wants.sound_file.is_empty() { rs.highlight_sound_file.clone() } else { wants.sound_file.clone() };
                fire.sound = Some((file, rs.highlight_volume.min(100)));
            }
        }
        if wants.flash && !active && since(self.last_flash) >= FLASH_GAP {
            self.last_flash = Some(now);
            fire.vibrate = true;
        }
        (fire.sound.is_some() || fire.vibrate).then_some(fire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wants(sound: bool, flash: bool) -> Wants {
        Wants { sound, sound_file: String::new(), flash }
    }

    #[test]
    fn a_message_in_two_tabs_alerts_once() {
        let mut a = Alerter::default();
        let now = Instant::now();
        let rs = Settings::default();
        assert!(a.once("twitch:a||m:1", &wants(true, false), &rs, true, now).is_some());
        assert!(a.once("twitch:a||m:1", &wants(true, false), &rs, true, now + Duration::from_secs(5)).is_none());
    }

    #[test]
    fn a_burst_plays_one_sound() {
        let mut a = Alerter::default();
        let now = Instant::now();
        let rs = Settings::default();
        assert!(a.once("1", &wants(true, false), &rs, true, now).is_some());
        assert!(a.once("2", &wants(true, false), &rs, true, now + Duration::from_millis(100)).is_none());
        assert!(a.once("3", &wants(true, false), &rs, true, now + Duration::from_millis(500)).is_some());
    }

    #[test]
    fn vibration_only_while_the_app_is_in_the_background_and_rate_limited() {
        let mut a = Alerter::default();
        let now = Instant::now();
        let rs = Settings::default();
        assert!(a.once("1", &wants(false, true), &rs, true, now).is_none());
        assert_eq!(a.once("2", &wants(false, true), &rs, false, now).map(|f| f.vibrate), Some(true));
        assert!(a.once("3", &wants(false, true), &rs, false, now + Duration::from_millis(300)).is_none());
    }

    #[test]
    fn sound_follows_the_focused_setting_and_volume() {
        let mut a = Alerter::default();
        let now = Instant::now();
        let mut rs = Settings::default();
        rs.highlight_sound_focused = false;
        assert!(a.once("1", &wants(true, false), &rs, true, now).is_none());
        let f = a.once("2", &wants(true, false), &rs, false, now).unwrap();
        assert_eq!(f.sound, Some((String::new(), 70)));
        rs.highlight_volume = 0;
        assert!(a.once("3", &wants(true, false), &rs, false, now + Duration::from_secs(2)).is_none());
    }
}
