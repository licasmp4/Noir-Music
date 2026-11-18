//! Cara do Jam, a mesma do Noir: preto e cinza, branco no principal, botões chapados (sem brilho
//! nem degradê), vidro só onde tem imagem atrás. Fonte da Apple no Mac, Inter no Windows.

use std::sync::Arc;

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontId, Pos2, Rect, Shape, Stroke, epaint::Mesh};

pub const BG: Color32 = Color32::from_rgb(0, 0, 0);
pub const TEXT: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
pub const DIM: Color32 = Color32::from_rgb(0xC2, 0xC2, 0xC8);
pub const FAINT: Color32 = Color32::from_rgb(0x8E, 0x8E, 0x94);
pub const LIVE: Color32 = Color32::from_rgb(0xFF, 0x45, 0x3A);
pub const GOOD: Color32 = Color32::from_rgb(0x30, 0xD1, 0x58);
/// Superfície chapada (coluna da direita, campos)
pub const SURFACE: Color32 = Color32::from_rgb(0x0E, 0x0E, 0x10);

pub fn chip() -> Color32 {
    Color32::from_white_alpha(20)
}
pub fn chip_hover() -> Color32 {
    Color32::from_white_alpha(34)
}
pub fn hairline() -> Color32 {
    Color32::from_white_alpha(16)
}

pub mod icon {
    pub const PLAY: &str = "\u{E3D0}";
    pub const PAUSE: &str = "\u{E39E}";
    pub const NEXT: &str = "\u{E5A6}";
    pub const PREV: &str = "\u{E5A4}";
    pub const PLUS: &str = "\u{E3D4}";
    pub const X: &str = "\u{E4F6}";
    pub const SEARCH: &str = "\u{E30C}";
    pub const SPEAKER: &str = "\u{E44A}";
    pub const SPEAKER_LOW: &str = "\u{E44C}";
    pub const SPEAKER_X: &str = "\u{E45C}";
    pub const COPY: &str = "\u{E1CA}";
    pub const CHECK: &str = "\u{E182}";
    pub const USERS: &str = "\u{E68E}";
    pub const SIGN_OUT: &str = "\u{E42A}";
    pub const GEAR: &str = "\u{E270}";
    pub const NOTE: &str = "\u{E33C}";
    pub const NOTES: &str = "\u{E340}";
    pub const QUEUE: &str = "\u{E6AC}";
    pub const TOP: &str = "\u{E066}";
    pub const SPOTIFY: &str = "\u{E66E}";
    pub const APPLE: &str = "\u{E516}";
    pub const KEY: &str = "\u{E2D6}";
    pub const WARN: &str = "\u{E4E2}";
    pub const VINYL: &str = "\u{ECAC}";
    pub const HEADPHONES: &str = "\u{E2A6}";
    pub const EXPAND: &str = "\u{E1D0}";
    pub const SHRINK: &str = "\u{E1CE}";
    pub const LYRICS: &str = "\u{E660}";
    pub const ARROW_RIGHT: &str = "\u{E06C}";
}

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let add = |f: &mut FontDefinitions, name: &str, bytes: &'static [u8]| {
        f.font_data.insert(name.into(), Arc::new(FontData::from_static(bytes)));
    };
    add(&mut fonts, "regular", include_bytes!("../assets/Inter-Regular.ttf"));
    add(&mut fonts, "semi", include_bytes!("../assets/Inter-SemiBold.ttf"));
    add(&mut fonts, "display", include_bytes!("../assets/InterDisplay-Bold.ttf"));
    add(&mut fonts, "medium", include_bytes!("../assets/Inter-Medium.ttf"));
    add(&mut fonts, "phosphor", include_bytes!("../assets/Phosphor.ttf"));
    add(&mut fonts, "phosphor-fill", include_bytes!("../assets/Phosphor-Fill.ttf"));
    // No Mac, a fonte do sistema (SF) em cada peso. A licença dela só vale em aparelho da Apple
    #[cfg(target_os = "macos")]
    if let Ok(sf) = std::fs::read("/System/Library/Fonts/SFNS.ttf") {
        use egui::epaint::text::{FontTweak, VariationCoords};
        let sf: Arc<[u8]> = sf.into();
        for (name, wght, opsz) in [("regular", 400.0, 14.0), ("semi", 590.0, 14.0), ("display", 700.0, 28.0), ("medium", 510.0, 13.0)] {
            let mut data = FontData::from_blob(Arc::new(sf.clone()), 0);
            data.tweak = FontTweak { coords: VariationCoords::new([("wght", wght), ("opsz", opsz)]), ..Default::default() };
            fonts.font_data.insert(name.into(), Arc::new(data));
        }
    }
    let fallback: Vec<String> = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let family = |main: &str, icons: &str| -> Vec<String> {
        let mut v = vec![main.to_string(), icons.to_string()];
        v.extend(fallback.iter().cloned());
        v
    };
    fonts.families.insert(FontFamily::Proportional, family("regular", "phosphor"));
    fonts.families.insert(FontFamily::Monospace, family("regular", "phosphor"));
    fonts.families.insert(FontFamily::Name("semi".into()), family("semi", "phosphor"));
    fonts.families.insert(FontFamily::Name("display".into()), family("display", "phosphor-fill"));
    fonts.families.insert(FontFamily::Name("medium".into()), family("medium", "phosphor"));
    fonts.families.insert(FontFamily::Name("fill".into()), family("phosphor-fill", "phosphor"));
    ctx.set_fonts(fonts);

    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = Color32::from_rgb(0x1C, 0x1C, 0x1E);
    v.extreme_bg_color = Color32::TRANSPARENT;
    v.selection.bg_fill = Color32::from_white_alpha(60);
    v.selection.stroke = Stroke::new(1.0, TEXT);
    v.text_cursor.stroke = Stroke::new(2.0, TEXT);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.active.fg_stroke = Stroke::new(1.0, TEXT);
    ctx.set_visuals(v);
}

pub fn regular(size: f32) -> FontId {
    FontId::proportional(size)
}
pub fn semi(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semi".into()))
}
pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("medium".into()))
}
pub fn display(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("display".into()))
}
pub fn fill(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("fill".into()))
}

/// Luz redonda que some na borda (o "holofote" do Noir)
pub fn glow(painter: &egui::Painter, center: Pos2, radius: egui::Vec2, color: Color32) {
    let mut mesh = Mesh::default();
    mesh.colored_vertex(center, color);
    let n = 72;
    for i in 0..=n {
        let a = i as f32 / n as f32 * std::f32::consts::TAU;
        mesh.colored_vertex(center + egui::vec2(a.cos() * radius.x, a.sin() * radius.y), Color32::TRANSPARENT);
    }
    for i in 1..=n as u32 {
        mesh.add_triangle(0, i, i + 1);
    }
    painter.add(Shape::mesh(mesh));
}

/// Luz em anéis (mais macia que um degradê só, sem a "quina" no meio)
pub fn spotlight(painter: &egui::Painter, center: Pos2, radius: egui::Vec2, color: Color32) {
    for k in [1.0f32, 0.66, 0.4] {
        let c = color.gamma_multiply(0.45);
        glow(painter, center, radius * k, c);
    }
}

/// Texto que não cabe ganha "…"
pub fn fit(painter: &egui::Painter, text: &str, font: FontId, max_w: f32) -> String {
    let w = |s: &str| painter.layout_no_wrap(s.to_string(), font.clone(), TEXT).size().x;
    if w(text) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let s: String = chars[..mid].iter().collect::<String>() + "…";
        if w(&s) <= max_w { lo = mid } else { hi = mid - 1 }
    }
    chars[..lo].iter().collect::<String>().trim_end().to_string() + "…"
}

pub fn time(s: f64) -> String {
    if !s.is_finite() || s < 0.0 {
        return "-:--".into();
    }
    let s = s as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Botão redondo ou quadrado chapado com um ícone. Devolve o clique.
pub fn icon_button(ui: &mut egui::Ui, rect: Rect, id: &str, glyph: &str, size: f32, enabled: bool) -> egui::Response {
    let resp = ui.interact(rect, ui.id().with(id), if enabled { egui::Sense::click() } else { egui::Sense::hover() });
    let hover = resp.hovered() && enabled;
    let press = resp.is_pointer_button_down_on() && enabled;
    let t = ui.ctx().animate_bool_with_time(ui.id().with(id).with("h"), hover, 0.12);
    if t > 0.0 {
        // Canopy 2: com o mouse, um quadrado de canto 14 com branco 10 %
        ui.painter().rect_filled(rect, 14.0, Color32::from_white_alpha((26.0 * t) as u8));
    }
    let scale = if press { 0.92 } else { 1.0 };
    let color = if enabled { TEXT } else { Color32::from_white_alpha(70) };
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, glyph, fill(size * scale), color);
    if enabled {
        resp.clone().on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// Botão em pílula, chapado: `primary` = branco com texto preto
pub fn pill(ui: &mut egui::Ui, rect: Rect, id: &str, label: &str, primary: bool, enabled: bool) -> egui::Response {
    let resp = ui.interact(rect, ui.id().with(id), if enabled { egui::Sense::click() } else { egui::Sense::hover() });
    let hover = resp.hovered() && enabled;
    let t = ui.ctx().animate_bool_with_time(ui.id().with(id).with("h"), hover, 0.12);
    let press = resp.is_pointer_button_down_on() && enabled;
    let r = if press { rect.shrink(1.0) } else { rect };
    let (bg, fg) = if !enabled {
        (Color32::from_white_alpha(14), Color32::from_white_alpha(80))
    } else if primary {
        (lerp(Color32::from_rgb(0xF5, 0xF5, 0xF7), TEXT, t), Color32::BLACK)
    } else {
        (lerp(chip(), chip_hover(), t), TEXT)
    };
    ui.painter().rect_filled(r, r.height() / 2.0, bg);
    ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, label, semi(14.0), fg);
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

pub fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()), m(a.a(), b.a()))
}

/// Iniciais pra bolinha de quem está na Jam
pub fn initials(name: &str) -> String {
    let mut it = name.split_whitespace();
    let a = it.next().and_then(|w| w.chars().next()).unwrap_or('?');
    match it.next().and_then(|w| w.chars().next()) {
        Some(b) => format!("{}{}", a.to_uppercase(), b.to_uppercase()),
        None => a.to_uppercase().to_string(),
    }
}

pub fn lerp_f(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
