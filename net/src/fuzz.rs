//! Teste de estresse: joga lixo aleatório (e lixo "quase certo") em tudo que lê dados de fora.
//! Nada aqui pode travar ou derrubar o app: o certo é sempre devolver None.

use std::net::SocketAddr;
use std::sync::Arc;

fn rng(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

fn junk(seed: &mut u64, max: usize) -> Vec<u8> {
    let n = (rng(seed) as usize) % (max + 1);
    (0..n).map(|_| rng(seed) as u8).collect()
}

/// Começa de um pacote válido e estraga alguns bytes: chega mais fundo no código que lixo puro
fn mutate(seed: &mut u64, base: &[u8]) -> Vec<u8> {
    let mut b = base.to_vec();
    for _ in 0..(1 + rng(seed) % 6) {
        if b.is_empty() {
            break;
        }
        let i = (rng(seed) as usize) % b.len();
        match rng(seed) % 4 {
            0 => b[i] = rng(seed) as u8,
            1 => b.truncate(i),
            2 => b.insert(i, rng(seed) as u8),
            _ => b.extend(junk(seed, 40)),
        }
    }
    b
}

const ROUNDS: usize = 300_000;

#[test]
fn stun_nao_cai_com_lixo() {
    let mut s = 0x9E37_79B9_7F4A_7C15;
    let txid = [7u8; 12];
    // resposta válida com XOR-MAPPED-ADDRESS IPv6, pra mutar
    let mut ok = vec![0x01, 0x01, 0, 24, 0x21, 0x12, 0xA4, 0x42];
    ok.extend_from_slice(&txid);
    ok.extend_from_slice(&[0x00, 0x20, 0, 20, 0, 0x02, 0x12, 0x34]);
    ok.extend_from_slice(&[0xAB; 16]);
    for _ in 0..ROUNDS {
        let _ = crate::stun::parse_binding_response(&junk(&mut s, 120), &txid);
        let _ = crate::stun::parse_binding_response(&mutate(&mut s, &ok), &txid);
        let v = junk(&mut s, 30);
        let _ = crate::stun::parse_addr(&v, true, &txid);
        let _ = crate::stun::parse_addr(&v, false, &txid);
    }
}

#[test]
fn turn_nao_cai_com_lixo() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let sock = Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let server: SocketAddr = "127.0.0.1:3478".parse().unwrap();
        let creds = crate::turn::Creds { servers: vec!["127.0.0.1:3478".into()], username: "u".into(), password: "p".into() };
        let relay = crate::turn::Relay::new(sock, server, creds);
        let mut s = 0xDEAD_BEEF_CAFE_F00D;
        // Data indication válida e ChannelData, pra mutar
        let mut ind = vec![0x00, 0x17, 0, 20, 0x21, 0x12, 0xA4, 0x42];
        ind.extend_from_slice(&[1; 12]);
        ind.extend_from_slice(&[0x00, 0x12, 0, 8, 0, 1, 0x12, 0x34, 1, 2, 3, 4]);
        ind.extend_from_slice(&[0x00, 0x13, 0, 4, 9, 9, 9, 9]);
        let chan = vec![0x40, 0x00, 0, 4, 1, 2, 3, 4];
        for _ in 0..ROUNDS {
            let _ = relay.handle(&junk(&mut s, 200));
            let _ = relay.handle(&mutate(&mut s, &ind));
            let _ = relay.handle(&mutate(&mut s, &chan));
        }
    });
}

#[test]
fn codigos_e_credenciais_nao_caem() {
    let mut s = 0x1234_5678_9ABC_DEF1;
    let base = ["NOIR-ABCD-EFGH", "TL-AAAAA-BBBBB-CCCCC-DDDDD", "noir abcd efgh", "TL-"];
    for _ in 0..ROUNDS / 3 {
        let raw = junk(&mut s, 60);
        let text = String::from_utf8_lossy(&raw);
        let _ = crate::cloud::parse(&text);
        let _ = crate::share_code::ShareCode::decode(&text);
        let b = base[(rng(&mut s) % 4) as usize];
        let m = String::from_utf8_lossy(&mutate(&mut s, b.as_bytes())).to_string();
        let _ = crate::cloud::parse(&m);
        let _ = crate::share_code::ShareCode::decode(&m);
        // emojis e acentos no meio do código
        let _ = crate::share_code::ShareCode::decode(&format!("TL-ÁÉ🎮{m}ç"));
    }
    let samples = [
        r#"{"iceServers":[{"urls":["turn:a:1?transport=udp"],"username":"x","credential":"y"}]}"#,
        r#"{"iceServers":{"urls":"turn:a:1","username":1,"credential":null}}"#,
        r#"{"iceServers":[]}"#,
        r#"[]"#,
        r#"{"iceServers":[{"urls":[null, 5, "turn:", "turn::::"]}]}"#,
    ];
    for _ in 0..ROUNDS / 10 {
        let b = samples[(rng(&mut s) % samples.len() as u64) as usize];
        let m = String::from_utf8_lossy(&mutate(&mut s, b.as_bytes())).to_string();
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&m) {
            let _ = crate::turn::Creds::from_json(&v);
        }
    }
    for b in samples {
        let v: serde_json::Value = serde_json::from_str(b).unwrap();
        let _ = crate::turn::Creds::from_json(&v);
    }
}

#[test]
fn codigo_gerado_sempre_volta() {
    // todo código que o app cria tem que ser lido de volta, com qualquer sala
    let mut s = 42u64;
    for _ in 0..20_000 {
        let room: String = (0..6).map(|_| b"ABCDEFGHJKLMNPQRSTUVWXYZ2345679"[(rng(&mut s) % 31) as usize] as char).collect();
        let c = crate::share_code::ShareCode { port: rng(&mut s) as u16, v4: None, v6: None, lan: Some(std::net::Ipv4Addr::from(rng(&mut s) as u32)), vpn: None, room };
        let text = c.encode();
        assert!(crate::share_code::ShareCode::decode(&text).is_some(), "{text}");
    }
}
