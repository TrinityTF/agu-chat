//! Port of src/renderer/chat/ (round 2): the chat view.
pub mod alerts;
pub mod chat_list;
pub mod chat_pane;
pub use agu_chat_model::chat_text;
pub use agu_chat_model::highlights;
pub mod hype_train;
pub mod input;
pub use agu_chat_model::ignores;
pub mod images;
pub mod inline;
pub mod link_card;
pub use agu_chat_model::mentions;
pub use agu_chat_model::mod_actions;
pub use agu_chat_model::mod_buttons;
pub use agu_chat_model::mod_caps;
pub mod mod_popover;
pub mod popout;
pub mod previews;
pub use agu_chat_model::net;
pub mod pane_render;
pub mod paint_text;
pub mod rows;
pub use agu_chat_model::search;
pub mod select;
pub mod services;
pub use agu_chat_model::store;
pub use agu_chat_model::thread;
pub mod twitch;
pub use agu_chat_model::types;
pub mod ui_util;
pub mod card_web;
pub mod user_card;

/// `Pixels` as a plain number (its field is private).
pub trait PxF {
    fn f(self) -> f32;
}
impl PxF for gpui::Pixels {
    fn f(self) -> f32 {
        f32::from(self)
    }
}
