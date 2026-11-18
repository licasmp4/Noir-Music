//! Cliente do servidor de salas (WebSocket com mensagens JSON).

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

pub struct Signal {
    /// Credencial temporária do relay TURN (só o servidor da nuvem manda)
    pub turn: Option<crate::turn::Creds>,
    tx: mpsc::UnboundedSender<Value>,
    pub rx: mpsc::UnboundedReceiver<Value>,
}

impl Signal {
    pub async fn connect(url: &str) -> Result<Self, String> {
        let (ws, resp) = tokio::time::timeout(Duration::from_secs(6), tokio_tungstenite::connect_async(url))
            .await
            .map_err(|_| "servidor de salas não respondeu".to_string())?
            .map_err(|e| format!("não deu pra conectar no servidor de salas: {e}"))?;
        let turn = resp
            .headers()
            .get("x-noir-turn")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .and_then(|v| crate::turn::Creds::from_json(&v));
        let (mut sink, mut source) = ws.split();
        let (tx, mut out) = mpsc::unbounded_channel::<Value>();
        let (inc, rx) = mpsc::unbounded_channel::<Value>();

        tokio::spawn(async move {
            // Ping regular: proxy/roteador derruba WebSocket parado
            let mut ping = tokio::time::interval(Duration::from_secs(20));
            loop {
                tokio::select! {
                    m = out.recv() => match m {
                        Some(v) => if sink.send(Message::text(v.to_string())).await.is_err() { break },
                        None => break,
                    },
                    _ = ping.tick() => if sink.send(Message::text(json!({"t": "ping"}).to_string())).await.is_err() { break },
                }
            }
        });
        tokio::spawn(async move {
            while let Some(Ok(msg)) = source.next().await {
                if let Message::Text(t) = msg {
                    if let Ok(v) = serde_json::from_str::<Value>(&t) {
                        if inc.send(v).is_err() {
                            return;
                        }
                    }
                }
            }
            let _ = inc.send(json!({"t": "closed"}));
        });
        Ok(Signal { turn, tx, rx })
    }

    /// Tenta vários endereços ao mesmo tempo; fica com o primeiro que responder.
    pub async fn connect_any(urls: Vec<String>) -> Result<Self, String> {
        if urls.is_empty() {
            return Err("código sem endereço".into());
        }
        let attempts = urls.iter().map(|u| Box::pin(Self::connect(u)));
        futures_util::future::select_ok(attempts)
            .await
            .map(|(s, _)| s)
            .map_err(|_| "não deu pra chegar em quem transmite (a porta dele pode estar fechada)".to_string())
    }

    pub fn send(&self, v: Value) {
        let _ = self.tx.send(v);
    }

    /// Um sinal parado (nada chega, o que sai some): fica no lugar do que caiu enquanto volta
    pub fn idle() -> Self {
        let (tx, _) = mpsc::unbounded_channel::<Value>();
        let (keep, rx) = mpsc::unbounded_channel::<Value>();
        // sem quem mande, o `rx` nunca acaba (senão o laço de quem usa sairia na hora)
        std::mem::forget(keep);
        Signal { turn: None, tx, rx }
    }

    /// Tenta voltar pro servidor de salas por até `secs` segundos (de 2 em 2), em outra tarefa.
    /// O resultado chega pelo canal que volta
    pub fn retry(url: String, secs: u64) -> mpsc::UnboundedReceiver<Result<Signal, String>> {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let end = tokio::time::Instant::now() + Duration::from_secs(secs);
            let mut last = String::new();
            while tokio::time::Instant::now() < end {
                match tokio::time::timeout(Duration::from_secs(4), Signal::connect(&url)).await {
                    Ok(Ok(s)) => {
                        let _ = tx.send(Ok(s));
                        return;
                    }
                    Ok(Err(e)) => last = e,
                    Err(_) => last = "sem resposta".into(),
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            let _ = tx.send(Err(last));
        });
        rx
    }
}
