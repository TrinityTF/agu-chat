//! Agu Chat's chat logic without any UI: the row store, message parts (emotes, mentions, links), badges, emote sets,
//! highlights, ignores, moderation commands. The desktop (GPUI) app and the Android app both draw from these.
pub mod alerts;
pub mod card_web;
pub mod channel_modes;
pub mod chat_text;
pub mod highlights;
pub mod hype_train;
pub mod ignores;
pub mod input;
pub mod mod_actions;
pub mod pins;
pub mod mod_buttons;
pub mod mentions;
pub mod mod_caps;
pub mod mod_row;
pub mod net;
pub mod search;
pub mod settings_text;
pub mod shown;
pub mod store;
pub mod stream_link;
pub mod thread;
pub mod twitch;
pub mod types;
pub mod user_card;
