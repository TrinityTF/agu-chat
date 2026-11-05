#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod images;
mod irc;
mod net;
mod sys;

use std::sync::Arc;

fn main() -> eframe::Result {
    let mut channels: Vec<String> = std::env::args()
        .skip(1)
        .map(|a| a.trim_start_matches('#').to_lowercase())
        .filter(|a| !a.is_empty())
        .collect();
    if channels.is_empty() {
        channels.push("a_g_u__".to_string());
    }
    let url = std::env::var("AGU_IRC_URL").unwrap_or_else(|_| "wss://irc-ws.chat.twitch.tv:443".to_string());
    logf!("starting: channels={:?} irc={}", channels, url);

    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("agu-net")
            .build()
            .expect("tokio runtime"),
    );

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Agu Chat (native prototype)")
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([480.0, 300.0]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "Agu Chat native",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, channels, runtime, url)))),
    )
}
