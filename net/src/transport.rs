//! Sockets UDP + descoberta de caminhos (candidatos ICE).
//!
//! Abre um socket por endereço de rede (IPv4 e IPv6 público), pergunta pro STUN qual é o
//! IP público do roteador e entrega tudo pro str0m escolher o melhor caminho.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, OnceLock};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use str0m::Candidate;
use str0m::net::{Protocol, Receive, Transmit};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::stun;
use crate::turn::{Creds, Relay};

const STUN_SERVERS: &[&str] = &["stun.cloudflare.com:3478", "stun.l.google.com:19302"];

pub struct Packet {
    pub data: Vec<u8>,
    pub source: SocketAddr,
    pub destination: SocketAddr,
    pub at: Instant,
}

impl Packet {
    pub fn as_receive(&self) -> Option<Receive<'_>> {
        Some(Receive {
            proto: Protocol::Udp,
            source: self.source,
            destination: self.destination,
            contents: self.data.as_slice().try_into().ok()?,
        })
    }
}

pub struct Transport {
    sockets: Vec<Arc<UdpSocket>>,
    pub candidates: Vec<Candidate>,
    /// IPv4 da rede local que sai pra internet (o que o STUN respondeu)
    pub lan_v4: Option<Ipv4Addr>,
    /// IPv4 público (visto pelo STUN)
    pub public_v4: Option<Ipv4Addr>,
    /// IPv6 global (não é NAT: dá pra receber conexão direto se o firewall deixar)
    pub global_v6: Option<Ipv6Addr>,
    /// IP numa rede virtual (Radmin VPN, Hamachi, Tailscale, ZeroTier) e o nome dela
    pub vpn_v4: Option<(Ipv4Addr, &'static str)>,
    /// Teste: descarta essa % dos pacotes recebidos (TELAO_SIM_LOSS=5)
    sim_loss: u32,
    dropped: AtomicU32,
    /// Endereço emprestado no servidor TURN (último recurso quando o direto não abre)
    relay: Arc<OnceLock<Arc<Relay>>>,
    /// Canal dos pacotes que chegam (o socket do relay entra depois)
    tx: mpsc::Sender<Packet>,
}

/// Rede virtual tipo "LAN pela internet": os amigos chegam direto, sem abrir porta no roteador.
/// Pelo nome do adaptador (Windows: "Radmin VPN") ou pela faixa de IP que cada uma usa.
pub fn vpn_kind(name: &str, ip: Ipv4Addr) -> Option<&'static str> {
    let n = name.to_lowercase();
    let o = ip.octets();
    if n.contains("radmin") || o[0] == 26 {
        Some("Radmin VPN")
    } else if n.contains("hamachi") || o[0] == 25 {
        Some("Hamachi")
    } else if n.contains("tailscale") || (o[0] == 100 && (64..128).contains(&o[1])) {
        Some("Tailscale")
    } else if n.contains("zerotier") || n.starts_with("zt") || n.starts_with("feth") {
        Some("ZeroTier")
    } else {
        None
    }
}

fn usable(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => !v4.is_loopback() && !v4.is_link_local() && !v4.is_unspecified(),
        // Sem link-local (fe80::/10); global e ULA servem
        IpAddr::V6(v6) => !v6.is_loopback() && !v6.is_unspecified() && (v6.segments()[0] & 0xffc0) != 0xfe80,
    }
}

fn bind(ip: IpAddr, port: u16) -> io::Result<UdpSocket> {
    use socket2::{Domain, Socket, Type};
    let addr = SocketAddr::new(ip, port);
    let s = Socket::new(Domain::for_address(addr), Type::DGRAM, Some(socket2::Protocol::UDP))?;
    if ip.is_ipv6() {
        s.set_only_v6(true)?;
    }
    // Keyframe de 4K são centenas de pacotes de uma vez
    let _ = s.set_send_buffer_size(8 << 20);
    let _ = s.set_recv_buffer_size(8 << 20);
    s.bind(&addr.into())?;
    s.set_nonblocking(true)?;
    UdpSocket::from_std(s.into())
}

impl Transport {
    /// Devolve o transporte e o canal com os pacotes que chegam.
    /// `port`: porta fixa (a que foi aberta no roteador); cai pra qualquer uma se estiver ocupada.
    pub async fn gather(port: Option<u16>) -> io::Result<(Self, mpsc::Receiver<Packet>)> {
        let ifaces = if_addrs::get_if_addrs()?;
        let vpn_v4 = ifaces.iter().find_map(|i| match i.ip() {
            IpAddr::V4(v4) if usable(&i.ip()) => vpn_kind(&i.name, v4).map(|k| (v4, k)),
            _ => None,
        });
        let is_vpn = |ip: IpAddr| matches!((ip, vpn_v4), (IpAddr::V4(a), Some((b, _))) if a == b);
        let mut ips: Vec<IpAddr> = ifaces.iter().map(|i| i.ip()).filter(usable).collect();
        ips.sort();
        ips.dedup();

        let mut sockets = Vec::new();
        let mut candidates = Vec::new();
        for ip in ips {
            let Some(sock) = port.and_then(|p| bind(ip, p).ok()).or_else(|| bind(ip, 0).ok()) else { continue };
            let local = sock.local_addr()?;
            if let Ok(c) = Candidate::host(local, "udp") {
                candidates.push(c);
                sockets.push(Arc::new(sock));
            }
        }
        if sockets.is_empty() {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "sem rede"));
        }

        // IP público (atrás do roteador) de cada socket IPv4
        let mut stun_addrs = Vec::new();
        for host in STUN_SERVERS {
            if let Ok(Ok(addrs)) = tokio::time::timeout(Duration::from_millis(800), tokio::net::lookup_host(host)).await {
                stun_addrs.extend(addrs.filter(|a| a.is_ipv4()).take(1));
            }
        }
        let lookups = sockets.iter().filter(|s| s.local_addr().map(|a| a.is_ipv4()).unwrap_or(false)).map(|s| {
            let s = s.clone();
            let servers = stun_addrs.clone();
            async move { (s.local_addr().ok(), stun_query(&s, &servers).await) }
        });
        let mut lan_v4 = None;
        let mut public_v4 = None;
        for (base, mapped) in futures_util::future::join_all(lookups).await {
            if let (Some(base), Some(mapped)) = (base, mapped) {
                if let (IpAddr::V4(b), IpAddr::V4(m), None, false) = (base.ip(), mapped.ip(), lan_v4, is_vpn(base.ip())) {
                    lan_v4 = Some(b);
                    public_v4 = Some(m);
                }
                if mapped != base {
                    if let Ok(c) = Candidate::server_reflexive(mapped, base, "udp") {
                        candidates.push(c);
                    }
                }
            }
        }

        // O roteador esquece a porta em poucos minutos sem tráfego e o endereço anunciado morre.
        // Quem cria a sala pode esperar bastante até o amigo entrar, então mantém vivo.
        if let Some(server) = stun_addrs.first().copied() {
            for s in sockets.iter().filter(|s| s.local_addr().map(|a| a.is_ipv4()).unwrap_or(false)) {
                let s = s.clone();
                tokio::spawn(async move {
                    let mut tick = tokio::time::interval(Duration::from_secs(20));
                    loop {
                        tick.tick().await;
                        let _ = s.try_send_to(&stun::binding_request(&rand::random()), server);
                    }
                });
            }
        }

        // Um leitor por socket, tudo cai num canal só
        let (tx, incoming) = mpsc::channel::<Packet>(4096);
        let tx = shaper(tx);
        let relay: Arc<OnceLock<Arc<Relay>>> = Arc::new(OnceLock::new());
        for s in &sockets {
            spawn_reader(s.clone(), tx.clone(), None);
        }

        let sim_loss = std::env::var("TELAO_SIM_LOSS").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
        // IPv6 global de verdade (ULA fc00::/7 não serve pela internet)
        let global_v6 = sockets.iter().filter_map(|s| s.local_addr().ok()).find_map(|a| match a.ip() {
            IpAddr::V6(v6) if (v6.segments()[0] & 0xfe00) != 0xfc00 => Some(v6),
            _ => None,
        });
        let lan_v4 = lan_v4.or_else(|| {
            sockets.iter().filter_map(|s| s.local_addr().ok()).find_map(|a| match a.ip() {
                IpAddr::V4(v4) if v4.is_private() && !is_vpn(a.ip()) => Some(v4),
                _ => None,
            })
        });
        Ok((Transport { sockets, candidates, lan_v4, public_v4, global_v6, vpn_v4, sim_loss, dropped: AtomicU32::new(0), relay, tx }, incoming))
    }

    /// Porta UDP local de um IP (pra anunciar o mapeamento do roteador)
    pub fn local_addr_for(&self, ip: IpAddr) -> Option<SocketAddr> {
        self.sockets.iter().filter_map(|s| s.local_addr().ok()).find(|a| a.ip() == ip)
    }

    /// Pede um endereço no servidor TURN e anuncia como caminho de último recurso.
    /// Sem credencial (sala local, worker sem TURN) ou sem resposta, segue só com os diretos.
    pub async fn add_relay(&mut self, creds: Option<&Creds>) -> Result<SocketAddr, String> {
        // Teste: TELAO_TURN="turn:host:porta|usuário|senha" usa outro servidor TURN (ex.: coturn local)
        let test = std::env::var("TELAO_TURN").ok().and_then(|v| {
            let mut p = v.split('|');
            let url = p.next()?.strip_prefix("turn:")?.to_string();
            Some(Creds { servers: vec![url], username: p.next()?.into(), password: p.next()?.into() })
        });
        let creds = test.as_ref().or(creds).ok_or("servidor de salas não mandou credencial do relay")?;
        if self.relay.get().is_some() {
            return Err("relay já pedido".into());
        }
        // Socket só dele, numa porta nova, no IPv4 que sai pra internet (não o da VPN). Na porta fixa da
        // sala o servidor ainda guardaria o endereço de quem abriu o app há menos de 10 min (erro 437).
        let ip = self.lan_v4.map(IpAddr::V4).unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        let sock = Arc::new(bind(ip, 0).map_err(|e| e.to_string())?);
        let local = sock.local_addr().map_err(|e| e.to_string())?;
        // todos os endereços (porta 3478 e 53, e cada IPv4 do nome): em ~40% das vezes o primeiro
        // não respondia em 2,5 s (registro de 17/11) e a sala ficava sem caminho de reserva
        let mut servers: Vec<SocketAddr> = Vec::new();
        for host in &creds.servers {
            if let Ok(Ok(addrs)) = tokio::time::timeout(Duration::from_millis(1500), tokio::net::lookup_host(host.as_str())).await {
                for a in addrs.filter(|a| a.is_ipv4()) {
                    if !servers.contains(&a) {
                        servers.push(a);
                    }
                }
            }
        }
        let first = *servers.first().ok_or("não achou o servidor do relay")?;
        let relay = Arc::new(Relay::new(sock.clone(), first, creds.clone()));
        let _ = self.relay.set(relay.clone());
        spawn_reader(sock, self.tx.clone(), Some(relay.clone()));
        let mut addr = None;
        // 2 tentativas de 0,7 s (a resposta leva ~40 ms; o pedido perdido esperava 2,5 s
        // inteiros e a entrada na sala atrasava junto: era o "entrou no meio" lento do teste)
        // no total no máximo ~1,4 s: quem precisa do relay e não pegou agora pega na volta automática
        // pra sala (a conexão direta falha e o app entra de novo)
        let mut tries: Vec<SocketAddr> = servers.iter().take(2).copied().collect();
        while tries.len() < 2 {
            tries.push(first);
        }
        for s in tries.iter() {
            relay.set_server(*s);
            let wait = 700;
            addr = tokio::time::timeout(Duration::from_millis(wait), relay.allocate()).await.ok().flatten();
            if addr.is_some() {
                break;
            }
        }
        let addr = addr.ok_or("servidor do relay não respondeu")?;
        let c = Candidate::relayed(addr, local, "udp").map_err(|e| e.to_string())?;
        self.candidates.push(c);
        // Teste: TELAO_SO_RELAY=1 anuncia só o relay, pra provar que o vídeo passa por ele
        if std::env::var_os("TELAO_SO_RELAY").is_some() {
            self.candidates.retain(|c| c.kind() == str0m::CandidateKind::Relayed);
        }
        tokio::spawn(relay.keepalive());
        Ok(addr)
    }

    pub fn send(&self, t: &Transmit) {
        if let Some(r) = self.relay.get().filter(|r| r.relayed.get() == Some(&t.source)) {
            r.send(t.destination, &t.contents);
            return;
        }
        if let Some(s) = self.sockets.iter().find(|s| s.local_addr().map(|a| a == t.source).unwrap_or(false)) {
            // UDP: se o buffer do sistema lotar, perde o pacote (o NACK recupera)
            let _ = s.try_send_to(&t.contents, t.destination);
        }
    }

    /// Simulação de perda (só pra teste)
    pub fn should_drop(&self) -> bool {
        if self.sim_loss == 0 {
            return false;
        }
        let drop = rand::random_range(0..100) < self.sim_loss;
        if drop {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        drop
    }
}

/// Um leitor por socket, tudo cai num canal só. No socket do relay, tira o envelope TURN e entrega
/// como se o pacote viesse direto do amigo.
fn spawn_reader(s: Arc<UdpSocket>, tx: mpsc::Sender<Packet>, relay: Option<Arc<Relay>>) {
    tokio::spawn(async move {
        let Ok(local) = s.local_addr() else { return };
        let mut buf = vec![0u8; 2048];
        loop {
            match s.recv_from(&mut buf).await {
                Ok((n, from)) => {
                    let p = match &relay {
                        Some(r) => {
                            if r.server() != from {
                                continue;
                            }
                            let (Some((peer, data)), Some(&relayed)) = (r.handle(&buf[..n]), r.relayed.get()) else { continue };
                            Packet { data, source: peer, destination: relayed, at: Instant::now() }
                        }
                        None => Packet { data: buf[..n].to_vec(), source: from, destination: local, at: Instant::now() },
                    };
                    if tx.send(p).await.is_err() {
                        break;
                    }
                }
                // ICMP "porta fechada" no Windows vira erro de leitura; ignora
                Err(_) => tokio::time::sleep(Duration::from_millis(1)).await,
            }
        }
    });
}

async fn stun_query(sock: &UdpSocket, servers: &[SocketAddr]) -> Option<SocketAddr> {
    let txid: [u8; 12] = rand::random();
    let req = stun::binding_request(&txid);
    let mut buf = [0u8; 512];
    for _attempt in 0..2 {
        for s in servers {
            let _ = sock.send_to(&req, s).await;
        }
        let deadline = tokio::time::Instant::now() + Duration::from_millis(400);
        while let Ok(Ok((n, _))) = tokio::time::timeout_at(deadline, sock.recv_from(&mut buf)).await {
            if let Some(addr) = stun::parse_binding_response(&buf[..n], &txid) {
                return Some(addr);
            }
        }
    }
    None
}

/// Teste: simula internet lenta na chegada (TELAO_SIM_RATE=10 Mbps), com fila de roteador de
/// TELAO_SIM_QUEUE_MS (padrão 150) que descarta o que não cabe. Sem a variável, não faz nada.
fn shaper(out: mpsc::Sender<Packet>) -> mpsc::Sender<Packet> {
    let Some(rate) = std::env::var("TELAO_SIM_RATE").ok().and_then(|v| v.parse::<f64>().ok()) else {
        return out;
    };
    let queue = Duration::from_millis(std::env::var("TELAO_SIM_QUEUE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(150));
    // TELAO_SIM_DELAY_MS: atraso fixo de ida (o ping da internet), além da fila
    let delay = Duration::from_millis(std::env::var("TELAO_SIM_DELAY_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(0));
    // TELAO_SIM_JITTER_MS: atraso extra que muda a cada 40 ms (0..J), como Wi-Fi; mantém a ordem
    let jitter = std::env::var("TELAO_SIM_JITTER_MS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    let bytes_per_sec = rate * 1e6 / 8.0;
    let (tx, mut rx) = mpsc::channel::<Packet>(4096);
    let (delayed_tx, mut delayed_rx) = mpsc::unbounded_channel::<(tokio::time::Instant, Packet)>();
    // TELAO_SIM_SHARED: todas as conexões do processo dividem a mesma linha (como o upload de quem
    // criou a sala, que manda uma cópia de cada tela pra cada amigo)
    static SHARED: std::sync::Mutex<Option<tokio::time::Instant>> = std::sync::Mutex::new(None);
    let shared = std::env::var_os("TELAO_SIM_SHARED").is_some();
    tokio::spawn(async move {
        let mut free_at = tokio::time::Instant::now();
        let (mut extra, mut extra_at, mut last_out) = (Duration::ZERO, tokio::time::Instant::now(), tokio::time::Instant::now());
        while let Some(p) = rx.recv().await {
            let now = tokio::time::Instant::now();
            if jitter > 0 && now - extra_at >= Duration::from_millis(40) {
                extra_at = now;
                extra = Duration::from_millis(rand::random_range(0..=jitter));
            }
            if shared {
                free_at = SHARED.lock().unwrap_or_else(|e| e.into_inner()).unwrap_or(now);
            }
            free_at = free_at.max(now);
            if free_at - now > queue {
                continue; // fila do roteador cheia
            }
            free_at += Duration::from_secs_f64(p.data.len() as f64 / bytes_per_sec);
            if shared {
                *SHARED.lock().unwrap_or_else(|e| e.into_inner()) = Some(free_at);
            }
            last_out = last_out.max(free_at + delay + extra);
            let _ = delayed_tx.send((last_out, p));
        }
    });
    tokio::spawn(async move {
        while let Some((at, mut p)) = delayed_rx.recv().await {
            tokio::time::sleep_until(at).await;
            p.at = Instant::now();
            if out.send(p).await.is_err() {
                break;
            }
        }
    });
    tx
}
