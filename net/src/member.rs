//! Uma pessoa na chamada: manda a voz (e a tela, se estiver transmitindo) pra sala e recebe,
//! por vaga, a voz de todo mundo e a tela de quem ela escolheu assistir.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use str0m::change::SdpOffer;
use str0m::format::{Codec, CodecExtra};
use str0m::media::{Frequency, KeyframeRequestKind, MediaTime, Mid, Pt};
use str0m::{Event, IceConnectionState, Input, Rtc};
use tokio::sync::mpsc;

use crate::cong::Cong;
use crate::peer::{PACE_DESIRED_BPS, PACE_START_BPS, drain, payload_type, rtc_config};
use crate::sfu::{SLOTS, STREAMS, Track};
use crate::signal::Signal;
use crate::transport::Transport;
use crate::viewer::Target;
use crate::{CallMediaSink, EventSink};

pub enum MemberCmd {
    Media { track: Track, data: Arc<[u8]>, capture_us: u64 },
    /// Mensagem pra sala (chat, perfil, tela, assistir, estado)
    Send(Value),
    /// A imagem dessa vaga quebrou: pede uma inteira
    RequestKeyframe(usize),
    /// Teste de internet até a sala (antes de transmitir): resposta no evento room_speed
    Probe,
}

/// Teste de envio até a sala pela trilha leve (que ninguém recebe enquanto não transmito): começa em
/// 1 Mbps e sobe 1,5× a cada meio segundo; para quando o ping sobe mais de 30 ms ou a perda passa de
/// 2%. Resultado = a maior taxa que passou limpa (pesquisa de 16/11: OBS, Cloudflare, IETF)
struct Probe {
    started: Instant,
    rate: f64,
    step_at: Instant,
    last_send: Instant,
    base_rtt: Option<f64>,
    rtt: Option<f64>,
    loss: f32,
    best: f64,
    sent: u64,
}

struct Session {
    tr: Transport,
    sig: Signal,
    events: EventSink,
    on_media: CallMediaSink,
    rtc: Option<Rtc>,
    timeout: Instant,
    /// mid → (vaga, mídia); vaga -1 = o que eu mando
    map: HashMap<Mid, (i64, Track)>,
    up: HashMap<Track, Mid>,
    vpt: Option<Pt>,
    apt: Option<Pt>,
    waiting_key: [bool; STREAMS],
    /// quem transmite na vaga faz atualização gradual (diz no anúncio da tela)
    refresh: [bool; SLOTS],
    /// perda numa tela com atualização gradual: (começo, última perda). Segue mostrando e a imagem se
    /// limpa sozinha em ~1 s; perda seguida por mais de 3 s pede a imagem inteira
    heal: [Option<(Instant, Instant)>; STREAMS],
    /// diagnóstico de quem assiste: imagens que chegaram, descartadas esperando imagem completa, perda
    diag_rx: (u64, u64, u64, Option<f32>),
    /// pacotes de som recebidos (pro log de diagnóstico)
    diag_audio: u64,
    diag_at: Instant,
    last_kf_req: [Instant; STREAMS],
    /// Meu upload (pela tela que mando)
    uplink: Cong,
    sfu_bps: Option<u64>,
    /// Quem a sala diz que está segurando a qualidade
    sfu_by: Option<String>,
    sent_by: Option<String>,
    max_bps: u64,
    fixed: bool,
    sent_bps: u64,
    done: bool,
    /// Até quando espera o caminho direto abrir
    connect_by: Option<Instant>,
    probe: Option<Probe>,
    /// Pra voltar pro servidor de salas se a ligação cair: endereço da sala, meu id e o recibo
    rejoin: (Option<String>, Option<String>, Option<String>),
}

/// Tempo pra tentar todos os caminhos antes de desistir
const CONNECT_TIMEOUT: Duration = Duration::from_secs(25);
/// Quanto esperar a oferta de quem criou depois de pedir pra entrar
const OFFER_TIMEOUT: Duration = Duration::from_secs(60);
/// Quanto esperar na porta de uma sala privada
const KNOCK_TIMEOUT: Duration = Duration::from_secs(90);

pub async fn run(target: Target, name: String, events: EventSink, on_media: CallMediaSink, mut cmds: mpsc::Receiver<MemberCmd>) {
    let (mut tr, mut incoming) = match Transport::gather(None).await {
        Ok(t) => t,
        Err(e) => return events(json!({"type": "error", "message": format!("rede: {e}")})),
    };
    // só a sala da nuvem sabe devolver o lugar de quem caiu
    let mut room_url: Option<String> = None;
    let (sig, code) = match target {
        Target::Share(sc) => {
            let urls = sc.addrs().iter().map(|a| format!("ws://{a}")).collect();
            (Signal::connect_any(urls).await, sc.room)
        }
        Target::Signal { url, room } => {
            room_url = Some(url.clone());
            (Signal::connect(&url).await, room)
        }
    };
    let sig = match sig {
        Ok(s) => s,
        Err(e) => return events(json!({"type": "error", "message": e})),
    };
    // entra primeiro e pede o relay enquanto a sala responde (a oferta só é lida depois, já com o
    // relay se ele veio): antes a entrada esperava o relay e atrasava quando ele demorava
    // na própria sala privada o dono entra direto (a senha só existe neste PC)
    match crate::cloud::owner_pass(&code) {
        Some(pass) => sig.send(json!({"t": "join", "code": code, "name": name, "pass": pass})),
        None => sig.send(json!({"t": "join", "code": code, "name": name})),
    }
    match tr.add_relay(sig.turn.as_ref()).await {
        Ok(a) => events(json!({"type": "diag", "message": format!("relay pronto ({a})")})),
        Err(e) => events(json!({"type": "diag", "message": format!("sem relay: {e}")})),
    }

    let now = Instant::now();
    let mut s = Session {
        tr,
        sig,
        events,
        on_media,
        rtc: None,
        // acorda pra conferir se a oferta chegou (sem ela, ninguém nunca ia avisar)
        timeout: now + OFFER_TIMEOUT,
        map: HashMap::new(),
        up: HashMap::new(),
        vpt: None,
        apt: None,
        waiting_key: [true; STREAMS],
        refresh: [false; SLOTS],
        heal: [None; STREAMS],
        diag_rx: (0, 0, 0, None),
        diag_audio: 0,
        diag_at: now,
        last_kf_req: [now - Duration::from_secs(1); STREAMS],
        uplink: Cong::new(20_000_000),
        sfu_bps: None,
        sfu_by: None,
        sent_by: None,
        max_bps: 20_000_000,
        fixed: false,
        sent_bps: 0,
        done: false,
        connect_by: Some(now + OFFER_TIMEOUT),
        probe: None,
        rejoin: (room_url, None, None),
    };
    let mut back: Option<mpsc::UnboundedReceiver<Result<Signal, String>>> = None;
    let mut leaving = false;

    while !s.done {
        tokio::select! {
            biased;
            cmd = cmds.recv() => match cmd {
                Some(c) => s.command(c),
                None => {
                    leaving = true;
                    break;
                }
            },
            Some(pkt) = incoming.recv() => {
                if s.tr.should_drop() { continue }
                let Some(recv) = pkt.as_receive() else { continue };
                let input = Input::Receive(pkt.at, recv);
                if let Some(rtc) = s.rtc.as_mut() {
                    if rtc.accepts(&input) {
                        let _ = rtc.handle_input(input);
                        s.pump();
                    }
                }
            },
            msg = s.sig.rx.recv() => match msg {
                // 17/11: a ligação com o servidor caiu, mas o vídeo vem direto: fica na sala e volta
                Some(m) if m["t"] == "closed" && back.is_none() && s.rtc.is_some() && s.rejoin_url().is_some() => {
                    let url = s.rejoin_url().unwrap_or_default();
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
                        s.emit(json!({"type": "diag", "message": "voltou pro servidor de salas"}));
                    }
                    Err(_) => {
                        s.emit(json!({"type": "error", "message": "caiu a conexão com a sala"}));
                        break;
                    }
                }
            },
            _ = tokio::time::sleep_until(s.timeout.into()) => {
                if let Some(rtc) = s.rtc.as_mut() {
                    let _ = rtc.handle_input(Input::Timeout(Instant::now()));
                    s.pump();
                } else {
                    s.timeout = Instant::now() + Duration::from_secs(3600);
                }
            },
            // depois do relógio da conexão: se os dois estão prontos, ela vai primeiro (senão os
            // relatórios com o ping e a perda nunca eram lidos durante o teste)
            _ = tokio::time::sleep(Duration::from_millis(10)), if s.probe.is_some() => s.probe_tick(),
        }
        if s.connect_by.is_some_and(|t| Instant::now() > t) {
            s.emit(json!({"type": "error", "message": "Não deu pra conectar na sala: a rede de um de vocês está bloqueando a conexão (acontece em Wi-Fi de empresa, escola ou com firewall). Tentem de novo em outra rede, como o 4G do celular."}));
            break;
        }
        if s.rtc.as_ref().is_some_and(|r| !r.is_alive()) {
            s.emit(json!({"type": "disconnected"}));
            break;
        }
    }
    // saiu de propósito: o servidor conta a saída na hora (sem isso ele esperaria a volta)
    if leaving {
        s.sig.send(json!({"t": "bye"}));
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    if let Some(rtc) = s.rtc.as_mut() {
        rtc.disconnect();
        s.pump();
    }
    // saiu do laço por qualquer motivo: o app fica sabendo e não fica numa sala morta "ao vivo"
    s.emit(json!({"type": "member_gone"}));
}

impl Session {
    fn emit(&self, v: Value) {
        (self.events)(v)
    }

    /// `…/room/CODIGO?p=MEU_ID&r=RECIBO`: volta pro mesmo lugar na sala
    fn rejoin_url(&self) -> Option<String> {
        match &self.rejoin {
            (Some(u), Some(p), Some(r)) => Some(format!("{u}?p={p}&r={r}")),
            _ => None,
        }
    }

    fn command(&mut self, c: MemberCmd) {
        match c {
            MemberCmd::Media { track, data, capture_us } => {
                let (Some(rtc), Some(mid)) = (self.rtc.as_mut(), self.up.get(&track).copied()) else { return };
                let (pt, t) = match track {
                    Track::Video | Track::Cam | Track::Low => (self.vpt, MediaTime::new(capture_us * 90 / 1000, Frequency::NINETY_KHZ)),
                    _ => (self.apt, MediaTime::new(capture_us * 48 / 1000, Frequency::FORTY_EIGHT_KHZ)),
                };
                let Some(pt) = pt else { return };
                if let Some(w) = rtc.writer(mid) {
                    let _ = w.write(pt, Instant::now(), t, data);
                }
                self.pump();
            }
            MemberCmd::Send(v) => {
                if v["kind"] == "share" {
                    let info = &v["info"];
                    self.max_bps = info["max_bps"].as_u64().unwrap_or(self.max_bps);
                    let _ = self.max_bps; // (só o vídeo do Noir precisava do piso do pacer)
                    self.fixed = info["fixed"].as_bool().unwrap_or(false);
                    self.uplink = Cong::new(self.max_bps);
                    self.sent_bps = 0;
                    self.sfu_bps = None;
                    self.retarget();
                }
                self.sig.send(json!({"t": "signal", "data": v}));
            }
            MemberCmd::RequestKeyframe(slot) => self.request_keyframe(slot, true),
            MemberCmd::Probe => {
                if !self.up.contains_key(&Track::Low) || self.rtc.is_none() {
                    // sala de versão antiga (sem a trilha leve) ou ainda conectando: o app usa o outro teste
                    self.emit(json!({"type": "room_speed", "mbps": null}));
                    return;
                }
                let now = Instant::now();
                self.probe = Some(Probe { started: now, rate: 300_000.0, step_at: now + Duration::from_millis(600), last_send: now, base_rtt: None, rtt: None, loss: 0.0, best: 0.0, sent: 0 });
            }
        }
    }

    fn signal(&mut self, m: Value) {
        match m["t"].as_str().unwrap_or("") {
            "joined" => {
                self.rejoin.1 = m["peer"].as_str().map(str::to_string);
                self.rejoin.2 = m["resume"].as_str().map(str::to_string);
                // depois de esperar na porta, o prazo da oferta conta de novo
                let by = Instant::now() + OFFER_TIMEOUT;
                self.timeout = by;
                self.connect_by = Some(by);
                self.emit(json!({"type": "joined"}));
            }
            "waiting" => {
                // sala privada: espera quem criou deixar entrar
                let by = Instant::now() + KNOCK_TIMEOUT;
                self.timeout = by;
                self.connect_by = Some(by);
                self.emit(json!({"type": "waiting"}));
            }
            "error" => {
                self.emit(json!({"type": "error", "message": m["msg"]}));
                self.done = true;
            }
            "host-left" => {
                self.emit(json!({"type": "room_closed"}));
                self.done = true;
            }
            "signal" => {
                let d = &m["data"];
                match d["kind"].as_str().unwrap_or("") {
                    "offer" => self.accept_offer(d),
                    "full" => {
                        self.emit(json!({"type": "error", "message": "A sala está cheia"}));
                        self.done = true;
                    }
                    "bitrate" => {
                        self.sfu_bps = d["bps"].as_u64();
                        self.sfu_by = d["by"].as_str().map(str::to_string);
                        self.retarget();
                    }
                    kind => {
                        if kind == "roster" {
                            self.refresh = [false; SLOTS];
                            for p in d["members"].as_array().into_iter().flatten() {
                                if let Some(slot) = p["slot"].as_u64().map(|v| v as usize).filter(|v| *v < SLOTS) {
                                    self.refresh[slot] = p["sharing"]["refresh"] == true;
                                }
                            }
                        }
                        // welcome, roster, profile, chat, history, pings: vão pro app como estão
                        let mut e = d.clone();
                        e["type"] = json!(kind);
                        self.emit(e);
                    }
                }
            }
            "closed" => {
                self.emit(json!({"type": "error", "message": "caiu a conexão com a sala"}));
                self.done = true;
            }
            _ => {}
        }
    }

    fn accept_offer(&mut self, d: &Value) {
        let Ok(offer) = SdpOffer::from_sdp_string(d["sdp"].as_str().unwrap_or("")) else { return };
        self.map.clear();
        self.up.clear();
        if let Some(map) = d["map"].as_object() {
            for (mid, v) in map {
                let slot = v[0].as_i64().unwrap_or(-2);
                let track = match v[1].as_str() {
                    Some("video") => Track::Video,
                    Some("sound") => Track::Sound,
                    Some("cam") => Track::Cam,
                    Some("low") => Track::Low,
                    _ => Track::Voice,
                };
                let mid = Mid::from(mid.as_str());
                self.map.insert(mid, (slot, track));
                if slot == -1 || (slot == crate::sfu::LOW_SLOT && track == Track::Low) {
                    self.up.insert(track, mid);
                }
            }
        }
        let mut rtc = rtc_config(Some(PACE_START_BPS)).build(Instant::now());
        rtc.bwe().set_desired_bitrate(str0m::bwe::Bitrate::bps(PACE_DESIRED_BPS));
        for c in &self.tr.candidates {
            let _ = rtc.add_local_candidate(c.clone());
        }
        let Ok(answer) = rtc.sdp_api().accept_offer(offer) else {
            self.emit(json!({"type": "error", "message": "não deu pra combinar a conexão"}));
            self.done = true;
            return;
        };
        self.sig.send(json!({"t": "signal", "data": {"kind": "answer", "sdp": answer.to_sdp_string()}}));
        self.rtc = Some(rtc);
        self.connect_by = Some(Instant::now() + CONNECT_TIMEOUT);
        self.pump();
    }

    /// Teste até a sala, a cada 10 ms: manda quadros de mentira no ritmo da vez e, a cada meio
    /// segundo, vê se o caminho aguentou
    fn probe_tick(&mut self) {
        let now = Instant::now();
        // o relógio da conexão (relatórios com ping e perda) também anda durante o teste
        if now >= self.timeout {
            if let Some(rtc) = self.rtc.as_mut() {
                let _ = rtc.handle_input(Input::Timeout(now));
            }
            self.pump();
        }
        let Some(p) = self.probe.as_mut() else { return };
        let finish = |best: f64, capped: bool| json!({"type": "room_speed", "mbps": if best > 0.0 { Some(best / 1e6) } else { None }, "capped": capped});
        if now >= p.step_at {
            let rising = matches!((p.rtt, p.base_rtt), (Some(r), Some(b)) if r > b + 30.0);
            let losing = p.loss > 0.02;
            let first = p.rate <= 300_000.0;
            if !first && (rising || losing) {
                let ev = finish(p.best, false);
                crate::member::log_probe(&self.events, p, "parou: o caminho encheu");
                self.probe = None;
                self.emit(ev);
                return;
            }
            if !first {
                p.best = p.rate;
            }
            p.rate = if first { 1_000_000.0 } else { p.rate * 1.5 };
            p.step_at = now + Duration::from_millis(500);
            if p.rate > 80_000_000.0 || now.duration_since(p.started) > Duration::from_millis(5500) {
                let ev = finish(p.best, true);
                crate::member::log_probe(&self.events, p, "fim");
                self.probe = None;
                self.emit(ev);
                return;
            }
        }
        let dt = now.duration_since(p.last_send).as_secs_f64();
        let bytes = (p.rate * dt / 8.0) as usize;
        if bytes < 1000 {
            return;
        }
        p.last_send = now;
        p.sent += bytes as u64;
        let mut nal = Vec::with_capacity(bytes + 8);
        // quadro comum (TRAIL_R), não enchimento: o str0m descarta NAL de enchimento sem mandar nada
        // (o primeiro teste "media" 38 Mbps sem ter mandado um byte). Ninguém recebe a trilha leve
        // antes de eu transmitir, então o conteúdo não importa
        nal.extend_from_slice(&[0, 0, 0, 1, 1 << 1, 1]);
        nal.resize(bytes + 6, 0x5A);
        let ts = now.duration_since(p.started).as_micros() as u64;
        let (Some(rtc), Some(mid), Some(pt)) = (self.rtc.as_mut(), self.up.get(&Track::Low).copied(), self.vpt) else {
            self.probe = None;
            self.emit(json!({"type": "room_speed", "mbps": null}));
            return;
        };
        if let Some(w) = rtc.writer(mid) {
            let _ = w.write(pt, now, MediaTime::new(ts * 90 / 1000, Frequency::NINETY_KHZ), nal);
        }
        self.pump();
    }

    /// `stream`: a tela da vaga `s` é `s`, a câmera é `s + SLOTS`
    fn request_keyframe(&mut self, stream: usize, force: bool) {
        if stream >= STREAMS || (!force && self.last_kf_req[stream].elapsed() < Duration::from_millis(200)) {
            return;
        }
        self.last_kf_req[stream] = Instant::now();
        let (slot, want) = (stream % SLOTS, if stream >= SLOTS { Track::Cam } else { Track::Video });
        let mid = self.map.iter().find(|(_, (s, t))| *s == slot as i64 && *t == want).map(|(m, _)| *m);
        let (Some(rtc), Some(mid)) = (self.rtc.as_mut(), mid) else { return };
        if let Some(mut w) = rtc.writer(mid) {
            let _ = w.request_keyframe(None, KeyframeRequestKind::Pli);
        }
        self.pump();
    }

    /// Bitrate da minha tela = o menor entre o que a sala pede (pior amigo assistindo) e o meu upload
    fn retarget(&mut self) {
        let sfu = self.sfu_bps.unwrap_or(self.max_bps);
        let target = if self.fixed { self.max_bps } else { sfu.min(self.uplink.cap).min(self.max_bps) };
        // por que baixou: o meu upload ou a internet de um amigo ("" = o meu)
        let by = if self.fixed || target as f64 >= self.max_bps as f64 * 0.85 {
            None
        } else if self.uplink.cap < sfu {
            Some(String::new())
        } else {
            self.sfu_by.clone()
        };
        if self.sent_bps == 0 || (target as f64 - self.sent_bps as f64).abs() / self.sent_bps as f64 > 0.05 || by != self.sent_by {
            self.sent_bps = target;
            self.sent_by = by.clone();
            self.emit(json!({"type": "bitrate", "bps": target, "by": by}));
        }
    }

    fn pump(&mut self) {
        let mut events = Vec::new();
        loop {
            let Some(rtc) = self.rtc.as_mut() else { return };
            self.timeout = drain(rtc, &self.tr, &mut events);
            if events.is_empty() {
                return;
            }
            let mut need_key = Vec::new();
            for e in events.drain(..) {
                match e {
                    Event::Connected => {
                        self.connect_by = None;
                        if let Some(rtc) = self.rtc.as_mut() {
                            if let Some(v) = self.up.get(&Track::Video) {
                                self.vpt = payload_type(rtc, *v, Codec::H265);
                            }
                            if let Some(a) = self.up.get(&Track::Voice) {
                                self.apt = payload_type(rtc, *a, Codec::Opus);
                            }
                        }
                        self.emit(json!({"type": "connected"}));
                    }
                    Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                        if let Some(r) = self.rtc.as_mut() {
                            r.disconnect();
                        }
                    }
                    Event::MediaData(d) => {
                        let Some((slot, track)) = self.map.get(&d.mid).copied() else { continue };
                        if slot < 0 || slot as usize >= SLOTS {
                            continue;
                        }
                        let slot = slot as usize;
                        let mut key = false;
                        if matches!(track, Track::Video | Track::Cam) {
                            let stream = if track == Track::Cam { slot + SLOTS } else { slot };
                            key = matches!(d.codec_extra, CodecExtra::H265(e) if e.is_keyframe);
                            let now = Instant::now();
                            // 1,5 s sem perda: a atualização gradual já limpou a imagem
                            if self.heal[stream].is_some_and(|(_, last)| now - last > Duration::from_millis(1500)) {
                                self.heal[stream] = None;
                            }
                            if key {
                                self.waiting_key[stream] = false;
                                self.heal[stream] = None;
                            } else if !d.contiguous && !self.waiting_key[stream] {
                                let gradual = track == Track::Video && self.refresh[slot];
                                match self.heal[stream] {
                                    Some((since, _)) if gradual && now - since < Duration::from_secs(3) => self.heal[stream] = Some((since, now)),
                                    None if gradual => self.heal[stream] = Some((now, now)),
                                    _ => {
                                        self.waiting_key[stream] = true;
                                        self.heal[stream] = None;
                                    }
                                }
                            }
                            if track == Track::Video {
                                self.diag_rx.0 += 1;
                                if key {
                                    self.diag_rx.2 += 1;
                                }
                                if self.waiting_key[stream] {
                                    self.diag_rx.1 += 1;
                                }
                            }
                            if self.waiting_key[stream] {
                                need_key.push(stream);
                                continue;
                            }
                        }
                        if matches!(track, Track::Voice | Track::Sound) {
                            self.diag_audio += 1;
                        }
                        (self.on_media)(slot, track, &d.data, key, d.time.numer() as u32);
                    }
                    Event::KeyframeRequest(r) if self.up.get(&Track::Video) == Some(&r.mid) => {
                        self.emit(json!({"type": "keyframe_request"}));
                    }
                    Event::KeyframeRequest(r) if self.up.get(&Track::Cam) == Some(&r.mid) => {
                        self.emit(json!({"type": "cam_keyframe_request"}));
                    }
                    Event::KeyframeRequest(r) if self.up.get(&Track::Low) == Some(&r.mid) => {
                        self.emit(json!({"type": "low_keyframe_request"}));
                    }
                    Event::MediaEgressStats(st) => {
                        let rtt = st.rtt.map(|d| d.as_secs_f64() * 1000.0);
                        if self.up.get(&Track::Low) == Some(&st.mid) {
                            if let Some(p) = self.probe.as_mut() {
                                if let Some(r) = rtt {
                                    p.rtt = Some(r);
                                    p.base_rtt = Some(p.base_rtt.map_or(r, |b| b.min(r)));
                                }
                                p.loss = st.loss.unwrap_or(0.0);
                            }
                        }
                        if self.up.get(&Track::Video) == Some(&st.mid) {
                            if self.uplink.update(rtt, st.loss, self.max_bps) {
                                self.retarget();
                            }
                        }
                        if self.up.get(&Track::Voice) == Some(&st.mid) && rtt.is_some() {
                            self.emit(json!({"type": "ping", "rtt_ms": rtt}));
                        }
                    }
                    Event::MediaIngressStats(st) => {
                        if let Some((slot, Track::Video)) = self.map.get(&st.mid).copied() {
                            self.diag_rx.3 = st.loss;
                            // a cada 4 s, uma linha no log: o vídeo está chegando? quanto se perde?
                            if self.diag_at.elapsed() >= Duration::from_secs(4) {
                                self.diag_at = Instant::now();
                                let (n, drop, keys, loss) = self.diag_rx;
                                self.emit(json!({"type": "diag", "message": format!(
                                    "vídeo recebido: {n} imagens ({keys} completas-chave), {drop} descartadas esperando imagem completa, perda {:.1}% · som: {} pacotes",
                                    loss.unwrap_or(0.0) * 100.0, self.diag_audio
                                )}));
                            }
                            self.emit(json!({
                                "type": "rx_stats", "slot": slot,
                                "rtt_ms": st.rtt.map(|d| d.as_secs_f64() * 1000.0), "loss": st.loss,
                            }));
                        }
                    }
                    _ => {}
                }
            }
            for slot in need_key {
                self.request_keyframe(slot, false);
            }
        }
    }
}

fn log_probe(events: &EventSink, p: &Probe, why: &str) {
    events(json!({"type": "diag", "message": format!(
        "teste até a sala ({why}): {:.1} Mbps limpos, ping {:?} (base {:?}), perda {:.1}%, {:.1} MB",
        p.best / 1e6, p.rtt, p.base_rtt, p.loss * 100.0, p.sent as f64 / 1e6
    )}));
}
