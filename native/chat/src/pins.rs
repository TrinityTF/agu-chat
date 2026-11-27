//! Twitch's pinned message: the rules both apps share (which time a pin may be set to, when a pin is over, how
//! "Hide for yourself" is remembered, where the "Unpin after" choices start).
use super::types::ChatMsg;

/// Twitch takes 30 seconds to 30 minutes, or "until unpinned / stream ends".
pub const PIN_MIN_S: i64 = 30;
pub const PIN_MAX_S: i64 = 1800;
/// The hint under an invalid "Customized time".
pub const PIN_RANGE_HINT: &str = "30 seconds to 30 minutes";
/// How many hidden pins are remembered.
pub const HIDDEN_PINS_KEPT: usize = 20;

/// The seconds a "Customized time" amount means, when it is a valid one.
pub fn unpin_secs(amount: &str, unit_min: bool) -> Option<i64> {
    let n: f64 = amount.trim().parse().ok()?;
    if !n.is_finite() {
        return None;
    }
    let secs = (n * if unit_min { 60.0 } else { 1.0 }).round() as i64;
    (PIN_MIN_S..=PIN_MAX_S).contains(&secs).then_some(secs)
}

/// Now, in ms since the epoch.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// When the pin ends (ms since the epoch), when it has an end.
pub fn pin_ends_at_ms(p: &ChatMsg) -> Option<i64> {
    let s = p.pin.as_ref()?.ends_at.as_deref()?;
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp_millis())
}

/// The pin is over (it still arrives in the frame a moment after it ended).
pub fn pin_expired(p: &ChatMsg, now_ms: i64) -> bool {
    pin_ends_at_ms(p).map(|e| e <= now_ms).unwrap_or(false)
}

/// Seconds the pin has left.
pub fn pin_left_secs(ends_at_ms: i64, now_ms: i64) -> i64 {
    ((ends_at_ms - now_ms) as f64 / 1000.0).round().max(0.0) as i64
}

/// "5 min" / "30s": what is left on a pin.
pub fn left_text(left: i64) -> String {
    if left >= 60 { format!("{} min", (left as f64 / 60.0).ceil() as i64) } else { format!("{left}s") }
}

/// What "Unpin after" starts from: (customized time?, unit is minutes?, the amount). From what is left on the pin
/// now, in minutes when it is a minute or more; a pin without an end starts at "end of stream" with 20 minutes ready.
pub fn unpin_after_start(left: Option<i64>) -> (bool, bool, String) {
    match left {
        None => (false, true, "20".to_string()),
        Some(l) if l >= 60 => (true, true, ((l as f64) / 60.0).ceil().to_string()),
        Some(l) => (true, false, l.to_string()),
    }
}

/// "Hide for yourself": the id goes last in the list; only the newest [`HIDDEN_PINS_KEPT`] are remembered.
pub fn hide_pin(list: &mut Vec<String>, id: &str) {
    list.retain(|x| x != id);
    list.push(id.to_string());
    let keep = list.len().saturating_sub(HIDDEN_PINS_KEPT);
    list.drain(..keep);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpin_after_takes_30_seconds_to_30_minutes() {
        assert_eq!(unpin_secs("20", true), Some(1200));
        assert_eq!(unpin_secs("30", false), Some(30));
        assert_eq!(unpin_secs("29", false), None);
        assert_eq!(unpin_secs("30", true), Some(1800));
        assert_eq!(unpin_secs("31", true), None);
        assert_eq!(unpin_secs("0.5", true), Some(30));
        assert_eq!(unpin_secs("", true), None);
        assert_eq!(unpin_secs("abc", false), None);
    }

    #[test]
    fn unpin_after_starts_from_what_is_left() {
        assert_eq!(unpin_after_start(None), (false, true, "20".into()));
        assert_eq!(unpin_after_start(Some(61)), (true, true, "2".into()));
        assert_eq!(unpin_after_start(Some(45)), (true, false, "45".into()));
        assert_eq!(left_text(61), "2 min");
        assert_eq!(left_text(9), "9s");
    }

    #[test]
    fn hidden_pins_keep_the_newest_twenty_once_each() {
        let mut l: Vec<String> = Vec::new();
        for i in 0..25 {
            hide_pin(&mut l, &i.to_string());
        }
        assert_eq!(l.len(), HIDDEN_PINS_KEPT);
        assert_eq!(l[0], "5");
        hide_pin(&mut l, "10");
        assert_eq!(l.last().unwrap(), "10");
        assert_eq!(l.iter().filter(|x| *x == "10").count(), 1);
    }

    #[test]
    fn a_pin_ends_at_its_end_time() {
        let mut m = ChatMsg::default();
        assert!(!pin_expired(&m, 10));
        m.pin = Some(super::super::types::PinInfo { ends_at: Some("1970-01-01T00:00:05Z".into()), ..Default::default() });
        assert_eq!(pin_ends_at_ms(&m), Some(5000));
        assert!(pin_expired(&m, 5000));
        assert!(!pin_expired(&m, 4999));
        assert_eq!(pin_left_secs(5000, 1000), 4);
    }
}
