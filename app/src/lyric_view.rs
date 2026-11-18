//! A letra na tela, no jeito do Apple Music: a linha cantada fica clara e as outras apagadas e
//! desfocadas pela distância. Cada linha anda com a sua mola, as de baixo um pouquinho depois das
//! de cima (a "onda"). Rolar com o mouse solta a letra; ela volta a seguir na próxima linha.
//! Números da pesquisa (recriações abertas do Apple Music): linha ativa a 35 % da altura, desfoque
//! de 1 px por linha de distância (até 5), mola k 170–220 / massa 0,9 / amortecimento √k·2,2,
//! atraso de 50 ms na primeira linha de baixo, dividido por 1,05 a cada uma.

use eframe::egui::{self, Align2, Color32, Id, Pos2, Rect, Sense, Ui, pos2, vec2};

use crate::lyrics::Lyrics;
use crate::theme;

#[derive(Clone, Copy, Default)]
struct Spring {
    x: f32,
    v: f32,
}

impl Spring {
    fn step(&mut self, goal: f32, dt: f32, k: f32, c: f32, m: f32) {
        // passos pequenos: estável mesmo com quadro lento
        let n = ((dt / (1.0 / 240.0)).ceil() as usize).clamp(1, 16);
        let h = dt / n as f32;
        for _ in 0..n {
            let a = (-k * (self.x - goal) - c * self.v) / m;
            self.v += a * h;
            self.x += self.v * h;
        }
        if (self.x - goal).abs() < 0.05 && self.v.abs() < 0.5 {
            self.x = goal;
            self.v = 0.0;
        }
    }
}

#[derive(Default)]
pub struct LyricView {
    key: String,
    ys: Vec<Spring>,
    goal: Vec<f32>,
    release: Vec<f64>,
    active: Option<usize>,
    /// Depois de pular de lugar (seek) a mola fica mais lenta por um tempo
    slow_until: f64,
    follow: bool,
    user: f32,
    last: f64,
}

pub struct Look {
    pub size: f32,
    /// Onde a linha cantada fica (fração da altura)
    pub anchor: f32,
}

impl LyricView {
    /// Desenha e devolve o tempo pra pular, se clicaram numa linha
    pub fn show(&mut self, ui: &mut Ui, rect: Rect, id: &str, key: &str, ly: &Lyrics, pos: f32, look: Look) -> Option<f32> {
        let p = ui.painter().with_clip_rect(rect);
        let now = ui.input(|i| i.time);
        let dt = if self.last == 0.0 { 1.0 / 60.0 } else { (now - self.last).clamp(0.0, 0.1) as f32 };
        self.last = now;
        let size = look.size;
        let font = theme::display(size);
        let wrap = rect.width();
        let gap = size * 0.55;

        // monta as linhas (o egui guarda o layout, então refazer todo quadro é barato)
        let galleys: Vec<_> = ly
            .lines
            .iter()
            .map(|l| if l.text.is_empty() { None } else { Some(p.layout(l.text.clone(), font.clone(), Color32::WHITE, wrap)) })
            .collect();
        let heights: Vec<f32> = galleys.iter().map(|g| g.as_ref().map_or(size * 0.9, |g| g.size().y)).collect();
        let mut tops = Vec::with_capacity(heights.len());
        let mut acc = 0.0;
        for h in &heights {
            tops.push(acc);
            acc += h + gap;
        }
        let total = acc;

        let active = crate::lyrics::current(ly, pos);
        let n = ly.lines.len();
        if self.key != key || self.ys.len() != n {
            *self = LyricView { key: key.to_string(), follow: true, last: now, ..Default::default() };
            self.ys = vec![Spring::default(); n];
            self.goal = vec![0.0; n];
            self.release = vec![0.0; n];
            self.active = active;
            // começa no lugar, sem animar
            let base = Self::base(rect, &tops, active, look.anchor, ly.synced);
            for i in 0..n {
                self.ys[i].x = base + tops[i];
                self.goal[i] = self.ys[i].x;
            }
        }

        // trocou de linha: a "onda" (seguindo a música) ou tudo junto e mais lento (pulou)
        if active != self.active {
            let normal = matches!((self.active, active), (Some(a), Some(b)) if b == a + 1) || (self.active.is_none() && active == Some(0));
            if normal {
                let from = active.unwrap_or(0).saturating_sub(1);
                let mut d = 0.05f64;
                let mut acc = 0.0;
                for i in 0..n {
                    if i >= from {
                        self.release[i] = now + acc;
                        acc += d;
                        d /= 1.05;
                    } else {
                        self.release[i] = now;
                    }
                }
            } else {
                self.release.iter_mut().for_each(|r| *r = now);
                self.slow_until = now + 1.2;
            }
            self.active = active;
            // a letra volta a seguir sozinha
            self.follow = true;
        }

        // rolar com o mouse solta
        let hovered = ui.rect_contains_pointer(rect);
        if hovered {
            let dy = ui.input(|i| i.smooth_scroll_delta.y);
            if dy.abs() > 0.1 {
                if self.follow {
                    self.follow = false;
                    self.user = 0.0;
                }
                self.user = (self.user + dy).clamp(-total, rect.height());
                self.release.iter_mut().for_each(|r| *r = now);
            }
        }
        let base = Self::base(rect, &tops, active, look.anchor, ly.synced) + if self.follow { 0.0 } else { self.user };
        let slow = now < self.slow_until;
        let (k, m): (f32, f32) = if slow { (90.0, 1.0) } else { (190.0, 0.9) };
        let c = if slow { 15.0 } else { k.sqrt() * 2.2 };
        let mut moving = false;
        for i in 0..n {
            if now >= self.release[i] || !self.follow {
                self.goal[i] = base + tops[i];
            }
            if !self.follow {
                // rolagem do usuário: direto, sem atraso
                self.ys[i].step(self.goal[i], dt, 400.0, 40.0, 1.0);
            } else {
                self.ys[i].step(self.goal[i], dt, k, c, m);
            }
            moving |= self.ys[i].x != self.goal[i] || now < self.release[i];
        }

        // desenha
        let mut seek = None;
        let fade = rect.height() * 0.10;
        for i in 0..n {
            let y = self.ys[i].x;
            let h = heights[i];
            if y > rect.bottom() + 40.0 || y + h < rect.top() - 40.0 {
                continue;
            }
            let line = &ly.lines[i];
            let is_active = active == Some(i);
            // apaga perto da borda de cima e de baixo
            let edge = ((y + h * 0.5 - rect.top()) / fade).min((rect.bottom() - y - h * 0.5) / fade).clamp(0.0, 1.0);
            let lit = ui.ctx().animate_bool_with_time(Id::new((id, "lit", i)), is_active, 0.35);
            let dist = match active {
                Some(a) if i < a => (a - i) as f32 + 1.0,
                Some(a) => (i as f32 - a as f32).abs(),
                None => i as f32 + 1.0,
            };
            let blur_goal = if ly.synced && self.follow && !is_active { dist.min(5.0) * size / 48.0 } else { 0.0 };
            let blur = ui.ctx().animate_value_with_time(Id::new((id, "blur", i)), blur_goal, 0.4);

            let row = Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), h));
            let click = ly.synced && !line.text.is_empty();
            let resp = ui.interact(row.intersect(rect), Id::new((id, "linha", i)), if click { Sense::click() } else { Sense::hover() });
            let hover = click && resp.hovered() && !is_active;
            if hover {
                p.rect_filled(row.expand2(vec2(12.0, 6.0)), 14.0, Color32::from_white_alpha(14));
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() && click {
                seek = Some(line.start);
            }

            match &galleys[i] {
                None => {
                    // pausa instrumental: três pontinhos que respiram e acendem com o tempo
                    if is_active {
                        let t = ((pos - line.start) / (line.end - line.start).max(0.1)).clamp(0.0, 1.0);
                        let breathe = 1.0 + 0.12 * ((now as f32) * std::f32::consts::TAU / 4.0).sin();
                        let out = ((line.end - pos) / 0.35).clamp(0.0, 1.0);
                        let r = size * 0.14 * breathe * (0.4 + 0.6 * out);
                        for d in 0..3 {
                            let on = ((t * 3.0 - d as f32).clamp(0.0, 1.0)) * 0.7 + 0.2;
                            let c = pos2(rect.left() + r + d as f32 * r * 3.2, y + h * 0.5);
                            p.circle_filled(c, r, Color32::from_white_alpha((255.0 * on * lit * edge) as u8));
                        }
                    }
                }
                Some(g) => {
                    let base_a = if ly.synced { 0.30 + 0.70 * lit } else { 0.80 };
                    let a = (if hover { base_a.max(0.6) } else { base_a }) * edge;
                    let origin = pos2(rect.left(), y);
                    if is_active && !line.words.is_empty() {
                        // palavra por palavra: o que já foi cantado claro, o resto a 40 %
                        let g = karaoke(&p, line, pos, &font, wrap);
                        p.galley(origin, g, Color32::WHITE);
                    } else {
                        draw_blurred(&p, origin, g.clone(), Color32::WHITE.gamma_multiply(a), blur);
                    }
                }
            }
        }
        if moving || ui.ctx().egui_is_using_pointer() {
            ui.ctx().request_repaint();
        } else {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
        seek
    }

    fn base(rect: Rect, tops: &[f32], active: Option<usize>, anchor: f32, synced: bool) -> f32 {
        if !synced {
            return rect.top() + 8.0;
        }
        let a = active.map_or(0.0, |a| tops[a]);
        rect.top() + rect.height() * anchor - a
    }
}

/// Texto "desfocado" sem shader: cópias em anel com pouca opacidade. Fica bom até uns 3 px
fn draw_blurred(p: &egui::Painter, at: Pos2, g: std::sync::Arc<egui::Galley>, color: Color32, blur: f32) {
    if blur < 0.25 {
        p.galley_with_override_text_color(at, g, color);
        return;
    }
    let b = blur.min(4.0);
    p.galley_with_override_text_color(at, g.clone(), color.gamma_multiply(0.30));
    for (ring, count, w) in [(0.5f32, 6usize, 0.20f32), (1.0, 8, 0.13)] {
        for k in 0..count {
            let ang = k as f32 / count as f32 * std::f32::consts::TAU + ring;
            let off = vec2(ang.cos(), ang.sin()) * b * ring;
            p.galley_with_override_text_color(at + off, g.clone(), color.gamma_multiply(w));
        }
    }
}

fn karaoke(p: &egui::Painter, line: &crate::lyrics::Line, pos: f32, font: &egui::FontId, wrap: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = wrap;
    for (k, (t, w)) in line.words.iter().enumerate() {
        let end = line.words.get(k + 1).map_or(line.end, |n| n.0);
        let f = ((pos - t) / (end - t).max(0.05)).clamp(0.0, 1.0);
        let c = Color32::from_white_alpha((255.0 * (0.4 + 0.6 * f)) as u8);
        job.append(w, 0.0, egui::TextFormat { font_id: font.clone(), color: c, ..Default::default() });
    }
    p.layout_job(job)
}

/// "Letra do LRCLIB" e o aviso quando não tem
pub fn empty(p: &egui::Painter, rect: Rect, msg: &str) {
    p.text(rect.center() - vec2(0.0, 14.0), Align2::CENTER_CENTER, crate::theme::icon::LYRICS, theme::regular(30.0), Color32::from_white_alpha(70));
    p.text(rect.center() + vec2(0.0, 24.0), Align2::CENTER_CENTER, msg, theme::medium(14.0), Color32::from_white_alpha(120));
}
