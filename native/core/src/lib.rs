//! agu-chat-core: the Rust port of the Electron main process (src/main). See docs/GPUI-PORT.md.
//! `protocol` is the contract the UI codes against; `Backend` implements it.
pub mod backend;
pub mod highlight_rules;
pub mod images;
pub mod kick;
pub mod log;
pub mod net;
pub mod platforms;
pub mod protocol;
pub mod relay;
pub mod rumble;
pub mod safe_storage;
pub mod settings_sanitize;
pub mod sounds;
pub mod store;
pub mod sys;
pub mod twitch;
pub mod updater;
pub mod util;
pub mod ws;
pub mod x;
pub mod youtube;

pub use backend::{default_data_dir, AppMeta, Backend};

use std::sync::Arc;

/// Called from background threads whenever there is something new for the UI to pick up.
pub type Wake = Arc<dyn Fn() + Send + Sync>;
