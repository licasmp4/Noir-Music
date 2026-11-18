//! Controle de banda (AIMD) por ping e perda MÉDIOS: um pico isolado (Radmin, Wi-Fi) não derruba a imagem.
//! Usado pela sala (pra cada amigo que assiste) e por quem transmite (pro próprio upload).

use std::time::{Duration, Instant};

/// O mínimo absoluto. Antes era 1,5 Mbps e 40% do máximo (8 Mbps com o padrão de 20): amigo com
/// internet fraca nunca conseguia receber, perdia pacote o tempo todo e a tela travava/pixelava.
/// Agora desce até caber; quem transmite baixa os fps junto (share.rs) pra imagem não virar borrão.
pub const MIN_BITRATE: u64 = 500_000;
const FLOOR_FRACTION: f64 = 0.03;
/// Perda média acima disso = a conexão não está aguentando (até ~4% o reenvio dá conta)
const LOSS_LIMIT: f32 = 0.03;
/// Ping médio subiu tanto acima do normal = fila enchendo em algum roteador (vira lag)
const QUEUE_RTT_MS: f64 = 45.0;

#[derive(Clone, Debug)]
pub struct Cong {
    pub cap: u64,
    pub rtt_ms: Option<f64>,
    pub loss: Option<f32>,
    srtt: Option<f64>,
    sloss: f32,
    rtt_base: Option<f64>,
    ceiling: Option<(u64, Instant)>,
    last_change: Instant,
}

impl Cong {
    pub fn new(cap: u64) -> Cong {
        Cong { cap, rtt_ms: None, loss: None, srtt: None, sloss: 0.0, rtt_base: None, ceiling: None, last_change: Instant::now() }
    }

    /// Um relatório (~200 ms). Devolve true se o teto mudou.
    /// - fila enchendo ou perda alta corta 15% e segura 1,5 s;
    /// - nunca desce de 40% do máximo;
    /// - tudo bem por 1 s sobe: 12% longe do ponto onde congestionou, 4% perto dele.
    pub fn update(&mut self, rtt_ms: Option<f64>, loss: Option<f32>, max: u64) -> bool {
        let now = Instant::now();
        self.rtt_ms = rtt_ms.or(self.rtt_ms);
        self.loss = loss;
        if let Some(rtt) = rtt_ms {
            let s = self.srtt.map_or(rtt, |s| s * 0.8 + rtt * 0.2);
            self.srtt = Some(s);
            // Base = menor ping médio; sobe devagar se a rota mudou (senão ficaria "congestionado" pra sempre)
            self.rtt_base = Some(match self.rtt_base {
                None => s,
                Some(b) if s < b => s,
                Some(b) => b + (s - b) * 0.003,
            });
        }
        self.sloss = self.sloss * 0.7 + loss.unwrap_or(0.0) * 0.3;
        let queueing = matches!((self.srtt, self.rtt_base), (Some(r), Some(b)) if r > b + QUEUE_RTT_MS.max(b * 0.6));
        let losing = self.sloss > LOSS_LIMIT;
        let floor = MIN_BITRATE.max((max as f64 * FLOOR_FRACTION) as u64).min(max);
        if self.ceiling.is_some_and(|(_, at)| now - at > Duration::from_secs(15)) {
            self.ceiling = None;
        }
        let old = self.cap;
        self.cap = self.cap.min(max);
        if queueing || losing {
            let wait = Duration::from_millis(self.rtt_ms.unwrap_or(100.0).max(400.0) as u64);
            if now - self.last_change >= wait {
                self.ceiling = Some((self.cap, now));
                // corta mais quanto maior a perda: 15% com fila ou até 7%, até 30% com perda alta
                let cut = if losing { (1.0 - 2.0 * self.sloss as f64).clamp(0.50, 0.85) } else { 0.85 };
                self.cap = ((self.cap as f64 * cut) as u64).max(floor);
                self.last_change = now + Duration::from_millis(500);
            }
        } else if now > self.last_change && now - self.last_change >= Duration::from_millis(1000) {
            let near = self.ceiling.is_some_and(|(c, _)| self.cap as f64 >= c as f64 * 0.85);
            let step = if near { 1.04 } else { 1.12 };
            self.cap = ((self.cap as f64 * step) as u64).min(max);
            self.last_change = now;
        }
        self.cap != old
    }
}
