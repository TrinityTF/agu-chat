//! The channel-modes panel's logic without any UI (Twitch room settings: Shield Mode, subscribers / emote /
//! unique chat, followers-only, slow mode, blocked terms): the options, how Helix's answer reads, and which
//! account sees the controls. The desktop's header button and the phone's sheet both draw from this.
use serde_json::Value;

use super::mod_caps::ModCaps;

pub const FOLLOWER_MODE_OPTIONS: [(i64, &str); 9] = [(-1, "Off"), (0, "Any"), (10, "10m"), (30, "30m"), (60, "1h"), (1440, "1d"), (10080, "1w"), (43200, "1mo"), (129600, "3mo")];
pub const SLOW_MODE_OPTIONS: [i64; 8] = [0, 3, 5, 10, 20, 30, 60, 120];

pub fn can_moderate(c: Option<&ModCaps>) -> bool {
    c.map(|c| c.modes || c.shield || c.terms).unwrap_or(false)
}

/// The room settings of Helix's `GET /chats/settings`, as the relay returns them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Modes {
    pub emote_mode: bool,
    pub follower_mode: bool,
    pub follower_mode_duration: Option<i64>,
    pub slow_mode: bool,
    pub slow_mode_wait_time: Option<i64>,
    pub subscriber_mode: bool,
    pub unique_chat_mode: bool,
}

pub fn parse_modes(v: &Value) -> Modes {
    let b = |k: &str| v.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
    let n = |k: &str| v.get(k).and_then(|x| x.as_i64());
    Modes {
        emote_mode: b("emote_mode"),
        follower_mode: b("follower_mode"),
        follower_mode_duration: n("follower_mode_duration"),
        slow_mode: b("slow_mode"),
        slow_mode_wait_time: n("slow_mode_wait_time"),
        subscriber_mode: b("subscriber_mode"),
        unique_chat_mode: b("unique_chat_mode"),
    }
}

/// `restricted`: a mode that limits who can talk is on (or Shield Mode).
pub fn restricted(m: Option<&Modes>, shield: bool) -> bool {
    shield || m.map(|m| m.emote_mode || m.follower_mode || m.slow_mode || m.subscriber_mode || m.unique_chat_mode).unwrap_or(false)
}

/// `withCurrent`: the options, plus the channel's current value when the list does not have it (set elsewhere).
pub fn with_current(opts: &[(i64, String)], current: i64, label: impl Fn(i64) -> String) -> Vec<(i64, String)> {
    if opts.iter().any(|(v, _)| *v == current) {
        return opts.to_vec();
    }
    let mut v = opts.to_vec();
    v.push((current, label(current)));
    v.sort_by_key(|(v, _)| *v);
    v
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_helix_settings_and_flags_restricting_modes() {
        let m = parse_modes(&json!({"emote_mode": false, "follower_mode": true, "follower_mode_duration": 30, "slow_mode": false, "slow_mode_wait_time": null, "subscriber_mode": false, "unique_chat_mode": null}));
        assert_eq!(m.follower_mode_duration, Some(30));
        assert!(restricted(Some(&m), false));
        assert!(!restricted(Some(&Modes::default()), false));
        assert!(restricted(Some(&Modes::default()), true));
        assert!(!restricted(None, false));
    }

    #[test]
    fn a_value_set_elsewhere_joins_the_options_in_order() {
        let opts: Vec<(i64, String)> = vec![(-1, "Off".into()), (0, "Any".into()), (10, "10m".into()), (30, "30m".into())];
        assert_eq!(with_current(&opts, 10, |v| format!("{v}m")), opts);
        let w = with_current(&opts, 20, |v| format!("{v}m"));
        assert_eq!(w.iter().map(|(v, _)| *v).collect::<Vec<_>>(), vec![-1, 0, 10, 20, 30]);
        assert_eq!(w[3].1, "20m");
    }

    #[test]
    fn only_someone_with_a_mode_right_sees_the_button() {
        assert!(!can_moderate(None));
        assert!(!can_moderate(Some(&ModCaps::default())));
        assert!(can_moderate(Some(&ModCaps { shield: true, ..Default::default() })));
        assert!(can_moderate(Some(&ModCaps { terms: true, ..Default::default() })));
    }
}
