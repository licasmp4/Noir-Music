//! Buscar músicas e achar capas.
//! - Spotify: API oficial (Client ID e Secret de um app criado em developer.spotify.com, de graça).
//! - Capas do Apple Music: busca pública do iTunes (título + artista), sem chave nenhuma.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::Value;

use crate::proto::Track;

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(Duration::from_secs(8)).user_agent("NoirMusic/0.1").build()
}

// MARK: - Spotify

static TOKEN: Mutex<Option<(String, Instant)>> = Mutex::new(None);

fn spotify_token(id: &str, secret: &str) -> Result<String, String> {
    if let Some((t, until)) = TOKEN.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        if Instant::now() < until {
            return Ok(t);
        }
    }
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("{id}:{secret}"));
    let v: Value = agent()
        .post("https://accounts.spotify.com/api/token")
        .set("Authorization", &format!("Basic {basic}"))
        .send_form(&[("grant_type", "client_credentials")])
        .map_err(|e| match e {
            ureq::Error::Status(400 | 401, _) => "Client ID ou Secret do Spotify errado".to_string(),
            e => e.to_string(),
        })?
        .into_json()
        .map_err(|e| e.to_string())?;
    let t = v["access_token"].as_str().ok_or("o Spotify não mandou a chave")?.to_string();
    let secs = v["expires_in"].as_u64().unwrap_or(3600).saturating_sub(60);
    *TOKEN.lock().unwrap_or_else(|e| e.into_inner()) = Some((t.clone(), Instant::now() + Duration::from_secs(secs)));
    Ok(t)
}

/// Esquece a chave guardada (trocou o Client ID)
pub fn reset_spotify() {
    *TOKEN.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

pub fn spotify(q: &str, id: &str, secret: &str) -> Result<Vec<Track>, String> {
    let token = spotify_token(id, secret)?;
    let v: Value = agent()
        .get("https://api.spotify.com/v1/search")
        .query("q", q)
        .query("type", "track")
        .query("limit", "10")
        .set("Authorization", &format!("Bearer {token}"))
        .call()
        .map_err(|e| e.to_string())?
        .into_json()
        .map_err(|e| e.to_string())?;
    Ok(v["tracks"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            let images = t["album"]["images"].as_array();
            // a do meio (300 px) chega rápido e fica nítida no tamanho da lista
            let art = images.and_then(|i| i.get(1).or(i.first())).and_then(|i| i["url"].as_str()).unwrap_or("").to_string();
            Some(Track {
                id: t["uri"].as_str()?.to_string(),
                title: t["name"].as_str()?.to_string(),
                artist: t["artists"].as_array().map(|a| a.iter().filter_map(|x| x["name"].as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default(),
                album: t["album"]["name"].as_str().unwrap_or("").to_string(),
                art,
                dur: t["duration_ms"].as_f64().unwrap_or(0.0) / 1000.0,
                by: String::new(),
            })
        })
        .collect())
}

/// Confere o Client ID e o Secret (botão Testar dos Ajustes)
pub fn spotify_check(id: &str, secret: &str) -> Result<(), String> {
    reset_spotify();
    spotify_token(id, secret).map(|_| ())
}

// MARK: - Capas pelo iTunes

static ART: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// Capa de uma música (1400 px) pela busca pública do iTunes. Guarda o que já achou.
pub fn itunes_art(title: &str, artist: &str) -> Option<String> {
    let key = format!("{title}\u{1}{artist}").to_lowercase();
    if let Some(u) = ART.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get(&key) {
        return Some(u.clone()).filter(|u| !u.is_empty());
    }
    // "(feat. …)" e "[Explicit]" atrapalham a busca
    let clean = |s: &str| s.split(['(', '[']).next().unwrap_or(s).trim().to_string();
    let term = format!("{} {}", clean(title), artist.split([',', '&']).next().unwrap_or(artist).trim());
    let v: Option<Value> = agent()
        .get("https://itunes.apple.com/search")
        .query("term", &term)
        .query("entity", "song")
        .query("limit", "1")
        .query("country", "BR")
        .call()
        .ok()
        .and_then(|r| r.into_json().ok());
    let url = v
        .as_ref()
        .and_then(|v| v["results"][0]["artworkUrl100"].as_str())
        .map(|u| u.replace("100x100bb", "1400x1400bb"))
        .unwrap_or_default();
    ART.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(key, url.clone());
    Some(url).filter(|u| !u.is_empty())
}

/// Baixa uma imagem (capa)
pub fn fetch(url: &str) -> Option<Vec<u8>> {
    if !url.starts_with("https://") {
        return None;
    }
    let r = agent().get(url).call().ok()?;
    let mut buf = Vec::new();
    use std::io::Read;
    r.into_reader().take(4_000_000).read_to_end(&mut buf).ok()?;
    Some(buf)
}
