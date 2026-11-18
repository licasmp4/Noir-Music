//! A Jam aberta: quem toca (sala + som + fila) ou quem ouve (sala + saída de som).
//!
//! Quem toca roda a sala no próprio computador (a rede do Noir: código pela nuvem, direto entre os
//! PCs e relay quando a internet não deixa) e entra nela como membro. O som vai na faixa de voz da
//! sala, que chega pra todo mundo sem precisar "assistir". Os pedidos vão como mensagens `jam`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jam_net::{CallMember, CallServer, Track as NetTrack};
use serde_json::{Value, json};

use crate::audio::{Decoder, Encoder, FRAME, Output};
use crate::capture::Capture;
use crate::player::{self, Cmd};
use crate::prefs::Prefs;
use crate::proto::{Now, State, Track};

pub type Repaint = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Opening,
    Joining,
    Live,
    Closed(String),
}

#[derive(Clone, Debug, Default)]
pub struct Results {
    pub rid: u64,
    pub q: String,
    pub items: Vec<Track>,
    pub loading: bool,
    pub error: Option<String>,
}

pub struct Shared {
    pub phase: Phase,
    pub code: Option<String>,
    pub state: State,
    pub state_at: Instant,
    /// Quem está na Jam (nomes)
    pub people: Vec<String>,
    pub results: Results,
    /// Aviso rápido embaixo ("Breno pôs Yoga na fila")
    pub toast: Option<(String, Instant)>,
    /// Quem toca: erro da captura do som (permissão)
    pub capture_error: Option<String>,
}

type MemberCell = Arc<Mutex<Option<Arc<CallMember>>>>;

pub struct Session {
    pub host: bool,
    pub shared: Arc<Mutex<Shared>>,
    member: MemberCell,
    server: Option<CallServer>,
    pub output: Option<Arc<Output>>,
    engine: Option<Sender<HostMsg>>,
    rid: AtomicU64,
    repaint: Repaint,
}

enum HostMsg {
    /// Pedido de alguém (nome, id na sala, dados). id vazio = eu mesmo
    Jam(String, String, Value),
    /// Entrou alguém: manda o estado de novo
    Roster,
    Art(String, String),
    Spotify(String, String),
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Session {
    fn new(host: bool, repaint: Repaint, phase: Phase) -> Session {
        Session {
            host,
            shared: Arc::new(Mutex::new(Shared {
                phase,
                code: None,
                state: State::default(),
                state_at: Instant::now(),
                people: Vec::new(),
                results: Results::default(),
                toast: None,
                capture_error: None,
            })),
            member: Arc::new(Mutex::new(None)),
            server: None,
            output: None,
            engine: None,
            rid: AtomicU64::new(1),
            repaint,
        }
    }

    /// Abre a minha Jam (só no Mac): a sala, o som do app de música e a fila
    pub fn open(prefs: &Prefs, repaint: Repaint) -> Session {
        let mut s = Session::new(true, repaint.clone(), Phase::Opening);
        let (tx, rx) = channel();
        s.engine = Some(tx.clone());
        let (shared, member, name) = (s.shared.clone(), s.member.clone(), prefs.name.clone());
        let creds = (prefs.spotify_id.trim().to_string(), prefs.spotify_secret.trim().to_string());
        {
            let (shared, member, repaint) = (shared.clone(), member.clone(), repaint.clone());
            let _ = std::thread::Builder::new().name("jam-host".into()).spawn(move || engine(rx, tx, shared, member, name, creds, repaint));
        }
        let url = Arc::new(Mutex::new(String::new()));
        let (sh, mem, rp, nm, eng) = (shared.clone(), member.clone(), repaint.clone(), prefs.name.clone(), s.engine.clone());
        s.server = Some(CallServer::start(
            String::new(),
            Arc::new(move |e: Value| {
                let kind = e["type"].as_str().unwrap_or("");
                if matches!(kind, "error" | "cloud_error" | "member_connected" | "member_left") {
                    crate::log::log(format!("sala: {kind} {}", e.get("message").or(e.get("name")).map(|m| m.to_string()).unwrap_or_default()));
                }
                match kind {
                    "signal_url" => *lock(&url) = e["url"].as_str().unwrap_or("").to_string(),
                    "room" => {
                        let code = e["code"].as_str().unwrap_or("").to_string();
                        let room = e["room"].as_str().unwrap_or("").to_string();
                        crate::log::log(format!("Jam aberta: {code}"));
                        {
                            let mut s = lock(&sh);
                            s.code = Some(code);
                            s.phase = Phase::Joining;
                        }
                        // quem criou entra como membro (fora da linha da rede: a conexão tem a dela)
                        let (sh, mem, rp, nm, eng, u) = (sh.clone(), mem.clone(), rp.clone(), nm.clone(), eng.clone(), lock(&url).clone());
                        std::thread::spawn(move || join(u, room, nm, sh, mem, rp, eng, None));
                    }
                    "error" => {
                        let msg = e["message"].as_str().unwrap_or("não deu pra abrir a sala").to_string();
                        let mut s = lock(&sh);
                        if s.code.is_none() {
                            s.phase = Phase::Closed(msg);
                        }
                    }
                    _ => {}
                }
                rp();
            }),
        ));
        s
    }

    /// Entra na Jam de um amigo pelo código
    pub fn join(prefs: &Prefs, code: String, repaint: Repaint) -> Session {
        let mut s = Session::new(false, repaint.clone(), Phase::Joining);
        lock(&s.shared).code = Some(code.clone());
        let out = Arc::new(Output::new());
        out.set_volume(prefs.volume);
        out.set_eq(prefs.eq_gains, prefs.eq_on);
        s.output = Some(out.clone());
        let (sh, mem, name) = (s.shared.clone(), s.member.clone(), prefs.name.clone());
        std::thread::spawn(move || join(String::new(), code, name, sh, mem, repaint, None, Some(out)));
        s
    }

    fn send(&self, data: Value) {
        if self.host {
            if let Some(tx) = &self.engine {
                let _ = tx.send(HostMsg::Jam(String::new(), String::new(), data));
            }
        } else if let Some(m) = lock(&self.member).as_ref() {
            m.send(json!({"kind": "jam", "data": data}));
        }
    }

    pub fn cmd(&self, c: &str) {
        self.send(json!({"t": "cmd", "c": c}));
        // mostra na hora; quem toca confirma logo depois
        if c == "toggle" {
            let mut s = lock(&self.shared);
            let at = s.state_at;
            if let Some(n) = s.state.now.as_mut() {
                if n.playing {
                    n.pos += at.elapsed().as_secs_f64();
                }
                n.playing = !n.playing;
            }
            s.state_at = Instant::now();
        }
        (self.repaint)();
    }

    pub fn seek(&self, secs: f64) {
        self.send(json!({"t": "cmd", "c": "seek", "v": secs}));
        let mut s = lock(&self.shared);
        if let Some(n) = s.state.now.as_mut() {
            n.pos = secs;
        }
        s.state_at = Instant::now();
    }

    pub fn add(&self, t: &Track) {
        self.send(json!({"t": "add", "track": t}));
    }

    pub fn play_now(&self, t: &Track) {
        self.send(json!({"t": "play", "track": t}));
    }

    pub fn remove(&self, i: usize) {
        self.send(json!({"t": "remove", "i": i}));
    }

    pub fn to_top(&self, i: usize) {
        self.send(json!({"t": "up", "i": i}));
    }

    pub fn search(&self, q: &str) {
        let rid = self.rid.fetch_add(1, Ordering::Relaxed);
        lock(&self.shared).results = Results { rid, q: q.to_string(), loading: true, ..Default::default() };
        self.send(json!({"t": "search", "q": q, "rid": rid}));
    }

    /// JAM_TESTE=1 (quem ouve): busca, põe na fila, pula, adianta, pausa, volta, tira da fila e
    /// anota no registro o que quem toca respondeu a cada passo
    pub fn run_test(&self) {
        let me = self;
        {
            let wait = |secs: u64| std::thread::sleep(Duration::from_secs(secs));
            let now = |s: &Session| {
                let sh = lock(&s.shared);
                let n = sh.state.now.clone();
                let q: Vec<String> = sh.state.queue.iter().map(|t| t.title.clone()).collect();
                drop(sh);
                let p = s.position();
                format!("tocando {:?} pos {:.1} {} · fila {:?}", n.map(|n| n.track.title), p.map_or(0.0, |p| p.0), if p.is_some_and(|p| p.2) { "▶" } else { "⏸" }, q)
            };
            for _ in 0..30 {
                if lock(&me.shared).phase == Phase::Live && lock(&me.shared).state.now.is_some() {
                    break;
                }
                wait(1);
            }
            crate::log::log(format!("TESTE início: {}", now(&me)));
            me.search(&std::env::var("JAM_TESTE_BUSCA").unwrap_or("love".into()));
            for _ in 0..20 {
                if !lock(&me.shared).results.loading {
                    break;
                }
                wait(1);
            }
            let items = lock(&me.shared).results.items.clone();
            crate::log::log(format!("TESTE busca: {} resultados, {:?}", items.len(), items.iter().take(3).map(|t| (&t.title, !t.art.is_empty())).collect::<Vec<_>>()));
            if items.len() < 3 {
                crate::log::log("TESTE parou: busca sem resultado");
                return;
            }
            me.add(&items[0]);
            me.add(&items[1]);
            me.add(&items[2]);
            wait(2);
            crate::log::log(format!("TESTE pôs 3 na fila: {}", now(&me)));
            me.to_top(2);
            wait(2);
            crate::log::log(format!("TESTE a 3ª subiu pro topo: {}", now(&me)));
            me.remove(1);
            wait(2);
            crate::log::log(format!("TESTE tirou a 2ª: {}", now(&me)));
            me.cmd("next");
            wait(3);
            crate::log::log(format!("TESTE próxima (deve tocar {}): {}", items[2].title, now(&me)));
            me.seek(60.0);
            wait(3);
            crate::log::log(format!("TESTE adiantou pra 60 s: {}", now(&me)));
            me.cmd("toggle");
            wait(3);
            crate::log::log(format!("TESTE pausou: {}", now(&me)));
            me.cmd("toggle");
            wait(3);
            crate::log::log(format!("TESTE voltou a tocar: {}", now(&me)));
            me.cmd("prev");
            wait(3);
            crate::log::log(format!("TESTE anterior: {}", now(&me)));
            me.play_now(&items[1]);
            wait(3);
            crate::log::log(format!("TESTE tocar agora {}: {}", items[1].title, now(&me)));
            // fim da música emenda na fila: adianta pra 4 s do fim
            let dur = lock(&me.shared).state.now.as_ref().map_or(0.0, |n| n.track.dur);
            me.seek((dur - 4.0).max(0.0));
            wait(7);
            crate::log::log(format!("TESTE no fim da música, a fila (deve tocar {}): {}", items[0].title, now(&me)));
            crate::log::log("TESTE fim");
        }
    }

    pub fn set_spotify(&self, id: &str, secret: &str) {
        if let Some(tx) = &self.engine {
            let _ = tx.send(HostMsg::Spotify(id.trim().into(), secret.trim().into()));
        }
    }

    /// Posição agora (anda sozinha enquanto toca). Quem ouve escuta com a folga do som de atraso.
    pub fn position(&self) -> Option<(f64, f64, bool)> {
        let s = lock(&self.shared);
        let n = s.state.now.as_ref()?;
        let mut p = n.pos;
        if n.playing {
            p += s.state_at.elapsed().as_secs_f64();
            if let Some(o) = &self.output {
                p -= o.stats().0 as f64 / 1000.0;
            }
        }
        Some((p.clamp(0.0, n.track.dur.max(0.0)), n.track.dur, n.playing))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // a conexão fecha numa thread (o tokio não deixa fechar de dentro dele, e a tela não espera)
        let m = lock(&self.member).take();
        let sv = self.server.take();
        std::thread::spawn(move || {
            drop(m);
            drop(sv);
        });
    }
}

/// Entra na sala como membro. `engine` = sou quem toca; `out` = sou quem ouve
#[allow(clippy::too_many_arguments)]
fn join(url: String, code: String, name: String, shared: Arc<Mutex<Shared>>, cell: MemberCell, repaint: Repaint, engine: Option<Sender<HostMsg>>, out: Option<Arc<Output>>) {
    let host = engine.is_some();
    let (sh, rp, out1) = (shared.clone(), repaint.clone(), out.clone());
    let events = Arc::new(move |e: Value| {
        let kind = e["type"].as_str().unwrap_or("");
        match kind {
            "connected" => lock(&sh).phase = Phase::Live,
            "roster" => {
                let names: Vec<String> = e["members"].as_array().into_iter().flatten().filter_map(|m| m["name"].as_str().map(String::from)).collect();
                lock(&sh).people = names;
                if let Some(tx) = &engine {
                    let _ = tx.send(HostMsg::Roster);
                }
            }
            "jam" => {
                let data = &e["data"];
                if let Some(tx) = &engine {
                    let _ = tx.send(HostMsg::Jam(e["name"].as_str().unwrap_or("Amigo").to_string(), e["from"].as_str().unwrap_or("").to_string(), data.clone()));
                } else {
                    on_listener_msg(&sh, data, out1.as_deref());
                }
            }
            "room_closed" => lock(&sh).phase = Phase::Closed("Quem estava tocando fechou a Jam.".into()),
            "error" | "disconnected" => {
                let msg = e["message"].as_str().unwrap_or("A conexão com a Jam caiu.").to_string();
                crate::log::log(format!("Jam: {kind} {msg}"));
                let mut s = lock(&sh);
                if !host || s.phase != Phase::Live {
                    s.phase = Phase::Closed(friendly(&msg));
                }
            }
            _ => {}
        }
        rp();
    });
    let decoder = Mutex::new(Decoder::new().ok());
    let out2 = out.clone();
    // registro de quem ouve a cada 5 s: o som está chegando inteiro?
    if let Some(o) = out.clone() {
        let sh = shared.clone();
        std::thread::spawn(move || {
            let mut last = 0u64;
            loop {
                std::thread::sleep(Duration::from_secs(5));
                if matches!(lock(&sh).phase, Phase::Closed(_)) {
                    break;
                }
                let (n, peak, q) = o.take_diag();
                let (target, unders) = o.stats();
                crate::log::log(format!("ouvindo: {:.1} s de som em 5 s, pico {peak:.2}, fila {q} ms, folga {target} ms, cortes {unders}", (n - last) as f64 / 48_000.0));
                last = n;
            }
        });
    }
    let on_media = Arc::new(move |_slot: usize, track: NetTrack, data: &[u8], _key: bool, ts: u32| {
        if track != NetTrack::Voice {
            return;
        }
        let mut dec = lock(&decoder);
        let (Some(o), Some(d)) = (out2.as_ref(), dec.as_mut()) else { return };
        let pcm = d.decode(data, ts);
        if (d.recovered + d.concealed) > 0 && (d.recovered + d.concealed) % 50 == 0 && pcm.len() > FRAME * 2 {
            crate::log::log(format!("perdas: {} salvas pela cópia, {} inventadas", d.recovered, d.concealed));
        }
        o.push(&pcm);
    });
    let m = Arc::new(CallMember::start(url, code, name, events, on_media));
    m.send(json!({"kind": "state", "state": {"mic": false, "deaf": false, "jam": if host { "host" } else { "ouvindo" }}}));
    *lock(&cell) = Some(m);
    repaint();
}

fn friendly(msg: &str) -> String {
    let low = msg.to_lowercase();
    if low.contains("not found") || low.contains("não achei") || low.contains("404") {
        "Não achei essa Jam. Confira o código ou peça um novo.".into()
    } else if low.contains("cheia") {
        "A Jam está cheia.".into()
    } else {
        msg.to_string()
    }
}

fn on_listener_msg(sh: &Mutex<Shared>, data: &Value, out: Option<&Output>) {
    match data["t"].as_str().unwrap_or("") {
        "state" => {
            if let Ok(st) = serde_json::from_value::<State>(data["state"].clone()) {
                if let Some(o) = out {
                    o.set_playing(st.now.as_ref().is_some_and(|n| n.playing));
                }
                let mut s = lock(sh);
                s.state = st;
                s.state_at = Instant::now();
            }
        }
        "results" => {
            let mut s = lock(sh);
            if data["rid"].as_u64() == Some(s.results.rid) {
                s.results.loading = false;
                s.results.items = serde_json::from_value(data["items"].clone()).unwrap_or_default();
                s.results.error = data["error"].as_str().map(String::from);
            }
        }
        "toast" => {
            if let Some(t) = data["text"].as_str() {
                lock(sh).toast = Some((t.to_string(), Instant::now()));
            }
        }
        _ => {}
    }
}

// MARK: - Quem toca

fn bundle(source: &str) -> &'static str {
    match source {
        "spotify" => "com.spotify.client",
        _ => "com.apple.Music",
    }
}

fn source_of(t: &Track) -> &'static str {
    if t.id.starts_with("spotify:") { "spotify" } else { "music" }
}

struct Engine {
    shared: Arc<Mutex<Shared>>,
    member: MemberCell,
    tx: Sender<HostMsg>,
    name: String,
    creds: (String, String),
    repaint: Repaint,
    source: String,
    now: Option<Now>,
    read_at: Instant,
    next_read: Instant,
    queue: Vec<Track>,
    /// Já pulei pra fila no fim desta música (id)
    advanced: Option<String>,
    capture: Option<Capture>,
    capture_for: String,
    capture_retry: Instant,
    arts: HashMap<String, String>,
    sent: Option<State>,
    sent_at: Instant,
    /// Ganho do som (x1000): o volume do Spotify a 50 % faria os amigos ouvirem baixo
    gain: Arc<std::sync::atomic::AtomicU32>,
}

fn engine(rx: Receiver<HostMsg>, tx: Sender<HostMsg>, shared: Arc<Mutex<Shared>>, member: MemberCell, name: String, creds: (String, String), repaint: Repaint) {
    let now = Instant::now();
    let mut e = Engine {
        shared,
        member,
        tx,
        name,
        creds,
        repaint,
        source: String::new(),
        now: None,
        read_at: now,
        next_read: now,
        queue: Vec::new(),
        advanced: None,
        capture: None,
        capture_for: String::new(),
        capture_retry: now,
        arts: HashMap::new(),
        sent: None,
        sent_at: now,
        gain: Arc::new(std::sync::atomic::AtomicU32::new(1000)),
    };
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(m) => e.handle(m),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if Instant::now() >= e.next_read {
            e.read();
        }
        e.advance();
        e.ensure_capture();
        e.publish(false);
    }
    crate::log::log("Jam fechada");
}

impl Engine {
    fn pos_now(&self) -> f64 {
        match &self.now {
            Some(n) if n.playing => n.pos + self.read_at.elapsed().as_secs_f64(),
            Some(n) => n.pos,
            None => 0.0,
        }
    }

    fn read(&mut self) {
        self.next_read = Instant::now() + Duration::from_millis(1000);
        let mut list = player::read();
        // teste: JAM_SOURCE=music só olha pro Apple Music (não mexe no Spotify de quem está ouvindo)
        if let Ok(only) = std::env::var("JAM_SOURCE") {
            list.retain(|r| r.source == only);
        }
        let prev = self.now.clone();
        let prev_left = prev.as_ref().map(|n| n.track.dur - self.pos_now());
        let pick = list.iter().find(|r| r.playing).or_else(|| list.iter().find(|r| r.source == self.source)).or(list.first()).cloned();
        match pick {
            Some(r) => {
                self.source = r.source.clone();
                // compensa o volume do app (até 4x; a 0 % não dá pra recuperar nada)
                let g = if r.volume >= 1.0 { (100.0 / r.volume).min(4.0) } else { 1.0 };
                self.gain.store((g * 1000.0) as u32, Ordering::Relaxed);
                let mut track = r.track;
                // a capa grande do Spotify (2000 px; a que o app dá tem 640 e fica borrada no Retina)
                track.art = track.art.replace("ab67616d0000b273", "ab67616d000082c1");
                if track.art.is_empty() {
                    track.art = self.art_for(&track);
                }
                // quem pôs na fila continua aparecendo enquanto a música dela toca
                if let Some(p) = prev.as_ref().filter(|p| p.track.id == track.id) {
                    track.by = p.track.by.clone();
                }
                self.now = Some(Now { track, pos: r.pos, playing: r.playing });
            }
            None => {
                self.now = None;
                let only = std::env::var("JAM_SOURCE").ok();
                let run: Vec<&str> = player::running().into_iter().filter(|s| only.as_deref().is_none_or(|o| o == *s)).collect();
                if let Some(s) = run.iter().find(|s| **s == self.source).or(run.first()).copied() {
                    self.source = s.to_string();
                } else {
                    self.source.clear();
                }
            }
        }
        self.read_at = Instant::now();
        // o app pulou sozinho pra outra música no fim da anterior (não deu tempo de eu pular): a fila vem antes
        if let (Some(p), Some(n)) = (&prev, &self.now) {
            if p.track.id != n.track.id && prev_left.is_some_and(|l| l < 3.0) && self.advanced.as_deref() != Some(&p.track.id) && !self.queue.is_empty() {
                self.advanced = Some(p.track.id.clone());
                self.play_next();
            }
        }
    }

    /// Capa das músicas do Apple Music (o AppleScript não dá link): busca do iTunes numa thread
    fn art_for(&mut self, t: &Track) -> String {
        if let Some(a) = self.arts.get(&t.id) {
            return a.clone();
        }
        self.arts.insert(t.id.clone(), String::new());
        let (tx, id, title, artist) = (self.tx.clone(), t.id.clone(), t.title.clone(), t.artist.clone());
        std::thread::spawn(move || {
            if let Some(u) = crate::search::itunes_art(&title, &artist) {
                let _ = tx.send(HostMsg::Art(id, u));
            }
        });
        String::new()
    }

    /// Faltando 0,4 s pro fim, já toca a próxima da fila (sem o app emendar outra coisa no meio)
    fn advance(&mut self) {
        let Some(n) = &self.now else { return };
        if !n.playing || n.track.dur < 2.0 || self.queue.is_empty() || self.advanced.as_deref() == Some(&n.track.id) {
            return;
        }
        if n.track.dur - self.pos_now() <= 0.4 {
            self.advanced = Some(n.track.id.clone());
            self.play_next();
        }
    }

    fn play_next(&mut self) {
        while !self.queue.is_empty() {
            let t = self.queue.remove(0);
            if source_of(&t) != self.source && !self.source.is_empty() {
                continue; // é do outro app (trocou de Spotify pra Apple Music no meio)
            }
            self.play(t);
            return;
        }
    }

    fn play(&mut self, t: Track) {
        let src = source_of(&t);
        if player::send(src, &Cmd::Track(t.id.clone())).is_ok() {
            self.source = src.to_string();
            self.now = Some(Now { track: t, pos: 0.0, playing: true });
            self.read_at = Instant::now();
            self.next_read = Instant::now() + Duration::from_millis(900);
            self.publish(true);
        }
    }

    fn ensure_capture(&mut self) {
        if self.source.is_empty() {
            return;
        }
        let want = bundle(&self.source);
        if self.capture_for == want || Instant::now() < self.capture_retry {
            return;
        }
        self.capture = None;
        let member = self.member.clone();
        let mut enc = match Encoder::new() {
            Ok(e) => e,
            Err(e) => {
                crate::log::log(format!("Opus: {e}"));
                return;
            }
        };
        let mut frames: u64 = 0;
        let gain = self.gain.clone();
        let mut buf: Vec<f32> = Vec::new();
        let mut cur = 1.0f32;
        let sink = Box::new(move |pcm: &[f32]| {
            let m = lock(&member).clone();
            // ganho que muda devagar (sem degrau quando mexem no volume) e nunca passa de 1,0
            let want = gain.load(Ordering::Relaxed) as f32 / 1000.0;
            buf.clear();
            for v in pcm {
                cur += (want - cur) * 0.0005;
                buf.push((v * cur).clamp(-1.0, 1.0));
            }
            enc.push(&buf, |pkt| {
                frames += 1;
                if let Some(m) = &m {
                    m.send_media(NetTrack::Voice, pkt, false, frames * 20_000);
                }
            });
        });
        match Capture::start(want, sink) {
            Ok(c) => {
                self.capture = Some(c);
                self.capture_for = want.to_string();
                lock(&self.shared).capture_error = None;
            }
            Err(err) => {
                crate::log::log(format!("captura: {err}"));
                lock(&self.shared).capture_error = Some(err);
                self.capture_retry = Instant::now() + Duration::from_secs(3);
                (self.repaint)();
            }
        }
    }

    fn state(&self) -> State {
        let can = match self.source.as_str() {
            "spotify" => !self.creds.0.is_empty() && !self.creds.1.is_empty(),
            "music" => true,
            _ => false,
        };
        State {
            now: self.now.clone().map(|mut n| {
                n.pos = self.pos_now().min(n.track.dur.max(0.0));
                n
            }),
            queue: self.queue.clone(),
            source: self.source.clone(),
            host: self.name.clone(),
            search: can,
            search_in: match self.source.as_str() {
                "spotify" => "Spotify".into(),
                "music" => format!("Biblioteca de {}", self.name),
                _ => String::new(),
            },
        }
    }

    /// Manda o estado pra todo mundo quando muda (e a cada 5 s, pra posição não escorregar)
    fn publish(&mut self, force: bool) {
        let st = self.state();
        let same = self.sent.as_ref().is_some_and(|s| {
            let mut a = s.clone();
            let mut b = st.clone();
            for x in [&mut a, &mut b] {
                if let Some(n) = x.now.as_mut() {
                    n.pos = 0.0;
                }
            }
            a == b
        });
        if same && !force && self.sent_at.elapsed() < Duration::from_secs(5) {
            return;
        }
        if let Some(m) = lock(&self.member).as_ref() {
            m.send(json!({"kind": "jam", "data": {"t": "state", "state": st}}));
        }
        {
            let mut s = lock(&self.shared);
            s.state = st.clone();
            s.state_at = Instant::now();
        }
        self.sent = Some(st);
        self.sent_at = Instant::now();
        (self.repaint)();
    }

    fn toast(&self, text: String) {
        if let Some(m) = lock(&self.member).as_ref() {
            m.send(json!({"kind": "jam", "data": {"t": "toast", "text": text}}));
        }
        lock(&self.shared).toast = Some((text, Instant::now()));
        (self.repaint)();
    }

    fn reply(&self, to: &str, data: Value) {
        if to.is_empty() {
            // eu mesmo
            let mut s = lock(&self.shared);
            if data["rid"].as_u64() == Some(s.results.rid) {
                s.results.loading = false;
                s.results.items = serde_json::from_value(data["items"].clone()).unwrap_or_default();
                s.results.error = data["error"].as_str().map(String::from);
            }
            drop(s);
            (self.repaint)();
        } else if let Some(m) = lock(&self.member).as_ref() {
            m.send(json!({"kind": "jam", "to": to, "data": data}));
        }
    }

    fn handle(&mut self, msg: HostMsg) {
        match msg {
            HostMsg::Roster => self.publish(true),
            HostMsg::Art(id, url) => {
                self.arts.insert(id.clone(), url.clone());
                if let Some(n) = self.now.as_mut().filter(|n| n.track.id == id) {
                    n.track.art = url;
                }
            }
            HostMsg::Spotify(id, secret) => {
                crate::search::reset_spotify();
                self.creds = (id, secret);
                self.publish(true);
            }
            HostMsg::Jam(who, from, d) => self.request(&who, &from, &d),
        }
    }

    fn request(&mut self, who: &str, from: &str, d: &Value) {
        let me = from.is_empty();
        let who = if me { self.name.clone() } else { who.to_string() };
        match d["t"].as_str().unwrap_or("") {
            "cmd" => {
                let src = self.source.clone();
                match d["c"].as_str().unwrap_or("") {
                    "toggle" => {
                        let _ = player::send(&src, &Cmd::Toggle);
                    }
                    "next" if !self.queue.is_empty() => self.play_next(),
                    "next" => {
                        let _ = player::send(&src, &Cmd::Next);
                    }
                    "prev" => {
                        let _ = player::send(&src, &Cmd::Prev);
                    }
                    "seek" => {
                        let v = d["v"].as_f64().unwrap_or(0.0);
                        if player::send(&src, &Cmd::Seek(v)).is_ok() {
                            if let Some(n) = self.now.as_mut() {
                                n.pos = v;
                            }
                            self.read_at = Instant::now();
                        }
                    }
                    _ => {}
                }
                // o app demora um tiquinho pra mudar: lê de novo logo
                self.next_read = Instant::now() + Duration::from_millis(250);
            }
            "add" | "play" => {
                let Ok(mut t) = serde_json::from_value::<Track>(d["track"].clone()) else { return };
                if t.id.is_empty() || t.title.is_empty() {
                    return;
                }
                t.by = who.chars().take(32).collect();
                let idle = self.now.as_ref().is_none_or(|n| !n.playing);
                if d["t"] == "play" || (idle && self.queue.is_empty()) {
                    // tocar agora tira a mesma música da fila (senão ela tocava duas vezes)
                    let id = t.id.clone();
                    let before = self.queue.len();
                    self.queue.retain(|q| q.id != id);
                    if self.queue.len() != before {
                        self.publish(true);
                    }
                    if !me {
                        self.toast(format!("{} tocou {}", t.by, t.title));
                    }
                    self.play(t);
                } else if self.queue.len() < 200 {
                    self.toast(format!("{} pôs {} na fila", t.by, t.title));
                    self.queue.push(t);
                    self.publish(true);
                }
            }
            "remove" => {
                if let Some(i) = d["i"].as_u64().map(|i| i as usize).filter(|i| *i < self.queue.len()) {
                    self.queue.remove(i);
                    self.publish(true);
                }
            }
            "up" => {
                if let Some(i) = d["i"].as_u64().map(|i| i as usize).filter(|i| *i < self.queue.len()) {
                    let t = self.queue.remove(i);
                    self.queue.insert(0, t);
                    self.publish(true);
                }
            }
            "search" => {
                let q: String = d["q"].as_str().unwrap_or("").chars().take(120).collect();
                let rid = d["rid"].as_u64().unwrap_or(0);
                let (src, creds, me_name, to) = (self.source.clone(), self.creds.clone(), self.name.clone(), from.to_string());
                let member = self.member.clone();
                let shared = self.shared.clone();
                let repaint = self.repaint.clone();
                std::thread::spawn(move || {
                    let (items, error) = match src.as_str() {
                        "spotify" if creds.0.is_empty() => (Vec::new(), Some(format!("{me_name} ainda não ligou a busca do Spotify no Jam."))),
                        "spotify" => match crate::search::spotify(&q, &creds.0, &creds.1) {
                            Ok(v) => (v, None),
                            Err(e) => (Vec::new(), Some(e)),
                        },
                        "music" => {
                            let mut v = player::search_music(&q);
                            v.truncate(20);
                            // capas em paralelo (a busca do iTunes leva ~0,3 s cada)
                            let hs: Vec<_> = v.iter().map(|t| {
                                let (a, b) = (t.title.clone(), t.artist.clone());
                                std::thread::spawn(move || crate::search::itunes_art(&a, &b).unwrap_or_default())
                            }).collect();
                            for (t, h) in v.iter_mut().zip(hs) {
                                t.art = h.join().unwrap_or_default();
                            }
                            (v, None)
                        }
                        _ => (Vec::new(), Some(format!("Nenhum app de música aberto no computador de {me_name}."))),
                    };
                    let data = json!({"t": "results", "rid": rid, "items": items, "error": error});
                    if to.is_empty() {
                        let mut s = lock(&shared);
                        if s.results.rid == rid {
                            s.results.loading = false;
                            s.results.items = items;
                            s.results.error = error;
                        }
                        drop(s);
                        repaint();
                    } else if let Some(m) = lock(&member).as_ref() {
                        m.send(json!({"kind": "jam", "to": to, "data": data}));
                    }
                });
            }
            _ => {}
        }
    }
}
