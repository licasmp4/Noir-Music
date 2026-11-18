//! Abre a porta do Telão no roteador sozinho (UPnP) e descobre se dá pra receber conexão:
//! IPv4 com porta aberta, IPv6 com passagem no firewall, ou nenhum (CGNAT/roteador sem UPnP).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};
use std::time::Duration;

use igd_next::PortMappingProtocol::{TCP, UDP};
use igd_next::SearchOptions;
use igd_next::aio::Gateway;
use igd_next::aio::tokio::{Tokio, search_gateway};
use serde_json::{Value, json};

const LEASE_SECS: u32 = 3600;
const DESCRIPTION: &str = "Telao";

#[derive(Default, Debug, Clone)]
pub struct Reach {
    pub upnp: bool,
    /// IP que o roteador diz ter na internet
    pub router_v4: Option<Ipv4Addr>,
    /// IP que o STUN viu (o de verdade)
    pub public_v4: Option<Ipv4Addr>,
    /// Porta aberta no IPv4 (TCP do servidor de salas e UDP do vídeo)
    pub v4_open: bool,
    /// Operadora divide o IPv4 com outros clientes: não dá pra receber conexão por ele
    pub cgnat: bool,
    pub v6: Option<Ipv6Addr>,
    pub v6_open: bool,
    /// O que deu errado (pra mostrar na tela)
    pub problem: Option<String>,
}

impl Reach {
    pub fn to_json(&self) -> Value {
        json!({
            "upnp": self.upnp, "router_v4": self.router_v4, "public_v4": self.public_v4,
            "v4_open": self.v4_open, "cgnat": self.cgnat, "v6": self.v6, "v6_open": self.v6_open,
            "problem": self.problem,
        })
    }

    /// IPv4 pra colocar no código (só se tem chance de funcionar)
    pub fn usable_v4(&self) -> Option<Ipv4Addr> {
        if self.cgnat { None } else { self.public_v4.or(self.router_v4) }
    }
}

pub struct PortMap {
    gateway: Option<Gateway<Tokio>>,
    port: u16,
    v4_mapped: bool,
    pinholes: Vec<u16>,
}

fn is_shared(ip: Ipv4Addr) -> bool {
    // 100.64.0.0/10 é a faixa de CGNAT; privado também indica NAT duplo
    let o = ip.octets();
    ip.is_private() || (o[0] == 100 && (64..128).contains(&o[1])) || ip.is_unspecified()
}

pub async fn open(port: u16, lan_v4: Option<Ipv4Addr>, v6: Option<Ipv6Addr>, public_v4: Option<Ipv4Addr>) -> (Reach, PortMap) {
    let mut reach = Reach { public_v4, v6, ..Default::default() };
    let mut map = PortMap { gateway: None, port, v4_mapped: false, pinholes: Vec::new() };

    let mut opts = SearchOptions::default();
    opts.timeout = Some(Duration::from_secs(3));
    // Sai pela placa de rede certa (com VPN/várias interfaces o multicast pode ir pro lugar errado)
    if let Some(lan) = lan_v4 {
        opts.bind_addr = SocketAddr::new(IpAddr::V4(lan), 0);
    }
    let gw = match search_gateway(opts).await {
        Ok(gw) => gw,
        Err(e) => {
            // Sem UPnP: IPv6 ainda pode estar aberto (alguns roteadores não bloqueiam)
            reach.problem = Some(format!("roteador não respondeu ao UPnP: {e}"));
            return (reach, map);
        }
    };
    reach.upnp = true;

    if let Ok(IpAddr::V4(ext)) = gw.get_external_ip().await {
        reach.router_v4 = Some(ext);
        reach.cgnat = is_shared(ext) || public_v4.is_some_and(|p| p != ext);
    }
    if let Some(lan) = lan_v4 {
        let local = SocketAddr::new(IpAddr::V4(lan), port);
        let tcp = gw.add_port(TCP, port, local, LEASE_SECS, DESCRIPTION).await;
        let udp = gw.add_port(UDP, port, local, LEASE_SECS, DESCRIPTION).await;
        map.v4_mapped = tcp.is_ok() && udp.is_ok();
        if let Err(e) = tcp.as_ref().and(udp.as_ref()) {
            reach.problem = Some(format!("roteador recusou abrir a porta: {e}"));
        }
        reach.v4_open = map.v4_mapped && !reach.cgnat;
    }
    if let Some(v6) = v6 {
        for proto in [TCP, UDP] {
            if let Ok(id) = gw.add_pinhole(proto, SocketAddrV6::new(v6, port, 0, 0), LEASE_SECS).await {
                map.pinholes.push(id);
            }
        }
        reach.v6_open = map.pinholes.len() == 2;
        if !reach.v6_open && gw.ipv6_firewall().is_none() {
            reach.problem.get_or_insert_with(|| "roteador não deixa abrir o firewall do IPv6 pelo app".into());
        }
    }
    map.gateway = Some(gw);
    (reach, map)
}

impl PortMap {
    /// Renova antes de vencer (o roteador apaga sozinho depois de 1 h)
    pub async fn renew(&self, lan_v4: Option<Ipv4Addr>) {
        let Some(gw) = &self.gateway else { return };
        if let (true, Some(lan)) = (self.v4_mapped, lan_v4) {
            let local = SocketAddr::new(IpAddr::V4(lan), self.port);
            let _ = gw.add_port(TCP, self.port, local, LEASE_SECS, DESCRIPTION).await;
            let _ = gw.add_port(UDP, self.port, local, LEASE_SECS, DESCRIPTION).await;
        }
        for &id in &self.pinholes {
            let _ = gw.update_pinhole(id, LEASE_SECS).await;
        }
    }

    pub async fn close(&self) {
        let Some(gw) = &self.gateway else { return };
        if self.v4_mapped {
            let _ = gw.remove_port(TCP, self.port).await;
            let _ = gw.remove_port(UDP, self.port).await;
        }
        for &id in &self.pinholes {
            let _ = gw.remove_pinhole(id).await;
        }
    }
}
