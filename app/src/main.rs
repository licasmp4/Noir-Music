// Sem a janela preta do terminal no Windows
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod arts;
mod audio;
mod capture;
mod glass_gl;
mod log;
mod player;
mod prefs;
mod proto;
mod room;
mod search;
mod theme;

fn main() -> eframe::Result<()> {
    std::panic::set_hook(Box::new(|p| crate::log::log(format!("travou: {p}"))));
    log::log(format!("Jam {} abrindo", env!("CARGO_PKG_VERSION")));
    let size = std::env::var("JAM_SIZE")
        .ok()
        .and_then(|s| s.split_once('x').and_then(|(w, h)| Some([w.parse().ok()?, h.parse().ok()?])))
        .unwrap_or([1080.0, 700.0]);
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Jam")
            .with_icon(app::icon())
            .with_inner_size(size)
            .with_min_inner_size([860.0, 580.0])
            // Mac: o app ocupa a janela toda (os três botões ficam por cima, sem barra de título)
            .with_fullsize_content_view(true)
            .with_title_shown(false)
            .with_titlebar_shown(false),
        ..Default::default()
    };
    eframe::run_native("Jam", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
