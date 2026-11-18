//! O som só do app de música (Spotify ou Apple Music), sem o resto do computador: nem Discord,
//! nem aviso, nem o próprio Jam. No Mac pelo ScreenCaptureKit, que captura o som de um app só.

pub type PcmSink = Box<dyn FnMut(&[f32]) + Send>;

#[cfg(target_os = "macos")]
mod mac {
    use std::sync::Mutex;

    use screencapturekit::cm::CMSampleBufferExt;
    use screencapturekit::prelude::*;

    use super::PcmSink;

    struct AudioOut {
        sink: Mutex<(PcmSink, Vec<f32>)>,
    }

    impl SCStreamOutputTrait for AudioOut {
        fn did_output_sample_buffer(&self, sample: CMSampleBuffer, of_type: SCStreamOutputType) {
            if !matches!(of_type, SCStreamOutputType::Audio) {
                return;
            }
            let Ok(list) = sample.audio_buffer_list() else { return };
            let Ok(mut guard) = self.sink.lock() else { return };
            let (sink, buf) = &mut *guard;
            buf.clear();
            let as_f32 = |b: &[u8]| -> Vec<f32> { b.chunks_exact(4).map(|c| f32::from_ne_bytes([c[0], c[1], c[2], c[3]])).collect() };
            // um buffer por canal (planar) ou já intercalado
            if list.num_buffers() >= 2 {
                let (Some(l), Some(r)) = (list.get(0), list.get(1)) else { return };
                let (l, r) = (as_f32(l.data()), as_f32(r.data()));
                for i in 0..l.len().min(r.len()) {
                    buf.push(l[i]);
                    buf.push(r[i]);
                }
            } else if let Some(b) = list.get(0) {
                let s = as_f32(b.data());
                if b.number_channels >= 2 {
                    buf.extend_from_slice(&s);
                } else {
                    for v in s {
                        buf.push(v);
                        buf.push(v);
                    }
                }
            }
            sink(buf);
        }
    }

    struct Watch;
    impl SCStreamDelegateTrait for Watch {
        fn did_stop_with_error(&self, error: SCError) {
            crate::log::log(format!("captura do som parou: {error}"));
        }
    }

    pub struct Capture {
        stream: SCStream,
        pub bundle: String,
    }

    impl Capture {
        pub fn start(bundle: &str, sink: PcmSink) -> Result<Capture, String> {
            let content = SCShareableContent::create().get().map_err(|e| hint(&e.to_string()))?;
            let displays = content.displays();
            let display = displays.first().ok_or("nenhuma tela")?;
            let apps = content.applications();
            let app = apps.iter().find(|a| a.bundle_identifier() == bundle).ok_or_else(|| format!("{bundle} não está aberto"))?;
            let filter = SCContentFilter::create().with_display(display).with_including_applications(&[app], &[]).build().map_err(|e| e.to_string())?;
            // a imagem é obrigatória pro ScreenCaptureKit: a menor possível, 1 por segundo, e ninguém usa
            let config = SCStreamConfiguration::new()
                .with_width(2)
                .with_height(2)
                .with_minimum_frame_interval(&CMTime::new(1, 1))
                .with_captures_audio(true)
                .with_excludes_current_process_audio(true)
                .with_sample_rate(48_000)
                .with_channel_count(2);
            let mut stream = SCStream::new_with_delegate(&filter, &config, Watch).map_err(|e| e.to_string())?;
            stream
                .add_output_handler(AudioOut { sink: Mutex::new((sink, Vec::with_capacity(4096))) }, SCStreamOutputType::Audio)
                .map_err(|e| e.to_string())?;
            stream.start_capture().map_err(|e| hint(&e.to_string()))?;
            crate::log::log(format!("capturando o som de {bundle}"));
            Ok(Capture { stream, bundle: bundle.to_string() })
        }
    }

    impl Drop for Capture {
        fn drop(&mut self) {
            let _ = self.stream.stop_capture();
        }
    }

    fn hint(e: &str) -> String {
        let low = e.to_lowercase();
        if low.contains("declined") || low.contains("permission") || low.contains("tcc") || low.contains("-3801") {
            "O Mac não deixou pegar o som. Abra Ajustes do Sistema › Privacidade e Segurança › Gravação de Tela e Áudio do Sistema, ligue o Jam e abra ele de novo.".into()
        } else {
            format!("não deu pra pegar o som: {e}")
        }
    }
}

#[cfg(target_os = "macos")]
pub use mac::Capture;

/// No Windows quem toca ainda não existe: por enquanto o Windows só ouve (próxima etapa: WASAPI
/// pegando o som só do processo do Spotify)
#[cfg(not(target_os = "macos"))]
pub struct Capture {
    pub bundle: String,
}

#[cfg(not(target_os = "macos"))]
impl Capture {
    pub fn start(_bundle: &str, _sink: PcmSink) -> Result<Capture, String> {
        Err("Por enquanto só dá pra tocar a Jam pelo Mac. No Windows dá pra entrar e ouvir.".into())
    }
}

/// Pode tocar a Jam daqui?
pub const CAN_HOST: bool = cfg!(target_os = "macos");
