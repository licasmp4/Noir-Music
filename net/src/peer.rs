//! Partes comuns de uma conexão WebRTC (str0m), dos dois lados.

use std::time::{Duration, Instant};

use str0m::bwe::Bitrate;
use str0m::format::Codec;
use str0m::media::{Mid, Pt};
use str0m::{Event, Output, Rtc, RtcConfig};

use crate::transport::Transport;

/// Com estimativa de banda ligada o str0m espaça os pacotes (pacer) em vez de mandar a imagem-chave
/// inteira (centenas de KB) de uma vez: pela internet o roteador descartava o pedaço e a imagem nunca
/// fechava (tela preta esperando). O ritmo nunca fica abaixo de 25 Mbps (vendor/str0m/src/pacer/control.rs):
/// a estimativa do str0m caía pela internet e travava a imagem.
pub const PACE_START_BPS: u64 = 20_000_000;
pub const PACE_DESIRED_BPS: u64 = 40_000_000;

pub fn rtc_config(bwe_initial: Option<u64>) -> RtcConfig {
    let mut c = Rtc::builder()
        .clear_codecs()
        .enable_h265(true)
        .enable_opus(true, true) // RED: cada pedaço de som vai 2x, pacote perdido não falha o som
        .set_stats_interval(Some(Duration::from_millis(200)))
        // Keyframe de 4K passa de 1000 pacotes; tem que caber inteiro pra reordenar
        .set_reordering_size_video(3000)
        // Espera no máximo isso por um reenvio; depois descarta e pede keyframe (não acumula lag)
        .set_reordering_timeout_video(Some(Duration::from_millis(200)))
        .set_reordering_timeout_audio(Some(Duration::from_millis(60)))
        // Histórico pra reenviar pacote perdido: ~0,5 s mesmo em 4K120
        .set_send_buffer_video(4000)
        // Relatório de perda/ping mais frequente = controle de banda reage antes de virar lag
        .set_rtcp_report_interval_video(Duration::from_millis(200));
    // Pacer DESLIGADO (como no protótipo). Medido em 05/11 com duas cópias e tela de mentira:
    // com ele 22-48 fps e buracos de 4 s; sem ele 60 fps cravados, inclusive com internet simulada
    // (50 Mbps/20 ms, 15 Mbps apertado, 1% de perda). TELAO_PACER=1 liga de novo pra testar.
    // Queda percebida em ~9 s em vez de ~20 (padrão: até 9 tentativas a cada 3 s). Com 8 tentativas
    // seguidas sem resposta, de 250 ms até 1,5 s, é internet caída mesmo, não só instável; aí o app
    // já tenta voltar pra sala (16/11)
    c.set_max_stun_rto(Duration::from_millis(1500));
    c.set_max_stun_retransmits(8);
    let bwe_initial = if std::env::var_os("TELAO_PACER").is_some() { bwe_initial } else { None };
    if let Some(b) = bwe_initial {
        c = c.enable_bwe(Some(Bitrate::bps(b)));
    }
    c
}

/// Esvazia a saída do str0m (manda os pacotes) e devolve os eventos + próximo prazo.
/// Regra do str0m: toda mudança no Rtc precisa disso logo em seguida.
pub fn drain(rtc: &mut Rtc, tr: &Transport, events: &mut Vec<Event>) -> Instant {
    loop {
        match rtc.poll_output() {
            Ok(Output::Timeout(t)) => return t,
            Ok(Output::Transmit(t)) => tr.send(&t),
            Ok(Output::Event(e)) => events.push(e),
            Err(_) => {
                rtc.disconnect();
                return Instant::now() + Duration::from_secs(1);
            }
        }
    }
}

pub fn payload_type(rtc: &mut Rtc, mid: Mid, codec: Codec) -> Option<Pt> {
    let w = rtc.writer(mid)?;
    w.payload_params().find(|p| p.spec().codec == codec).map(|p| p.pt())
}
