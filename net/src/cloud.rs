//! Servidor de salas no Cloudflare (`cloud-signal/`): permite entrar só com o código, sem VPN e
//! sem abrir porta. Ele só apresenta os dois lados; vídeo e áudio continuam indo direto entre os PCs.

use std::sync::Mutex;
use std::time::Duration;

use crate::rendezvous::ALPHABET;
use crate::signal::Signal;
use crate::viewer::Target;

const DEFAULT_URL: &str = "wss://noir-signal.lucasgluiz-9.workers.dev";
const PREFIX: &str = "JAM";
const ROOM_LEN: usize = 8;

fn base() -> String {
    std::env::var("JAM_CLOUD").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| DEFAULT_URL.into())
}

/// Como a próxima sala abre (o app escolhe antes de criar)
#[derive(Clone, Debug, Default)]
pub struct RoomOpts {
    /// Quem chega bate na porta e o dono deixa entrar
    pub private: bool,
    /// Sala fixa: (código curto de 8, chave do dono). A chave fica só no app de quem criou
    pub fixed: Option<(String, String)>,
    /// Nome do dono, pro aviso "a sala de Lucas está fechada"
    pub name: String,
}

static OPTS: Mutex<Option<RoomOpts>> = Mutex::new(None);
/// A senha do dono da sala aberta neste PC: (código curto, senha). O dono também entra como
/// membro pela nuvem e, numa sala privada, não pode ficar esperando ele mesmo deixar entrar
static OWNER: Mutex<Option<(String, String)>> = Mutex::new(None);

pub fn set_opts(o: RoomOpts) {
    *OPTS.lock().unwrap_or_else(|e| e.into_inner()) = Some(o);
}

pub(crate) fn opts() -> RoomOpts {
    OPTS.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

pub(crate) fn set_owner(room: &str, pass: &str) {
    *OWNER.lock().unwrap_or_else(|e| e.into_inner()) = Some((room.to_string(), pass.to_string()));
}

pub(crate) fn owner_pass(room: &str) -> Option<String> {
    OWNER.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|(r, _)| r == room).map(|(_, p)| p.clone())
}

/// Uma chave nova pra sala fixa (32 letras aleatórias)
pub fn new_key() -> String {
    use rand::Rng;
    let mut r = rand::rng();
    (0..32).map(|_| ALPHABET[r.random_range(0..ALPHABET.len())] as char).collect()
}

/// Cria uma sala nova (ou abre a fixa). Sem internet (ou com `TELAO_CLOUD=off`) dá erro e quem chamou
/// usa o servidor embutido. A fixa que não abrir (a chave não vale mais) cai numa sala comum.
pub async fn create() -> Result<Signal, String> {
    if base() == "off" {
        return Err("desligado".into());
    }
    let connect = |url: String| async move { tokio::time::timeout(Duration::from_secs(4), Signal::connect(&url)).await.map_err(|_| "sem resposta".to_string())? };
    if let Some((room, key)) = opts().fixed {
        if let Ok(s) = connect(format!("{}/fixa/{room}?k={key}", base())).await {
            return Ok(s);
        }
    }
    connect(format!("{}/new", base())).await
}

/// Pra onde quem criou volta se a ligação com o servidor de salas cair: a fixa com a chave, a comum
/// com o recibo que o servidor mandou ao abrir
pub(crate) fn return_url(room: &str, resume: Option<&str>) -> Option<String> {
    if let Some((r, key)) = opts().fixed.filter(|(r, _)| r == room) {
        return Some(format!("{}/fixa/{r}?k={key}", base()));
    }
    resume.map(|t| format!("{}/volta/{room}?r={t}", base()))
}

/// Quanto tempo tentar voltar (o servidor segura o lugar por 25 s)
pub(crate) const RETURN_SECS: u64 = 22;

/// `K7M2QXAB` vira `JAM-K7M2-QXAB`
pub fn encode(room: &str) -> String {
    let (a, b) = room.split_at(room.len().min(ROOM_LEN / 2));
    format!("{PREFIX}-{a}-{b}")
}

/// Aceita minúscula, sem traço e com espaço: quem cola ou dita nem sempre copia certinho
pub fn parse(code: &str) -> Option<String> {
    let clean: String = code.chars().filter(|c| !c.is_whitespace() && *c != '-').collect::<String>().to_uppercase();
    let room = clean.strip_prefix(PREFIX)?;
    (room.len() == ROOM_LEN && room.bytes().all(|c| ALPHABET.contains(&c))).then(|| room.to_string())
}

/// Pra onde conectar a partir do código, se ele for da nuvem
pub fn target(code: &str) -> Option<Target> {
    parse(code).map(|room| Target::Signal { url: format!("{}/room/{room}", base()), room })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codigo_ida_e_volta() {
        assert_eq!(encode("K7M2QXAB"), "JAM-K7M2-QXAB");
        assert_eq!(parse("JAM-K7M2-QXAB").as_deref(), Some("K7M2QXAB"));
        assert_eq!(parse(" jam k7m2 qxab\n").as_deref(), Some("K7M2QXAB"));
    }

    #[test]
    fn codigo_invalido() {
        assert_eq!(parse("TL-043VN-E5HHB"), None);
        assert_eq!(parse("JAM-K7M2"), None);
        assert_eq!(parse("JAM-K7M2-QXA0"), None); // 0 não existe no alfabeto
    }
}
