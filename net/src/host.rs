//! Quem transmite: um vídeo codificado uma vez, mandado pra cada amigo conectado.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use str0m::change::{SdpAnswer, SdpPendingOffer};
use str0m::format::Codec;
use str0m::media::{Direction, Frequency, MediaKind, MediaTime, Mid, Pt};
use str0m::{Event, IceConnectionState, Input, Rtc};
use tokio::sync::mpsc;

use std::net::{Ipv4Addr, Ipv6Addr};

use crate::EventSink;
use crate::portmap::{self, PortMap, Reach};
use crate::share_code::ShareCode;
use crate::peer::{drain, payload_type, rtc_config};
use crate::signal::Signal;
use crate::transport::Transport;

pub enum HostCmd {
    Video { data: Arc<[u8]>, keyframe: bool, capture_us: u64 },
    Audio { data: Arc<[u8]>, capture_us: u64 },
    Info(Value),
    MaxBitrate(u64),
    /// Qualidade travada: ignora o controle de banda
    Fixed(bool),
}

struct Viewer {
    id: String,
    name: String,
    rtc: Rtc,
    timeout: Instant,
    pending: Option<SdpPendingOffer>,
    video: Mid,
    audio: Mid,
    video_pt: Option<Pt>,
    audio_pt: Option<Pt>,
    connected: bool,
    /// Só começa a mandar vídeo a partir de um keyframe (antes disso é lixo pro decoder)
    got_keyframe: bool,
    /// Teto de bitrate desse amigo (controle de congestionamento abaixo)
    cap: u64,
    rtt_ms: Option<f64>,
    /// Menor ping visto: acima disso + margem = fila enchendo no caminho
    rtt_base: Option<f64>,
    loss: Option<f32>,
    /// Ping e perda suavizados (um pico isolado do Radmin/Wi-Fi não derruba a qualidade)
    srtt: Option<f64>,
    sloss: f32,
    /// Teto em que o caminho congestionou da última vez: perto dele sobe devagar
    ceiling: Option<(u64, Instant)>,
    nacks: u64,
    plis: u64,
    last_change: Instant,
}

/// O mínimo (igual ao cong.rs): antes 1,5 Mbps e 40% do máximo, internet fraca nunca cabia
const MIN_BITRATE: u64 = 500_000;
const FLOOR_FRACTION: f64 = 0.03;
/// Perda média acima disso = conexão do amigo não está aguentando (até ~4% o reenvio dá conta)
const LOSS_LIMIT: f32 = 0.05;
/// Ping médio subiu tanto acima do normal = fila enchendo em algum roteador (vira lag)
const QUEUE_RTT_MS: f64 = 45.0;

struct Host {
    tr: Transport,
    sig: Signal,
    events: EventSink,
    viewers: Vec<Viewer>,
    info: Value,
    max_bitrate: u64,
    target: u64,
    fixed: bool,
    last_keyframe_req: Instant,
    /// Servidor embutido: porta, alcance e endereços pra montar o código
    share: Option<(u16, Reach, Option<Ipv4Addr>, Option<Ipv6Addr>, Option<Ipv4Addr>)>,
}

/// Onde fica o servidor de salas
pub enum SignalMode {
    /// Servidor externo (testes)
    External(String),
    /// Dentro do próprio app: abre a porta no roteador e o código leva o endereço deste PC
    Embedded { port: u16 },
}

/// Porta padrão do Telão (TCP pro servidor de salas, UDP pro vídeo)
pub const DEFAULT_PORT: u16 = 47800;

/// Escuta em IPv4 e IPv6 na mesma porta; tenta as 10 seguintes se estiver ocupada
pub(crate) fn bind_dual_stack(first: u16) -> Option<(tokio::net::TcpListener, u16)> {
    use socket2::{Domain, Socket, Type};
    for port in first..first + 10 {
        let Ok(s) = Socket::new(Domain::IPV6, Type::STREAM, None) else { continue };
        let _ = s.set_only_v6(false);
        let _ = s.set_reuse_address(true);
        let addr = std::net::SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, port));
        if s.bind(&addr.into()).is_ok() && s.listen(64).is_ok() && s.set_nonblocking(true).is_ok() {
            if let Ok(l) = tokio::net::TcpListener::from_std(s.into()) {
                return Some((l, port));
            }
        }
    }
    None
}

pub async fn run(mode: SignalMode, max_bitrate: u64, events: EventSink, mut cmds: mpsc::Receiver<HostCmd>) {
    let (signal_url, port) = match mode {
        SignalMode::External(url) => (url, None),
        SignalMode::Embedded { port } => match bind_dual_stack(port) {
            Some((listener, port)) => {
                tokio::spawn(crate::rendezvous::serve(listener));
                (format!("ws://127.0.0.1:{port}"), Some(port))
            }
            None => return events(json!({"type": "error", "message": "não deu pra abrir a porta do Telão neste PC"})),
        },
    };
    let (mut tr, mut incoming) = match Transport::gather(port).await {
        Ok(t) => t,
        Err(e) => return events(json!({"type": "error", "message": format!("rede: {e}")})),
    };

    // Abre a porta no roteador e descobre por onde os amigos conseguem chegar
    let mut share: Option<(u16, Reach)> = None;
    let mut portmap: Option<PortMap> = None;
    if let Some(port) = port {
        let (reach, map) = portmap::open(port, tr.lan_v4, tr.global_v6, tr.public_v4).await;
        // Vídeo pelo IPv4 mapeado: anuncia ip_externo:porta como caminho
        if let (true, Some(ext), Some(lan)) = (reach.v4_open, reach.usable_v4(), tr.lan_v4) {
            if let Some(base) = tr.local_addr_for(std::net::IpAddr::V4(lan)) {
                if let Ok(c) = str0m::Candidate::server_reflexive(std::net::SocketAddr::new(ext.into(), base.port()), base, "udp") {
                    tr.candidates.push(c);
                }
            }
        }
        let mut r = reach.to_json();
        r["vpn"] = json!(tr.vpn_v4.map(|(ip, _)| ip));
        r["vpn_name"] = json!(tr.vpn_v4.map(|(_, k)| k));
        events(json!({"type": "reach", "port": port, "reach": r}));
        share = Some((port, reach));
        portmap = Some(map);
    }

    let sig = match Signal::connect(&signal_url).await {
        Ok(s) => s,
        Err(e) => return events(json!({"type": "error", "message": e})),
    };
    sig.send(json!({"t": "host"}));
    events(json!({"type": "paths", "count": tr.candidates.len()}));

    let lan_v4 = tr.lan_v4;
    let global_v6 = tr.global_v6;
    let vpn_v4 = tr.vpn_v4.map(|(ip, _)| ip);
    let mut h = Host {
        tr,
        sig,
        events,
        viewers: Vec::new(),
        info: Value::Null,
        max_bitrate,
        target: max_bitrate,
        fixed: false,
        last_keyframe_req: Instant::now() - Duration::from_secs(1),
        share: share.map(|(port, reach)| (port, reach, lan_v4, global_v6, vpn_v4)),
    };
    // Roteador apaga a porta depois de 1 h: renova a cada 30 min
    let mut renew_tick = tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_secs(1800), Duration::from_secs(1800));
    let mut stats_tick = tokio::time::interval(Duration::from_secs(1));

    loop {
        let next = h.viewers.iter().map(|v| v.timeout).min().unwrap_or_else(|| Instant::now() + Duration::from_millis(200));
        tokio::select! {
            biased;
            cmd = cmds.recv() => match cmd {
                Some(cmd) => h.command(cmd),
                None => break, // app fechou a transmissão
            },
            Some(pkt) = incoming.recv() => {
                if h.tr.should_drop() { continue }
                let Some(recv) = pkt.as_receive() else { continue };
                let input = Input::Receive(pkt.at, recv);
                if let Some(i) = h.viewers.iter().position(|v| v.rtc.accepts(&input)) {
                    let _ = h.viewers[i].rtc.handle_input(input);
                    h.pump(i);
                }
            },
            msg = h.sig.rx.recv() => match msg {
                Some(m) => h.signal(m),
                None => break,
            },
            _ = stats_tick.tick() => h.stats(),
            _ = renew_tick.tick() => if let Some(m) = &portmap { m.renew(lan_v4).await },
            _ = tokio::time::sleep_until(next.into()) => {
                let now = Instant::now();
                for i in 0..h.viewers.len() {
                    if h.viewers[i].timeout <= now {
                        let _ = h.viewers[i].rtc.handle_input(Input::Timeout(now));
                        h.pump(i);
                    }
                }
            },
        }
        h.cleanup();
    }
    for i in 0..h.viewers.len() {
        h.viewers[i].rtc.disconnect();
        h.pump(i);
    }
    if let Some(m) = &portmap {
        m.close().await;
    }
}

impl Host {
    fn emit(&self, v: Value) {
        (self.events)(v)
    }

    fn command(&mut self, cmd: HostCmd) {
        match cmd {
            HostCmd::Video { data, keyframe, capture_us } => {
                let t = MediaTime::new(capture_us * 90 / 1000, Frequency::NINETY_KHZ);
                for i in 0..self.viewers.len() {
                    let v = &mut self.viewers[i];
                    if !v.connected {
                        continue;
                    }
                    v.got_keyframe |= keyframe;
                    let (Some(pt), true) = (v.video_pt, v.got_keyframe) else { continue };
                    if let Some(w) = v.rtc.writer(v.video) {
                        let _ = w.write(pt, Instant::now(), t, data.clone());
                    }
                    self.pump(i);
                }
            }
            HostCmd::Audio { data, capture_us } => {
                let t = MediaTime::new(capture_us * 48 / 1000, Frequency::FORTY_EIGHT_KHZ);
                for i in 0..self.viewers.len() {
                    let v = &mut self.viewers[i];
                    let (true, Some(pt)) = (v.connected, v.audio_pt) else { continue };
                    if let Some(w) = v.rtc.writer(v.audio) {
                        let _ = w.write(pt, Instant::now(), t, data.clone());
                    }
                    self.pump(i);
                }
            }
            HostCmd::Info(info) => {
                self.info = info;
                for v in &self.viewers {
                    self.sig.send(json!({"t": "signal", "to": v.id, "data": {"kind": "info", "info": self.info}}));
                }
            }
            HostCmd::Fixed(fixed) => {
                self.fixed = fixed;
                if fixed {
                    for v in &mut self.viewers {
                        v.cap = self.max_bitrate;
                    }
                }
                self.retarget(true);
            }
            HostCmd::MaxBitrate(b) => {
                self.max_bitrate = b;
                for v in &mut self.viewers {
                    v.cap = v.cap.min(b).max(MIN_BITRATE.min(b));
                }
                self.retarget(true);
            }
        }
    }

    fn signal(&mut self, m: Value) {
        match m["t"].as_str().unwrap_or("") {
            "room" => {
                let room = m["code"].as_str().unwrap_or("").to_string();
                let code = match &self.share {
                    Some((port, reach, lan, v6, vpn)) => ShareCode {
                        port: *port,
                        v4: reach.usable_v4(),
                        v6: *v6,
                        lan: *lan,
                        vpn: *vpn,
                        room: room.clone(),
                    }
                    .encode(),
                    None => room.clone(),
                };
                self.emit(json!({"type": "room", "code": code, "room": room}));
            }
            "peer-joined" => {
                let id = m["peer"].as_str().unwrap_or("").to_string();
                let name = m["name"].as_str().unwrap_or("Amigo").to_string();
                self.add_viewer(id, name);
            }
            "peer-left" => {
                let id = m["peer"].as_str().unwrap_or("");
                if let Some(v) = self.viewers.iter_mut().find(|v| v.id == id) {
                    v.rtc.disconnect();
                }
            }
            "signal" => {
                let from = m["from"].as_str().unwrap_or("");
                if m["data"]["kind"] == "answer" {
                    let Some(i) = self.viewers.iter().position(|v| v.id == from) else { return };
                    let sdp = m["data"]["sdp"].as_str().unwrap_or("");
                    let (Ok(answer), Some(pending)) = (SdpAnswer::from_sdp_string(sdp), self.viewers[i].pending.take()) else {
                        return;
                    };
                    if self.viewers[i].rtc.sdp_api().accept_answer(pending, answer).is_ok() {
                        self.pump(i);
                    }
                }
            }
            "closed" => self.emit(json!({"type": "error", "message": "caiu a conexão com o servidor de salas (quem já está assistindo continua)"})),
            _ => {}
        }
    }

    fn add_viewer(&mut self, id: String, name: String) {
        let now = Instant::now();
        // Começa conservador e sobe sondando até o máximo escolhido
        // Sem o estimador/fila do str0m: ele começa baixo e sobe devagar, e a fila dele
        // segura o vídeo (lag). O controle é nosso, por perda e ping (ver `congestion`).
        let mut rtc = rtc_config(None).build(now);
        for c in &self.tr.candidates {
            let _ = rtc.add_local_candidate(c.clone());
        }
        let mut api = rtc.sdp_api();
        let video = api.add_media(MediaKind::Video, Direction::SendOnly, Some("telao".into()), Some("tela".into()), None);
        let audio = api.add_media(MediaKind::Audio, Direction::SendOnly, Some("telao".into()), Some("som".into()), None);
        let Some((offer, pending)) = api.apply() else { return };

        self.sig.send(json!({"t": "signal", "to": id, "data": {"kind": "offer", "sdp": offer.to_sdp_string()}}));
        if !self.info.is_null() {
            self.sig.send(json!({"t": "signal", "to": id, "data": {"kind": "info", "info": self.info}}));
        }
        self.emit(json!({"type": "viewer_joined", "id": id, "name": name}));
        self.viewers.push(Viewer {
            id,
            name,
            rtc,
            timeout: now,
            pending: Some(pending),
            video,
            audio,
            video_pt: None,
            audio_pt: None,
            connected: false,
            got_keyframe: false,
            cap: self.max_bitrate,
            rtt_ms: None,
            rtt_base: None,
            loss: None,
            srtt: None,
            sloss: 0.0,
            ceiling: None,
            nacks: 0,
            plis: 0,
            last_change: now,
        });
        let i = self.viewers.len() - 1;
        self.pump(i);
    }

    /// Esvazia a saída do Rtc e trata os eventos (que podem gerar mais saída)
    fn pump(&mut self, i: usize) {
        let mut events = Vec::new();
        loop {
            let v = &mut self.viewers[i];
            v.timeout = drain(&mut v.rtc, &self.tr, &mut events);
            if events.is_empty() {
                break;
            }
            for e in events.drain(..) {
                self.event(i, e);
            }
        }
    }

    fn event(&mut self, i: usize, e: Event) {
        let mut want_keyframe = false;
        let mut retarget = false;
        {
            let v = &mut self.viewers[i];
            match e {
                Event::Connected => {
                    v.connected = true;
                    v.video_pt = payload_type(&mut v.rtc, v.video, Codec::H265);
                    v.audio_pt = payload_type(&mut v.rtc, v.audio, Codec::Opus);
                    want_keyframe = true;
                    (self.events)(json!({"type": "viewer_connected", "id": v.id, "name": v.name}));
                }
                Event::IceConnectionStateChange(IceConnectionState::Disconnected) => v.rtc.disconnect(),
                Event::KeyframeRequest(_) => want_keyframe = true,
                Event::MediaEgressStats(s) if s.mid == v.video => {
                    v.rtt_ms = s.rtt.map(|d| d.as_secs_f64() * 1000.0);
                    v.loss = s.loss;
                    v.nacks = s.nacks;
                    v.plis = s.plis + s.firs;
                    retarget = if self.fixed { false } else { congestion(v, self.max_bitrate) };
                }
                _ => {}
            }
        }
        if want_keyframe && self.last_keyframe_req.elapsed() > Duration::from_millis(250) {
            self.last_keyframe_req = Instant::now();
            self.emit(json!({"type": "keyframe_request"}));
        }
        if retarget {
            self.retarget(false);
        }
    }

    /// Um encoder só pra todo mundo: o bitrate segue o amigo com pior conexão
    /// (e o seu upload, que derruba o de todos quando lota).
    fn retarget(&mut self, force: bool) {
        let target = self.viewers.iter().filter(|v| v.connected).map(|v| v.cap).min().unwrap_or(self.max_bitrate);
        let change = (target as f64 - self.target as f64).abs() / self.target.max(1) as f64;
        if force || change > 0.05 {
            self.target = target;
            self.emit(json!({"type": "bitrate", "bps": target}));
        }
    }

    fn stats(&self) {
        let viewers: Vec<Value> = self
            .viewers
            .iter()
            .map(|v| {
                json!({
                    "id": v.id, "name": v.name, "connected": v.connected,
                    "cap_bps": v.cap, "rtt_ms": v.rtt_ms, "loss": v.loss, "nacks": v.nacks, "plis": v.plis,
                })
            })
            .collect();
        self.emit(json!({"type": "stats", "target_bps": self.target, "fixed": self.fixed, "viewers": viewers}));
    }

    fn cleanup(&mut self) {
        let before = self.viewers.len();
        let mut gone = Vec::new();
        self.viewers.retain(|v| {
            let alive = v.rtc.is_alive();
            if !alive {
                gone.push((v.id.clone(), v.name.clone()));
            }
            alive
        });
        for (id, name) in gone {
            self.emit(json!({"type": "viewer_left", "id": id, "name": name}));
        }
        if self.viewers.len() != before {
            self.retarget(false);
        }
    }
}

/// Controle de congestionamento (estilo AIMD), roda a cada relatório do amigo (~200 ms):
/// - olha ping e perda MÉDIOS (um pico isolado não conta);
/// - fila enchendo ou perda alta corta 15% e segura 1,5 s pra fila do roteador esvaziar;
/// - nunca desce de 40% do máximo (o reenvio de pacotes cobre o resto);
/// - tudo bem por 1 s sobe: 12% longe do ponto onde congestionou, 4% perto dele.
/// Devolve true se mudou o teto.
fn congestion(v: &mut Viewer, max: u64) -> bool {
    let now = Instant::now();
    if let Some(rtt) = v.rtt_ms {
        let s = v.srtt.map_or(rtt, |s| s * 0.8 + rtt * 0.2);
        v.srtt = Some(s);
        // Base = menor ping médio; sobe devagar se a rota mudou (senão ficaria "congestionado" pra sempre)
        v.rtt_base = Some(match v.rtt_base {
            None => s,
            Some(b) if s < b => s,
            Some(b) => b + (s - b) * 0.003,
        });
    }
    v.sloss = v.sloss * 0.7 + v.loss.unwrap_or(0.0) * 0.3;
    let queueing = matches!((v.srtt, v.rtt_base), (Some(r), Some(b)) if r > b + QUEUE_RTT_MS.max(b * 0.6));
    let losing = v.sloss > LOSS_LIMIT;
    let floor = MIN_BITRATE.max((max as f64 * FLOOR_FRACTION) as u64).min(max);
    // Esquece o ponto de congestionamento depois de 15 s (a rede pode ter melhorado)
    if v.ceiling.is_some_and(|(_, at)| now - at > Duration::from_secs(15)) {
        v.ceiling = None;
    }
    let old = v.cap;
    if queueing || losing {
        // Uma vez por ping, senão um único pico derruba várias vezes seguidas
        let wait = Duration::from_millis(v.rtt_ms.unwrap_or(100.0).max(400.0) as u64);
        if now - v.last_change >= wait {
            v.ceiling = Some((v.cap, now));
            // perda alta corta mais (até metade): desce rápido até caber na internet do amigo
            let cut = if losing { (1.0 - 2.0 * v.sloss as f64).clamp(0.50, 0.85) } else { 0.85 };
            v.cap = ((v.cap as f64 * cut) as u64).max(floor);
            v.last_change = now + Duration::from_millis(500); // +0,5 s = segura 1,5 s até subir
        }
    } else if now > v.last_change && now - v.last_change >= Duration::from_millis(1000) {
        let near = v.ceiling.is_some_and(|(c, _)| v.cap as f64 >= c as f64 * 0.85);
        let step = if near { 1.04 } else { 1.12 };
        v.cap = ((v.cap as f64 * step) as u64).min(max);
        v.last_change = now;
    }
    v.cap != old
}
