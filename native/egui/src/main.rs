#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;

use agu_chat_core::logf;

fn main() -> eframe::Result {
    let channels = agu_chat_core::cli_channels();
    let url = agu_chat_core::irc_url();
    logf!("starting: channels={:?} irc={}", channels, url);
    let runtime = agu_chat_core::make_runtime();

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
