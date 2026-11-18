//! O que vai pela sala entre quem toca e quem ouve (dentro do `{"kind": "musica", "data": ...}`).
//!
//! Quem ouve → quem toca: `cmd` (tocar/pausar, próxima, anterior, ir pra um ponto), `add`
//! (pôr na fila), `play` (tocar agora), `remove`/`up` (mexer na fila), `search` (buscar).
//! Quem toca → todo mundo: `state` (música, fila, de onde vem); → uma pessoa: `results`.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Track {
    /// Spotify: `spotify:track:…`. Apple Music: id da biblioteca (persistent ID)
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub album: String,
    /// Capa (https)
    #[serde(default)]
    pub art: String,
    /// Segundos
    #[serde(default)]
    pub dur: f64,
    /// Quem pôs na fila
    #[serde(default)]
    pub by: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Now {
    pub track: Track,
    /// Segundos, no momento em que o estado saiu de quem toca
    pub pos: f64,
    pub playing: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct State {
    pub now: Option<Now>,
    pub queue: Vec<Track>,
    /// "spotify", "music" ou vazio (nada aberto)
    pub source: String,
    /// Nome de quem toca
    pub host: String,
    /// Dá pra buscar? (Spotify sem a chave da API não dá)
    pub search: bool,
    /// Onde a busca procura ("o Spotify inteiro" ou "a biblioteca do Lucas")
    #[serde(default)]
    pub search_in: String,
}

pub fn source_name(s: &str) -> &'static str {
    match s {
        "spotify" => "Spotify",
        "music" => "Apple Music",
        _ => "",
    }
}
