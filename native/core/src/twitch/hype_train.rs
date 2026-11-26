//! A Twitch channel's hype train, for the bar at the top of the chat: the same public fields twitch.tv's own chat
//! shows (level, progress, time left, the level's emotes, the conductors), read from gql.twitch.tv.
//!
//! It is public data that twitch.tv also shows to people who are not signed in, so the call carries only the web
//! page's client id (`web::gql_public`), never the user's session: an expired sign-in cannot hide the bar and the
//! bar cannot mark a sign-in expired. No integrity token, no browser imitation.
//!
//! LIVE UPDATES. The chat asks again on a timer (`GET /api/twitch/hype-train`): every few seconds while a train is
//! on, rarely while none is. Answers are kept for `CACHE_TTL` per channel so several splits of one channel share
//! one call.
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::web::{gql_public, GqlError};
use crate::relay::{Handler, RouteError, RouteReq, RouteRes};

const CACHE_TTL: Duration = Duration::from_secs(3);

/// What one level-up needs, in Twitch's points, when its config does not say (1 Bit = 1 point, a Tier 1 sub = 500).
const DEFAULT_SUB_POINTS: u64 = 500;
const DEFAULT_BIT_POINTS: u64 = 1;

pub const HYPE_TRAIN_QUERY: &str = "query HypeTrain($login: String!) {
  user(login: $login) {
    id
    channel {
      hypeTrain {
        execution {
          id isActive startedAt expiresAt endedAt updatedAt
          isTreasureTrain isGoldenKappaTrain isFastMode
          sharedHypeTrainDetails { __typename }
          progress {
            goal progression total
            level { value goal rewards { __typename ... on HypeTrainEmoteReward { id emote { id token } } } }
          }
          allTimeHigh { goal level { value } }
          participations { source action quantity }
          conductors {
            source
            user { id login displayName profileImageURL(width: 50) }
            participation { source action quantity }
          }
          config {
            difficulty willUseCreatorColor primaryHexColor
            participationConversionRates { action source value }
            conductorRewards { source type }
          }
        }
      }
    }
  }
}";

fn num(v: &Value) -> u64 {
    v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)).unwrap_or(0)
}

fn rate(config: &Value, action: &str, default: u64) -> u64 {
    config["participationConversionRates"]
        .as_array()
        .and_then(|a| a.iter().find(|r| r["action"] == action))
        .map(|r| num(&r["value"]))
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

/// Whole percent of the level done, the way twitch.tv rounds it (down, so 100% only once the level is complete).
pub fn percent(progression: u64, goal: u64) -> u64 {
    if goal == 0 {
        return 0;
    }
    (progression.saturating_mul(100) / goal).min(100)
}

/// Points still needed -> (Bits, subs) that would finish the level: Bits at the cheer rate, subs rounded up.
pub fn remaining_as(remaining: u64, bit_points: u64, sub_points: u64) -> (u64, u64) {
    let bits = remaining.div_ceil(bit_points.max(1));
    let subs = remaining.div_ceil(sub_points.max(1));
    (bits, subs)
}

/// Twitch's emote image (dark theme, 2x).
pub fn emote_url(id: &str) -> String {
    format!("https://static-cdn.jtvnw.net/emoticons/v2/{id}/default/dark/2.0")
}

/// "FFBF00" / "#ffbf00" -> "ffbf00"; None for anything that is not six hex digits.
pub fn clean_hex(s: &str) -> Option<String> {
    let h = s.trim().trim_start_matches('#');
    (h.len() == 6 && h.chars().all(|c| c.is_ascii_hexdigit())).then(|| h.to_ascii_lowercase())
}

/// `HypeTrain` data -> what the bar shows. `{"active": false}` when the channel has no train going.
pub fn parse_hype_train(data: &Value) -> Value {
    let ex = data.pointer("/user/channel/hypeTrain/execution").cloned().unwrap_or(Value::Null);
    let active = ex["isActive"].as_bool().unwrap_or(false) && ex["endedAt"].is_null();
    if !ex.is_object() || !active {
        return json!({"active": false});
    }
    let p = &ex["progress"];
    let config = &ex["config"];
    let goal = num(&p["goal"]);
    let progression = num(&p["progression"]);
    let remaining = goal.saturating_sub(progression);
    let (bits, subs) = remaining_as(remaining, rate(config, "CHEER", DEFAULT_BIT_POINTS), rate(config, "TIER_1_SUB", DEFAULT_SUB_POINTS));

    let emotes: Vec<Value> = p
        .pointer("/level/rewards")
        .and_then(|r| r.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    let id = r.pointer("/emote/id").and_then(|i| i.as_str()).or_else(|| r["id"].as_str()).filter(|i| !i.is_empty())?;
                    let token = r.pointer("/emote/token").and_then(|t| t.as_str()).unwrap_or("");
                    Some(json!({"id": id, "token": token, "url": emote_url(id)}))
                })
                .collect()
        })
        .unwrap_or_default();

    // the conductors: top sub gifter and top cheerer of this train
    let conductors: Vec<Value> = ex["conductors"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    let u = &c["user"];
                    let login = u["login"].as_str().filter(|l| !l.is_empty())?;
                    let source = c["source"].as_str().unwrap_or("");
                    let quantity: u64 = c["participation"].as_array().map(|ps| ps.iter().filter(|x| x["source"].as_str().map(|s| s == source).unwrap_or(true)).map(|x| num(&x["quantity"])).sum()).unwrap_or(0);
                    Some(json!({
                        "source": source,
                        "login": login,
                        "name": u["displayName"].as_str().filter(|d| !d.is_empty()).unwrap_or(login),
                        "avatar": u["profileImageURL"].as_str().unwrap_or(""),
                        "quantity": quantity,
                    }))
                })
                .collect()
        })
        .unwrap_or_default();

    let mut contributed_bits = 0u64;
    let mut contributed_subs = 0u64;
    for x in ex["participations"].as_array().into_iter().flatten() {
        match x["source"].as_str() {
            Some("BITS") => contributed_bits += num(&x["quantity"]),
            Some("SUBS") => contributed_subs += num(&x["quantity"]),
            _ => {}
        }
    }

    let reward_sources: Vec<String> = config["conductorRewards"]
        .as_array()
        .map(|a| {
            let mut v: Vec<String> = a.iter().filter(|r| r["type"] == "CURRENT").filter_map(|r| r["source"].as_str().map(String::from)).collect();
            v.sort();
            v.dedup();
            v
        })
        .unwrap_or_default();

    let color = if config["willUseCreatorColor"].as_bool().unwrap_or(false) { config["primaryHexColor"].as_str().and_then(clean_hex) } else { None };
    let record = &ex["allTimeHigh"];
    json!({
        "active": true,
        "id": ex["id"].as_str().unwrap_or(""),
        "level": num(p.pointer("/level/value").unwrap_or(&Value::Null)).max(1),
        "progression": progression,
        "goal": goal,
        "percent": percent(progression, goal),
        "total": num(&p["total"]),
        "remaining": remaining,
        "bits": bits,
        "subs": subs,
        "startedAt": ex["startedAt"].as_str().unwrap_or(""),
        "expiresAt": ex["expiresAt"].as_str().unwrap_or(""),
        "emotes": emotes,
        "conductors": conductors,
        "contributed": {"bits": contributed_bits, "subs": contributed_subs},
        "conductorRewards": reward_sources,
        "treasure": ex["isTreasureTrain"].as_bool().unwrap_or(false),
        "goldenKappa": ex["isGoldenKappaTrain"].as_bool().unwrap_or(false),
        "shared": ex["sharedHypeTrainDetails"].is_object(),
        "color": color,
        "difficulty": config["difficulty"].as_str().unwrap_or(""),
        "record": if record.is_object() { json!({"level": num(record.pointer("/level/value").unwrap_or(&Value::Null))}) } else { Value::Null },
    })
}

// ---------------------------------------------------------------------------------------------------------
// Route
// ---------------------------------------------------------------------------------------------------------

static CACHE: LazyLock<Mutex<HashMap<String, (Instant, Value)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

async fn fetch(login: &str) -> Result<Value, GqlError> {
    if let Some((at, v)) = CACHE.lock().unwrap().get(login) {
        if at.elapsed() < CACHE_TTL {
            return Ok(v.clone());
        }
    }
    let data = gql_public("HypeTrain", HYPE_TRAIN_QUERY, json!({"login": login})).await?;
    if data["user"].is_null() {
        return Err(GqlError::Other("channel not found".into()));
    }
    let v = parse_hype_train(&data);
    let mut c = CACHE.lock().unwrap();
    c.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(600));
    c.insert(login.to_string(), (Instant::now(), v.clone()));
    Ok(v)
}

/// GET /api/twitch/hype-train : the source channel's hype train (see `parse_hype_train`).
async fn hype_train(req: RouteReq) -> Result<RouteRes, RouteError> {
    let login = match super::channel::normalize_channel(&req.source.channel) {
        Ok(l) => l,
        Err(e) => return Ok(RouteRes { status: 400, body: json!({"error": e}) }),
    };
    match fetch(&login).await {
        Ok(v) => Ok(RouteRes { status: 200, body: v }),
        Err(e) => Ok(RouteRes { status: 502, body: json!({"error": e.message()}) }),
    }
}

pub fn routes() -> Vec<(String, Handler)> {
    let h: Handler = Arc::new(|req| Box::pin(hype_train(req)));
    vec![("GET /api/twitch/hype-train".into(), h)]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape gql.twitch.tv answered for a live Level 2 train (trimmed, names changed).
    fn sample() -> Value {
        json!({"user": {"id": "1", "channel": {"hypeTrain": {"execution": {
            "id": "t1", "isActive": true, "startedAt": "2026-11-19T00:54:28Z", "expiresAt": "2026-11-19T01:00:07Z", "endedAt": null,
            "isTreasureTrain": true, "isGoldenKappaTrain": false, "isFastMode": false, "sharedHypeTrainDetails": null,
            "progress": {"goal": 4000, "progression": 700, "total": 3700, "level": {"value": 2, "goal": 7000, "rewards": [
                {"__typename": "HypeTrainEmoteReward", "id": "emotesv2_a", "emote": {"id": "emotesv2_a", "token": "KittyHype"}},
                {"__typename": "HypeTrainEmoteReward", "id": "emotesv2_b", "emote": {"id": "emotesv2_b", "token": "DangerDance"}},
                {"__typename": "HypeTrainBadgeReward"}
            ]}},
            "allTimeHigh": {"goal": 100000, "level": {"value": 16}},
            "participations": [{"source": "BITS", "action": "CHEER", "quantity": 700}, {"source": "SUBS", "action": "TIER_1_SUB", "quantity": 6}, {"source": "SUBS", "action": "TIER_1_GIFTED_SUB", "quantity": 1}],
            "conductors": [
                {"source": "SUBS", "user": {"id": "2", "login": "giver", "displayName": "Giver", "profileImageURL": "https://x/a.png"}, "participation": [{"source": "SUBS", "action": "TIER_1_GIFTED_SUB", "quantity": 1}]},
                {"source": "BITS", "user": {"id": "3", "login": "cheerer", "displayName": "", "profileImageURL": null}, "participation": [{"source": "BITS", "action": "CHEER", "quantity": 500}]}
            ],
            "config": {"difficulty": "HARD", "willUseCreatorColor": true, "primaryHexColor": "FFBF00",
                "participationConversionRates": [{"action": "CHEER", "source": "BITS", "value": 1}, {"action": "TIER_1_SUB", "source": "SUBS", "value": 500}],
                "conductorRewards": [{"source": "BITS", "type": "CURRENT"}, {"source": "BITS", "type": "FORMER"}, {"source": "SUBS", "type": "CURRENT"}]}
        }}}}})
    }

    #[test]
    fn parses_a_running_train() {
        let v = parse_hype_train(&sample());
        assert_eq!(v["active"], true);
        assert_eq!(v["level"], 2);
        assert_eq!(v["progression"], 700);
        assert_eq!(v["goal"], 4000);
        assert_eq!(v["percent"], 17);
        assert_eq!(v["remaining"], 3300);
        assert_eq!(v["bits"], 3300);
        assert_eq!(v["subs"], 7); // 3300 / 500, rounded up
        assert_eq!(v["expiresAt"], "2026-11-19T01:00:07Z");
        assert_eq!(v["color"], "ffbf00");
        assert_eq!(v["treasure"], true);
        assert_eq!(v["shared"], false);
        assert_eq!(v["record"]["level"], 16);
        assert_eq!(v["contributed"]["bits"], 700);
        assert_eq!(v["contributed"]["subs"], 7);
        assert_eq!(v["conductorRewards"], json!(["BITS", "SUBS"]));
    }

    #[test]
    fn emotes_skip_non_emote_rewards() {
        let v = parse_hype_train(&sample());
        let e = v["emotes"].as_array().unwrap();
        assert_eq!(e.len(), 2);
        assert_eq!(e[0]["token"], "KittyHype");
        assert_eq!(e[0]["url"], "https://static-cdn.jtvnw.net/emoticons/v2/emotesv2_a/default/dark/2.0");
    }

    #[test]
    fn conductors_fall_back_to_login() {
        let v = parse_hype_train(&sample());
        let c = v["conductors"].as_array().unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0]["name"], "Giver");
        assert_eq!(c[0]["quantity"], 1);
        assert_eq!(c[1]["name"], "cheerer");
        assert_eq!(c[1]["source"], "BITS");
        assert_eq!(c[1]["quantity"], 500);
        assert_eq!(c[1]["avatar"], "");
    }

    #[test]
    fn no_train_or_ended_train_is_inactive() {
        assert_eq!(parse_hype_train(&json!({"user": {"channel": {"hypeTrain": {"execution": null}}}})), json!({"active": false}));
        assert_eq!(parse_hype_train(&json!({"user": null})), json!({"active": false}));
        let mut ended = sample();
        ended["user"]["channel"]["hypeTrain"]["execution"]["endedAt"] = json!("2026-11-19T01:00:07Z");
        assert_eq!(parse_hype_train(&ended)["active"], false);
        let mut off = sample();
        off["user"]["channel"]["hypeTrain"]["execution"]["isActive"] = json!(false);
        assert_eq!(parse_hype_train(&off)["active"], false);
    }

    #[test]
    fn creator_color_only_when_the_channel_uses_it() {
        let mut s = sample();
        s["user"]["channel"]["hypeTrain"]["execution"]["config"]["willUseCreatorColor"] = json!(false);
        assert_eq!(parse_hype_train(&s)["color"], Value::Null);
        assert_eq!(clean_hex("#A1b2C3"), Some("a1b2c3".into()));
        assert_eq!(clean_hex("fff"), None);
        assert_eq!(clean_hex("zzzzzz"), None);
    }

    #[test]
    fn percent_rounds_down_and_caps() {
        assert_eq!(percent(0, 0), 0);
        assert_eq!(percent(1240, 2000), 62);
        assert_eq!(percent(1999, 2000), 99);
        assert_eq!(percent(2500, 2000), 100);
    }

    #[test]
    fn remaining_in_bits_and_subs() {
        assert_eq!(remaining_as(600, 1, 500), (600, 2));
        assert_eq!(remaining_as(1000, 1, 500), (1000, 2));
        assert_eq!(remaining_as(0, 1, 500), (0, 0));
        assert_eq!(remaining_as(5, 0, 0), (5, 5)); // a zero rate never divides by zero
    }

    #[test]
    fn query_asks_by_login() {
        assert!(HYPE_TRAIN_QUERY.contains("user(login: $login)"));
        assert!(HYPE_TRAIN_QUERY.contains("hypeTrain"));
    }
}
