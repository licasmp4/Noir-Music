//! Quem assiste: recebe vídeo HEVC e áudio Opus e entrega pro app decodificar.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use str0m::change::SdpOffer;
use str0m::format::CodecExtra;
use str0m::media::{KeyframeRequestKind, MediaKind, Mid};
use str0m::{Event, IceConnectionState, Input, Rtc};
use tokio::sync::mpsc;

use crate::peer::{drain, rtc_config};
use crate::signal::Signal;
use crate::transport::Transport;
use crate::share_code::ShareCode;
use crate::{AudioSink, EventSink, VideoSink};

pub enum ViewerCmd {
    RequestKeyframe,
}

struct Session {
    tr: Transport,
    sig: Signal,
    events: EventSink,
    on_video: VideoSink,
    on_audio: AudioSink,
    rtc: Option<Rtc>,
    timeout: Instant,
    video: Option<Mid>,
    audio: Option<Mid>,
    /// Perdeu pedaço de frame: descarta até o próximo keyframe (evita imagem quebrada)
    waiting_keyframe: bool,
    last_keyframe_req: Instant,
    done: bool,
}

/// A quem se conectar
pub enum Target {
    /// Código "TL-…" (servidor embutido no PC de quem transmite)
    Share(ShareCode),
    /// Servidor de salas externo + código curto (testes)
    Signal { url: String, room: String },
}

pub async fn run(
    target: Target,
    name: String,
    events: EventSink,
    on_video: VideoSink,
    on_audio: AudioSink,
    mut cmds: mpsc::Receiver<ViewerCmd>,
) {
    let (mut tr, mut incoming) = match Transport::gather(None).await {
        Ok(t) => t,
        Err(e) => return events(json!({"type": "error", "message": format!("rede: {e}")})),
    };
    let (sig, code) = match target {
        Target::Share(sc) => {
            let urls = sc.addrs().iter().map(|a| format!("ws://{a}")).collect();
            (Signal::connect_any(urls).await, sc.room)
        }
        Target::Signal { url, room } => (Signal::connect(&url).await, room),
    };
    let sig = match sig {
        Ok(s) => s,
        Err(e) => return events(json!({"type": "error", "message": e})),
    };
    match tr.add_relay(sig.turn.as_ref()).await {
        Ok(a) => events(json!({"type": "diag", "message": format!("relay pronto ({a})")})),
        Err(e) => events(json!({"type": "diag", "message": format!("sem relay: {e}")})),
    }
    sig.send(json!({"t": "join", "code": code, "name": name}));

    let mut s = Session {
        tr,
        sig,
        events,
        on_video,
        on_audio,
        rtc: None,
        timeout: Instant::now() + Duration::from_secs(3600),
        video: None,
        audio: None,
        waiting_keyframe: true,
        last_keyframe_req: Instant::now() - Duration::from_secs(1),
        done: false,
    };

    while !s.done {
        tokio::select! {
            biased;
            cmd = cmds.recv() => match cmd {
                Some(ViewerCmd::RequestKeyframe) => s.request_keyframe(true),
                None => break,
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
                Some(m) => s.signal(m),
                None => break,
            },
            _ = tokio::time::sleep_until(s.timeout.into()) => {
                if let Some(rtc) = s.rtc.as_mut() {
                    let _ = rtc.handle_input(Input::Timeout(Instant::now()));
                    s.pump();
                } else {
                    s.timeout = Instant::now() + Duration::from_secs(3600);
                }
            },
        }
        if s.rtc.as_ref().is_some_and(|r| !r.is_alive()) {
            s.emit(json!({"type": "disconnected"}));
            break;
        }
    }
    if let Some(rtc) = s.rtc.as_mut() {
        rtc.disconnect();
    }
}

impl Session {
    fn emit(&self, v: Value) {
        (self.events)(v)
    }

    fn signal(&mut self, m: Value) {
        match m["t"].as_str().unwrap_or("") {
            "joined" => self.emit(json!({"type": "joined"})),
            "error" => {
                self.emit(json!({"type": "error", "message": m["msg"]}));
                self.done = true;
            }
            "host-left" => {
                self.emit(json!({"type": "host_left"}));
                self.done = true;
            }
            "signal" => match m["data"]["kind"].as_str().unwrap_or("") {
                "offer" => self.accept_offer(m["data"]["sdp"].as_str().unwrap_or("")),
                "info" => self.emit(json!({"type": "info", "info": m["data"]["info"]})),
                _ => {}
            },
            "closed" if self.rtc.is_none() => {
                self.emit(json!({"type": "error", "message": "caiu a conexão com o servidor de salas"}));
                self.done = true;
            }
            _ => {}
        }
    }

    fn accept_offer(&mut self, sdp: &str) {
        let Ok(offer) = SdpOffer::from_sdp_string(sdp) else { return };
        let mut rtc = rtc_config(None).build(Instant::now());
        for c in &self.tr.candidates {
            let _ = rtc.add_local_candidate(c.clone());
        }
        let Ok(answer) = rtc.sdp_api().accept_offer(offer) else {
            self.emit(json!({"type": "error", "message": "não deu pra combinar a conexão"}));
            return;
        };
        self.sig.send(json!({"t": "signal", "data": {"kind": "answer", "sdp": answer.to_sdp_string()}}));
        self.rtc = Some(rtc);
        self.pump();
    }

    fn request_keyframe(&mut self, force: bool) {
        if !force && self.last_keyframe_req.elapsed() < Duration::from_millis(200) {
            return;
        }
        let (Some(rtc), Some(mid)) = (self.rtc.as_mut(), self.video) else { return };
        self.last_keyframe_req = Instant::now();
        if let Some(mut w) = rtc.writer(mid) {
            let _ = w.request_keyframe(None, KeyframeRequestKind::Pli);
        }
        self.pump();
    }

    fn pump(&mut self) {
        let mut events = Vec::new();
        loop {
            let Some(rtc) = self.rtc.as_mut() else { return };
            self.timeout = drain(rtc, &self.tr, &mut events);
            if events.is_empty() {
                return;
            }
            let mut need_keyframe = false;
            for e in events.drain(..) {
                match e {
                    Event::Connected => self.emit(json!({"type": "connected"})),
                    Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                        if let Some(r) = self.rtc.as_mut() {
                            r.disconnect();
                        }
                    }
                    Event::MediaAdded(m) => match m.kind {
                        MediaKind::Video => self.video = Some(m.mid),
                        MediaKind::Audio => self.audio = Some(m.mid),
                    },
                    Event::MediaData(d) if Some(d.mid) == self.video => {
                        let key = matches!(d.codec_extra, CodecExtra::H265(e) if e.is_keyframe);
                        if key {
                            self.waiting_keyframe = false;
                        } else if !d.contiguous {
                            self.waiting_keyframe = true;
                        }
                        if self.waiting_keyframe {
                            need_keyframe = true;
                            continue;
                        }
                        (self.on_video)(&d.data, key, d.time.numer() as u32);
                    }
                    Event::MediaData(d) if Some(d.mid) == self.audio => {
                        (self.on_audio)(&d.data, d.time.numer() as u32);
                    }
                    Event::MediaIngressStats(st) if Some(st.mid) == self.video => {
                        self.emit(json!({
                            "type": "stats",
                            "rtt_ms": st.rtt.map(|d| d.as_secs_f64() * 1000.0),
                            "loss": st.loss,
                        }));
                    }
                    _ => {}
                }
            }
            if need_keyframe {
                self.request_keyframe(false);
            }
        }
    }
}
