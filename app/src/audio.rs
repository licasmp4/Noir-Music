//! O som da Jam: o Opus pra música (estéreo, 48 kHz, 256 kbps), uma cópia do pacote anterior
//! dentro de cada pacote (se um se perde na internet, o próximo traz ele de volta) e a saída de
//! quem ouve com folga que cresce sozinha quando a internet oscila.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use audiopus::coder::{Decoder as ODec, Encoder as OEnc};
use audiopus::{Application, Bitrate, Channels, SampleRate, Signal};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// 20 ms a 48 kHz (amostras por canal)
pub const FRAME: usize = 960;
pub const RATE: u32 = 48_000;
/// Música: 256 kbps em estéreo já é transparente pro ouvido (o mesmo que o Apple Music manda em AAC)
const BITRATE: i32 = 256_000;

// MARK: - Quem toca

pub struct Encoder {
    enc: OEnc,
    buf: Vec<u8>,
    prev: Vec<u8>,
    pending: Vec<f32>,
}

impl Encoder {
    pub fn new() -> Result<Encoder, String> {
        let mut enc = OEnc::new(SampleRate::Hz48000, Channels::Stereo, Application::Audio).map_err(|e| e.to_string())?;
        let _ = enc.set_bitrate(Bitrate::BitsPerSecond(BITRATE));
        let _ = enc.set_complexity(10);
        let _ = enc.set_signal(Signal::Music);
        let _ = enc.set_vbr(true);
        Ok(Encoder { enc, buf: vec![0; 4000], prev: Vec::new(), pending: Vec::with_capacity(FRAME * 8) })
    }

    /// Recebe som estéreo intercalado (qualquer tamanho) e devolve os pacotes de 20 ms prontos,
    /// cada um já com a cópia do anterior: [versão 1][tamanho do atual u16][atual][anterior]
    pub fn push(&mut self, pcm: &[f32], mut out: impl FnMut(&[u8])) {
        self.pending.extend_from_slice(pcm);
        let mut start = 0;
        while self.pending.len() - start >= FRAME * 2 {
            let frame = &self.pending[start..start + FRAME * 2];
            start += FRAME * 2;
            let Ok(n) = self.enc.encode_float(frame, &mut self.buf) else { continue };
            let cur = &self.buf[..n];
            let mut pkt = Vec::with_capacity(3 + n + self.prev.len());
            pkt.push(1u8);
            pkt.extend_from_slice(&(n as u16).to_be_bytes());
            pkt.extend_from_slice(cur);
            pkt.extend_from_slice(&self.prev);
            out(&pkt);
            self.prev.clear();
            self.prev.extend_from_slice(cur);
        }
        self.pending.drain(..start);
    }
}

// MARK: - Quem ouve

/// Separa o pacote: (atual, anterior)
fn split(pkt: &[u8]) -> Option<(&[u8], &[u8])> {
    if pkt.len() < 3 || pkt[0] != 1 {
        return None;
    }
    let n = u16::from_be_bytes([pkt[1], pkt[2]]) as usize;
    if pkt.len() < 3 + n {
        return None;
    }
    Some((&pkt[3..3 + n], &pkt[3 + n..]))
}

pub struct Decoder {
    dec: ODec,
    buf: Vec<f32>,
    last_ts: Option<u32>,
    /// pacotes salvos pela cópia e inventados pelo Opus (pro registro)
    pub recovered: u64,
    pub concealed: u64,
}

impl Decoder {
    pub fn new() -> Result<Decoder, String> {
        Ok(Decoder { dec: ODec::new(SampleRate::Hz48000, Channels::Stereo).map_err(|e| e.to_string())?, buf: vec![0.0; 5760 * 2], last_ts: None, recovered: 0, concealed: 0 })
    }

    /// `ts` é o relógio do pacote (48 kHz). Devolve o som estéreo intercalado, já com o pedaço que
    /// faltou: da cópia se perdeu um só, inventado pelo Opus se perdeu mais.
    pub fn decode(&mut self, pkt: &[u8], ts: u32) -> Vec<f32> {
        let Some((cur, prev)) = split(pkt) else { return Vec::new() };
        let mut out = Vec::new();
        if let Some(last) = self.last_ts {
            let gap = ts.wrapping_sub(last);
            if gap == 0 || gap > u32::MAX / 2 {
                return out; // repetido ou chegou atrasado demais
            }
            let missing = (gap as usize / FRAME).saturating_sub(1);
            if missing > 0 && missing <= 25 {
                // inventa os que se perderam, menos o último, que veio de carona neste pacote
                let invent = if prev.is_empty() { missing } else { missing - 1 };
                for _ in 0..invent {
                    if let Ok(k) = self.dec.decode_float(None::<&[u8]>, &mut self.buf[..FRAME * 2], false) {
                        out.extend_from_slice(&self.buf[..k * 2]);
                        self.concealed += 1;
                    }
                }
                if !prev.is_empty() {
                    if let Ok(k) = self.dec.decode_float(Some(prev), &mut self.buf[..], false) {
                        out.extend_from_slice(&self.buf[..k * 2]);
                        self.recovered += 1;
                    }
                }
            }
        }
        self.last_ts = Some(ts);
        if let Ok(k) = self.dec.decode_float(Some(cur), &mut self.buf[..], false) {
            out.extend_from_slice(&self.buf[..k * 2]);
        }
        out
    }
}

/// Folga: começa em 250 ms (música não precisa de atraso baixo, precisa de não cortar) e sobe
/// 100 ms a cada corte, até 1,2 s. Desce devagar depois de 60 s sem corte.
const MIN_MS: usize = 250;
const MAX_MS: usize = 1200;
const STEP_MS: usize = 100;

fn ms(n: usize) -> usize {
    n * RATE as usize / 1000 * 2
}

struct Shared {
    q: VecDeque<f32>,
    target: usize,
    started: bool,
    volume: f32,
    /// volta do silêncio sem estalo
    fade: f32,
    unders: u64,
    calm_since: Instant,
    /// nível (pro desenho que pulsa com o som)
    level: f32,
    /// posição fracionária pra trocar 48 kHz pela taxa da saída
    pos: f64,
    hist: [f32; 8],
    /// quem toca está tocando (pausado, o som acabar é normal e não aumenta a folga)
    playing: bool,
    /// pro registro: amostras que chegaram e o pico (antes do volume)
    pushed: u64,
    in_peak: f32,
}

pub struct Output {
    shared: Arc<Mutex<Shared>>,
    _stream: Option<cpal::Stream>,
    pub device: String,
    pub error: Option<String>,
}

impl Output {
    pub fn new() -> Output {
        let shared = Arc::new(Mutex::new(Shared {
            q: VecDeque::with_capacity(ms(2000)),
            target: ms(MIN_MS),
            started: false,
            volume: 1.0,
            fade: 0.0,
            unders: 0,
            calm_since: Instant::now(),
            level: 0.0,
            pos: 0.0,
            hist: [0.0; 8],
            playing: true,
            pushed: 0,
            in_peak: 0.0,
        }));
        match open(shared.clone()) {
            Ok((stream, device)) => Output { shared, _stream: Some(stream), device, error: None },
            Err(e) => {
                crate::log::log(format!("saída de som: {e}"));
                Output { shared, _stream: None, device: String::new(), error: Some(e) }
            }
        }
    }

    pub fn push(&self, pcm: &[f32]) {
        let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        s.pushed += pcm.len() as u64 / 2;
        let pk = pcm.iter().fold(0.0f32, |a, v| a.max(v.abs()));
        s.in_peak = s.in_peak.max(pk);
        s.q.extend(pcm.iter().copied());
        // relógio de quem toca um tiquinho mais rápido que o daqui: a fila não pode crescer pra sempre
        let cap = s.target + ms(800);
        if s.q.len() > cap {
            let drop = s.q.len() - s.target;
            s.q.drain(..drop & !1);
        }
        if !s.started && s.q.len() >= s.target {
            s.started = true;
        }
        if s.calm_since.elapsed().as_secs() > 60 && s.target > ms(MIN_MS) {
            s.target -= ms(50);
            s.calm_since = Instant::now();
        }
    }

    pub fn set_volume(&self, v: f32) {
        self.shared.lock().unwrap_or_else(|e| e.into_inner()).volume = v.clamp(0.0, 1.0);
    }

    pub fn set_playing(&self, on: bool) {
        self.shared.lock().unwrap_or_else(|e| e.into_inner()).playing = on;
    }

    pub fn level(&self) -> f32 {
        self.shared.lock().unwrap_or_else(|e| e.into_inner()).level
    }

    /// (folga em ms, cortes)
    pub fn stats(&self) -> (usize, u64) {
        let s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        (s.target * 1000 / (RATE as usize * 2), s.unders)
    }

    /// (amostras recebidas, pico desde a última vez, na fila agora em ms)
    pub fn take_diag(&self) -> (u64, f32, usize) {
        let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        let p = std::mem::take(&mut s.in_peak);
        (s.pushed, p, s.q.len() * 1000 / (RATE as usize * 2))
    }

    /// Começa de novo (trocou de sala)
    pub fn clear(&self) {
        let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        s.q.clear();
        s.started = false;
    }
}

fn open(shared: Arc<Mutex<Shared>>) -> Result<(cpal::Stream, String), String> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or("nenhuma saída de som")?;
    let name = device.description().map(|d| d.name().to_string()).unwrap_or_default();
    // 48 kHz se a saída aceitar (sem conversão nenhuma); senão a taxa padrão dela
    let default = device.default_output_config().map_err(|e| e.to_string())?;
    let mut config: cpal::StreamConfig = default.config();
    let fmt = default.sample_format();
    if let Ok(list) = device.supported_output_configs() {
        for c in list {
            if c.min_sample_rate() <= RATE && c.max_sample_rate() >= RATE && c.sample_format() == fmt {
                config.sample_rate = RATE;
                break;
            }
        }
    }
    let channels = config.channels as usize;
    let step = RATE as f64 / config.sample_rate as f64;
    crate::log::log(format!("saída de som: {name} {} Hz {channels} canais {fmt:?}", config.sample_rate));
    
    let stream = match fmt {
        cpal::SampleFormat::F32 => device.build_output_stream(config.clone(), move |out: &mut [f32], _| fill(&shared, out, channels, step, |v| v), |e| crate::log::log(format!("saída de som: {e}")), None),
        cpal::SampleFormat::I16 => device.build_output_stream(config.clone(), move |out: &mut [i16], _| fill(&shared, out, channels, step, |v| (v * 32767.0) as i16), |e| crate::log::log(format!("saída de som: {e}")), None),
        f => return Err(format!("formato de som não suportado: {f:?}")),
    }
    .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok((stream, name))
}

/// Catmull-Rom: troca de taxa (48 kHz → 44,1 kHz) bem mais limpa que a reta entre dois pontos
fn cubic(y0: f32, y1: f32, y2: f32, y3: f32, t: f32) -> f32 {
    let a = -0.5 * y0 + 1.5 * y1 - 1.5 * y2 + 0.5 * y3;
    let b = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c = -0.5 * y0 + 0.5 * y2;
    ((a * t + b) * t + c) * t + y1
}

fn fill<T: Copy>(shared: &Arc<Mutex<Shared>>, out: &mut [T], channels: usize, step: f64, conv: impl Fn(f32) -> T) {
    let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
    let frames = out.len() / channels.max(1);
    let mut peak = 0.0f32;
    for f in 0..frames {
        let (l, r) = if !s.started {
            (0.0, 0.0)
        } else if step == 1.0 {
            match (s.q.pop_front(), s.q.pop_front()) {
                (Some(l), Some(r)) => (l, r),
                _ => {
                    under(&mut s);
                    (0.0, 0.0)
                }
            }
        } else {
            // precisa de 2 amostras à frente pra interpolar
            if s.q.len() < 6 {
                under(&mut s);
                (0.0, 0.0)
            } else {
                let t = s.pos as f32;
                let h = s.hist;
                let l = cubic(h[0], h[2], s.q[0], s.q[2], t);
                let r = cubic(h[1], h[3], s.q[1], s.q[3], t);
                s.pos += step;
                while s.pos >= 1.0 {
                    s.pos -= 1.0;
                    let (a, b) = (s.q.pop_front().unwrap_or(0.0), s.q.pop_front().unwrap_or(0.0));
                    s.hist = [s.hist[2], s.hist[3], a, b, 0.0, 0.0, 0.0, 0.0];
                }
                (l, r)
            }
        };
        let g = s.volume * s.fade;
        if s.started && s.fade < 1.0 {
            s.fade = (s.fade + 1.0 / 240.0).min(1.0);
        }
        let (l, r) = (l * g, r * g);
        peak = peak.max(l.abs()).max(r.abs());
        let o = &mut out[f * channels..(f + 1) * channels];
        if channels == 1 {
            o[0] = conv((l + r) * 0.5);
        } else {
            o[0] = conv(l);
            o[1] = conv(r);
            for x in o.iter_mut().skip(2) {
                *x = conv(0.0);
            }
        }
    }
    s.level = s.level * 0.8 + peak * 0.2;
}

fn under(s: &mut Shared) {
    // acabou o som: espera encher de novo com folga maior (melhor esperar que cortar toda hora)
    s.started = false;
    s.fade = 0.0;
    if !s.playing {
        return;
    }
    s.unders += 1;
    s.target = (s.target + ms(STEP_MS)).min(ms(MAX_MS));
    s.calm_since = Instant::now();
    crate::log::log(format!("som acabou (corte {}), folga agora {} ms", s.unders, s.target * 1000 / (RATE as usize * 2)));
}
