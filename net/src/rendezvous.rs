//! Servidor de salas (WebSocket). Roda dentro do app de quem transmite, ou sozinho (`telao-signal`).
//!
//! Só troca as mensagens de conexão (ofertas/respostas WebRTC) entre quem transmite
//! e quem assiste. Vídeo e áudio nunca passam por aqui: vão direto entre os PCs.

use std::collections::HashMap;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

type Tx = mpsc::UnboundedSender<String>;

#[derive(Default)]
struct State {
    rooms: HashMap<String, Room>,
    next_id: u64,
}

struct Room {
    host: Tx,
    viewers: HashMap<String, Tx>,
}

#[derive(Clone)]
enum Role {
    None,
    Host(String),
    Viewer { code: String, id: String },
}

/// Sem 0/O, 1/I/L: dá pra ditar o código por voz sem confusão
pub(crate) const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";

/// Porta exposta na internet: segura tamanho de mensagem e quantidade de gente
/// 12 MB: perfil com foto e banner em GIF vai numa mensagem só
const MAX_MESSAGE: usize = 12 * 1024 * 1024;
const MAX_VIEWERS_PER_ROOM: usize = 32;

fn new_code(state: &State) -> String {
    loop {
        let code: String = (0..6)
            .map(|_| ALPHABET[rand::random_range(0..ALPHABET.len())] as char)
            .collect();
        if !state.rooms.contains_key(&code) {
            return code;
        }
    }
}

fn send(tx: &Tx, v: Value) {
    let _ = tx.send(v.to_string());
}

pub async fn serve(listener: TcpListener) {
    let state = Arc::new(Mutex::new(State::default()));
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        let _ = stream.set_nodelay(true);
        tokio::spawn(handle(stream, state.clone()));
    }
}

async fn handle(stream: TcpStream, state: Arc<Mutex<State>>) {
    let config = WebSocketConfig::default().max_message_size(Some(MAX_MESSAGE)).max_frame_size(Some(MAX_MESSAGE));
    let Ok(ws) = tokio_tungstenite::accept_async_with_config(stream, Some(config)).await else { return };
    let (mut sink, mut source) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if sink.send(Message::text(m)).await.is_err() {
                break;
            }
        }
    });

    let mut role = Role::None;
    while let Some(Ok(msg)) = source.next().await {
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
        let mut st = state.lock().await;
        match (v["t"].as_str().unwrap_or(""), role.clone()) {
            ("host", Role::None) => {
                let code = new_code(&st);
                st.rooms.insert(code.clone(), Room { host: tx.clone(), viewers: HashMap::new() });
                send(&tx, json!({"t": "room", "code": code}));
                role = Role::Host(code);
            }
            ("join", Role::None) => {
                let code = v["code"].as_str().unwrap_or("").trim().to_uppercase();
                let name = v["name"].as_str().unwrap_or("Amigo").chars().take(32).collect::<String>();
                st.next_id += 1;
                let id = format!("p{}", st.next_id);
                match st.rooms.get_mut(&code) {
                    Some(room) if room.viewers.len() < MAX_VIEWERS_PER_ROOM => {
                        room.viewers.insert(id.clone(), tx.clone());
                        send(&room.host, json!({"t": "peer-joined", "peer": id, "name": name}));
                        send(&tx, json!({"t": "joined", "peer": id}));
                        role = Role::Viewer { code, id };
                    }
                    Some(_) => send(&tx, json!({"t": "error", "msg": "A sala está cheia"})),
                    None => send(&tx, json!({"t": "error", "msg": "Sala não encontrada"})),
                }
            }
            ("signal", Role::Host(code)) => {
                if let Some(to) = st.rooms.get(&code).and_then(|r| r.viewers.get(v["to"].as_str().unwrap_or(""))) {
                    send(to, json!({"t": "signal", "from": "host", "data": v["data"]}));
                }
            }
            ("signal", Role::Viewer { code, id }) => {
                if let Some(room) = st.rooms.get(&code) {
                    send(&room.host, json!({"t": "signal", "from": id, "data": v["data"]}));
                }
            }
            ("ping", _) => send(&tx, json!({"t": "pong"})),
            _ => {}
        }
    }

    let mut st = state.lock().await;
    match role {
        Role::Host(code) => {
            if let Some(room) = st.rooms.remove(&code) {
                for v in room.viewers.values() {
                    send(v, json!({"t": "host-left"}));
                }
            }
        }
        Role::Viewer { code, id } => {
            if let Some(room) = st.rooms.get_mut(&code) {
                room.viewers.remove(&id);
                send(&room.host, json!({"t": "peer-left", "peer": id}));
            }
        }
        Role::None => {}
    }
    writer.abort();
}
