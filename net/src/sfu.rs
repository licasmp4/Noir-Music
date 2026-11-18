//! A sala (chamada) estilo Discord: roda no PC de quem cria e repassa tudo entre as pessoas.
//!
//! - Cada pessoa tem UMA conexão WebRTC com a sala. Por ela manda a própria voz e, se quiser, a tela
//!   (vídeo + som do PC). Quem transmite manda uma vez só; a sala copia pra quem está assistindo.
//! - Por ela também recebe, em "vagas" fixas (uma por pessoa): voz de todo mundo e a tela de quem
//!   ela escolheu assistir. Vagas fixas = a conexão é combinada uma vez só, sem renegociar.
//! - Chat, perfis (foto/banner, até GIF) e o estado de cada um passam pelo servidor de salas (WebSocket).
//! - Só quem cria precisa de porta aberta (ou Radmin VPN): os amigos só falam com ele.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use str0m::change::{SdpAnswer, SdpPendingOffer};
use str0m::format::{Codec, CodecExtra};
use str0m::media::{Direction, KeyframeRequestKind, MediaKind, Mid, Pt};
use str0m::{Event, IceConnectionState, Input, Rtc};
use tokio::sync::mpsc;

use crate::EventSink;
use crate::cong::Cong;
use crate::host::SignalMode;
use crate::peer::{drain, payload_type, rtc_config};
use crate::portmap::{self, PortMap, Reach};
use crate::share_code::ShareCode;
use crate::signal::Signal;
use crate::transport::Transport;

/// Pessoas por sala (cada uma ocupa uma vaga)
pub const SLOTS: usize = 10;
/// Mensagens do chat guardadas pra quem entra depois
const HISTORY: usize = 200;
/// Teto de bitrate de uma tela se quem transmite não disser
const DEFAULT_MAX_BPS: u64 = 20_000_000;
/// Ping até a sala abaixo disso = mesmo computador ou mesma casa (não conta como "o melhor")
const LOCAL_RTT_MS: f64 = 4.0;
/// Abaixo disso a tela boa já teria que descer de 1080p60 (5,6 Mbps no mínimo): aí sim vale a leve
const LOW_BELOW_BPS: u64 = 6_000_000;

pub enum SfuCmd {
    /// Deixar entrar (ou não) quem bateu na porta
    Admit(String, bool),
    /// Sala privada: liga ou desliga
    Private(bool),
    /// Deixar este código fixo (com a chave) ou soltar
    Keep(String, bool),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Track {
    /// Vídeo da tela
    Video,
    /// Som do PC de quem transmite
    Sound,
    /// Microfone
    Voice,
    /// Webcam (vídeo à parte da tela: dá pra mandar as duas juntas)
    Cam,
    /// Versão leve da tela (540p a 30), feita só quando algum amigo precisa: a sala manda ela pra
    /// quem tem internet fraca (e pras prévias da aba Assistir) e a boa pro resto (16/11). Também
    /// leva o teste de internet até a sala, antes de transmitir
    Low,
}

/// Marca da trilha leve na oferta. Não é -1 de propósito: o Noir antigo só usa as trilhas -1 pra
/// mandar e trata nome desconhecido como voz; com -3 ele simplesmente ignora
pub const LOW_SLOT: i64 = -3;
/// Quadros guardados desde o último quadro-chave (quem entra recebe na hora, sem pedir outro)
const GOP_FRAMES: usize = 150;
const GOP_BYTES: usize = 6_000_000;

/// Telas e câmeras têm números diferentes: a tela da vaga `s` é `s`, a câmera é `s + SLOTS`
pub const STREAMS: usize = SLOTS * 2;

/// As três mídias de uma vaga
#[derive(Clone, Copy)]
struct Tracks {
    video: Mid,
    sound: Mid,
    voice: Mid,
    cam: Mid,
}

struct Member {
    id: String,
    name: String,
    slot: usize,
    rtc: Rtc,
    timeout: Instant,
    pending: Option<SdpPendingOffer>,
    /// O que chega dessa pessoa
    up: Tracks,
    /// O que vai pra essa pessoa, por vaga de quem fala/transmite
    down: Vec<Tracks>,
    vpt: Option<Pt>,
    apt: Option<Pt>,
    connected: bool,
    /// diagnóstico: pedaços de vídeo e keyframes que chegaram dessa pessoa
    diag_video: u64,
    diag_key: u64,
    /// Telas que ela escolheu assistir (por vaga)
    watching: [bool; STREAMS],
    /// Das que ela recebe, quais são só a prévia da aba Assistir: o vídeo vai, mas ela não conta
    /// como "vendo" nem segura a qualidade de quem transmite (16/11)
    preview: [bool; STREAMS],
    /// Já mandou um keyframe dessa tela pra ela? Antes disso é lixo pro decoder.
    got_key: [bool; STREAMS],
    /// Por vaga de quem transmite: ela recebe a versão leve da tela (internet fraca ou prévia)
    on_low: [bool; SLOTS],
    /// Desde quando está fraca (pra ir pra leve) ou boa (pra voltar), por vaga
    weak_since: [Option<Instant>; SLOTS],
    good_since: [Option<Instant>; SLOTS],
    /// Quando foi pra leve, e quanto esperar antes de tentar a boa de novo (dobra se não aguentar)
    low_at: [Option<Instant>; SLOTS],
    low_backoff: [u64; SLOTS],
    /// Trilha leve que chega dessa pessoa
    up_low: Mid,
    /// Como quem transmite: a versão leve está sendo pedida a ela (bps, 0 = desligada)
    low_sent: u64,
    /// A versão leve nunca chegou depois de pedida: não pede mais (Mac, placa sem GPU)
    low_dead: bool,
    low_asked_at: Option<Instant>,
    low_seen: bool,
    last_low_kf_req: Instant,
    /// Como quem transmite: quadros desde o último quadro-chave da tela (quem entra recebe na hora)
    gop: Vec<(str0m::media::MediaTime, Arc<[u8]>)>,
    gop_bytes: usize,
    /// Banda de descida dela (mede pela tela que assiste)
    cong: Cong,
    profile: Value,
    /// Null = não está transmitindo; senão {width, height, fps, title, app, max_bps, fixed}
    sharing: Value,
    /// Miniatura ao vivo da tela dela (JPEG em base64), pros cartões da sala
    thumb: Value,
    thumb_cam: Value,
    /// Null = câmera desligada; senão {width, height, fps}
    cam: Value,
    /// A música que ela deixou mostrar pra sala ({title, artist, playing, pos, dur, art}) ou null
    song: Value,
    /// {mic, deaf}
    state: Value,
    /// Último bitrate pedido a ela como quem transmite
    sent_target: u64,
    /// Quem está segurando a qualidade dela (o amigo com a internet mais fraca), já avisado
    sent_by: Option<String>,
    last_kf_req: Instant,
    last_cam_kf_req: Instant,
    joined_at: u64,
}

struct Sfu {
    tr: Transport,
    sig: Signal,
    events: EventSink,
    members: Vec<Member>,
    chat: Vec<Value>,
    chat_seq: u64,
    room: String,
    share: Option<(u16, Reach, Option<Ipv4Addr>, Option<Ipv6Addr>, Option<Ipv4Addr>)>,
    /// Sala criada no servidor da nuvem
    cloud: bool,
    /// Senha do dono pra entrar direto na própria sala privada
    pass: String,
    /// Nome do dono (vai pro servidor quando a sala fica fixa)
    name: String,
    /// O código curto (sem o NOIR-) e o recibo pra voltar se a ligação com o servidor cair
    raw_room: String,
    resume: Option<String>,
    private: bool,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub async fn run(mode: SignalMode, events: EventSink, mut cmds: mpsc::Receiver<SfuCmd>) {
    // Nuvem primeiro: entra só com o código. Sem ela (sem internet), o código leva os IPs deste PC.
    let cloud_sig = match &mode {
        SignalMode::Embedded { .. } => match crate::cloud::create().await {
            Ok(s) => Some(s),
            Err(e) => {
                events(json!({"type": "cloud_error", "message": e}));
                None
            }
        },
        SignalMode::External(_) => None,
    };
    let cloud = cloud_sig.is_some();
    let (signal_url, port) = match mode {
        SignalMode::External(url) => (url, None),
        // A porta UDP fixa continua: se o roteador abrir, é um caminho a mais pro vídeo
        SignalMode::Embedded { port } if cloud => (String::new(), Some(port)),
        SignalMode::Embedded { port } => match crate::host::bind_dual_stack(port) {
            Some((listener, port)) => {
                tokio::spawn(crate::rendezvous::serve(listener));
                (format!("ws://127.0.0.1:{port}"), Some(port))
            }
            None => return events(json!({"type": "error", "message": "não deu pra abrir a porta do Noir neste PC"})),
        },
    };
    events(json!({"type": "signal_url", "url": signal_url}));
    let (mut tr, mut incoming) = match Transport::gather(port).await {
        Ok(t) => t,
        Err(e) => return events(json!({"type": "error", "message": format!("rede: {e}")})),
    };

    let mut share = None;
    let mut portmap: Option<PortMap> = None;
    if let Some(port) = port {
        let (reach, map) = portmap::open(port, tr.lan_v4, tr.global_v6, tr.public_v4).await;
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
        r["cloud"] = json!(cloud);
        events(json!({"type": "reach", "port": port, "reach": r}));
        share = Some((port, reach, tr.lan_v4, tr.global_v6, tr.vpn_v4.map(|(ip, _)| ip)));
        portmap = Some(map);
    }
    let lan_v4 = tr.lan_v4;

    let sig = match cloud_sig {
        Some(s) => s,
        None => match Signal::connect(&signal_url).await {
            Ok(s) => s,
            Err(e) => return events(json!({"type": "error", "message": e})),
        },
    };
    let opts = crate::cloud::opts();
    let pass = crate::cloud::new_key();
    sig.send(json!({"t": "host", "private": opts.private, "pass": pass, "name": opts.name}));
    // Antes de alguém entrar: o caminho pelo relay tem que estar na lista quando a conexão for combinada
    if cloud {
        match tr.add_relay(sig.turn.as_ref()).await {
            Ok(a) => events(json!({"type": "diag", "message": format!("relay pronto ({a})")})),
            Err(e) => events(json!({"type": "diag", "message": format!("sem relay: {e}")})),
        }
    }

    let mut s = Sfu { tr, sig, events, members: Vec::new(), chat: Vec::new(), chat_seq: 0, room: String::new(), share, cloud, pass, name: opts.name, raw_room: String::new(), resume: None, private: opts.private };
    // voltando pro servidor de salas (a internet piscou): o resultado chega aqui
    let mut back: Option<tokio::sync::mpsc::UnboundedReceiver<Result<Signal, String>>> = None;
    let mut leaving = false;
    let mut renew_tick = tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_secs(1800), Duration::from_secs(1800));
    let mut stats_tick = tokio::time::interval(Duration::from_secs(1));

    loop {
        let next = s.members.iter().map(|m| m.timeout).min().unwrap_or_else(|| Instant::now() + Duration::from_millis(200));
        tokio::select! {
            biased;
            cmd = cmds.recv() => match cmd {
                None => {
                    leaving = true;
                    break;
                }
                Some(SfuCmd::Admit(peer, ok)) => s.sig.send(json!({"t": "admit", "to": peer, "ok": ok})),
                Some(SfuCmd::Private(on)) => {
                    s.private = on;
                    s.sig.send(json!({"t": "private", "on": on}));
                }
                Some(SfuCmd::Keep(key, on)) => s.sig.send(json!({"t": "keep", "key": key, "on": on, "name": s.name})),
            },
            Some(pkt) = incoming.recv() => {
                if s.tr.should_drop() { continue }
                let Some(recv) = pkt.as_receive() else { continue };
                let input = Input::Receive(pkt.at, recv);
                if let Some(i) = s.members.iter().position(|m| m.rtc.accepts(&input)) {
                    let _ = s.members[i].rtc.handle_input(input);
                    s.pump(i);
                }
            },
            msg = s.sig.rx.recv() => match msg {
                // 17/11: a ligação com o servidor de salas caiu (a internet piscou). Antes a sala
                // fechava pra todo mundo; agora o vídeo segue direto entre os PCs e o Noir tenta voltar
                Some(m) if m["t"] == "closed" && back.is_none() && crate::cloud::return_url(&s.raw_room, s.resume.as_deref()).is_some() => {
                    let url = crate::cloud::return_url(&s.raw_room, s.resume.as_deref()).unwrap_or_default();
                    s.sig = Signal::idle();
                    back = Some(Signal::retry(url, crate::cloud::RETURN_SECS));
                    s.emit(json!({"type": "diag", "message": "servidor de salas caiu: voltando"}));
                }
                Some(m) => s.signal(m),
                None => break,
            },
            Some(r) = async { match back.as_mut() { Some(b) => b.recv().await, None => std::future::pending().await } } => {
                back = None;
                match r {
                    Ok(sig) => {
                        s.sig = sig;
                        s.sig.send(json!({"t": "host", "private": s.private, "pass": s.pass, "name": s.name}));
                        s.emit(json!({"type": "diag", "message": "voltou pro servidor de salas"}));
                    }
                    Err(e) => {
                        s.emit(json!({"type": "error", "message": format!("o servidor de salas parou ({e})")}));
                        break;
                    }
                }
            },
            _ = stats_tick.tick() => {
                s.stats();
                s.review_low();
            }
            _ = renew_tick.tick() => if let Some(m) = &portmap { m.renew(lan_v4).await },
            _ = tokio::time::sleep_until(next.into()) => {
                let now = Instant::now();
                for i in 0..s.members.len() {
                    if s.members[i].timeout <= now {
                        let _ = s.members[i].rtc.handle_input(Input::Timeout(now));
                        s.pump(i);
                    }
                }
            },
        }
        s.cleanup();
    }
    // saiu de propósito: o servidor fecha a sala na hora (sem isso ele esperaria a volta)
    if leaving {
        s.sig.send(json!({"t": "bye"}));
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    for i in 0..s.members.len() {
        s.members[i].rtc.disconnect();
        s.pump(i);
    }
    if let Some(m) = &portmap {
        m.close().await;
    }
}

impl Sfu {
    fn emit(&self, v: Value) {
        (self.events)(v)
    }

    fn send_to(&self, id: &str, data: Value) {
        self.sig.send(json!({"t": "signal", "to": id, "data": data}));
    }

    fn broadcast(&self, data: Value) {
        for m in &self.members {
            self.send_to(&m.id, data.clone());
        }
    }

    fn roster(&self) -> Value {
        let list: Vec<Value> = self
            .members
            .iter()
            .map(|m| {
                json!({
                    "id": m.id, "slot": m.slot, "name": m.name, "connected": m.connected,
                    "sharing": m.sharing, "cam": m.cam, "state": m.state, "joined_at": m.joined_at,
                    "rtt_ms": m.cong.rtt_ms,
                    "watching": (0..STREAMS).filter(|s| m.watching[*s] && !m.preview[*s]).collect::<Vec<usize>>(),
                })
            })
            .collect();
        json!({"kind": "roster", "members": list})
    }

    fn signal(&mut self, m: Value) {
        match m["t"].as_str().unwrap_or("") {
            "room" => {
                if let Some(r) = m["resume"].as_str() {
                    self.resume = Some(r.to_string());
                }
                // voltou depois de cair: a sala é a mesma, nada muda pro app
                if !self.raw_room.is_empty() {
                    return;
                }
                self.raw_room = m["code"].as_str().unwrap_or("").to_string();
                self.room = m["code"].as_str().unwrap_or("").to_string();
                if self.cloud {
                    crate::cloud::set_owner(&self.room, &self.pass);
                    // Quem criou também entra pela nuvem, com o mesmo código dos amigos
                    self.room = crate::cloud::encode(&self.room);
                    self.emit(json!({"type": "room", "code": self.room, "room": self.room, "fixed": m["fixed"].as_bool().unwrap_or(false)}));
                    return;
                }
                let code = match &self.share {
                    Some((port, reach, lan, v6, vpn)) => {
                        ShareCode { port: *port, v4: reach.usable_v4(), v6: *v6, lan: *lan, vpn: *vpn, room: self.room.clone() }.encode()
                    }
                    None => self.room.clone(),
                };
                self.emit(json!({"type": "room", "code": code, "room": self.room}));
            }
            "knock" => {
                let peer = m["peer"].as_str().unwrap_or("").to_string();
                let name = m["name"].as_str().unwrap_or("Amigo").to_string();
                self.emit(json!({"type": "knock", "peer": peer, "name": name}));
            }
            "knock-gone" => self.emit(json!({"type": "knock_gone", "peer": m["peer"]})),
            "kept" => self.emit(json!({"type": "kept", "ok": m["ok"], "on": m["on"]})),
            "peer-joined" => {
                let id = m["peer"].as_str().unwrap_or("").to_string();
                let name = m["name"].as_str().unwrap_or("Amigo").to_string();
                self.add_member(id, name);
            }
            "peek" => {
                // alguém com o código quer ver de quem é a sala antes de entrar: nome e foto de quem criou
                let peer = m["peer"].as_str().unwrap_or("").to_string();
                let owner = self.members.first();
                let profile = owner.map(|o| o.profile.clone()).unwrap_or(Value::Null);
                let data = json!({
                    "name": owner.map(|o| o.name.clone()).unwrap_or_default(),
                    "avatar": profile["avatar"], "color": profile["color"], "pid": profile["pid"],
                    "people": self.members.len(),
                    "sharing": self.members.iter().filter(|o| !o.sharing.is_null()).count(),
                });
                self.sig.send(json!({"t": "peek-reply", "to": peer, "data": data}));
            }
            "peer-left" => {
                let id = m["peer"].as_str().unwrap_or("");
                if let Some(v) = self.members.iter_mut().find(|v| v.id == id) {
                    v.rtc.disconnect();
                }
            }
            "signal" => {
                let from = m["from"].as_str().unwrap_or("").to_string();
                if let Some(i) = self.members.iter().position(|v| v.id == from) {
                    self.member_message(i, &m["data"]);
                }
            }
            "closed" => self.emit(json!({"type": "error", "message": "o servidor de salas parou"})),
            _ => {}
        }
    }

    fn add_member(&mut self, id: String, name: String) {
        let Some(slot) = (0..SLOTS).find(|s| !self.members.iter().any(|m| m.slot == *s)) else {
            self.send_to(&id, json!({"kind": "full"}));
            return;
        };
        let now = Instant::now();
        let mut rtc = rtc_config(Some(crate::peer::PACE_START_BPS)).build(now);
        rtc.bwe().set_desired_bitrate(str0m::bwe::Bitrate::bps(crate::peer::PACE_DESIRED_BPS));
        for c in &self.tr.candidates {
            let _ = rtc.add_local_candidate(c.clone());
        }
        let mut api = rtc.sdp_api();
        let mut map = serde_json::Map::new();
        let add = |api: &mut str0m::change::SdpApi, kind, dir, slot: i64, track: &str, map: &mut serde_json::Map<String, Value>| {
            let mid = api.add_media(kind, dir, Some(format!("vaga{slot}")), Some(track.into()), None);
            map.insert(mid.to_string(), json!([slot, track]));
            mid
        };
        let up = Tracks {
            video: add(&mut api, MediaKind::Video, Direction::RecvOnly, -1, "video", &mut map),
            sound: add(&mut api, MediaKind::Audio, Direction::RecvOnly, -1, "sound", &mut map),
            voice: add(&mut api, MediaKind::Audio, Direction::RecvOnly, -1, "voice", &mut map),
            cam: add(&mut api, MediaKind::Video, Direction::RecvOnly, -1, "cam", &mut map),
        };
        let up_low = add(&mut api, MediaKind::Video, Direction::RecvOnly, LOW_SLOT, "low", &mut map);
        let down: Vec<Tracks> = (0..SLOTS)
            .map(|s| Tracks {
                video: add(&mut api, MediaKind::Video, Direction::SendOnly, s as i64, "video", &mut map),
                sound: add(&mut api, MediaKind::Audio, Direction::SendOnly, s as i64, "sound", &mut map),
                voice: add(&mut api, MediaKind::Audio, Direction::SendOnly, s as i64, "voice", &mut map),
                cam: add(&mut api, MediaKind::Video, Direction::SendOnly, s as i64, "cam", &mut map),
            })
            .collect();
        let Some((offer, pending)) = api.apply() else { return };

        self.send_to(&id, json!({"kind": "welcome", "id": id, "slot": slot, "room": self.room}));
        self.send_to(&id, json!({"kind": "offer", "sdp": offer.to_sdp_string(), "map": map}));
        self.send_to(&id, json!({"kind": "history", "messages": self.chat}));
        for o in &self.members {
            if !o.profile.is_null() {
                self.send_to(&id, json!({"kind": "profile", "id": o.id, "profile": o.profile}));
            }
            if !o.thumb.is_null() && !o.sharing.is_null() {
                self.send_to(&id, json!({"kind": "thumb", "slot": o.slot, "jpg": o.thumb}));
            }
            if !o.thumb_cam.is_null() && !o.cam.is_null() {
                self.send_to(&id, json!({"kind": "thumb", "slot": o.slot + SLOTS, "jpg": o.thumb_cam}));
            }
            if !o.song.is_null() {
                self.send_to(&id, json!({"kind": "song", "id": o.id, "song": o.song}));
            }
        }
        self.members.push(Member {
            id,
            name,
            slot,
            rtc,
            timeout: now,
            pending: Some(pending),
            up,
            down,
            vpt: None,
            apt: None,
            connected: false,
            diag_video: 0,
            diag_key: 0,
            watching: [false; STREAMS],
            preview: [false; STREAMS],
            got_key: [false; STREAMS],
            on_low: [false; SLOTS],
            weak_since: [None; SLOTS],
            good_since: [None; SLOTS],
            low_at: [None; SLOTS],
            low_backoff: [30; SLOTS],
            up_low,
            low_sent: 0,
            low_dead: false,
            low_asked_at: None,
            low_seen: false,
            last_low_kf_req: now - Duration::from_secs(1),
            gop: Vec::new(),
            gop_bytes: 0,
            cong: Cong::new(DEFAULT_MAX_BPS),
            profile: Value::Null,
            sharing: Value::Null,
            thumb: Value::Null,
            thumb_cam: Value::Null,
            cam: Value::Null,
            song: Value::Null,
            state: json!({"mic": true, "deaf": false}),
            sent_target: 0,
            sent_by: None,
            last_kf_req: now - Duration::from_secs(1),
            last_cam_kf_req: now - Duration::from_secs(1),
            joined_at: now_ms(),
        });
        let i = self.members.len() - 1;
        self.pump(i);
        self.broadcast(self.roster());
    }

    fn member_message(&mut self, i: usize, d: &Value) {
        match d["kind"].as_str().unwrap_or("") {
            "answer" => {
                let sdp = d["sdp"].as_str().unwrap_or("");
                let (Ok(answer), Some(pending)) = (SdpAnswer::from_sdp_string(sdp), self.members[i].pending.take()) else { return };
                if self.members[i].rtc.sdp_api().accept_answer(pending, answer).is_ok() {
                    self.pump(i);
                }
            }
            "profile" => {
                let mut p = d["profile"].clone();
                if let Some(n) = p["name"].as_str() {
                    let n: String = n.trim().chars().take(32).collect();
                    if !n.is_empty() {
                        self.members[i].name = n.clone();
                        p["name"] = json!(n);
                    }
                }
                self.members[i].profile = p.clone();
                let id = self.members[i].id.clone();
                self.broadcast(json!({"kind": "profile", "id": id, "profile": p}));
                self.broadcast(self.roster());
            }
            "chat" => {
                let text: String = d["text"].as_str().unwrap_or("").chars().take(2000).collect();
                if text.trim().is_empty() {
                    return;
                }
                self.chat_seq += 1;
                let m = &self.members[i];
                let msg = json!({"kind": "chat", "seq": self.chat_seq, "id": m.id, "name": m.name, "text": text, "at": now_ms()});
                self.chat.push(msg.clone());
                if self.chat.len() > HISTORY {
                    self.chat.remove(0);
                }
                self.broadcast(msg);
            }
            "diag_req" => {
                // alguém quer o diagnóstico da sala inteira: pede o log de cada um
                let from = self.members[i].id.clone();
                for m in &self.members {
                    if m.id != from {
                        self.send_to(&m.id, json!({"kind": "diag_req", "from": from}));
                    }
                }
            }
            "diag_resp" => {
                let to = d["to"].as_str().unwrap_or("").to_string();
                let text: String = d["text"].as_str().unwrap_or("").chars().take(60_000).collect();
                let name = self.members[i].name.clone();
                if self.members.iter().any(|m| m.id == to) {
                    self.send_to(&to, json!({"kind": "diag_resp", "name": name, "text": text}));
                }
            }
            "react" => {
                // reação rápida (um emoji) por cima do vídeo; não fica no histórico
                let emoji: String = d["emoji"].as_str().unwrap_or("").chars().take(12).collect();
                if emoji.is_empty() {
                    return;
                }
                let m = &self.members[i];
                self.broadcast(json!({"kind": "react", "id": m.id, "name": m.name, "emoji": emoji}));
            }
            "thumb" => {
                // até ~60 KB; só de quem está transmitindo; vai pra todo mundo menos ela
                let jpg = &d["jpg"];
                let cam = d["cam"].as_bool().unwrap_or(false);
                let on = if cam { !self.members[i].cam.is_null() } else { !self.members[i].sharing.is_null() };
                if !on || jpg.as_str().is_none_or(|j| j.len() > 80_000) {
                    return;
                }
                if cam {
                    self.members[i].thumb_cam = jpg.clone();
                } else {
                    self.members[i].thumb = jpg.clone();
                }
                let (from, slot) = (self.members[i].id.clone(), self.members[i].slot + if cam { SLOTS } else { 0 });
                let msg = json!({"kind": "thumb", "slot": slot, "jpg": jpg});
                for m in &self.members {
                    if m.id != from {
                        self.send_to(&m.id, msg.clone());
                    }
                }
            }
            "share" => {
                let was = !self.members[i].sharing.is_null();
                self.members[i].sharing = d["info"].clone();
                if self.members[i].sharing.is_null() {
                    self.members[i].thumb = Value::Null;
                }
                let slot = self.members[i].slot;
                if self.members[i].sharing.is_null() {
                    // Parou: ninguém mais "tem" keyframe dessa tela
                    for m in &mut self.members {
                        m.got_key[slot] = false;
                        m.watching[slot] = false;
                        m.on_low[slot] = false;
                    }
                    let me = &mut self.members[i];
                    me.gop.clear();
                    me.gop_bytes = 0;
                    me.low_sent = 0;
                    me.low_asked_at = None;
                } else if !was {
                    self.members[i].sent_target = 0;
                    self.members[i].low_dead = false;
                    self.members[i].low_seen = false;
                }
                self.broadcast(self.roster());
                self.retarget(slot);
            }
            "cam" => {
                self.members[i].cam = d["info"].clone();
                let cam = self.members[i].slot + SLOTS;
                if self.members[i].cam.is_null() {
                    self.members[i].thumb_cam = Value::Null;
                    for m in &mut self.members {
                        m.got_key[cam] = false;
                        m.watching[cam] = false;
                    }
                }
                self.broadcast(self.roster());
            }
            "watch" => {
                let slot = d["slot"].as_u64().unwrap_or(999) as usize;
                if slot >= STREAMS {
                    return;
                }
                let on = d["on"].as_bool().unwrap_or(false);
                let preview = on && d["preview"].as_bool().unwrap_or(false);
                let was_low = slot < SLOTS && self.members[i].on_low[slot];
                self.members[i].watching[slot] = on;
                self.members[i].preview[slot] = preview;
                if slot < SLOTS {
                    // prévia vai na versão leve (se quem transmite consegue fazer); assistir de verdade
                    // começa na boa, e a sala desce essa pessoa pra leve se a internet dela não aguentar
                    let low = on && preview && self.low_ok(slot);
                    self.members[i].on_low[slot] = low;
                    self.members[i].weak_since[slot] = None;
                    self.members[i].good_since[slot] = None;
                    self.members[i].low_at[slot] = low.then(Instant::now);
                    if on && (low != was_low || !self.members[i].got_key[slot]) {
                        self.members[i].got_key[slot] = false;
                        if low {
                            self.low_keyframe_from(slot);
                        } else if !self.send_gop(i, slot) {
                            self.keyframe_from(slot);
                        }
                    }
                    self.low_demand(slot);
                } else {
                    self.members[i].got_key[slot] = false;
                    if on {
                        self.keyframe_from(slot);
                    }
                }
                self.retarget(slot % SLOTS);
                // todo mundo fica sabendo quem está vendo quem (as fotinhas no canto de quem transmite)
                self.broadcast(self.roster());
            }
            "song" => {
                // a música de quem transmite (opcional, a pessoa liga nos Ajustes); vai pra todo mundo menos ela
                let song = clean_song(&d["song"]);
                if song == self.members[i].song && song.is_null() {
                    return;
                }
                self.members[i].song = song.clone();
                let from = self.members[i].id.clone();
                let msg = json!({"kind": "song", "id": from, "song": song});
                for m in &self.members {
                    if m.id != from {
                        self.send_to(&m.id, msg.clone());
                    }
                }
            }
            "state" => {
                self.members[i].state = d["state"].clone();
                self.broadcast(self.roster());
            }
            "jam" => {
                // Jam: pedidos de quem ouve (tocar, pular, pôr na fila) e o estado de quem toca.
                // A sala só repassa; quem toca decide. `to` manda pra uma pessoa só.
                let data = &d["data"];
                if data.to_string().len() > 300_000 {
                    return;
                }
                let msg = json!({"kind": "jam", "from": self.members[i].id, "name": self.members[i].name, "data": data});
                match d["to"].as_str() {
                    Some(to) => {
                        let to = to.to_string();
                        self.send_to(&to, msg);
                    }
                    None => {
                        let from = self.members[i].id.clone();
                        for m in &self.members {
                            if m.id != from {
                                self.send_to(&m.id, msg.clone());
                            }
                        }
                    }
                }
            }
            "tx_stats" => {
                // o placar de quem transmite (captura, codificador, envio): vai só pra quem está vendo
                let slot = self.members[i].slot;
                if slot >= SLOTS {
                    return;
                }
                let (from, name) = (self.members[i].id.clone(), self.members[i].name.clone());
                let msg = json!({"kind": "tx_stats", "slot": slot, "name": name, "stats": d["stats"], "fps": d["fps"]});
                for m in &self.members {
                    if m.id != from && m.watching[slot] {
                        self.send_to(&m.id, msg.clone());
                    }
                }
            }
            "keyframe" => {
                // Quem assiste perdeu pedaço: pede uma imagem inteira pra quem transmite
                let slot = d["slot"].as_u64().unwrap_or(999) as usize;
                if slot < STREAMS {
                    self.members[i].got_key[slot] = false;
                    self.keyframe_from(slot);
                }
            }
            _ => {}
        }
    }

    /// Pede keyframe pra quem está transmitindo na vaga `slot` (no máximo 4 por segundo)
    fn keyframe_from(&mut self, stream: usize) {
        let (slot, cam) = (stream % SLOTS, stream >= SLOTS);
        let Some(p) = self.members.iter().position(|m| m.slot == slot) else { return };
        let m = &mut self.members[p];
        let last = if cam { &mut m.last_cam_kf_req } else { &mut m.last_kf_req };
        if last.elapsed() < Duration::from_millis(250) {
            return;
        }
        *last = Instant::now();
        let mid = if cam { m.up.cam } else { m.up.video };
        if let Some(mut w) = m.rtc.writer(mid) {
            let _ = w.request_keyframe(None, KeyframeRequestKind::Pli);
        }
        self.pump(p);
    }

    /// Bitrate de quem transmite na vaga `slot` = a pior descida entre quem assiste (um encoder só)
    fn retarget(&mut self, slot: usize) {
        let Some(p) = self.members.iter().position(|m| m.slot == slot) else { return };
        let info = &self.members[p].sharing;
        if info.is_null() {
            return;
        }
        let max = info["max_bps"].as_u64().unwrap_or(DEFAULT_MAX_BPS);
        // a sala repassa esse vídeo: o ritmo de envio tem que folgar acima dele
        let _ = max;
        let fixed = info["fixed"].as_bool().unwrap_or(false);
        let worst = self
            .members
            .iter()
            .filter(|m| m.connected && m.watching[slot] && !m.preview[slot] && !m.on_low[slot] && m.slot != slot)
            .min_by_key(|m| m.cong.cap)
            .map(|m| (m.cong.cap, m.name.clone()));
        let target = if fixed { max } else { worst.as_ref().map_or(max, |w| w.0).min(max) };
        // só conta como "segurando" se baixou de verdade (mais de 15% abaixo do escolhido)
        let by = worst.filter(|w| !fixed && (w.0 as f64) < max as f64 * 0.85).map(|w| w.1);
        let old = self.members[p].sent_target;
        if old == 0 || (target as f64 - old as f64).abs() / old as f64 > 0.05 || by != self.members[p].sent_by {
            self.members[p].sent_target = target;
            self.members[p].sent_by = by.clone();
            let id = self.members[p].id.clone();
            self.send_to(&id, json!({"kind": "bitrate", "bps": target, "by": by}));
        }
    }

    fn pump(&mut self, i: usize) {
        let mut events = Vec::new();
        loop {
            let m = &mut self.members[i];
            m.timeout = drain(&mut m.rtc, &self.tr, &mut events);
            if events.is_empty() {
                break;
            }
            for e in events.drain(..) {
                self.event(i, e);
            }
        }
    }

    fn event(&mut self, i: usize, e: Event) {
        match e {
            Event::Connected => {
                let m = &mut self.members[i];
                m.connected = true;
                // Todas as vagas de descida usam o mesmo número de codec; pega de uma vaga que não é dela
                let any = m.down[(m.slot + 1) % SLOTS];
                m.vpt = payload_type(&mut m.rtc, any.video, Codec::H265);
                m.apt = payload_type(&mut m.rtc, any.voice, Codec::Opus);
                let ev = json!({"type": "member_connected", "id": m.id, "name": m.name});
                let watching: Vec<usize> = (0..STREAMS).filter(|s| m.watching[*s]).collect();
                self.emit(ev);
                self.emit(json!({"type": "diag", "message": format!("conexão aberta com {}", self.members[i].name)}));
                for s in watching {
                    self.keyframe_from(s);
                }
                self.broadcast(self.roster());
            }
            Event::IceConnectionStateChange(IceConnectionState::Disconnected) => self.members[i].rtc.disconnect(),
            Event::MediaData(d) => {
                let m = &self.members[i];
                let slot = m.slot;
                let data: Arc<[u8]> = d.data.into();
                if d.mid == m.up.video {
                    let key = matches!(d.codec_extra, CodecExtra::H265(x) if x.is_keyframe);
                    let (name, len) = (m.name.clone(), data.len());
                    let m = &mut self.members[i];
                    m.diag_video += 1;
                    if key {
                        m.diag_key += 1;
                    }
                    let (gv, gk) = (m.diag_video, m.diag_key);
                    if gv == 1 || (key && gk == 1) || gv == 600 {
                        let watchers = self.members.iter().filter(|o| o.connected && o.watching[slot] && o.slot != slot).count();
                        self.emit(json!({"type": "diag", "message": format!("sala recebeu vídeo de {name}: {gv} pedaços, {gk} keyframes (último {len} bytes, chave={key}), {watchers} assistindo")}));
                    }
                    // guarda desde o último quadro-chave: quem entra recebe tudo na hora
                    let m = &mut self.members[i];
                    if key {
                        m.gop.clear();
                        m.gop_bytes = 0;
                    }
                    if key || !m.gop.is_empty() {
                        m.gop_bytes += data.len();
                        m.gop.push((d.time, data.clone()));
                        if m.gop.len() > GOP_FRAMES || m.gop_bytes > GOP_BYTES {
                            m.gop.clear();
                            m.gop_bytes = 0;
                        }
                    }
                    self.forward(slot, Track::Video, key, d.time, data);
                } else if d.mid == m.up_low {
                    let key = matches!(d.codec_extra, CodecExtra::H265(x) if x.is_keyframe);
                    self.members[i].low_seen = true;
                    self.forward_low(slot, key, d.time, data);
                } else if d.mid == m.up.cam {
                    let key = matches!(d.codec_extra, CodecExtra::H265(x) if x.is_keyframe);
                    self.forward(slot, Track::Cam, key, d.time, data);
                } else if d.mid == m.up.sound {
                    self.forward(slot, Track::Sound, false, d.time, data);
                } else if d.mid == m.up.voice {
                    self.forward(slot, Track::Voice, false, d.time, data);
                }
            }
            Event::KeyframeRequest(r) => {
                // Pedido de quem assiste numa vaga: repassa pra quem transmite nela
                let m = &mut self.members[i];
                if let Some(slot) = m.down.iter().position(|t| t.video == r.mid) {
                    m.got_key[slot] = false;
                    if m.on_low[slot] {
                        self.low_keyframe_from(slot);
                    } else {
                        self.keyframe_from(slot);
                    }
                } else if let Some(slot) = m.down.iter().position(|t| t.cam == r.mid) {
                    m.got_key[slot + SLOTS] = false;
                    self.keyframe_from(slot + SLOTS);
                }
            }
            Event::MediaEgressStats(st) => {
                // Teto = o máximo de quem transmite esse vídeo. Antes era fixo em 80 Mbps, e o piso
                // (40% do teto) ficava em 32: a sala nunca baixava a qualidade pra quem tem internet
                // mais fraca, e a pessoa ficava pedindo imagem-chave sem parar (medido 06/11).
                let slot_max = |slot: usize| {
                    self.members.iter().find(|o| o.slot == slot).and_then(|o| o.sharing["max_bps"].as_u64()).unwrap_or(DEFAULT_MAX_BPS)
                };
                let maxes: Vec<u64> = (0..SLOTS).map(slot_max).collect();
                let m = &mut self.members[i];
                if let Some(slot) = m.down.iter().position(|t| t.video == st.mid) {
                    let rtt = st.rtt.map(|d| d.as_secs_f64() * 1000.0);
                    if m.watching[slot] && m.cong.update(rtt, st.loss, maxes[slot]) {
                        for s in 0..SLOTS {
                            if self.members[i].watching[s] {
                                self.retarget(s);
                            }
                        }
                    }
                }
            }
            Event::MediaIngressStats(st) => {
                // Ping de quem só fala (não assiste nada) também aparece pra todo mundo
                let m = &mut self.members[i];
                if st.mid == m.up.voice || st.mid == m.up.video {
                    if let Some(r) = st.rtt {
                        m.cong.rtt_ms = Some(r.as_secs_f64() * 1000.0);
                    }
                }
            }
            _ => {}
        }
    }

    /// Copia uma mídia de quem está na vaga `from` pra todo mundo que deve receber.
    /// Tela (vídeo e som) só pra quem escolheu assistir; voz pra todo mundo.
    fn forward(&mut self, from: usize, track: Track, key: bool, time: str0m::media::MediaTime, data: Arc<[u8]>) {
        let video = matches!(track, Track::Video | Track::Cam);
        let only_watchers = track != Track::Voice;
        // tela e câmera da mesma pessoa são assistidas separadamente
        let stream = if track == Track::Cam { from + SLOTS } else { from };
        let mut need_key = false;
        for j in 0..self.members.len() {
            let m = &mut self.members[j];
            if m.slot == from || !m.connected || (only_watchers && !m.watching[stream]) {
                continue;
            }
            // quem está na versão leve recebe ela no lugar (forward_low)
            if track == Track::Video && m.on_low[from] {
                continue;
            }
            if video {
                m.got_key[stream] |= key;
                if !m.got_key[stream] {
                    need_key = true;
                    continue;
                }
            }
            let pt = if video { m.vpt } else { m.apt };
            let Some(pt) = pt else { continue };
            let t = m.down[from];
            let mid = match track {
                Track::Video => t.video,
                Track::Sound => t.sound,
                Track::Voice => t.voice,
                Track::Cam => t.cam,
                Track::Low => t.video,
            };
            if let Some(w) = m.rtc.writer(mid) {
                let _ = w.write(pt, Instant::now(), time, data.clone());
            }
            self.pump(j);
        }
        if need_key {
            self.keyframe_from(stream);
        }
    }

    /// A versão leve vai pra quem está nela, no lugar da tela boa (mesma trilha de descida)
    fn forward_low(&mut self, from: usize, key: bool, time: str0m::media::MediaTime, data: Arc<[u8]>) {
        let mut need_key = false;
        for j in 0..self.members.len() {
            let m = &mut self.members[j];
            if m.slot == from || !m.connected || !m.watching[from] || !m.on_low[from] {
                continue;
            }
            m.got_key[from] |= key;
            if !m.got_key[from] {
                need_key = true;
                continue;
            }
            let Some(pt) = m.vpt else { continue };
            let mid = m.down[from].video;
            if let Some(w) = m.rtc.writer(mid) {
                let _ = w.write(pt, Instant::now(), time, data.clone());
            }
            self.pump(j);
        }
        if need_key {
            self.low_keyframe_from(from);
        }
    }

    /// Quem entra recebe na hora os quadros desde o último quadro-chave (guardados), sem pedir outro
    /// a quem transmite: imagem mais rápida e sem rajada pros outros. false = não tinha o que mandar
    fn send_gop(&mut self, j: usize, slot: usize) -> bool {
        let Some(p) = self.members.iter().position(|m| m.slot == slot) else { return false };
        if self.members[p].gop.is_empty() || self.members[p].sharing.is_null() {
            return false;
        }
        let frames = self.members[p].gop.clone();
        let m = &self.members[j];
        let (Some(pt), true) = (m.vpt, m.connected) else { return false };
        let mid = m.down[slot].video;
        for (t, d) in frames {
            if let Some(w) = self.members[j].rtc.writer(mid) {
                let _ = w.write(pt, Instant::now(), t, d);
            }
            self.pump(j);
        }
        self.members[j].got_key[slot] = true;
        true
    }

    /// Pede um quadro-chave da versão leve a quem transmite na vaga `slot`
    fn low_keyframe_from(&mut self, slot: usize) {
        let Some(p) = self.members.iter().position(|m| m.slot == slot) else { return };
        let m = &mut self.members[p];
        if m.last_low_kf_req.elapsed() < Duration::from_millis(250) {
            return;
        }
        m.last_low_kf_req = Instant::now();
        let mid = m.up_low;
        if let Some(mut w) = m.rtc.writer(mid) {
            let _ = w.request_keyframe(None, KeyframeRequestKind::Pli);
        }
        self.pump(p);
    }

    /// Quem transmite na vaga consegue fazer a versão leve?
    fn low_ok(&self, slot: usize) -> bool {
        self.members.iter().find(|m| m.slot == slot).is_some_and(|m| m.sharing["low"].as_bool().unwrap_or(false) && !m.low_dead)
    }

    /// Liga, ajusta ou desliga a versão leve de quem transmite na vaga, pelo que os amigos nela precisam
    fn low_demand(&mut self, slot: usize) {
        let Some(p) = self.members.iter().position(|m| m.slot == slot) else { return };
        let users: Vec<(bool, u64)> = self.members.iter().filter(|m| m.connected && m.watching[slot] && m.on_low[slot] && m.slot != slot).map(|m| (m.preview[slot], m.cong.cap)).collect();
        let bps = if users.is_empty() {
            0
        } else {
            // quem assiste de verdade na leve manda na taxa; só prévia: o bastante pra um cartão
            let real = users.iter().filter(|u| !u.0).map(|u| u.1).min();
            match real {
                Some(c) => (c as f64 * 0.85) as u64,
                None => 900_000,
            }
            .clamp(300_000, 2_500_000)
        };
        let old = self.members[p].low_sent;
        if (old == 0) != (bps == 0) || (old > 0 && (bps as f64 - old as f64).abs() / old as f64 > 0.10) {
            self.members[p].low_sent = bps;
            if old == 0 && bps > 0 {
                self.members[p].low_asked_at = Some(Instant::now());
            }
            let id = self.members[p].id.clone();
            self.send_to(&id, json!({"kind": "low", "bps": bps}));
        }
    }

    /// A cada 1 s: quem assiste com internet bem mais fraca que os outros vai pra versão leve (depois
    /// de 3 s fraca) e volta pra boa quando melhora (10 s boa, tentando de novo com espera que dobra)
    fn review_low(&mut self) {
        let now = Instant::now();
        for slot in 0..SLOTS {
            let Some(p) = self.members.iter().position(|m| m.slot == slot) else { continue };
            if self.members[p].sharing.is_null() {
                continue;
            }
            // pediu a leve e ela não veio em 4 s: quem transmite não consegue (Mac, sem placa): volta
            // todo mundo pra boa e não pede mais
            let dead = self.members[p].low_sent > 0
                && !self.members[p].low_seen
                && self.members[p].low_asked_at.is_some_and(|a| now.duration_since(a) > Duration::from_secs(4));
            if dead {
                self.members[p].low_dead = true;
                self.emit(json!({"type": "diag", "message": format!("versão leve da tela de {} não chegou: todo mundo na boa", self.members[p].name)}));
            }
            let ok = self.low_ok(slot);
            let viewers: Vec<usize> = (0..self.members.len()).filter(|&j| {
                let m = &self.members[j];
                m.connected && m.watching[slot] && m.slot != slot
            }).collect();
            let mut changed = false;
            for &j in &viewers {
                let preview = self.members[j].preview[slot];
                let want_low = if !ok {
                    false
                } else if preview {
                    true
                } else {
                    let cap = self.members[j].cong.cap;
                    // quem está no mesmo computador ou na mesma casa da sala (ping < 4 ms) não entra
                    // na comparação: a internet dele com a sala não tem limite, e perto dele todo
                    // amigo de verdade parecia fraco e ia pra 540p (16/11: "toda live é 540p")
                    let local = |o: usize| self.members[o].cong.rtt_ms.is_some_and(|r| r < LOCAL_RTT_MS);
                    let best = viewers.iter().filter(|&&o| o != j && !self.members[o].preview[slot] && !local(o)).map(|&o| self.members[o].cong.cap).max();
                    let m = &mut self.members[j];
                    match best {
                        // assistindo sozinho: a tela boa já se ajusta a ela
                        None => false,
                        Some(b) if !m.on_low[slot] => {
                            // só vai pra leve (540p) quem não aguenta nem a boa enxuta: com 9 Mbps,
                            // por exemplo, a boa ainda é 1080p60, e antes ele caía pra 540p só por
                            // ter menos da metade do melhor da sala (16/11)
                            if (cap as f64) < b as f64 * 0.5 && cap < LOW_BELOW_BPS {
                                let since = *m.weak_since[slot].get_or_insert(now);
                                now.duration_since(since) >= Duration::from_secs(3)
                            } else {
                                m.weak_since[slot] = None;
                                false
                            }
                        }
                        Some(b) => {
                            let waited = m.low_at[slot].is_some_and(|a| now.duration_since(a) >= Duration::from_secs(m.low_backoff[slot]));
                            if (cap as f64) >= b as f64 * 0.7 || cap >= LOW_BELOW_BPS * 6 / 5 || waited {
                                let since = *m.good_since[slot].get_or_insert(now);
                                !(now.duration_since(since) >= Duration::from_secs(10) || waited)
                            } else {
                                m.good_since[slot] = None;
                                true
                            }
                        }
                    }
                };
                let m = &mut self.members[j];
                if want_low != m.on_low[slot] {
                    let back_fast = !want_low && m.low_at[slot].is_some_and(|a| now.duration_since(a) < Duration::from_secs(20));
                    m.on_low[slot] = want_low;
                    m.got_key[slot] = false;
                    m.weak_since[slot] = None;
                    m.good_since[slot] = None;
                    if want_low {
                        // voltou pra leve logo depois de tentar a boa: espera o dobro antes da próxima
                        if m.low_at[slot].is_some() {
                            m.low_backoff[slot] = (m.low_backoff[slot] * 2).min(240);
                        }
                        m.low_at[slot] = Some(now);
                    } else if !back_fast {
                        m.low_backoff[slot] = 30;
                    }
                    let name = m.name.clone();
                    self.emit(json!({"type": "diag", "message": format!("{name} {} na tela da vaga {slot}", if want_low { "foi pra versão leve" } else { "voltou pra versão boa" })}));
                    if want_low {
                        self.low_keyframe_from(slot);
                    } else {
                        self.keyframe_from(slot);
                    }
                    changed = true;
                }
            }
            if changed {
                self.retarget(slot);
            }
            self.low_demand(slot);
        }
    }

    fn stats(&mut self) {
        // Ping de cada um na lista (a cada 1 s, só se mudou bastante, pra não inundar)
        let members: Vec<Value> = self.members.iter().map(|m| json!({"id": m.id, "rtt_ms": m.cong.rtt_ms, "cap": m.cong.cap})).collect();
        self.emit(json!({"type": "stats", "members": members}));
        self.broadcast(json!({"kind": "pings", "members": members}));
    }

    fn cleanup(&mut self) {
        let mut gone = Vec::new();
        self.members.retain(|m| {
            let alive = m.rtc.is_alive();
            if !alive {
                gone.push((m.id.clone(), m.name.clone(), m.slot, !m.sharing.is_null()));
            }
            alive
        });
        for (id, name, slot, _) in &gone {
            self.emit(json!({"type": "member_left", "id": id, "name": name}));
            for m in &mut self.members {
                m.watching[*slot] = false;
                m.got_key[*slot] = false;
                m.watching[*slot + SLOTS] = false;
                m.got_key[*slot + SLOTS] = false;
            }
        }
        if !gone.is_empty() {
            self.broadcast(self.roster());
        }
    }
}

/// Só o que a ilha precisa, com limite de tamanho (a capa vem como JPEG pequeno em base64)
fn clean_song(v: &Value) -> Value {
    let Some(title) = v["title"].as_str().map(|t| t.chars().take(200).collect::<String>()).filter(|t| !t.trim().is_empty()) else {
        return Value::Null;
    };
    let artist: String = v["artist"].as_str().unwrap_or("").chars().take(200).collect();
    let art = v["art"].as_str().filter(|a| a.len() <= 60_000);
    json!({
        "title": title,
        "artist": artist,
        "playing": v["playing"].as_bool().unwrap_or(false),
        "pos": v["pos"].as_f64().unwrap_or(0.0).clamp(0.0, 86_400.0),
        "dur": v["dur"].as_f64().unwrap_or(0.0).clamp(0.0, 86_400.0),
        "art": art,
    })
}
