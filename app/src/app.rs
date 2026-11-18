//! As telas: Início (abrir a minha Jam ou entrar com código) e a Jam (palco com a capa, o player
//! de vidro do Canopy 2 e a coluna da fila e da busca).

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, CornerRadius, Id, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

use crate::arts::Arts;
use crate::glass_gl::{self, Glass};
use crate::prefs::Prefs;
use crate::proto::{State, Track, source_name};
use crate::room::{Phase, Session};
use crate::theme::{self, DIM, FAINT, SURFACE, TEXT, icon};

pub fn icon() -> egui::IconData {
    match image::load_from_memory(include_bytes!("../assets/jam-256.png")).map(|i| i.to_rgba8()) {
        Ok(i) => egui::IconData { width: i.width(), height: i.height(), rgba: i.into_raw() },
        Err(_) => egui::IconData::default(),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Queue,
    Search,
}

pub struct App {
    prefs: Prefs,
    session: Option<Session>,
    arts: Arts,
    code: String,
    error: Option<String>,
    tab: Tab,
    query: String,
    query_at: Option<Instant>,
    sent_query: String,
    settings: bool,
    sp_id: String,
    sp_secret: String,
    sp_check: Option<Result<(), String>>,
    sp_checking: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    copied_at: Option<Instant>,
    muted: bool,
    /// Capa que estava antes (some devagar quando troca de música)
    prev_art: Option<(String, Instant)>,
    shown_art: String,
    /// Prévia sem rede (JAM_DEMO=1) e foto da tela (JAM_SHOT=arquivo.png)
    demo: Option<crate::room::Shared>,
    frame_no: u64,
    shot: Option<String>,
}

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl App {
    pub fn new(cc: &eframe::CreationContext) -> App {
        theme::install(&cc.egui_ctx);
        let prefs = Prefs::load();
        let mut arts = Arts::new();
        arts.repaint = Some(cc.egui_ctx.clone());
        let mut prefs = prefs;
        if let Some(v) = std::env::var("JAM_VOLUME").ok().and_then(|v| v.parse().ok()) {
            prefs.volume = v;
        }
        // teste sem clicar: JAM_AUTO=host abre a minha Jam; JAM_AUTO=join:CODIGO entra
        let rp = Self::repaint_fn(&cc.egui_ctx);
        let session = match std::env::var("JAM_AUTO").ok() {
            Some(a) if a == "host" => Some(Session::open(&prefs, rp)),
            Some(a) if a.starts_with("join:") => Some(Session::join(&prefs, a[5..].to_string(), rp)),
            _ => None,
        };
        App {
            code: prefs.last_code.clone(),
            sp_id: prefs.spotify_id.clone(),
            sp_secret: prefs.spotify_secret.clone(),
            prefs,
            session,
            arts,
            error: None,
            tab: if std::env::var("JAM_DEMO_ABA").as_deref() == Ok("busca") { Tab::Search } else { Tab::Queue },
            query: String::new(),
            query_at: None,
            sent_query: String::new(),
            settings: false,
            sp_check: None,
            sp_checking: None,
            copied_at: None,
            muted: false,
            prev_art: None,
            shown_art: String::new(),
            demo: crate::app::demo(),
            frame_no: 0,
            shot: std::env::var("JAM_SHOT").ok(),
        }
    }

    fn repaint_fn(ctx: &egui::Context) -> crate::room::Repaint {
        let c = ctx.clone();
        Arc::new(move || c.request_repaint())
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.arts.pump(&ctx);
        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0.0, theme::BG);

        // a Jam fechou (quem tocava saiu, código errado): volta pro início com o motivo
        let closed = self.session.as_ref().and_then(|s| match lock(&s.shared).phase.clone() {
            Phase::Closed(m) => Some(m),
            _ => None,
        });
        if let Some(msg) = closed {
            self.error = Some(msg);
            self.session = None;
        }
        if self.session.is_some() || self.demo.is_some() {
            self.jam(ui, full);
        } else {
            self.home(ui, full);
        }
        if self.settings {
            self.settings_sheet(ui, full);
        }
        if let Some(rx) = &self.sp_checking {
            if let Ok(r) = rx.try_recv() {
                self.sp_check = Some(r);
                self.sp_checking = None;
            }
        }
        self.take_shot(&ctx);
        // o tempo da música anda: redesenha uma vez por segundo mesmo parado (e rápido animando)
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn clear_color(&self, _v: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.prefs.save();
        self.session = None;
        std::thread::sleep(Duration::from_millis(300));
    }
}

// MARK: - Início

impl App {
    fn home(&mut self, ui: &mut Ui, full: Rect) {
        let p = ui.painter().clone();
        // luz neutra vinda de cima, como o holofote do Noir sem capa
        theme::spotlight(&p, pos2(full.center().x, full.top() - 80.0), vec2(full.width() * 0.55, full.height() * 0.75), Color32::from_gray(54));

        let w = 400.0;
        let x = full.center().x - w / 2.0;
        let mut y = full.center().y - 250.0;
        // disco girando devagar atrás do nome
        let t = ui.input(|i| i.time) as f32;
        let c = pos2(full.center().x, y + 34.0);
        vinyl(&p, c, 30.0, t * 0.6);
        y += 84.0;
        p.text(pos2(full.center().x, y), Align2::CENTER_TOP, "Jam", theme::display(46.0), TEXT);
        y += 58.0;
        p.text(pos2(full.center().x, y), Align2::CENTER_TOP, "A mesma música, ao vivo, pra todo mundo da roda.", theme::regular(15.0), DIM);
        y += 48.0;

        // seu nome
        let name_r = Rect::from_min_size(pos2(x, y), vec2(w, 46.0));
        field(ui, name_r, "nome", &mut self.prefs.name, "Seu nome", None);
        y += 62.0;

        // abrir a minha (só no Mac)
        let can_host = crate::capture::CAN_HOST;
        let card = Rect::from_min_size(pos2(x, y), vec2(w, 124.0));
        p.rect_filled(card, 22.0, SURFACE);
        p.rect_stroke(card, 22.0, Stroke::new(1.0, theme::hairline()), StrokeKind::Inside);
        p.text(card.min + vec2(20.0, 18.0), Align2::LEFT_TOP, "Tocar pra galera", theme::semi(15.0), TEXT);
        let sub = if can_host { "Seu Spotify ou Apple Music, pra todo mundo ouvir." } else { "Por enquanto só dá pra tocar pelo Mac. Aqui você entra e ouve." };
        p.text(card.min + vec2(20.0, 42.0), Align2::LEFT_TOP, sub, theme::regular(13.0), FAINT);
        let btn = Rect::from_min_size(card.min + vec2(20.0, 72.0), vec2(w - 40.0, 36.0));
        let name_ok = !self.prefs.name.trim().is_empty();
        if theme::pill(ui, btn, "abrir", "Abrir minha Jam", true, can_host && name_ok).clicked() {
            self.prefs.save();
            self.error = None;
            self.session = Some(Session::open(&self.prefs, Self::repaint_fn(ui.ctx())));
        }
        y += 140.0;

        // entrar com o código de um amigo
        let card = Rect::from_min_size(pos2(x, y), vec2(w, 110.0));
        p.rect_filled(card, 22.0, SURFACE);
        p.rect_stroke(card, 22.0, Stroke::new(1.0, theme::hairline()), StrokeKind::Inside);
        p.text(card.min + vec2(20.0, 18.0), Align2::LEFT_TOP, "Entrar na Jam de um amigo", theme::semi(15.0), TEXT);
        let code_r = Rect::from_min_size(card.min + vec2(20.0, 50.0), vec2(w - 40.0 - 108.0, 40.0));
        let enter = field(ui, code_r, "codigo", &mut self.code, "JAM-XXXX-XXXX", Some(icon::HEADPHONES));
        let valid = jam_net::parse_cloud_code(&self.code).is_some() || jam_net::share_code::ShareCode::decode(self.code.trim()).is_some();
        let go = Rect::from_min_size(pos2(code_r.right() + 8.0, code_r.top() + 2.0), vec2(100.0, 36.0));
        if (theme::pill(ui, go, "entrar", "Entrar", false, valid && name_ok).clicked() || (enter && valid && name_ok)) && self.session.is_none() {
            self.prefs.last_code = self.code.trim().to_uppercase();
            self.prefs.save();
            self.error = None;
            self.session = Some(Session::join(&self.prefs, self.code.trim().to_string(), Self::repaint_fn(ui.ctx())));
        }
        y += 140.0;

        if let Some(e) = &self.error {
            p.text(pos2(full.center().x, y), Align2::CENTER_TOP, e, theme::regular(13.5), Color32::from_rgb(0xFF, 0x8A, 0x80));
        }
        // ajustes (busca do Spotify) no canto
        if can_host {
            let g = Rect::from_center_size(pos2(full.right() - 34.0, full.top() + 30.0), vec2(36.0, 36.0));
            if theme::icon_button(ui, g, "ajustes", icon::GEAR, 18.0, true).on_hover_text("Busca do Spotify").clicked() {
                self.settings = true;
            }
        }
    }
}

/// Campo de texto chapado. Devolve true quando apertou Enter
fn field(ui: &mut Ui, rect: Rect, id: &str, text: &mut String, hint: &str, glyph: Option<&str>) -> bool {
    let has_focus = ui.ctx().memory(|m| m.has_focus(Id::new(("campo", id))));
    let bg = if has_focus { Color32::from_white_alpha(26) } else { Color32::from_white_alpha(16) };
    ui.painter().rect_filled(rect, 12.0, bg);
    let mut inner = rect.shrink2(vec2(14.0, 0.0));
    if let Some(g) = glyph {
        ui.painter().text(pos2(inner.left() + 8.0, rect.center().y), Align2::CENTER_CENTER, g, theme::regular(16.0), FAINT);
        inner.min.x += 24.0;
    }
    let edit = egui::TextEdit::singleline(text)
        .id(Id::new(("campo", id)))
        .hint_text(egui::RichText::new(hint).color(Color32::from_white_alpha(90)))
        .font(theme::regular(15.0))
        .text_color(TEXT)
        .frame(egui::Frame::NONE)
        .vertical_align(egui::Align::Center)
        .margin(vec2(0.0, 0.0));
    let resp = ui.put(inner, edit);
    resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
}

/// Disco de vinil: o símbolo do app (chapado, cinza, com um brilho girando)
fn vinyl(p: &egui::Painter, c: Pos2, r: f32, angle: f32) {
    p.circle_filled(c, r, Color32::from_gray(22));
    for k in [0.86, 0.72, 0.58] {
        p.circle_stroke(c, r * k, Stroke::new(1.0, Color32::from_white_alpha(14)));
    }
    // brilho que gira
    let a = vec2(angle.cos(), angle.sin());
    p.line_segment([c + a * r * 0.45, c + a * r * 0.95], Stroke::new(2.0, Color32::from_white_alpha(40)));
    p.line_segment([c - a * r * 0.45, c - a * r * 0.95], Stroke::new(2.0, Color32::from_white_alpha(22)));
    p.circle_filled(c, r * 0.34, Color32::from_gray(235));
    p.circle_filled(c, r * 0.07, Color32::from_gray(22));
}

// MARK: - A Jam

struct View {
    state: State,
    state_age: f64,
    phase: Phase,
    code: Option<String>,
    people: Vec<String>,
    results: crate::room::Results,
    toast: Option<(String, Instant)>,
    capture_error: Option<String>,
    host: bool,
    pos: Option<(f64, f64, bool)>,
}

impl App {
    fn view(&self) -> View {
        let (sh, host, pos) = match (&self.session, &self.demo) {
            (Some(s), _) => {
                let v = clone_shared(&lock(&s.shared));
                (v, s.host, s.position())
            }
            (None, Some(d)) => {
                let n = d.state.now.clone();
                (clone_shared(d), true, n.map(|n| (n.pos, n.track.dur, n.playing)))
            }
            _ => unreachable!(),
        };
        View {
            state: sh.state,
            state_age: sh.state_at.elapsed().as_secs_f64(),
            phase: sh.phase,
            code: sh.code,
            people: sh.people,
            results: sh.results,
            toast: sh.toast,
            capture_error: sh.capture_error,
            host,
            pos,
        }
    }

    fn jam(&mut self, ui: &mut Ui, full: Rect) {
        let v = self.view();
        let _ = v.state_age;
        let top = 58.0;
        let right_w = if full.width() >= 1040.0 { 380.0 } else { 340.0 };
        let stage = Rect::from_min_max(pos2(full.left(), full.top() + top), pos2(full.right() - right_w - 16.0, full.bottom()));
        let side = Rect::from_min_max(pos2(stage.right(), full.top() + top), pos2(full.right() - 16.0, full.bottom() - 16.0));

        self.stage(ui, stage, &v);
        self.top_bar(ui, full, &v);
        self.side(ui, side, &v);
        if let Some((t, at)) = &v.toast {
            let age = at.elapsed().as_secs_f32();
            if age < 3.2 {
                let a = (age / 0.2).min(1.0).min((3.2 - age) / 0.3);
                let p = ui.painter();
                let g = p.layout_no_wrap(t.clone(), theme::medium(13.5), TEXT);
                let r = Rect::from_center_size(pos2(stage.center().x, stage.bottom() - 30.0 + (1.0 - a) * 8.0), vec2(g.size().x + 36.0, 36.0));
                p.rect_filled(r, 18.0, Color32::from_rgb(0x2A, 0x2A, 0x2E).gamma_multiply(a));
                p.text(r.center(), Align2::CENTER_CENTER, t, theme::medium(13.5), TEXT.gamma_multiply(a));
                ui.ctx().request_repaint();
            }
        }
        // abrindo / entrando
        if matches!(v.phase, Phase::Opening | Phase::Joining) && self.demo.is_none() {
            let msg = if v.host {
                if v.code.is_none() { "Abrindo a sua Jam…" } else { "Quase lá…" }
            } else {
                "Entrando na Jam…"
            };
            let p = ui.painter();
            let r = Rect::from_center_size(pos2(stage.center().x, stage.top() + 24.0), vec2(200.0, 32.0));
            p.rect_filled(r, 16.0, theme::chip());
            p.text(r.center(), Align2::CENTER_CENTER, msg, theme::medium(13.0), DIM);
        }
    }

    fn top_bar(&mut self, ui: &mut Ui, full: Rect, v: &View) {
        let p = ui.painter().clone();
        let y = full.top() + 29.0;
        // os três botões do Mac ficam à esquerda
        let mut x = full.left() + if cfg!(target_os = "macos") { 86.0 } else { 20.0 };
        let wm = p.text(pos2(x, y), Align2::LEFT_CENTER, "Jam", theme::display(19.0), TEXT);
        x = wm.right() + 14.0;
        if let Some(code) = &v.code {
            let copied = self.copied_at.is_some_and(|t| t.elapsed() < Duration::from_millis(1600));
            let label = if copied { "Copiado".to_string() } else { code.clone() };
            let g = p.layout_no_wrap(label.clone(), theme::medium(13.0), TEXT);
            let r = Rect::from_min_size(pos2(x, y - 15.0), vec2(g.size().x + 46.0, 30.0));
            let resp = ui.interact(r, Id::new("codigo-topo"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
            let fill = if resp.hovered() { theme::chip_hover() } else { theme::chip() };
            p.rect_filled(r, 15.0, fill);
            p.text(pos2(r.left() + 14.0, y), Align2::LEFT_CENTER, label, theme::medium(13.0), TEXT);
            p.text(pos2(r.right() - 18.0, y), Align2::CENTER_CENTER, if copied { icon::CHECK } else { icon::COPY }, theme::regular(14.0), DIM);
            if resp.on_hover_text("Copiar o código pra mandar pros amigos").clicked() {
                ui.ctx().copy_text(code.clone());
                self.copied_at = Some(Instant::now());
            }
            if v.phase == Phase::Live || self.demo.is_some() {
                // bolinha verde: a Jam está no ar
                let live = pos2(r.right() + 16.0, y);
                let pulse = (ui.input(|i| i.time) as f32 * 2.2).sin() * 0.5 + 0.5;
                p.circle_filled(live, 4.0 + pulse * 3.0, theme::GOOD.gamma_multiply(0.18));
                p.circle_filled(live, 3.5, theme::GOOD);
            }
        }
        // à direita: Sair, ajustes e quem está ouvindo
        let mut rx = full.right() - 16.0;
        let out = Rect::from_min_max(pos2(rx - 84.0, y - 16.0), pos2(rx, y + 16.0));
        if theme::pill(ui, out, "sair", if v.host { "Fechar" } else { "Sair" }, false, true).clicked() {
            self.session = None;
            self.demo = None;
            self.error = None;
            return;
        }
        rx = out.left() - 8.0;
        if v.host {
            let g = Rect::from_center_size(pos2(rx - 18.0, y), vec2(34.0, 34.0));
            if theme::icon_button(ui, g, "ajustes-topo", icon::GEAR, 17.0, true).on_hover_text("Busca do Spotify").clicked() {
                self.settings = true;
            }
            rx = g.left() - 6.0;
        }
        let people: Vec<&String> = v.people.iter().collect();
        let n = people.len();
        if n > 0 {
            let shown = n.min(5);
            let d = 26.0;
            let step = 19.0;
            let w = d + step * (shown as f32 - 1.0);
            let label = if n == 1 { "só você".to_string() } else { format!("{n} ouvindo") };
            let lg = p.layout_no_wrap(label.clone(), theme::medium(12.5), DIM);
            let lx = rx - 12.0 - lg.size().x;
            p.text(pos2(lx, y), Align2::LEFT_CENTER, label, theme::medium(12.5), DIM);
            let start = lx - 10.0 - w;
            for (i, name) in people.iter().take(shown).enumerate() {
                let c = pos2(start + d / 2.0 + i as f32 * step, y);
                p.circle_filled(c, d / 2.0 + 2.0, theme::BG);
                let shade = 52 + (i as u8 * 14) % 50;
                p.circle_filled(c, d / 2.0, Color32::from_gray(shade));
                p.text(c, Align2::CENTER_CENTER, theme::initials(name), theme::semi(10.5), TEXT);
            }
            let hover = Rect::from_min_max(pos2(start, y - 14.0), pos2(rx, y + 14.0));
            ui.interact(hover, Id::new("quem"), Sense::hover()).on_hover_text(people.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n"));
        }
    }

    fn stage(&mut self, ui: &mut Ui, stage: Rect, v: &View) {
        let p = ui.painter().clone();
        let now = v.state.now.as_ref();
        let art_url = now.map(|n| n.track.art.clone()).unwrap_or_default();
        if art_url != self.shown_art {
            self.prev_art = Some((std::mem::take(&mut self.shown_art), Instant::now()));
            self.shown_art = art_url.clone();
        }
        let (art_tex, art_color) = match self.arts.get(&art_url) {
            Some(a) => (Some(a.tex.clone()), a.color),
            None => (None, Color32::from_gray(90)),
        };
        // holofote com só 22 % da cor da capa (o resto cinza): o fundo continua preto e cinza
        let gray = Color32::from_gray(((art_color.r() as u32 + art_color.g() as u32 + art_color.b() as u32) / 3) as u8);
        let tint = theme::lerp(gray, art_color, 0.22);
        let light = theme::lerp(Color32::from_gray(40), tint, 0.85);
        let light = ui.ctx().animate_value_with_time(Id::new("luz-r"), light.r() as f32, 0.6) as u8;
        let lg = ui.ctx().animate_value_with_time(Id::new("luz-g"), tint.g() as f32 * 0.85, 0.6) as u8;
        let lb = ui.ctx().animate_value_with_time(Id::new("luz-b"), tint.b() as f32 * 0.85, 0.6) as u8;
        theme::spotlight(&p, pos2(stage.center().x, stage.top() - 40.0), vec2(stage.width() * 0.62, stage.height() * 0.95), Color32::from_rgb(light, lg, lb));

        // capa + player: a capa grande e o vidro por cima da parte de baixo dela
        // o vidro fica em cima da parte de baixo da capa: a lente entorta a própria capa (Canopy 2)
        let cover = (stage.width() - 120.0).min(stage.height() - 70.0).clamp(300.0, 560.0);
        let inset = 14.0;
        let card_w = cover - inset * 2.0;
        let k = card_w / 320.0;
        let card_h = 176.0 * k.min(1.18);
        let top = stage.center().y - cover / 2.0 - 4.0;
        let cover_r = Rect::from_min_size(pos2(stage.center().x - cover / 2.0, top), vec2(cover, cover));
        // aviso de permissão (quem toca)
        if let Some(err) = &v.capture_error {
            let r = Rect::from_center_size(pos2(stage.center().x, stage.top() + 26.0), vec2((stage.width() - 60.0).min(620.0), 40.0));
            p.rect_filled(r, 20.0, Color32::from_rgb(0x3A, 0x16, 0x14));
            p.text(pos2(r.left() + 18.0, r.center().y), Align2::LEFT_CENTER, icon::WARN, theme::regular(16.0), theme::LIVE);
            let txt = theme::fit(&p, err, theme::medium(12.5), r.width() - 170.0);
            p.text(pos2(r.left() + 40.0, r.center().y), Align2::LEFT_CENTER, txt, theme::medium(12.5), TEXT);
            let b = Rect::from_min_max(pos2(r.right() - 116.0, r.top() + 6.0), pos2(r.right() - 6.0, r.bottom() - 6.0));
            if theme::pill(ui, b, "permissao", "Abrir Ajustes", true, true).clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"));
            }
        }

        // a capa (a anterior some por baixo quando troca)
        let radius = 32.0 * k.min(1.15) + inset;
        let shadow = cover_r.translate(vec2(0.0, 18.0)).expand(6.0);
        for i in 0..6 {
            p.rect_filled(shadow.expand(i as f32 * 6.0), radius + i as f32 * 6.0, Color32::from_black_alpha(30));
        }
        if let Some((old, at)) = &self.prev_art {
            let t = at.elapsed().as_secs_f32() / 0.35;
            if t < 1.0 {
                if let Some(a) = self.arts.get(old) {
                    egui::Image::from_texture((a.tex.id(), cover_r.size())).corner_radius(radius).tint(Color32::WHITE.gamma_multiply(1.0 - t)).paint_at(ui, cover_r);
                }
                ui.ctx().request_repaint();
            }
        }
        match &art_tex {
            Some(tex) => {
                let t = self.prev_art.as_ref().map_or(1.0, |(_, at)| (at.elapsed().as_secs_f32() / 0.35).min(1.0));
                let s = 0.965 + 0.035 * ease(t);
                let r = Rect::from_center_size(cover_r.center(), cover_r.size() * s);
                egui::Image::from_texture((tex.id(), r.size())).corner_radius(radius).tint(Color32::WHITE.gamma_multiply(ease(t))).paint_at(ui, r);
            }
            None => {
                p.rect_filled(cover_r, radius, Color32::from_gray(24));
                let t = ui.input(|i| i.time) as f32;
                vinyl(&p, cover_r.center() - vec2(0.0, 26.0), cover * 0.22, if now.is_some_and(|n| n.playing) { t * 1.2 } else { 0.0 });
            }
        }

        // o player de vidro
        let card = Rect::from_min_size(pos2(stage.center().x - card_w / 2.0, cover_r.bottom() - inset - card_h), vec2(card_w, card_h));
        glass_gl::paint(&p, card, Glass::island(32.0 * k.min(1.15)));
        self.player_card(ui, card, k, v);
    }

    fn player_card(&mut self, ui: &mut Ui, card: Rect, k: f32, v: &View) {
        let p = ui.painter().clone();
        let at = |x: f32, y: f32| pos2(card.left() + x * k, card.top() + y * k.min(1.18));
        let now = v.state.now.as_ref();
        let src = v.state.source.as_str();
        let pad = 25.0 * k;
        // título e artista (Canopy 2: semibold 13,5 e 12, artista a 50 %)
        match now {
            Some(n) => {
                let maxw = card.width() - pad * 2.0;
                let by = (!n.track.by.is_empty()).then(|| format!("pedido por {}", n.track.by));
                let by_w = by.as_ref().map_or(0.0, |b| p.layout_no_wrap(b.clone(), theme::medium(11.5), FAINT).size().x + 12.0);
                let title = theme::fit(&p, &n.track.title, theme::semi(16.0), maxw - by_w);
                let tr = p.text(at(25.0, 34.0), Align2::LEFT_CENTER, title, theme::semi(16.0), TEXT);
                if let Some(b) = by {
                    p.text(pos2(card.right() - pad, tr.center().y), Align2::RIGHT_CENTER, b, theme::medium(11.5), FAINT);
                }
                let artist = theme::fit(&p, &n.track.artist, theme::regular(13.5), maxw);
                p.text(at(25.0, 55.0), Align2::LEFT_CENTER, artist, theme::regular(13.5), Color32::from_white_alpha(140));
            }
            None => {
                let msg = if src.is_empty() { "Nada tocando" } else { "Parado" };
                p.text(at(25.0, 34.0), Align2::LEFT_CENTER, msg, theme::semi(16.0), FAINT);
                let sub = if v.host {
                    "Dê play no Spotify ou no Apple Music".to_string()
                } else if v.state.host.is_empty() {
                    "Esperando quem toca…".to_string()
                } else {
                    format!("{} ainda não deu play", v.state.host)
                };
                p.text(at(25.0, 55.0), Align2::LEFT_CENTER, sub, theme::regular(13.5), Color32::from_white_alpha(110));
            }
        }

        // tempo e barra (y 94,5; altura 6, 8 com o mouse)
        let (pos, dur, playing) = v.pos.unwrap_or((0.0, 0.0, false));
        let bar = Rect::from_min_max(pos2(card.left() + 57.0 * k, at(0.0, 94.5).y - 4.0), pos2(card.left() + 263.0 * k, at(0.0, 94.5).y + 4.0));
        let bar_hit = bar.expand2(vec2(0.0, 8.0));
        let resp = ui.interact(bar_hit, Id::new("barra"), if now.is_some() { Sense::click_and_drag() } else { Sense::hover() });
        let hover = resp.hovered() || resp.dragged();
        let h = ui.ctx().animate_bool_with_time(Id::new("barra-h"), hover, 0.12) * 2.0 + 6.0;
        let track = Rect::from_center_size(bar.center(), vec2(bar.width(), h));
        let mut frac = if dur > 0.0 { (pos / dur) as f32 } else { 0.0 };
        if let Some(m) = resp.interact_pointer_pos().filter(|_| resp.dragged() || resp.clicked()) {
            frac = ((m.x - bar.left()) / bar.width()).clamp(0.0, 1.0);
        }
        if (resp.drag_stopped() || resp.clicked()) && dur > 0.0 {
            if let Some(s) = &self.session {
                s.seek(frac as f64 * dur);
            }
        }
        p.rect_filled(track, h / 2.0, Color32::from_white_alpha(41));
        let filled = Rect::from_min_max(track.min, pos2(track.left() + track.width() * frac.clamp(0.0, 1.0), track.bottom()));
        p.rect_filled(filled, h / 2.0, Color32::from_white_alpha(if hover { 230 } else { 128 }));
        let shown_pos = frac as f64 * dur;
        p.text(pos2(card.left() + 25.0 * k, track.center().y), Align2::LEFT_CENTER, theme::time(if now.is_some() { shown_pos } else { f64::NAN }), theme::medium(11.5), Color32::from_white_alpha(150));
        p.text(pos2(card.left() + 292.0 * k, track.center().y), Align2::RIGHT_CENTER, theme::time(if now.is_some() { (dur - shown_pos).max(0.0) } else { f64::NAN }), theme::medium(11.5), Color32::from_white_alpha(150));
        if resp.hovered() && now.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        // botões (y 133,5, a 58,5 um do outro): app, anterior, tocar, próxima, volume
        let by = at(0.0, 133.5).y;
        let bs = 40.0 * k.min(1.15);
        let has = now.is_some();
        let b = |x: f32| Rect::from_center_size(pos2(card.left() + x * k, by), vec2(bs, bs));
        // de onde vem a música (só mostra)
        let src_glyph = match src {
            "spotify" => icon::SPOTIFY,
            "music" => icon::APPLE,
            _ => icon::NOTES,
        };
        p.text(b(43.0).center(), Align2::CENTER_CENTER, src_glyph, theme::fill(19.0 * k.min(1.15)), Color32::from_white_alpha(if src.is_empty() { 60 } else { 150 }));
        ui.interact(b(43.0), Id::new("fonte"), Sense::hover()).on_hover_text(if src.is_empty() { "Nenhum app de música aberto".to_string() } else { format!("Tocando do {} de {}", source_name(src), v.state.host) });
        let side_a = if has && playing { 1.0 } else { 0.5 };
        if theme::icon_button(ui, b(101.5), "anterior", icon::PREV, 24.0 * k.min(1.15), has).clicked() {
            self.cmd("prev");
        }
        let _ = side_a;
        if theme::icon_button(ui, b(160.0), "tocar", if playing { icon::PAUSE } else { icon::PLAY }, 30.0 * k.min(1.15), has).clicked() {
            self.cmd("toggle");
        }
        if theme::icon_button(ui, b(218.5), "proxima", icon::NEXT, 24.0 * k.min(1.15), has).clicked() {
            self.cmd("next");
        }
        // volume de quem ouve (quem toca ouve direto do app de música)
        if !v.host {
            let g = if self.muted || self.prefs.volume <= 0.001 { icon::SPEAKER_X } else if self.prefs.volume < 0.5 { icon::SPEAKER_LOW } else { icon::SPEAKER };
            if theme::icon_button(ui, b(277.0), "mudo", g, 21.0 * k.min(1.15), true).on_hover_text(if self.muted { "Ligar o som" } else { "Tirar o som" }).clicked() {
                self.muted = !self.muted;
                self.apply_volume();
            }
        } else {
            let g = b(277.0);
            let n = v.state.queue.len();
            p.text(g.center(), Align2::CENTER_CENTER, icon::QUEUE, theme::fill(20.0 * k.min(1.15)), Color32::from_white_alpha(150));
            if n > 0 {
                let c = g.center() + vec2(10.0, -9.0) * k.min(1.15);
                p.circle_filled(c, 8.0, TEXT);
                p.text(c, Align2::CENTER_CENTER, n.min(99).to_string(), theme::semi(10.0), Color32::BLACK);
            }
            if ui.interact(g, Id::new("fila-botao"), Sense::click()).on_hover_text("Ver a fila").on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                self.tab = Tab::Queue;
            }
        }
    }

    fn cmd(&self, c: &str) {
        if let Some(s) = &self.session {
            s.cmd(c);
        }
    }

    fn apply_volume(&mut self) {
        if let Some(o) = self.session.as_ref().and_then(|s| s.output.as_ref()) {
            o.set_volume(if self.muted { 0.0 } else { self.prefs.volume });
        }
    }

    fn side(&mut self, ui: &mut Ui, side: Rect, v: &View) {
        let p = ui.painter().clone();
        p.rect_filled(side, 24.0, SURFACE);
        p.rect_stroke(side, 24.0, Stroke::new(1.0, theme::hairline()), StrokeKind::Inside);
        let inner = side.shrink(16.0);

        // abas: Fila | Buscar (seletor chapado, a escolhida branca)
        let seg = Rect::from_min_size(inner.min, vec2(inner.width(), 36.0));
        p.rect_filled(seg, 18.0, Color32::from_white_alpha(12));
        let half = seg.width() / 2.0;
        let qn = v.state.queue.len();
        for (i, (tab, label)) in [(Tab::Queue, if qn > 0 { format!("Fila · {qn}") } else { "Fila".to_string() }), (Tab::Search, "Buscar".to_string())].into_iter().enumerate() {
            let r = Rect::from_min_size(pos2(seg.left() + half * i as f32, seg.top()), vec2(half, seg.height())).shrink(3.0);
            let resp = ui.interact(r, Id::new(("aba", i)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
            if resp.clicked() {
                self.tab = tab;
            }
        }
        let t = ui.ctx().animate_value_with_time(Id::new("aba-x"), if self.tab == Tab::Queue { 0.0 } else { 1.0 }, 0.18);
        let sel = Rect::from_min_size(pos2(seg.left() + 3.0 + half * t, seg.top() + 3.0), vec2(half - 6.0, seg.height() - 6.0));
        p.rect_filled(sel, 15.0, Color32::from_rgb(0xF5, 0xF5, 0xF7));
        for (i, label) in [if qn > 0 { format!("Fila · {qn}") } else { "Fila".to_string() }, "Buscar".to_string()].into_iter().enumerate() {
            let c = pos2(seg.left() + half * (i as f32 + 0.5), seg.center().y);
            let on = (i as f32 - t).abs() < 0.5;
            p.text(c, Align2::CENTER_CENTER, label, theme::semi(13.0), if on { Color32::BLACK } else { DIM });
        }

        let body = Rect::from_min_max(pos2(inner.left(), seg.bottom() + 14.0), pos2(inner.right(), inner.bottom() - if v.host { 0.0 } else { 46.0 }));
        match self.tab {
            Tab::Queue => self.queue_list(ui, body, v),
            Tab::Search => self.search(ui, body, v),
        }

        // volume de quem ouve
        if !v.host {
            let row = Rect::from_min_max(pos2(inner.left(), inner.bottom() - 34.0), inner.max);
            p.line_segment([pos2(row.left(), row.top() - 8.0), pos2(row.right(), row.top() - 8.0)], Stroke::new(1.0, theme::hairline()));
            p.text(pos2(row.left() + 10.0, row.center().y), Align2::CENTER_CENTER, icon::SPEAKER_LOW, theme::fill(15.0), FAINT);
            p.text(pos2(row.right() - 10.0, row.center().y), Align2::CENTER_CENTER, icon::SPEAKER, theme::fill(15.0), FAINT);
            let bar = Rect::from_min_max(pos2(row.left() + 30.0, row.center().y - 3.0), pos2(row.right() - 30.0, row.center().y + 3.0));
            let resp = ui.interact(bar.expand2(vec2(6.0, 10.0)), Id::new("volume"), Sense::click_and_drag());
            if let Some(m) = resp.interact_pointer_pos().filter(|_| resp.dragged() || resp.clicked()) {
                self.prefs.volume = ((m.x - bar.left()) / bar.width()).clamp(0.0, 1.0);
                self.muted = false;
                self.apply_volume();
            }
            if resp.drag_stopped() || resp.clicked() {
                self.prefs.save();
            }
            let vol = if self.muted { 0.0 } else { self.prefs.volume };
            p.rect_filled(bar, 3.0, Color32::from_white_alpha(36));
            p.rect_filled(Rect::from_min_max(bar.min, pos2(bar.left() + bar.width() * vol, bar.bottom())), 3.0, TEXT);
            p.circle_filled(pos2(bar.left() + bar.width() * vol, bar.center().y), if resp.hovered() || resp.dragged() { 8.0 } else { 6.5 }, TEXT);
            if let Some(o) = self.session.as_ref().and_then(|s| s.output.as_ref()) {
                if let Some(e) = &o.error {
                    p.text(pos2(row.center().x, row.top() - 20.0), Align2::CENTER_CENTER, format!("Sem saída de som: {e}"), theme::regular(11.5), theme::LIVE);
                }
            }
        }
    }

    fn queue_list(&mut self, ui: &mut Ui, body: Rect, v: &View) {
        let p = ui.painter().clone();
        let mut y = body.top();
        if let Some(n) = &v.state.now {
            p.text(pos2(body.left() + 4.0, y), Align2::LEFT_TOP, "TOCANDO AGORA", theme::semi(11.0), FAINT);
            y += 22.0;
            let r = Rect::from_min_size(pos2(body.left(), y), vec2(body.width(), 58.0));
            self.track_row(ui, r, &n.track, RowKind::Now, 0);
            y += 66.0;
        }
        p.text(pos2(body.left() + 4.0, y), Align2::LEFT_TOP, "PRÓXIMAS", theme::semi(11.0), FAINT);
        y += 22.0;
        if v.state.queue.is_empty() {
            let r = Rect::from_min_max(pos2(body.left(), y), pos2(body.right(), (y + 150.0).min(body.bottom())));
            p.text(pos2(r.center().x, r.top() + 34.0), Align2::CENTER_CENTER, icon::QUEUE, theme::regular(26.0), Color32::from_white_alpha(70));
            p.text(pos2(r.center().x, r.top() + 70.0), Align2::CENTER_CENTER, "A fila está vazia", theme::semi(14.0), DIM);
            let hint = if v.state.search { "Busque uma música e aperte +" } else { "Quando alguém pôr música, ela aparece aqui" };
            p.text(pos2(r.center().x, r.top() + 92.0), Align2::CENTER_CENTER, hint, theme::regular(12.5), FAINT);
            if v.state.search {
                let b = Rect::from_center_size(pos2(r.center().x, r.top() + 130.0), vec2(150.0, 34.0));
                if theme::pill(ui, b, "ir-buscar", "Buscar música", false, true).clicked() {
                    self.tab = Tab::Search;
                }
            }
            return;
        }
        let list = Rect::from_min_max(pos2(body.left(), y), body.max);
        let queue = v.state.queue.clone();
        ui.scope_builder(egui::UiBuilder::new().max_rect(list), |ui| {
            egui::ScrollArea::vertical().id_salt("fila").auto_shrink([false, false]).show(ui, |ui| {
                for (i, t) in queue.iter().enumerate() {
                    let (r, _) = ui.allocate_exact_size(vec2(list.width(), 58.0), Sense::hover());
                    self.track_row(ui, r, t, RowKind::Queue, i);
                }
            });
        });
    }

    fn search(&mut self, ui: &mut Ui, body: Rect, v: &View) {
        let p = ui.painter().clone();
        let fr = Rect::from_min_size(body.min, vec2(body.width(), 42.0));
        if !v.state.search {
            let msg = match v.state.source.as_str() {
                "spotify" if v.host => "Pra buscar no Spotify, ligue a busca nos ajustes (leva 2 minutos).",
                "spotify" => "Quem toca ainda não ligou a busca do Spotify.",
                _ if v.host => "Abra o Spotify ou o Apple Music pra buscar.",
                _ => "Quem toca não está com nenhum app de música aberto.",
            };
            let r = Rect::from_min_size(body.min + vec2(0.0, 30.0), vec2(body.width(), 140.0));
            p.text(pos2(r.center().x, r.top() + 10.0), Align2::CENTER_CENTER, icon::SEARCH, theme::regular(26.0), Color32::from_white_alpha(70));
            let g = p.layout(msg.to_string(), theme::regular(13.0), DIM, r.width() - 40.0);
            let gw = g.size().x;
            p.galley(pos2(r.center().x - gw / 2.0, r.top() + 36.0), g, DIM);
            if v.host && v.state.source == "spotify" {
                let b = Rect::from_center_size(pos2(r.center().x, r.top() + 112.0), vec2(170.0, 34.0));
                if theme::pill(ui, b, "ligar-busca", "Ligar a busca", true, true).clicked() {
                    self.settings = true;
                }
            }
            return;
        }
        let hint = format!("Buscar em {}", if v.state.search_in.is_empty() { "músicas" } else { &v.state.search_in });
        let enter = field(ui, fr, "busca", &mut self.query, &hint, Some(icon::SEARCH));
        // busca sozinha 450 ms depois de parar de digitar (ou na hora com Enter)
        let q = self.query.trim().to_string();
        if q != self.sent_query {
            let due = self.query_at.get_or_insert_with(Instant::now);
            if (enter || due.elapsed() > Duration::from_millis(450)) && q.chars().count() >= 2 {
                if let Some(s) = &self.session {
                    s.search(&q);
                }
                self.sent_query = q.clone();
                self.query_at = None;
            } else {
                ui.ctx().request_repaint_after(Duration::from_millis(100));
            }
        } else {
            self.query_at = None;
        }
        let list = Rect::from_min_max(pos2(body.left(), fr.bottom() + 12.0), body.max);
        let r = &v.results;
        if r.loading {
            let t = ui.input(|i| i.time) as f32;
            for i in 0..3 {
                let a = ((t * 4.0 - i as f32 * 0.6).sin() * 0.5 + 0.5) * 0.8 + 0.2;
                p.circle_filled(pos2(list.center().x - 14.0 + i as f32 * 14.0, list.top() + 40.0), 3.5, Color32::from_white_alpha((a * 200.0) as u8));
            }
            ui.ctx().request_repaint();
            return;
        }
        if let Some(e) = &r.error {
            let g = p.layout(e.clone(), theme::regular(13.0), theme::LIVE, list.width() - 30.0);
            let gw = g.size().x;
            p.galley(pos2(list.center().x - gw / 2.0, list.top() + 30.0), g, theme::LIVE);
            return;
        }
        if r.items.is_empty() {
            let msg = if r.q.is_empty() { "Digite o nome da música ou do artista" } else { "Nada encontrado" };
            p.text(pos2(list.center().x, list.top() + 40.0), Align2::CENTER_CENTER, msg, theme::regular(13.0), FAINT);
            return;
        }
        let items = r.items.clone();
        ui.scope_builder(egui::UiBuilder::new().max_rect(list), |ui| {
            egui::ScrollArea::vertical().id_salt("resultados").auto_shrink([false, false]).show(ui, |ui| {
                for (i, t) in items.iter().enumerate() {
                    let (r, _) = ui.allocate_exact_size(vec2(list.width(), 58.0), Sense::hover());
                    self.track_row(ui, r, t, RowKind::Result, i);
                }
            });
        });
    }

    fn track_row(&mut self, ui: &mut Ui, r: Rect, t: &Track, kind: RowKind, i: usize) {
        let p = ui.painter().clone();
        let id = Id::new(("linha", kind as u8, i, &t.id));
        let resp = ui.interact(r, id, Sense::hover());
        let hover = resp.hovered() || ui.rect_contains_pointer(r);
        let h = ui.ctx().animate_bool_with_time(id.with("h"), hover, 0.1);
        if h > 0.0 {
            p.rect_filled(r, 12.0, Color32::from_white_alpha((14.0 * h) as u8));
        }
        let art = Rect::from_min_size(pos2(r.left() + 8.0, r.center().y - 21.0), vec2(42.0, 42.0));
        match self.arts.get(&t.art) {
            Some(a) => egui::Image::from_texture((a.tex.id(), art.size())).corner_radius(8.0).paint_at(ui, art),
            None => {
                p.rect_filled(art, 8.0, Color32::from_gray(34));
                p.text(art.center(), Align2::CENTER_CENTER, icon::NOTE, theme::regular(16.0), Color32::from_white_alpha(90));
            }
        }
        if kind == RowKind::Now {
            // barrinhas de som
            let tm = ui.input(|i| i.time) as f32;
            for b in 0..3 {
                let hh = 4.0 + ((tm * (5.0 + b as f32 * 1.7)).sin() * 0.5 + 0.5) * 9.0;
                let x = art.center().x - 6.0 + b as f32 * 6.0;
                p.rect_filled(Rect::from_min_max(pos2(x - 1.5, art.center().y + 7.0 - hh), pos2(x + 1.5, art.center().y + 7.0)), 1.5, Color32::WHITE);
            }
            p.rect_filled(art, 8.0, Color32::from_black_alpha(0));
            ui.ctx().request_repaint_after(Duration::from_millis(33));
        }
        let buttons = if hover && kind != RowKind::Now { 2 } else { 0 };
        let right_pad = if buttons > 0 { 84.0 } else { 54.0 };
        let tx = art.right() + 12.0;
        let maxw = r.right() - right_pad - tx;
        p.text(pos2(tx, r.center().y - 9.0), Align2::LEFT_CENTER, theme::fit(&p, &t.title, theme::semi(13.5), maxw), theme::semi(13.5), TEXT);
        let sub = match kind {
            RowKind::Queue if !t.by.is_empty() => format!("{} · {}", t.artist, t.by),
            _ => t.artist.clone(),
        };
        p.text(pos2(tx, r.center().y + 10.0), Align2::LEFT_CENTER, theme::fit(&p, &sub, theme::regular(12.0), maxw), theme::regular(12.0), FAINT);
        if buttons == 0 {
            if t.dur > 0.0 {
                p.text(pos2(r.right() - 12.0, r.center().y), Align2::RIGHT_CENTER, theme::time(t.dur), theme::medium(12.0), FAINT);
            }
            return;
        }
        let b2 = Rect::from_center_size(pos2(r.right() - 24.0, r.center().y), vec2(34.0, 34.0));
        let b1 = b2.translate(vec2(-38.0, 0.0));
        let Some(s) = &self.session else { return };
        match kind {
            RowKind::Result => {
                if theme::icon_button(ui, b1, &format!("tocar-{i}"), icon::PLAY, 17.0, true).on_hover_text("Tocar agora").clicked() {
                    s.play_now(t);
                }
                if theme::icon_button(ui, b2, &format!("fila-{i}"), icon::PLUS, 18.0, true).on_hover_text("Pôr na fila").clicked() {
                    s.add(t);
                }
            }
            RowKind::Queue => {
                if theme::icon_button(ui, b1, &format!("subir-{i}"), icon::TOP, 17.0, i > 0).on_hover_text("Tocar em seguida").clicked() {
                    s.to_top(i);
                }
                if theme::icon_button(ui, b2, &format!("tirar-{i}"), icon::X, 16.0, true).on_hover_text("Tirar da fila").clicked() {
                    s.remove(i);
                }
            }
            RowKind::Now => {}
        }
    }

    fn settings_sheet(&mut self, ui: &mut Ui, full: Rect) {
        let p = ui.painter().clone();
        // fundo escurece e bloqueia o resto
        p.rect_filled(full, 0.0, Color32::from_black_alpha(170));
        let back = ui.interact(full, Id::new("ajustes-fundo"), Sense::click());
        let r = Rect::from_center_size(full.center(), vec2(480.0, 470.0));
        p.rect_filled(r, 26.0, Color32::from_rgb(0x16, 0x16, 0x18));
        p.rect_stroke(r, 26.0, Stroke::new(1.0, theme::hairline()), StrokeKind::Inside);
        let x = r.left() + 28.0;
        let mut y = r.top() + 28.0;
        p.text(pos2(x, y), Align2::LEFT_TOP, icon::SPOTIFY, theme::fill(24.0), TEXT);
        p.text(pos2(x + 34.0, y + 2.0), Align2::LEFT_TOP, "Busca do Spotify", theme::semi(18.0), TEXT);
        y += 44.0;
        let steps = [
            "1. Entre em developer.spotify.com/dashboard com a sua conta",
            "2. Create app: qualquer nome, Redirect URI http://127.0.0.1:8888",
            "3. Marque Web API e salve",
            "4. Em Settings, copie o Client ID e o Client secret pra cá",
        ];
        for s in steps {
            p.text(pos2(x, y), Align2::LEFT_TOP, s, theme::regular(13.0), DIM);
            y += 22.0;
        }
        let link = Rect::from_min_size(pos2(x, y + 4.0), vec2(190.0, 30.0));
        if theme::pill(ui, link, "abrir-spotify-dev", "Abrir o site do Spotify", false, true).clicked() {
            ui.ctx().open_url(egui::OpenUrl::new_tab("https://developer.spotify.com/dashboard"));
        }
        y += 50.0;
        p.text(pos2(x, y), Align2::LEFT_TOP, "Client ID", theme::medium(12.0), FAINT);
        y += 18.0;
        field(ui, Rect::from_min_size(pos2(x, y), vec2(r.width() - 56.0, 40.0)), "sp-id", &mut self.sp_id, "cole aqui", Some(icon::KEY));
        y += 54.0;
        p.text(pos2(x, y), Align2::LEFT_TOP, "Client secret", theme::medium(12.0), FAINT);
        y += 18.0;
        field(ui, Rect::from_min_size(pos2(x, y), vec2(r.width() - 56.0, 40.0)), "sp-secret", &mut self.sp_secret, "cole aqui", Some(icon::KEY));
        y += 54.0;
        match &self.sp_check {
            Some(Ok(())) => {
                p.text(pos2(x, y), Align2::LEFT_TOP, "Funcionou. Seus amigos já podem buscar.", theme::medium(12.5), theme::GOOD);
            }
            Some(Err(e)) => {
                p.text(pos2(x, y), Align2::LEFT_TOP, theme::fit(&p, e, theme::medium(12.5), r.width() - 56.0), theme::medium(12.5), theme::LIVE);
            }
            None if self.sp_checking.is_some() => {
                p.text(pos2(x, y), Align2::LEFT_TOP, "Conferindo…", theme::medium(12.5), DIM);
            }
            None => {}
        }
        let save = Rect::from_min_size(pos2(r.right() - 28.0 - 150.0, r.bottom() - 28.0 - 38.0), vec2(150.0, 38.0));
        let close = save.translate(vec2(-160.0, 0.0));
        let ok = !self.sp_id.trim().is_empty() && !self.sp_secret.trim().is_empty();
        if theme::pill(ui, save, "salvar-spotify", "Salvar e testar", true, ok && self.sp_checking.is_none()).clicked() {
            self.prefs.spotify_id = self.sp_id.trim().to_string();
            self.prefs.spotify_secret = self.sp_secret.trim().to_string();
            self.prefs.save();
            if let Some(s) = &self.session {
                s.set_spotify(&self.prefs.spotify_id, &self.prefs.spotify_secret);
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let (a, b, c) = (self.prefs.spotify_id.clone(), self.prefs.spotify_secret.clone(), ui.ctx().clone());
            std::thread::spawn(move || {
                let _ = tx.send(crate::search::spotify_check(&a, &b));
                c.request_repaint();
            });
            self.sp_checking = Some(rx);
            self.sp_check = None;
        }
        if theme::pill(ui, close, "fechar-ajustes", "Fechar", false, true).clicked() || (back.clicked() && !r.contains(back.interact_pointer_pos().unwrap_or(r.center()))) {
            self.settings = false;
        }
    }

    /// JAM_SHOT=arquivo.png: salva a tela depois de uns quadros e fecha (pra conferir o desenho)
    fn take_shot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.shot.clone() else { return };
        self.frame_no += 1;
        let at: u64 = std::env::var("JAM_SHOT_QUADRO").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
        if self.frame_no == at {
            let c = ctx.clone();
            ctx.request_screenshot(move |img| {
                let [w, h] = img.size;
                let bytes: Vec<u8> = img.pixels.iter().flat_map(|p| p.to_array()).collect();
                let _ = image::save_buffer(&path, &bytes, w as u32, h as u32, image::ExtendedColorType::Rgba8);
                c.send_viewport_cmd(egui::ViewportCommand::Close);
            });
        }
        ctx.request_repaint();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowKind {
    Now,
    Queue,
    Result,
}

fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

fn clone_shared(s: &crate::room::Shared) -> crate::room::Shared {
    crate::room::Shared {
        phase: s.phase.clone(),
        code: s.code.clone(),
        state: s.state.clone(),
        state_at: s.state_at,
        people: s.people.clone(),
        results: s.results.clone(),
        toast: s.toast.clone(),
        capture_error: s.capture_error.clone(),
    }
}

/// JAM_DEMO=1: uma Jam de mentira (sem rede) pra conferir o desenho. JAM_DEMO_ART = capa (arquivo)
pub fn demo() -> Option<crate::room::Shared> {
    std::env::var_os("JAM_DEMO")?;
    let art = std::env::var("JAM_DEMO_ART").map(|p| format!("file://{p}")).unwrap_or_default();
    let t = |title: &str, artist: &str, by: &str, dur: f64| Track { id: format!("spotify:track:{title}"), title: title.into(), artist: artist.into(), album: String::new(), art: art.clone(), dur, by: by.into() };
    let tab = std::env::var("JAM_DEMO_ABA").unwrap_or_default();
    let mut results = crate::room::Results::default();
    if tab == "busca" {
        results.q = "yoga".into();
        results.items = vec![t("Yoga", "Janelle Monáe, Jidenna", "", 196.0), t("Make Me Feel", "Janelle Monáe", "", 194.0), t("Tightrope", "Janelle Monáe, Big Boi", "", 262.0)];
    }
    Some(crate::room::Shared {
        phase: Phase::Live,
        code: Some("JAM-K7M2-QXAB".into()),
        state: State {
            now: Some(crate::proto::Now { track: t("Runnin' Thru the 7th with My Woadies", "$uicideboy$", "Breno", 197.8), pos: 72.0, playing: true }),
            queue: vec![t("Yoga", "Janelle Monáe, Jidenna", "Breno", 196.0), t("Nights", "Frank Ocean", "Gui", 307.0), t("Redbone", "Childish Gambino", "Ana", 327.0)],
            source: "spotify".into(),
            host: "Lucas".into(),
            search: true,
            search_in: "Spotify".into(),
        },
        state_at: Instant::now(),
        people: vec!["Lucas".into(), "Breno".into(), "Gui".into(), "Ana Clara".into()],
        results,
        toast: None,
        capture_error: None,
    })
}

#[allow(dead_code)]
fn unused(_: CornerRadius, _: Vec2) {}
