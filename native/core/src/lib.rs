//! Everything both Agu Chat native frontends share: IRC parsing, networking, emote/badge maps,
//! image fetching + decoding, message tokenization and the channel model.
pub mod chat;
pub mod images;
pub mod irc;
pub mod maps;
pub mod net;
pub mod sys;
pub mod tokens;

use std::sync::Arc;

/// Channel names from argv (`#` stripped, lowercased); defaults to `a_g_u__`.
pub fn cli_channels() -> Vec<String> {
    let mut channels: Vec<String> = std::env::args()
        .skip(1)
        .map(|a| a.trim_start_matches('#').to_lowercase())
        .filter(|a| !a.is_empty())
        .collect();
    if channels.is_empty() {
        channels.push("a_g_u__".to_string());
    }
    channels
}

/// `AGU_IRC_URL` override (the local fake server) or the real Twitch endpoint.
pub fn irc_url() -> String {
    std::env::var("AGU_IRC_URL").unwrap_or_else(|_| "wss://irc-ws.chat.twitch.tv:443".to_string())
}

/// The background runtime that all networking and image decoding runs on.
pub fn make_runtime() -> Arc<tokio::runtime::Runtime> {
    Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("agu-net")
            .build()
            .expect("tokio runtime"),
    )
}
