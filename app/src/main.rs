// Sem a janela preta do terminal no Windows
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod arts;
mod audio;
mod capture;
mod eq;
mod glass_gl;
mod log;
mod lyric_view;
mod lyrics;
mod player;
mod prefs;
mod proto;
mod room;
mod search;
mod theme;

fn main() -> eframe::Result<()> {
    std::panic::set_hook(Box::new(|p| crate::log::log(format!("travou: {p}"))));
    log::log(format!("Noir Music {} abrindo", env!("CARGO_PKG_VERSION")));
    // NM_TESTE=1 NM_AUTO=join:CODIGO: roda o roteiro de controles sem janela e sai
    if std::env::var_os("NM_TESTE").is_some() {
        if let Some(code) = std::env::var("NM_AUTO").ok().and_then(|a| a.strip_prefix("join:").map(String::from)) {
            let mut p = prefs::Prefs::load();
            p.name = "Teste".into();
            p.volume = 0.0;
            let s = room::Session::join(&p, code, std::sync::Arc::new(|| {}));
            s.run_test();
            drop(s);
            std::thread::sleep(std::time::Duration::from_millis(500));
            return Ok(());
        }
    }
    let size = std::env::var("NM_SIZE")
        .ok()
        .and_then(|s| s.split_once('x').and_then(|(w, h)| Some([w.parse().ok()?, h.parse().ok()?])))
        .unwrap_or([1080.0, 700.0]);
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Noir Music")
            .with_icon(app::icon())
            .with_inner_size(size)
            .with_min_inner_size([860.0, 580.0])
            // Mac: o app ocupa a janela toda (os três botões ficam por cima, sem barra de título)
            .with_fullsize_content_view(true)
            .with_title_shown(false)
            .with_titlebar_shown(false),
        ..Default::default()
    };
    eframe::run_native("Noir Music", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
