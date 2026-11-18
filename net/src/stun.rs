//! STUN mínimo (RFC 5389): só o pedido de "qual é meu IP público?".

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

const MAGIC: u32 = 0x2112_A442;

pub fn binding_request(txid: &[u8; 12]) -> Vec<u8> {
    let mut b = Vec::with_capacity(20);
    b.extend_from_slice(&0x0001u16.to_be_bytes());
    b.extend_from_slice(&0u16.to_be_bytes());
    b.extend_from_slice(&MAGIC.to_be_bytes());
    b.extend_from_slice(txid);
    b
}

/// Endereço público visto pelo servidor STUN, se a resposta for nossa.
pub fn parse_binding_response(buf: &[u8], txid: &[u8; 12]) -> Option<SocketAddr> {
    if buf.len() < 20 || u16::from_be_bytes([buf[0], buf[1]]) != 0x0101 {
        return None;
    }
    if u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) != MAGIC || &buf[8..20] != txid {
        return None;
    }
    let len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
    let body = buf.get(20..20 + len)?;
    let mut i = 0;
    let mut plain = None;
    while i + 4 <= body.len() {
        let typ = u16::from_be_bytes([body[i], body[i + 1]]);
        let alen = u16::from_be_bytes([body[i + 2], body[i + 3]]) as usize;
        let val = body.get(i + 4..i + 4 + alen)?;
        match typ {
            0x0020 => return parse_addr(val, true, txid),
            0x0001 => plain = parse_addr(val, false, txid),
            _ => {}
        }
        i += 4 + ((alen + 3) & !3);
    }
    plain
}

pub(crate) fn parse_addr(v: &[u8], xor: bool, txid: &[u8; 12]) -> Option<SocketAddr> {
    if v.len() < 8 {
        return None;
    }
    let mut port = u16::from_be_bytes([v[2], v[3]]);
    if xor {
        port ^= (MAGIC >> 16) as u16;
    }
    let ip = match v[1] {
        0x01 => {
            let mut o = [v[4], v[5], v[6], v[7]];
            if xor {
                for (b, m) in o.iter_mut().zip(MAGIC.to_be_bytes()) {
                    *b ^= m;
                }
            }
            IpAddr::V4(Ipv4Addr::from(o))
        }
        0x02 if v.len() >= 20 => {
            let mut o: [u8; 16] = v[4..20].try_into().ok()?;
            if xor {
                let mut key = [0u8; 16];
                key[..4].copy_from_slice(&MAGIC.to_be_bytes());
                key[4..].copy_from_slice(txid);
                for (b, k) in o.iter_mut().zip(key) {
                    *b ^= k;
                }
            }
            IpAddr::V6(Ipv6Addr::from(o))
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}
