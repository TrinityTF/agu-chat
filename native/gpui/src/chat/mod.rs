//! Port of src/renderer/chat/ (round 2): the chat view.
pub mod alerts;
pub mod chat_pane;
pub mod chat_text;
pub mod highlights;
pub mod ignores;
pub mod images;
pub mod inline;
pub mod mod_actions;
pub mod mod_buttons;
pub mod mod_caps;
pub mod net;
pub mod pane_render;
pub mod rows;
pub mod search;
pub mod services;
pub mod store;
pub mod twitch;
pub mod types;
pub mod ui_util;

/// `Pixels` as a plain number (its field is private).
pub trait PxF {
    fn f(self) -> f32;
}
impl PxF for gpui::Pixels {
    fn f(self) -> f32 {
        f32::from(self)
    }
}
