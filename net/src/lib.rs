//! Rede do Telão: salas por código + WebRTC (str0m) direto entre os PCs.
//!
//! Usado pelo app do Mac (via `ffi`, como biblioteca C) e pelo app do Windows (direto em Rust).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

mod cong;

mod host;
mod member;
mod sfu;
mod peer;
mod portmap;
pub mod rendezvous;
pub mod share_code;
mod signal;
mod stun;
mod cloud;
mod transport;
mod turn;
mod viewer;
#[cfg(test)]
mod fuzz;

pub use host::DEFAULT_PORT;
pub use sfu::{SLOTS, Track};
pub use cloud::{RoomOpts, new_key as new_room_key, parse as parse_cloud_code, set_opts as set_room_opts};

pub type EventSink = Arc<dyn Fn(Value) + Send + Sync>;
pub type VideoSink = Arc<dyn Fn(&[u8], bool, u32) + Send + Sync>;
pub type AudioSink = Arc<dyn Fn(&[u8], u32) + Send + Sync>;
/// Chamada: (vaga de quem mandou, mídia, dados, é keyframe, horário RTP)
pub type CallMediaSink = Arc<dyn Fn(usize, Track, &[u8], bool, u32) + Send + Sync>;

fn runtime() -> Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("telao-net")
        .enable_all()
        .build()
        .expect("runtime")
}

pub struct HostHandle {
    tx: mpsc::Sender<host::HostCmd>,
    events: EventSink,
    /// Frame descartado por fila cheia quebra a sequência: próximo precisa ser keyframe
    broken: AtomicBool,
    /// Avisa quando terminou de fechar (fecha a porta no roteador)
    done: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    rt: Option<Runtime>,
}

impl HostHandle {
    /// `signal_url` vazio = servidor de salas dentro deste app (porta aberta no roteador)
    pub fn start(signal_url: String, max_bitrate: u64, events: EventSink) -> Self {
        let rt = runtime();
        // Folga pra ~0,3 s de vídeo + som: com 8 lugares (som manda 100 por segundo) qualquer engasgo
        // da rede jogava frame fora → keyframe gigante → perda → qualidade despencando.
        let (tx, rx) = mpsc::channel(64);
        let mode = if signal_url.is_empty() {
            host::SignalMode::Embedded { port: DEFAULT_PORT }
        } else {
            host::SignalMode::External(signal_url)
        };
        let (done_tx, done) = std::sync::mpsc::channel();
        let ev = events.clone();
        rt.spawn(async move {
            host::run(mode, max_bitrate, ev, rx).await;
            let _ = done_tx.send(());
        });
        HostHandle { tx, events, broken: AtomicBool::new(false), done: std::sync::Mutex::new(Some(done)), rt: Some(rt) }
    }

    /// `data` em Annex-B (00 00 00 01 + NAL), keyframe já com VPS/SPS/PPS na frente.
    pub fn send_video(&self, data: &[u8], keyframe: bool, capture_us: u64) -> bool {
        if self.broken.load(Ordering::Relaxed) && !keyframe {
            return false;
        }
        let ok = self.tx.try_send(host::HostCmd::Video { data: data.into(), keyframe, capture_us }).is_ok();
        if ok {
            if keyframe {
                self.broken.store(false, Ordering::Relaxed);
            }
        } else if !self.broken.swap(true, Ordering::Relaxed) {
            (self.events)(json!({"type": "keyframe_request"}));
        }
        ok
    }

    pub fn send_audio(&self, data: &[u8], capture_us: u64) -> bool {
        self.tx.try_send(host::HostCmd::Audio { data: data.into(), capture_us }).is_ok()
    }

    pub fn set_info(&self, info: Value) {
        let _ = self.tx.try_send(host::HostCmd::Info(info));
    }

    pub fn set_max_bitrate(&self, bps: u64) {
        let _ = self.tx.try_send(host::HostCmd::MaxBitrate(bps));
    }

    /// true = qualidade travada no máximo (não baixa sozinho quando a rede oscila)
    pub fn set_fixed_quality(&self, fixed: bool) {
        let _ = self.tx.try_send(host::HostCmd::Fixed(fixed));
    }
}

impl Drop for HostHandle {
    fn drop(&mut self) {
        // Fecha o canal de comandos: a transmissão encerra, desconecta todos e fecha a porta
        let (closed, _) = mpsc::channel(1);
        drop(std::mem::replace(&mut self.tx, closed));
        if let Some(done) = self.done.lock().ok().and_then(|mut d| d.take()) {
            let _ = done.recv_timeout(std::time::Duration::from_secs(2));
        }
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

pub struct ViewerHandle {
    tx: mpsc::Sender<viewer::ViewerCmd>,
    rt: Option<Runtime>,
}

impl ViewerHandle {
    /// `code`: código "TL-…" de quem transmite, ou código curto + `signal_url` (servidor externo)
    pub fn start(signal_url: String, code: String, name: String, events: EventSink, on_video: VideoSink, on_audio: AudioSink) -> Self {
        let rt = runtime();
        let (tx, rx) = mpsc::channel(8);
        let target = cloud::target(&code).unwrap_or_else(|| match share_code::ShareCode::decode(&code) {
            Some(sc) => viewer::Target::Share(sc),
            None => viewer::Target::Signal { url: signal_url, room: code },
        });
        rt.spawn(viewer::run(target, name, events, on_video, on_audio, rx));
        ViewerHandle { tx, rt: Some(rt) }
    }

    pub fn request_keyframe(&self) {
        let _ = self.tx.try_send(viewer::ViewerCmd::RequestKeyframe);
    }
}

impl Drop for ViewerHandle {
    fn drop(&mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

/// A sala (chamada) rodando neste PC: servidor de salas + quem repassa as mídias.
/// Quem cria também entra nela como uma pessoa normal (`CallMember` com o `signal_url` e a sala).
pub struct CallServer {
    tx: mpsc::Sender<sfu::SfuCmd>,
    done: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    rt: Option<Runtime>,
}

impl CallServer {
    /// `signal_url` vazio = servidor de salas aqui dentro (porta aberta no roteador).
    /// Eventos: signal_url, reach, room {code, room}, member_connected, member_left, stats, error.
    pub fn start(signal_url: String, events: EventSink) -> Self {
        let rt = runtime();
        let (tx, rx) = mpsc::channel(16);
        let mode = if signal_url.is_empty() {
            host::SignalMode::Embedded { port: DEFAULT_PORT }
        } else {
            host::SignalMode::External(signal_url)
        };
        let (done_tx, done) = std::sync::mpsc::channel();
        rt.spawn(async move {
            sfu::run(mode, events, rx).await;
            let _ = done_tx.send(());
        });
        CallServer { tx, done: std::sync::Mutex::new(Some(done)), rt: Some(rt) }
    }

    /// Sala privada: deixa entrar (ou não) quem bateu na porta (`peer` do evento `knock`)
    pub fn admit(&self, peer: &str, ok: bool) {
        let _ = self.tx.try_send(sfu::SfuCmd::Admit(peer.to_string(), ok));
    }

    /// Liga ou desliga a sala privada com a sala aberta
    pub fn set_private(&self, on: bool) {
        let _ = self.tx.try_send(sfu::SfuCmd::Private(on));
    }

    /// Deixa o código desta sala fixo (com a chave do dono) ou solta. Responde no evento `kept`
    pub fn keep(&self, key: &str, on: bool) {
        let _ = self.tx.try_send(sfu::SfuCmd::Keep(key.to_string(), on));
    }
}

impl Drop for CallServer {
    fn drop(&mut self) {
        let (closed, _) = mpsc::channel(1);
        drop(std::mem::replace(&mut self.tx, closed));
        if let Some(done) = self.done.lock().ok().and_then(|mut d| d.take()) {
            let _ = done.recv_timeout(std::time::Duration::from_secs(2));
        }
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

/// Uma pessoa na chamada.
pub struct CallMember {
    tx: mpsc::Sender<member::MemberCmd>,
    events: EventSink,
    /// Frame da tela descartado por fila cheia: o próximo precisa ser keyframe
    broken: AtomicBool,
    /// O mesmo, pra câmera
    cam_broken: AtomicBool,
    rt: Option<Runtime>,
}

impl CallMember {
    /// `code`: código "TL-…" da sala, ou a sala curta + `signal_url` (quem criou, entrando na própria sala).
    /// Eventos: joined, welcome {id, slot, room}, connected, roster {members}, profile {id, profile},
    /// chat {...}, history {messages}, pings, ping {rtt_ms}, rx_stats {slot, rtt_ms, loss},
    /// bitrate {bps} (pra minha tela), keyframe_request, room_closed, disconnected, error.
    pub fn start(signal_url: String, code: String, name: String, events: EventSink, on_media: CallMediaSink) -> Self {
        let rt = runtime();
        let (tx, rx) = mpsc::channel(256);
        let target = cloud::target(&code).unwrap_or_else(|| match share_code::ShareCode::decode(&code) {
            Some(sc) => viewer::Target::Share(sc),
            None => viewer::Target::Signal { url: signal_url, room: code },
        });
        rt.spawn(member::run(target, name, events.clone(), on_media, rx));
        CallMember { tx, events, broken: AtomicBool::new(false), cam_broken: AtomicBool::new(false), rt: Some(rt) }
    }

    /// Voz/som em Opus; tela em HEVC Annex-B (keyframe com VPS/SPS/PPS na frente)
    pub fn send_media(&self, track: Track, data: &[u8], keyframe: bool, capture_us: u64) -> bool {
        let flag = match track {
            Track::Video => Some((&self.broken, "keyframe_request")),
            Track::Cam => Some((&self.cam_broken, "cam_keyframe_request")),
            _ => None,
        };
        if let Some((broken, _)) = flag {
            if broken.load(Ordering::Relaxed) && !keyframe {
                return false;
            }
        }
        let ok = self.tx.try_send(member::MemberCmd::Media { track, data: data.into(), capture_us }).is_ok();
        if let Some((broken, ask)) = flag {
            if ok && keyframe {
                broken.store(false, Ordering::Relaxed);
            } else if !ok && !broken.swap(true, Ordering::Relaxed) {
                (self.events)(json!({"type": ask}));
            }
        }
        ok
    }

    /// chat {text}, profile {profile}, share {info|null}, cam {info|null}, watch {slot, on}, state {state}
    pub fn send(&self, v: Value) {
        let _ = self.tx.try_send(member::MemberCmd::Send(v));
    }

    pub fn request_keyframe(&self, slot: usize) {
        let _ = self.tx.try_send(member::MemberCmd::RequestKeyframe(slot));
    }

    /// Testa o envio até a sala (uns 5 s). Resposta: evento room_speed {mbps} (null = não deu)
    pub fn probe(&self) {
        let _ = self.tx.try_send(member::MemberCmd::Probe);
    }
}

impl Drop for CallMember {
    fn drop(&mut self) {
        let (closed, _) = mpsc::channel(1);
        drop(std::mem::replace(&mut self.tx, closed));
        if let Some(rt) = self.rt.take() {
            rt.shutdown_timeout(std::time::Duration::from_millis(500));
        }
    }
}

/// Antes de entrar: pergunta à sala (código da nuvem) o nome e a foto de quem criou.
/// Bloqueia no máximo uns 8 s; chame numa thread. `None` = sala não achada ou versão antiga.
pub fn peek(code: &str) -> Option<Value> {
    let viewer::Target::Signal { url, .. } = cloud::target(code)? else { return None };
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;
    rt.block_on(async move {
        let mut sig = signal::Signal::connect(&url).await.ok()?;
        sig.send(json!({"t": "peek"}));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while let Some(v) = sig.rx.recv().await {
                match v["t"].as_str() {
                    Some("peek") => return Some(v["data"].clone()),
                    Some("error") | Some("closed") => return None,
                    _ => {}
                }
            }
            None
        })
        .await
        .ok()
        .flatten()
    })
}
