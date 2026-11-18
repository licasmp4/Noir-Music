//! Código que quem transmite manda pros amigos: endereços do PC dele + a sala (que é a senha).
//!
//! Formato: "TL-" + base32 (Crockford) de [versão][flags][porta][IPv4?][IPv6?][IPv4 da rede local?]
//! [IPv4 do Radmin/Hamachi/Tailscale?][sala].

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use crate::rendezvous::ALPHABET;

const VERSION: u8 = 1;
const HAS_V4: u8 = 1;
const HAS_V6: u8 = 2;
const HAS_LAN: u8 = 4;
const HAS_VPN: u8 = 8;
const B32: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

#[derive(Debug, Clone, PartialEq)]
pub struct ShareCode {
    pub port: u16,
    pub v4: Option<Ipv4Addr>,
    pub v6: Option<Ipv6Addr>,
    pub lan: Option<Ipv4Addr>,
    /// IP na rede virtual (Radmin VPN etc.)
    pub vpn: Option<Ipv4Addr>,
    pub room: String,
}

impl ShareCode {
    pub fn encode(&self) -> String {
        let mut b = vec![VERSION, 0];
        b.extend_from_slice(&self.port.to_be_bytes());
        if let Some(ip) = self.v4 {
            b[1] |= HAS_V4;
            b.extend_from_slice(&ip.octets());
        }
        if let Some(ip) = self.v6 {
            b[1] |= HAS_V6;
            b.extend_from_slice(&ip.octets());
        }
        if let Some(ip) = self.lan {
            b[1] |= HAS_LAN;
            b.extend_from_slice(&ip.octets());
        }
        if let Some(ip) = self.vpn {
            b[1] |= HAS_VPN;
            b.extend_from_slice(&ip.octets());
        }
        // Sala de 6 letras do alfabeto de 31 cabe em 4 bytes
        let room = self.room.bytes().fold(0u32, |acc, c| {
            acc * ALPHABET.len() as u32 + ALPHABET.iter().position(|&a| a == c).unwrap_or(0) as u32
        });
        b.extend_from_slice(&room.to_be_bytes());

        let text = base32(&b);
        let groups: Vec<&str> = text.as_bytes().chunks(5).map(|c| std::str::from_utf8(c).unwrap()).collect();
        format!("TL-{}", groups.join("-"))
    }

    pub fn decode(code: &str) -> Option<Self> {
        let clean: String = code.trim().trim_start_matches(|c: char| c.is_whitespace()).to_uppercase();
        let body = clean.strip_prefix("TL")?;
        let b = unbase32(body)?;
        let mut r = Reader(&b);
        if r.u8()? != VERSION {
            return None;
        }
        let flags = r.u8()?;
        let port = u16::from_be_bytes(r.take::<2>()?);
        let v4 = if flags & HAS_V4 != 0 { Some(Ipv4Addr::from(r.take::<4>()?)) } else { None };
        let v6 = if flags & HAS_V6 != 0 { Some(Ipv6Addr::from(r.take::<16>()?)) } else { None };
        let lan = if flags & HAS_LAN != 0 { Some(Ipv4Addr::from(r.take::<4>()?)) } else { None };
        let vpn = if flags & HAS_VPN != 0 { Some(Ipv4Addr::from(r.take::<4>()?)) } else { None };
        let mut n = u32::from_be_bytes(r.take::<4>()?);
        let mut room = [0u8; 6];
        for c in room.iter_mut().rev() {
            *c = ALPHABET[(n % ALPHABET.len() as u32) as usize];
            n /= ALPHABET.len() as u32;
        }
        Some(ShareCode { port, v4, v6, lan, vpn, room: String::from_utf8(room.to_vec()).ok()? })
    }

    /// Endereços pra tentar, na ordem de preferência (rede local primeiro: se der, é o melhor)
    pub fn addrs(&self) -> Vec<SocketAddr> {
        [self.lan.map(IpAddr::V4), self.vpn.map(IpAddr::V4), self.v6.map(IpAddr::V6), self.v4.map(IpAddr::V4)]
            .into_iter()
            .flatten()
            .map(|ip| SocketAddr::new(ip, self.port))
            .collect()
    }
}

struct Reader<'a>(&'a [u8]);
impl Reader<'_> {
    fn u8(&mut self) -> Option<u8> {
        Some(self.take::<1>()?[0])
    }
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, rest) = self.0.split_at_checked(N)?;
        self.0 = rest;
        head.try_into().ok()
    }
}

fn base32(data: &[u8]) -> String {
    let mut out = String::new();
    let (mut buf, mut bits) = (0u32, 0);
    for &byte in data {
        buf = (buf << 8) | byte as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(B32[((buf >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(B32[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

fn unbase32(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut buf, mut bits) = (0u32, 0);
    for c in text.chars() {
        // Crockford: O = 0, I/L = 1; traço e espaço são só enfeite
        let c = match c {
            '-' | ' ' | '\n' | '\t' => continue,
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        };
        let v = B32.iter().position(|&b| b as char == c)? as u32;
        buf = (buf << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ida_e_volta() {
        let c = ShareCode {
            port: 47800,
            v4: Some("179.94.55.81".parse().unwrap()),
            v6: Some("2804:7f0:9c81:cad:d0d:7241:495a:9fac".parse().unwrap()),
            lan: Some("192.168.15.3".parse().unwrap()),
            vpn: Some("26.12.200.7".parse().unwrap()),
            room: "K7M2QX".into(),
        };
        let s = c.encode();
        assert!(s.starts_with("TL-"));
        assert_eq!(ShareCode::decode(&s), Some(c.clone()));
        assert_eq!(ShareCode::decode(&s.to_lowercase().replace('-', " ")), Some(c));
    }

    #[test]
    fn so_rede_local() {
        let c = ShareCode { port: 47801, v4: None, v6: None, lan: Some("10.0.0.2".parse().unwrap()), vpn: None, room: "AAAAAA".into() };
        assert_eq!(ShareCode::decode(&c.encode()), Some(c));
    }

    #[test]
    fn so_radmin() {
        let c = ShareCode { port: 47800, v4: None, v6: None, lan: None, vpn: Some("26.44.1.9".parse().unwrap()), room: "QWERTY".into() };
        assert_eq!(ShareCode::decode(&c.encode()), Some(c.clone()));
        assert_eq!(c.addrs(), vec!["26.44.1.9:47800".parse().unwrap()]);
    }
}
