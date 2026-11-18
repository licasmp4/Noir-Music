//! O que o Noir Music lembra entre uma vez e outra (prefs.json na pasta do log).

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Prefs {
    pub name: String,
    pub volume: f32,
    pub last_code: String,
    /// App do Spotify em developer.spotify.com (só quem toca precisa, pra buscar)
    pub spotify_id: String,
    pub spotify_secret: String,
    /// Equalizador: modo escolhido ("" = Original, "custom" = mexido à mão), faixas e se está ligado
    pub eq_preset: String,
    pub eq_gains: [f32; crate::eq::N],
    pub eq_on: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        let name = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default();
        Prefs { name: name.split('.').next().unwrap_or("").to_string(), volume: 0.8, last_code: String::new(), spotify_id: String::new(), spotify_secret: String::new(), eq_preset: String::new(), eq_gains: [0.0; crate::eq::N], eq_on: true }
    }
}

fn path() -> std::path::PathBuf {
    crate::log::dir().join("prefs.json")
}

impl Prefs {
    pub fn load() -> Prefs {
        std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(b) = serde_json::to_vec_pretty(self) {
            let tmp = path().with_extension("tmp");
            if std::fs::write(&tmp, b).is_ok() {
                let _ = std::fs::rename(tmp, path());
            }
        }
    }

    pub fn has_spotify(&self) -> bool {
        !self.spotify_id.trim().is_empty() && !self.spotify_secret.trim().is_empty()
    }
}
