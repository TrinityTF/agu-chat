//! Port of src/renderer/chat/types.ts: the wire types of every platform's chat frames plus the unified `Row`
//! of the merged message list. Frames stay JSON across the bridge; the store converts them into these once.
//! `null` and missing fields both become the type's default (the JS read them leniently too).
use std::sync::Arc;

use agu_chat_core::protocol::Platform;
use serde::{Deserialize, Deserializer};

fn nd<'de, D: Deserializer<'de>, T: Deserialize<'de> + Default>(d: D) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

macro_rules! lenient {
    ($(#[$m:meta])* pub struct $name:ident { $($(#[$fm:meta])* pub $f:ident : $t:ty),* $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Debug, Default, Deserialize, PartialEq)]
        #[serde(default, rename_all = "camelCase")]
        pub struct $name { $($(#[$fm])* #[serde(deserialize_with = "nd")] pub $f: $t),* }
    };
}

lenient! { pub struct ChatUser { pub id: String, pub login: String, pub name: String, pub color: String } }

lenient! {
    /// `bits` is set (and `cost` None) for a Bits Power-up.
    pub struct RewardInfo { pub title: String, pub cost: Option<f64>, pub bits: Option<f64> }
}
lenient! { pub struct PinBy { pub id: String, pub login: String, pub name: String } }
lenient! { pub struct PinInfo { pub by: PinBy, pub starts_at: Option<String>, pub ends_at: Option<String> } }
lenient! { pub struct ReplyInfo { pub user: String, pub text: String } }

lenient! {
    pub struct ChatMsg {
        pub id: String,
        pub ts: i64,
        pub user: ChatUser,
        pub badges: String,
        /// IRC `badge-info`: "subscriber/14" is the months behind the subscriber badge
        pub badge_info: String,
        pub text: String,
        pub emotes: String,
        pub gifs: Option<String>,
        pub action: bool,
        pub first_msg: bool,
        pub reply: Option<ReplyInfo>,
        pub reward_id: Option<String>,
        pub msg_id: Option<String>,
        pub reward: Option<RewardInfo>,
        pub bits: Option<f64>,
        pub deleted: bool,
        pub returning: bool,
        pub pin: Option<PinInfo>,
    }
}

lenient! {
    pub struct Notice {
        pub id: String,
        pub ts: i64,
        pub system: String,
        pub msg_id: String,
        pub source_msg_id: Option<String>,
        pub gift_id: Option<String>,
        pub gift_count: Option<f64>,
        pub reward: Option<RewardInfo>,
        pub user: ChatUser,
        pub text: String,
        pub emotes: Option<String>,
    }
}

lenient! { pub struct ModFeedEntry { pub ts: i64, pub feed_kind: String, pub text: String } }

lenient! { pub struct HeldUser { pub id: String, pub login: String, pub name: String } }
lenient! {
    pub struct HeldMsg {
        pub id: String,
        pub ts: i64,
        pub user: HeldUser,
        pub text: String,
        pub reason: String,
        pub category: Option<String>,
        pub level: Option<f64>,
        pub status: String,
        pub resolved_by: Option<String>,
    }
}

lenient! {
    pub struct SuspiciousUser {
        pub id: String,
        pub login: String,
        pub name: String,
        pub status: String,
        pub types: Vec<String>,
        pub evaluation: Option<String>,
        pub shared_ban_channels: f64,
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EsubStatus {
    pub active: bool,
    pub reason: Option<String>,
}

// ---- co-chats -------------------------------------------------------------------------------

lenient! { pub struct RumbleUser { pub id: String, pub login: String, pub name: String, pub color: String, pub avatar: String } }
lenient! { pub struct RumbleChannel { pub id: String, pub name: String, pub avatar: String } }
lenient! { pub struct Rant { pub cents: f64, pub duration: f64, pub expires_at: f64 } }
lenient! {
    pub struct RumbleMsg {
        pub id: String,
        pub ts: i64,
        pub user: RumbleUser,
        pub badges: Vec<String>,
        pub text: String,
        pub emotes: String,
        pub action: bool,
        pub first_msg: bool,
        pub is_follower: bool,
        pub channel: Option<RumbleChannel>,
        pub rant: Option<Rant>,
        pub notice: Option<String>,
        pub deleted: bool,
    }
}
/// id -> { icons: {size: path}, label: {lang: text} }
pub type RumbleBadgeDefs = serde_json::Map<String, serde_json::Value>;
/// name -> { file }
pub type RumbleEmotes = serde_json::Map<String, serde_json::Value>;

lenient! { pub struct KickUser { pub id: String, pub login: String, pub name: String, pub color: String, pub avatar: String, pub verified: bool, pub anonymous: bool } }
lenient! { pub struct KickBadge { pub r#type: String, pub text: String, pub count: f64, pub url: Option<String> } }
lenient! { pub struct KickReply { pub id: String, pub text: String, pub name: String } }
lenient! {
    pub struct KickMsg {
        pub id: String,
        pub ts: i64,
        pub user: KickUser,
        pub badges: Vec<KickBadge>,
        pub text: String,
        pub reply: Option<KickReply>,
        pub action: bool,
        pub notice: Option<String>,
        pub deleted: bool,
    }
}

lenient! { pub struct YouTubePart { pub t: String, pub v: String, pub u: String, pub a: String, pub custom: bool } }
lenient! { pub struct YouTubeBadge { pub label: String, pub icon: String, pub url: String } }
lenient! { pub struct YouTubeUser { pub id: String, pub name: String, pub avatar: String } }
lenient! { pub struct YouTubePaid { pub amount: String, pub bg: String, pub header_bg: String, pub sticker: bool } }
lenient! {
    pub struct YouTubeMsg {
        pub id: String,
        pub ts: i64,
        pub user: YouTubeUser,
        pub badges: Vec<YouTubeBadge>,
        pub parts: Vec<YouTubePart>,
        pub text: String,
        pub paid: Option<YouTubePaid>,
        pub notice: Option<String>,
    }
}

lenient! { pub struct XUser { pub id: String, pub name: String, pub login: String, pub avatar: String, pub twitter_id: String } }
lenient! { pub struct XMsg { pub id: String, pub ts: i64, pub user: XUser, pub text: String } }

// ---- the merged list --------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum RowBody {
    Msg(ChatMsg),
    Notice(Notice),
    ModFeed(ModFeedEntry),
    Automod(HeldMsg),
    Rumble(RumbleMsg),
    Kick(KickMsg),
    YouTube(YouTubeMsg),
    X(XMsg),
}

/// One line of the message list. `sk` is the source key `${platform}:${channel}`.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub key: String,
    pub ts: i64,
    pub sk: String,
    pub platform: Platform,
    /// arrival counter, stable under trimming: alternating row shading keys off it
    pub ord: u64,
    /// came in with a history / snapshot batch, not live: never alerts or marks a tab
    pub backlog: bool,
    pub body: RowBody,
}

pub type RowRef = Arc<Row>;

/// The person behind a row (user card / recent messages).
#[derive(Clone, Debug, Default)]
pub struct RowUser {
    pub id: String,
    pub login: String,
    pub name: String,
}
