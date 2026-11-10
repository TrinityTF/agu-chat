//! YouTube moderation with the user's own session (auth.rs): delete a message, time a chatter out, hide (ban) them.
//!
//! Every chat message carries `contextMenuEndpoint.liveChatItemContextMenuEndpoint.params`. Asking
//! `live_chat/get_item_context_menu` with them returns the menu youtube.com shows for that message to THIS
//! account: only an owner or moderator gets items with a `moderateLiveChatEndpoint`. Choosing one is a POST of its
//! params to `live_chat/moderate`. Nobody is moderated without the account's own menu offering it.
use serde_json::{json, Value};

use super::auth::{self, ApiError};

pub const NOT_MODERATOR: &str = "You can't moderate this channel";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Delete,
    Timeout,
    Ban,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "delete" => Some(Kind::Delete),
            "timeout" => Some(Kind::Timeout),
            "ban" => Some(Kind::Ban),
            _ => None,
        }
    }
}

/// One menu entry that moderates: what it does and the params that do it.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuAction {
    pub kind: Kind,
    pub params: String,
}

/// What a menu item does, from its text and icon (youtube.com's own words; unknown items are left alone).
fn classify(text: &str, icon: &str) -> Option<Kind> {
    let t = text.to_lowercase();
    // the reverse actions of an already moderated chatter
    if ["unhide", "unban", "unblock", "out of timeout", "remove timeout", "end timeout", "cancel timeout"].iter().any(|s| t.contains(s)) {
        return None;
    }
    if t.contains("timeout") || t.contains("time out") {
        return Some(Kind::Timeout);
    }
    if t.contains("hide user") || t.contains("block") || t.contains("ban ") || t == "ban" {
        return Some(Kind::Ban);
    }
    if t.starts_with("remove") || t.starts_with("delete") {
        return Some(Kind::Delete);
    }
    match icon {
        "HOURGLASS" | "HOURGLASS_EMPTY" | "TIMER" => Some(Kind::Timeout),
        "BLOCK" | "HIDE_SOURCE" | "REMOVE_CIRCLE" | "PERSON_REMOVE" => Some(Kind::Ban),
        "DELETE" | "DELETE_FOREVER" | "TRASH" => Some(Kind::Delete),
        _ => None,
    }
}

/// The first `moderateLiveChatEndpoint.params` anywhere under `v` (a confirm dialog nests it).
fn find_moderate_params(v: &Value) -> Option<String> {
    match v {
        Value::Object(m) => {
            if let Some(p) = m.get("moderateLiveChatEndpoint").and_then(|e| e["params"].as_str()).filter(|p| !p.is_empty()) {
                return Some(p.to_string());
            }
            m.values().find_map(find_moderate_params)
        }
        Value::Array(a) => a.iter().find_map(find_moderate_params),
        _ => None,
    }
}

fn collect_items<'a>(v: &'a Value, out: &mut Vec<&'a Value>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                if k == "menuServiceItemRenderer" || k == "menuNavigationItemRenderer" {
                    out.push(x);
                } else {
                    collect_items(x, out);
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_items(x, out)),
        _ => {}
    }
}

/// `get_item_context_menu` answer -> the moderating items it offers (none for a viewer who is not owner or mod).
pub fn parse_menu(body: &Value) -> Vec<MenuAction> {
    let mut items = Vec::new();
    collect_items(body, &mut items);
    items
        .into_iter()
        .filter_map(|it| {
            let params = find_moderate_params(it)?;
            let kind = classify(&auth::text_of(&it["text"]), it["icon"]["iconType"].as_str().unwrap_or(""))?;
            Some(MenuAction { kind, params })
        })
        .collect()
}

/// The first offered item of a kind.
pub fn pick(actions: &[MenuAction], kind: Kind) -> Option<&MenuAction> {
    actions.iter().find(|a| a.kind == kind)
}

fn urlencode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

fn fail(e: ApiError) -> String {
    e.message
}

/// Message text of a failed answer, with the expiry wording for a refused session.
fn error_of(status: u16, body: &Value) -> String {
    match auth::error_message(body) {
        Some(m) => m,
        None if status == 401 => auth::EXPIRED_MESSAGE.into(),
        None => format!("YouTube answered {status}"),
    }
}

/// Run `kind` on the message whose context-menu params are `menu`. The error is the text to show.
pub async fn moderate(menu: &str, kind: Kind) -> Result<(), String> {
    if !auth::is_logged_in() {
        return Err(auth::logged_out_reason().into());
    }
    let version = auth::client_version();
    let key = auth::api_key();
    let mut url = format!("https://www.youtube.com/youtubei/v1/live_chat/get_item_context_menu?params={}&prettyPrint=false", urlencode(menu));
    if !key.is_empty() {
        url.push_str(&format!("&key={}", urlencode(&key)));
    }
    // a 403 here can be "not a moderator", so only 401 ends the session
    let (status, body) = auth::call(&url, &auth::with_context(&version, json!({})), false).await.map_err(fail)?;
    if !(200..300).contains(&status) {
        return Err(if status == 403 { NOT_MODERATOR.to_string() } else { error_of(status, &body) });
    }
    let actions = parse_menu(&body);
    if actions.is_empty() {
        return Err(NOT_MODERATOR.into());
    }
    let Some(chosen) = pick(&actions, kind) else { return Err("YouTube does not offer that action for this message".into()) };
    let mut url = "https://www.youtube.com/youtubei/v1/live_chat/moderate?prettyPrint=false".to_string();
    if !key.is_empty() {
        url.push_str(&format!("&key={}", urlencode(&key)));
    }
    let (status, body) = auth::call(&url, &auth::with_context(&version, json!({"params": chosen.params})), false).await.map_err(fail)?;
    if (200..300).contains(&status) && auth::error_message(&body).is_none() {
        Ok(())
    } else {
        Err(if status == 403 { NOT_MODERATOR.to_string() } else { error_of(status, &body) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(text: &str, icon: &str, endpoint: Value) -> Value {
        json!({"menuServiceItemRenderer": {"text": {"runs": [{"text": text}]}, "icon": {"iconType": icon}, "serviceEndpoint": endpoint}})
    }

    fn fixture() -> Value {
        json!({"liveChatItemContextMenuSupportedRenderers": {"menuRenderer": {"items": [
            item("Remove", "DELETE", json!({"moderateLiveChatEndpoint": {"params": "P_DEL"}})),
            item("Put user in timeout", "HOURGLASS", json!({"moderateLiveChatEndpoint": {"params": "P_TO"}})),
            // hide user asks for confirmation first: the endpoint sits inside a dialog
            item("Hide user on this channel", "REMOVE_CIRCLE", json!({"signalServiceEndpoint": {"actions": [{"openPopupAction": {"popup": {"confirmDialogRenderer": {"confirmButton": {"buttonRenderer": {"serviceEndpoint": {"moderateLiveChatEndpoint": {"params": "P_BAN"}}}}}}}}]}})),
            item("Report", "FLAG", json!({"getReportFormEndpoint": {"params": "R"}})),
            item("Unhide user", "ADD_CIRCLE", json!({"moderateLiveChatEndpoint": {"params": "P_UNBAN"}})),
        ]}}})
    }

    #[test]
    fn menu_items_are_classified() {
        let a = parse_menu(&fixture());
        assert_eq!(a, vec![
            MenuAction { kind: Kind::Delete, params: "P_DEL".into() },
            MenuAction { kind: Kind::Timeout, params: "P_TO".into() },
            MenuAction { kind: Kind::Ban, params: "P_BAN".into() },
        ]);
        assert_eq!(pick(&a, Kind::Timeout).unwrap().params, "P_TO");
    }

    #[test]
    fn viewer_menu_has_nothing_to_moderate() {
        let viewer = json!({"liveChatItemContextMenuSupportedRenderers": {"menuRenderer": {"items": [item("Report", "FLAG", json!({"getReportFormEndpoint": {"params": "R"}}))]}}});
        assert!(parse_menu(&viewer).is_empty());
        assert!(parse_menu(&Value::Null).is_empty());
    }

    #[test]
    fn icon_decides_when_text_does_not() {
        assert_eq!(classify("", "HOURGLASS"), Some(Kind::Timeout));
        assert_eq!(classify("Something", "DELETE"), Some(Kind::Delete));
        assert_eq!(classify("Something", "UNKNOWN"), None);
        assert_eq!(classify("Remove timeout", "HOURGLASS"), None);
        assert_eq!(Kind::parse("ban"), Some(Kind::Ban));
        assert_eq!(Kind::parse("x"), None);
    }
}
