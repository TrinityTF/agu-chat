//! Port of src/renderer/alerts.ts: highlight alerts, the sound and the taskbar flash. Shared by every split, so a
//! message that shows in two splits (the same channel open twice) alerts once, and a burst of matches plays one
//! sound, not twenty.
//!
//! Sounds: the built-in ping is synthesized (two sine blips a fifth apart, same envelope as the Web Audio one),
//! a custom file is read from the sounds folder through the bridge and decoded by rodio (wav / mp3 / ogg /
//! flac). rodio lives on its own thread (its output stream is not `Send`). The taskbar flash is
//! `FlashWindowEx`, skipped while the window is active (focusing the window stops a running flash).
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agu_chat_core::protocol::{AguBridge, Settings};

const SOUND_GAP_MS: u128 = 400;
const FLASH_GAP_MS: u128 = 1000;
const SEEN_TTL_MS: u128 = 5 * 60_000;

#[derive(Clone, Debug, Default)]
pub struct AlertWants {
    pub sound: bool,
    pub sound_file: String,
    pub flash: bool,
}

enum Cmd {
    /// custom file bytes or None for the ping, volume 0..1
    Play(Option<Arc<Vec<u8>>>, f32),
}

pub struct Alerts {
    seen: RefCell<HashMap<String, Instant>>,
    last_sound: Cell<Option<Instant>>,
    last_flash: Cell<Option<Instant>>,
    tx: RefCell<Option<Sender<Cmd>>>,
    files: RefCell<HashMap<String, Arc<Vec<u8>>>>,
    /// Count of alerts that actually fired (diagnostics / tests).
    pub fired: Cell<usize>,
}

impl Alerts {
    pub fn new() -> Alerts {
        Alerts { seen: RefCell::new(HashMap::new()), last_sound: Cell::new(None), last_flash: Cell::new(None), tx: RefCell::new(None), files: RefCell::new(HashMap::new()), fired: Cell::new(0) }
    }

    fn sender(&self) -> Sender<Cmd> {
        let mut tx = self.tx.borrow_mut();
        if let Some(t) = tx.as_ref() {
            return t.clone();
        }
        let (t, rx) = channel::<Cmd>();
        std::thread::Builder::new()
            .name("agu-audio".into())
            .spawn(move || {
                let mut stream: Option<(rodio::OutputStream, rodio::OutputStreamHandle)> = None;
                while let Ok(Cmd::Play(file, volume)) = rx.recv() {
                    if stream.is_none() {
                        stream = rodio::OutputStream::try_default().ok();
                    }
                    let Some((_, handle)) = stream.as_ref() else { continue };
                    let Ok(sink) = rodio::Sink::try_new(handle) else { continue };
                    sink.set_volume(volume);
                    let mut played = false;
                    if let Some(bytes) = file {
                        if let Ok(dec) = rodio::Decoder::new(Cursor::new((*bytes).clone())) {
                            sink.append(dec);
                            played = true;
                        }
                    }
                    if !played {
                        sink.append(ping_source());
                    }
                    sink.detach();
                }
            })
            .ok();
        *tx = Some(t.clone());
        t
    }

    /// Fire the alerts a highlighted message asks for, once per message across all splits. `key` identifies the
    /// message (platform, channel and message id). `file_bytes` resolves a custom sound file name.
    pub fn alert_once(&self, key: &str, wants: &AlertWants, rs: &Settings, window_active: bool, hwnd: isize, load_file: &mut dyn FnMut(&str) -> Option<Arc<Vec<u8>>>) {
        if !wants.sound && !wants.flash {
            return;
        }
        let now = Instant::now();
        {
            let mut seen = self.seen.borrow_mut();
            if seen.contains_key(key) {
                return;
            }
            seen.insert(key.to_string(), now);
            if seen.len() > 2000 {
                seen.retain(|_, t| now.duration_since(*t).as_millis() <= SEEN_TTL_MS);
                if seen.len() > 1500 {
                    let mut keys: Vec<_> = seen.iter().map(|(k, t)| (k.clone(), *t)).collect();
                    keys.sort_by_key(|(_, t)| *t);
                    for (k, _) in keys.into_iter().take(seen.len() - 1500) {
                        seen.remove(&k);
                    }
                }
            }
        }
        let since = |l: &Cell<Option<Instant>>| l.get().map(|t| now.duration_since(t).as_millis()).unwrap_or(u128::MAX);
        if wants.sound && (rs.highlight_sound_focused || !window_active) && since(&self.last_sound) >= SOUND_GAP_MS {
            self.last_sound.set(Some(now));
            let file = if wants.sound_file.is_empty() { rs.highlight_sound_file.clone() } else { wants.sound_file.clone() };
            self.play(&file, rs.highlight_volume as f32, load_file);
            self.fired.set(self.fired.get() + 1);
        }
        if wants.flash && !window_active && since(&self.last_flash) >= FLASH_GAP_MS {
            self.last_flash.set(Some(now));
            flash_window(hwnd);
            self.fired.set(self.fired.get() + 1);
        }
    }

    /// Play a custom sound file, or the built-in ping when `file` is "" or cannot be played.
    pub fn play(&self, file: &str, volume_0_100: f32, load_file: &mut dyn FnMut(&str) -> Option<Arc<Vec<u8>>>) {
        let volume = volume_0_100.clamp(0.0, 100.0) / 100.0;
        if volume <= 0.0 {
            return;
        }
        let bytes = if file.is_empty() {
            None
        } else {
            let cached = self.files.borrow().get(file).cloned();
            match cached {
                Some(b) => Some(b),
                None => {
                    let b = load_file(file);
                    if let Some(b) = &b {
                        self.files.borrow_mut().insert(file.to_string(), b.clone());
                    }
                    b
                }
            }
        };
        let _ = self.sender().send(Cmd::Play(bytes, volume));
    }
}

impl Default for Alerts {
    fn default() -> Self {
        Self::new()
    }
}

/// Two short sine blips a fifth apart: audible over stream audio without being harsh.
pub fn ping_samples(rate: u32) -> Vec<f32> {
    let total = (rate as f32 * 0.5) as usize;
    let mut out = vec![0.0f32; total];
    for (freq, at) in [(988.0f32, 0.0f32), (1480.0, 0.09)] {
        let start = (at * rate as f32) as usize;
        let len = (0.34 * rate as f32) as usize;
        for i in 0..len {
            if start + i >= total {
                break;
            }
            let t = i as f32 / rate as f32;
            // exponential ramp 0.0001 -> 0.5 over 12 ms, then down to 0.0001 at 0.32 s
            let env = if t < 0.012 {
                0.0001 * (0.5f32 / 0.0001).powf(t / 0.012)
            } else if t < 0.32 {
                0.5 * (0.0001f32 / 0.5).powf((t - 0.012) / (0.32 - 0.012))
            } else {
                0.0
            };
            out[start + i] += env * (2.0 * std::f32::consts::PI * freq * t).sin();
        }
    }
    out
}

fn ping_source() -> rodio::buffer::SamplesBuffer<f32> {
    rodio::buffer::SamplesBuffer::new(1, 44100, ping_samples(44100))
}

#[cfg(windows)]
fn flash_window(hwnd: isize) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{FlashWindowEx, FLASHWINFO, FLASHW_ALL, FLASHW_TIMERNOFG};
    if hwnd == 0 {
        return;
    }
    let info = FLASHWINFO { cbSize: std::mem::size_of::<FLASHWINFO>() as u32, hwnd: HWND(hwnd as *mut _), dwFlags: FLASHW_ALL | FLASHW_TIMERNOFG, uCount: 0, dwTimeout: 0 };
    unsafe {
        let _ = FlashWindowEx(&info);
    }
}

/// macOS: one informational Dock bounce (AppKit ignores it while the app is active).
#[cfg(target_os = "macos")]
fn flash_window(_hwnd: isize) {
    crate::layout::macwin::request_attention();
}

#[cfg(not(any(windows, target_os = "macos")))]
fn flash_window(_hwnd: isize) {}

#[allow(dead_code)]
fn _d(_: Duration, _: &dyn AguBridge) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn rs() -> Settings {
        Settings::default()
    }

    fn wants(sound: bool, flash: bool) -> AlertWants {
        AlertWants { sound, sound_file: String::new(), flash }
    }

    #[test]
    fn the_same_message_alerts_once_across_splits() {
        let a = Alerts::new();
        let mut load = |_: &str| None;
        let mut s = rs();
        s.highlight_volume = 0; // no audio device needed in tests; flash path counts
        a.alert_once("twitch:a|m:1", &wants(false, true), &s, false, 0, &mut load);
        a.alert_once("twitch:a|m:1", &wants(false, true), &s, false, 0, &mut load);
        assert_eq!(a.fired.get(), 1);
    }

    #[test]
    fn flash_is_skipped_while_focused_and_rate_limited() {
        let a = Alerts::new();
        let mut load = |_: &str| None;
        let s = rs();
        a.alert_once("k1", &wants(false, true), &s, true, 0, &mut load);
        assert_eq!(a.fired.get(), 0);
        a.alert_once("k2", &wants(false, true), &s, false, 0, &mut load);
        a.alert_once("k3", &wants(false, true), &s, false, 0, &mut load);
        assert_eq!(a.fired.get(), 1, "a second flash within a second is dropped");
    }

    #[test]
    fn sound_only_when_unfocused_unless_setting_allows() {
        let a = Alerts::new();
        let mut load = |_: &str| None;
        let mut s = rs();
        s.highlight_volume = 0;
        s.highlight_sound_focused = false;
        a.alert_once("k1", &wants(true, false), &s, true, 0, &mut load);
        assert_eq!(a.fired.get(), 0);
        a.alert_once("k2", &wants(true, false), &s, false, 0, &mut load);
        assert_eq!(a.fired.get(), 1);
    }

    #[test]
    fn ping_has_two_blips_and_stays_in_range() {
        let s = ping_samples(44100);
        assert!(s.iter().all(|v| v.abs() <= 1.0));
        assert!(s[..2000].iter().any(|v| v.abs() > 0.1));
        assert!(s[s.len() - 100..].iter().all(|v| v.abs() < 0.01));
    }
}
