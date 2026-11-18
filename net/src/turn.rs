//! Cliente TURN mínimo (RFC 8656) pro relay da Cloudflare.
//!
//! Quando a conexão direta não abre (CGNAT, NAT simétrico, firewall), os dois lados conversam
//! através de um endereço emprestado no servidor TURN. Só UDP, um endereço por app, e um canal
//! (ChannelData, 4 bytes de cabeçalho) por amigo. O ICE do str0m só usa esse caminho quando
//! nenhum direto responde, porque candidato de relay tem a menor prioridade.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use md5::Md5;
use serde_json::Value;
use sha1::{Digest, Sha1};
use tokio::net::UdpSocket;
use tokio::sync::oneshot;

use crate::stun;

const MAGIC: u32 = 0x2112_A442;

const ALLOCATE: u16 = 0x0003;
const REFRESH: u16 = 0x0004;
const CHANNEL_BIND: u16 = 0x0009;
const DATA_INDICATION: u16 = 0x0017;

const A_USERNAME: u16 = 0x0006;
const A_INTEGRITY: u16 = 0x0008;
const A_ERROR: u16 = 0x0009;
const A_CHANNEL: u16 = 0x000C;
const A_LIFETIME: u16 = 0x000D;
const A_PEER: u16 = 0x0012;
const A_DATA: u16 = 0x0013;
const A_REALM: u16 = 0x0014;
const A_NONCE: u16 = 0x0015;
const A_RELAYED: u16 = 0x0016;
const A_TRANSPORT: u16 = 0x0019;

/// Renova o endereço e os canais antes dos 5 min (canal/permissão) e 10 min (endereço) do servidor
const REFRESH_EVERY: Duration = Duration::from_secs(240);

/// Credenciais temporárias que o servidor de salas entrega junto com a conexão
#[derive(Clone, Debug)]
pub struct Creds {
    /// `host:porta` dos servidores TURN por UDP
    pub servers: Vec<String>,
    pub username: String,
    pub password: String,
}

impl Creds {
    /// Formato da Cloudflare: `{"iceServers": [{"urls": [...], "username", "credential"}]}`
    /// (ou `iceServers` como objeto único, no formato antigo)
    pub fn from_json(v: &Value) -> Option<Self> {
        let list = match &v["iceServers"] {
            Value::Array(a) => a.clone(),
            o @ Value::Object(_) => vec![o.clone()],
            _ => return None,
        };
        for s in list {
            let (Some(user), Some(pass)) = (s["username"].as_str(), s["credential"].as_str()) else { continue };
            let urls: Vec<&str> = match &s["urls"] {
                Value::Array(a) => a.iter().filter_map(|u| u.as_str()).collect(),
                Value::String(u) => vec![u.as_str()],
                _ => continue,
            };
            let servers: Vec<String> = urls
                .iter()
                .filter_map(|u| u.strip_prefix("turn:"))
                .filter(|u| !u.contains("transport=") || u.contains("transport=udp"))
                .map(|u| {
                    let hp = u.split('?').next().unwrap_or(u);
                    if hp.contains(':') { hp.to_string() } else { format!("{hp}:3478") }
                })
                .collect();
            if !servers.is_empty() {
                return Some(Creds { servers, username: user.into(), password: pass.into() });
            }
        }
        None
    }
}

#[derive(Clone, Default)]
struct Auth {
    realm: String,
    nonce: String,
    key: [u8; 16],
}

#[derive(Default)]
struct Channels {
    /// amigo → (número do canal, já confirmado pelo servidor)
    by_peer: HashMap<SocketAddr, (u16, bool)>,
    by_num: HashMap<u16, SocketAddr>,
    next: u16,
}

pub struct Relay {
    sock: Arc<UdpSocket>,
    /// O servidor em uso (troca se o primeiro endereço não responder: ver `transport::add_relay`)
    pub server: Mutex<SocketAddr>,
    pub relayed: OnceLock<SocketAddr>,
    creds: Creds,
    auth: Mutex<Option<Auth>>,
    pending: Mutex<HashMap<[u8; 12], oneshot::Sender<Vec<u8>>>>,
    channels: Mutex<Channels>,
}

impl Relay {
    pub fn server(&self) -> SocketAddr {
        *self.server.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_server(&self, a: SocketAddr) {
        *self.server.lock().unwrap_or_else(|e| e.into_inner()) = a;
    }

    pub fn new(sock: Arc<UdpSocket>, server: SocketAddr, creds: Creds) -> Self {
        Relay {
            sock,
            server: Mutex::new(server),
            relayed: OnceLock::new(),
            creds,
            auth: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            channels: Mutex::new(Channels { next: 0x4000, ..Default::default() }),
        }
    }

    /// Pede o endereço emprestado. Devolve o endereço público do relay.
    pub async fn allocate(&self) -> Option<SocketAddr> {
        let resp = self.call(ALLOCATE, &[(A_TRANSPORT, vec![17, 0, 0, 0])]).await?;
        let txid: [u8; 12] = resp[8..20].try_into().ok()?;
        let addr = attrs(&resp).find(|(t, _)| *t == A_RELAYED).and_then(|(_, v)| stun::parse_addr(v, true, &txid))?;
        let _ = self.relayed.set(addr);
        Some(addr)
    }

    /// Mantém o endereço, os canais e a porta do roteador vivos enquanto o app estiver aberto
    pub async fn keepalive(self: Arc<Self>) {
        let mut refresh = tokio::time::interval_at(tokio::time::Instant::now() + REFRESH_EVERY, REFRESH_EVERY);
        // O roteador esquece a porta em poucos minutos sem tráfego
        let mut nat = tokio::time::interval(Duration::from_secs(20));
        loop {
            tokio::select! {
                _ = refresh.tick() => {
                    if self.call(REFRESH, &[(A_LIFETIME, 600u32.to_be_bytes().to_vec())]).await.is_none() {
                        return; // credencial venceu ou servidor sumiu: o relay para aqui
                    }
                    let bound: Vec<(SocketAddr, u16)> =
                        self.channels.lock().unwrap_or_else(|e| e.into_inner()).by_peer.iter().filter(|(_, c)| c.1).map(|(p, c)| (*p, c.0)).collect();
                    for (peer, num) in bound {
                        let me = self.clone();
                        tokio::spawn(async move { me.bind(peer, num).await });
                    }
                }
                _ = nat.tick() => {
                    let _ = self.sock.try_send_to(&stun::binding_request(&rand::random()), self.server());
                }
            }
        }
    }

    /// Manda um pacote do ICE/vídeo pro amigo através do relay
    pub fn send(self: &Arc<Self>, peer: SocketAddr, data: &[u8]) {
        let mut ch = self.channels.lock().unwrap_or_else(|e| e.into_inner());
        match ch.by_peer.get(&peer).copied() {
            Some((num, true)) => {
                drop(ch);
                let mut b = Vec::with_capacity(4 + data.len());
                b.extend_from_slice(&num.to_be_bytes());
                b.extend_from_slice(&(data.len() as u16).to_be_bytes());
                b.extend_from_slice(data);
                let _ = self.sock.try_send_to(&b, self.server());
            }
            // Canal ainda sendo aberto: o ICE reenvia o teste daqui a pouco
            Some((_, false)) => {}
            None => {
                if ch.next > 0x4FFF {
                    return;
                }
                let num = ch.next;
                ch.next += 1;
                ch.by_peer.insert(peer, (num, false));
                ch.by_num.insert(num, peer);
                drop(ch);
                let me = self.clone();
                tokio::spawn(async move { me.bind(peer, num).await });
            }
        }
    }

    async fn bind(&self, peer: SocketAddr, num: u16) {
        let mut c = num.to_be_bytes().to_vec();
        c.extend_from_slice(&[0, 0]);
        let ok = self.call(CHANNEL_BIND, &[(A_CHANNEL, c), (A_PEER, xor_addr(peer))]).await.is_some();
        let mut ch = self.channels.lock().unwrap_or_else(|e| e.into_inner());
        if ok {
            ch.by_peer.insert(peer, (num, true));
        } else if ch.by_peer.get(&peer).is_some_and(|c| !c.1) {
            // Não abriu: esquece, a próxima tentativa do ICE pede de novo
            ch.by_peer.remove(&peer);
            ch.by_num.remove(&num);
        }
    }

    /// Pacote que chegou do servidor TURN. Devolve (amigo, dados) se for dado de um amigo;
    /// respostas do servidor vão pra quem está esperando.
    pub fn handle(&self, buf: &[u8]) -> Option<(SocketAddr, Vec<u8>)> {
        if buf.len() < 4 {
            return None;
        }
        if buf[0] & 0xC0 == 0x40 {
            let num = u16::from_be_bytes([buf[0], buf[1]]);
            let len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
            let peer = *self.channels.lock().unwrap_or_else(|e| e.into_inner()).by_num.get(&num)?;
            return Some((peer, buf.get(4..4 + len)?.to_vec()));
        }
        if buf.len() < 20 || u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) != MAGIC {
            return None;
        }
        let typ = u16::from_be_bytes([buf[0], buf[1]]);
        let txid: [u8; 12] = buf[8..20].try_into().ok()?;
        if typ == DATA_INDICATION {
            let mut peer = None;
            let mut data = None;
            for (t, v) in attrs(buf) {
                match t {
                    A_PEER => peer = stun::parse_addr(v, true, &txid),
                    A_DATA => data = Some(v.to_vec()),
                    _ => {}
                }
            }
            return Some((peer?, data?));
        }
        if typ & 0x0100 != 0 {
            if let Some(tx) = self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&txid) {
                let _ = tx.send(buf.to_vec());
            }
        }
        None
    }

    /// Pedido com autenticação: se o servidor pedir senha (401) ou trocar o nonce (438), refaz.
    async fn call(&self, typ: u16, extra: &[(u16, Vec<u8>)]) -> Option<Vec<u8>> {
        for _ in 0..3 {
            let txid: [u8; 12] = rand::random();
            let mut m = Msg::new(typ, &txid);
            for (t, v) in extra {
                m.attr(*t, v);
            }
            let auth = self.auth.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(a) = &auth {
                m.attr(A_USERNAME, self.creds.username.as_bytes());
                m.attr(A_REALM, a.realm.as_bytes());
                m.attr(A_NONCE, a.nonce.as_bytes());
                m.integrity(&a.key);
            }
            let t0 = Instant::now();
            let resp = self.transact(txid, &m.buf).await;
            if std::env::var_os("TELAO_RELAY_VER").is_some() {
                eprintln!("RELAY {typ:#x} -> {} em {} ms ({})", resp.as_ref().map(|r| format!("{:#x}", u16::from_be_bytes([r[0], r[1]]))).unwrap_or("nada".into()), t0.elapsed().as_millis(), self.server());
            }
            let resp = resp?;
            let rtype = u16::from_be_bytes([resp[0], resp[1]]);
            if rtype & 0x0110 == 0x0100 {
                return Some(resp);
            }
            let code = attrs(&resp).find(|(t, _)| *t == A_ERROR).and_then(|(_, v)| Some((*v.get(2)? as u16 & 7) * 100 + *v.get(3)? as u16));
            if !matches!(code, Some(401 | 438)) {
                return None;
            }
            let mut a = auth.unwrap_or_default();
            for (t, v) in attrs(&resp) {
                match t {
                    A_REALM => a.realm = String::from_utf8_lossy(v).into_owned(),
                    A_NONCE => a.nonce = String::from_utf8_lossy(v).into_owned(),
                    _ => {}
                }
            }
            a.key = Md5::digest(format!("{}:{}:{}", self.creds.username, a.realm, self.creds.password).as_bytes()).into();
            *self.auth.lock().unwrap_or_else(|e| e.into_inner()) = Some(a);
        }
        None
    }

    /// Manda e espera a resposta com o mesmo id, reenviando se o UDP perder
    async fn transact(&self, txid: [u8; 12], msg: &[u8]) -> Option<Vec<u8>> {
        let (tx, mut rx) = oneshot::channel();
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(txid, tx);
        let mut wait = Duration::from_millis(250);
        for _ in 0..4 {
            let _ = self.sock.send_to(msg, self.server()).await;
            match tokio::time::timeout(wait, &mut rx).await {
                Ok(Ok(r)) => return Some(r),
                Ok(Err(_)) => break,
                Err(_) => wait *= 2,
            }
        }
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&txid);
        None
    }
}

struct Msg {
    buf: Vec<u8>,
}

impl Msg {
    fn new(typ: u16, txid: &[u8; 12]) -> Self {
        let mut buf = Vec::with_capacity(128);
        buf.extend_from_slice(&typ.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&MAGIC.to_be_bytes());
        buf.extend_from_slice(txid);
        Msg { buf }
    }

    fn set_len(&mut self, len: usize) {
        self.buf[2..4].copy_from_slice(&(len as u16).to_be_bytes());
    }

    fn attr(&mut self, t: u16, v: &[u8]) {
        self.buf.extend_from_slice(&t.to_be_bytes());
        self.buf.extend_from_slice(&(v.len() as u16).to_be_bytes());
        self.buf.extend_from_slice(v);
        while self.buf.len() % 4 != 0 {
            self.buf.push(0);
        }
        self.set_len(self.buf.len() - 20);
    }

    /// MESSAGE-INTEGRITY: HMAC-SHA1 da mensagem com o tamanho já contando este atributo
    fn integrity(&mut self, key: &[u8]) {
        self.set_len(self.buf.len() - 20 + 24);
        let mac = hmac_sha1(key, &self.buf);
        self.buf.extend_from_slice(&A_INTEGRITY.to_be_bytes());
        self.buf.extend_from_slice(&20u16.to_be_bytes());
        self.buf.extend_from_slice(&mac);
    }
}

fn hmac_sha1(key: &[u8], msg: &[u8]) -> [u8; 20] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..20].copy_from_slice(&Sha1::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha1::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(msg);
    let mut outer = Sha1::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// Atributos (tipo, valor) de uma mensagem STUN
fn attrs(buf: &[u8]) -> impl Iterator<Item = (u16, &[u8])> {
    let len = if buf.len() >= 20 { u16::from_be_bytes([buf[2], buf[3]]) as usize } else { 0 };
    let body = buf.get(20..20 + len).unwrap_or(&[]);
    let mut i = 0;
    std::iter::from_fn(move || {
        if i + 4 > body.len() {
            return None;
        }
        let t = u16::from_be_bytes([body[i], body[i + 1]]);
        let l = u16::from_be_bytes([body[i + 2], body[i + 3]]) as usize;
        let v = body.get(i + 4..i + 4 + l)?;
        i += 4 + ((l + 3) & !3);
        Some((t, v))
    })
}

/// XOR-PEER-ADDRESS (só IPv4: o relay é IPv4)
fn xor_addr(a: SocketAddr) -> Vec<u8> {
    let mut v = vec![0, 0x01];
    v.extend_from_slice(&(a.port() ^ (MAGIC >> 16) as u16).to_be_bytes());
    match a.ip() {
        std::net::IpAddr::V4(ip) => {
            for (b, m) in ip.octets().iter().zip(MAGIC.to_be_bytes()) {
                v.push(b ^ m);
            }
        }
        std::net::IpAddr::V6(_) => v.extend_from_slice(&[0; 4]),
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credenciais_da_cloudflare() {
        let v: Value = serde_json::from_str(
            r#"{"iceServers":[{"urls":["stun:stun.cloudflare.com:3478","turn:turn.cloudflare.com:3478?transport=udp","turn:turn.cloudflare.com:3478?transport=tcp","turns:turn.cloudflare.com:5349?transport=tcp"],"username":"u","credential":"p"}]}"#,
        )
        .unwrap();
        let c = Creds::from_json(&v).unwrap();
        assert_eq!(c.servers, vec!["turn.cloudflare.com:3478"]);
        assert_eq!((c.username.as_str(), c.password.as_str()), ("u", "p"));
    }

    #[test]
    fn hmac_rfc2202() {
        let mac = hmac_sha1(b"key", b"The quick brown fox jumps over the lazy dog");
        let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "de7c9b85b8b78aa6bc8a7a36f70a90701c9db4d9");
    }

    #[test]
    fn endereco_xor_ida_e_volta() {
        let a: SocketAddr = "203.0.113.7:50123".parse().unwrap();
        assert_eq!(stun::parse_addr(&xor_addr(a), true, &[0; 12]), Some(a));
    }
}
