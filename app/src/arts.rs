//! Capas: baixa numa thread, vira textura na tela e guarda a cor média (pra luz do palco).

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui::{self, Color32, ColorImage, TextureHandle, TextureOptions};

pub struct Art {
    pub tex: TextureHandle,
    pub color: Color32,
}

enum Slot {
    Loading,
    Ready(Art),
    Failed,
}

pub struct Arts {
    map: HashMap<String, Slot>,
    tx: Sender<(String, Option<(ColorImage, Color32)>)>,
    rx: Receiver<(String, Option<(ColorImage, Color32)>)>,
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
                Some((img, color)) => Slot::Ready(Art { tex: ctx.load_texture(format!("capa {url}"), img, TextureOptions::LINEAR.with_mipmap_mode(Some(egui::TextureFilter::Linear))), color }),
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
                let img = load(&u);
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

fn load(url: &str) -> Option<(ColorImage, Color32)> {
    let bytes = if let Some(path) = url.strip_prefix("file://") {
        std::fs::read(path).ok()?
    } else {
        crate::search::fetch(url)?
    };
    let img = image::load_from_memory(&bytes).ok()?;
    // até 1600 px: o palco tem ~560 pontos (1120 px no Retina) e a lente do vidro amplia a borda
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
    Some((ColorImage::from_rgba_unmultiplied([w, h], rgba.as_raw()), color))
}
