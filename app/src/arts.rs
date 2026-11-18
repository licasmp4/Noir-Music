//! Capas: baixa numa thread, vira textura na tela e guarda a cor média (pra luz do palco).

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui::{self, Color32, ColorImage, TextureHandle, TextureOptions};

pub struct Art {
    pub tex: TextureHandle,
    /// A capa bem borrada (o fundo da Noir Music)
    pub blur: TextureHandle,
    pub color: Color32,
}

enum Slot {
    Loading,
    Ready(Art),
    Failed,
}

pub struct Arts {
    map: HashMap<String, Slot>,
    tx: Sender<(String, Option<(ColorImage, ColorImage, Color32)>)>,
    rx: Receiver<(String, Option<(ColorImage, ColorImage, Color32)>)>,
    pub repaint: Option<egui::Context>,
}

impl Arts {
    pub fn new() -> Arts {
        let (tx, rx) = channel();
        Arts { map: HashMap::new(), tx, rx, repaint: None }
    }

    /// Recebe as que chegaram (uma vez por quadro)
    pub fn pump(&mut self, ctx: &egui::Context) {
        while let Ok((url, img)) = self.rx.try_recv() {
            let slot = match img {
                Some((img, blur, color)) => Slot::Ready(Art {
                    tex: ctx.load_texture(format!("capa {url}"), img, TextureOptions::LINEAR.with_mipmap_mode(Some(egui::TextureFilter::Linear))),
                    blur: ctx.load_texture(format!("fundo {url}"), blur, TextureOptions::LINEAR),
                    color,
                }),
                None => Slot::Failed,
            };
            self.map.insert(url, slot);
        }
        // não guarda capa demais (cada busca traz 10 a 20)
        if self.map.len() > 300 {
            self.map.retain(|_, s| matches!(s, Slot::Ready(_)));
            let extra = self.map.len().saturating_sub(200);
            let keys: Vec<String> = self.map.keys().take(extra).cloned().collect();
            for k in keys {
                self.map.remove(&k);
            }
        }
    }

    pub fn get(&mut self, url: &str) -> Option<&Art> {
        if url.is_empty() {
            return None;
        }
        if !self.map.contains_key(url) {
            self.map.insert(url.to_string(), Slot::Loading);
            let (tx, u, ctx) = (self.tx.clone(), url.to_string(), self.repaint.clone());
            std::thread::spawn(move || {
                let t0 = std::time::Instant::now();
                let img = load(&u);
                crate::log::log(format!("capa {} em {} ms: {}", if img.is_some() { "ok" } else { "FALHOU" }, t0.elapsed().as_millis(), u));
                let _ = tx.send((u, img));
                if let Some(c) = ctx {
                    c.request_repaint();
                }
            });
        }
        match self.map.get(url) {
            Some(Slot::Ready(a)) => Some(a),
            _ => None,
        }
    }
}

fn load(url: &str) -> Option<(ColorImage, ColorImage, Color32)> {
    let bytes = if let Some(path) = url.strip_prefix("file://") {
        std::fs::read(path).ok()?
    } else {
        // a capa de 2000 px do Spotify não existe pra toda música: volta pra de 640
        crate::search::fetch(url).or_else(|| url.contains("ab67616d000082c1").then(|| crate::search::fetch(&url.replace("ab67616d000082c1", "ab67616d0000b273"))).flatten())?
    };
    let img = image::load_from_memory(&bytes).ok()?;
    // o fundo: capa em 72 px borrada duas vezes e mais saturada. Bem pequena e lisa de propósito:
    // esticada na janela com filtro linear vira mancha de cor macia (o grão vai à parte, em tamanho real)
    let blur = {
        const S: u32 = 72;
        let small = img.resize_exact(S, S, image::imageops::FilterType::Lanczos3).to_rgba8();
        let b = image::imageops::blur(&image::imageops::blur(&small, 5.0), 3.5);
        let mut px = b.into_raw();
        for c in px.chunks_exact_mut(4) {
            let l = 0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32;
            for k in 0..3 {
                c[k] = (l + (c[k] as f32 - l) * 1.6).clamp(0.0, 255.0) as u8;
            }
        }
        ColorImage::from_rgba_unmultiplied([S as usize, S as usize], &px)
    };
    let img = if img.width() > 1600 { img.resize(1600, 1600, image::imageops::FilterType::Lanczos3) } else { img };
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width() as usize, rgba.height() as usize);
    // cor média, puxando pra cor mais viva (a média de tudo dá um cinza sujo)
    let (mut r, mut g, mut b, mut n) = (0f64, 0f64, 0f64, 0f64);
    for p in rgba.pixels().step_by(7) {
        let [pr, pg, pb, _] = p.0;
        let mx = pr.max(pg).max(pb) as f64;
        let mn = pr.min(pg).min(pb) as f64;
        let wgt = 0.15 + (mx - mn) / 255.0;
        r += pr as f64 * wgt;
        g += pg as f64 * wgt;
        b += pb as f64 * wgt;
        n += wgt;
    }
    let color = if n > 0.0 { Color32::from_rgb((r / n) as u8, (g / n) as u8, (b / n) as u8) } else { Color32::GRAY };
    Some((ColorImage::from_rgba_unmultiplied([w, h], rgba.as_raw()), blur, color))
}
