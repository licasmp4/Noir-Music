# Juntar a Jam no Noir (aba "Noir Música")

A Jam foi feita com as mesmas peças do Noir: `net/` é cópia da `Telao/net`, e `glass_gl.rs` e o tema
chapado também vêm de lá. Por isso juntar é **trazer os arquivos da Jam pra uma pasta própria
dentro do Noir** e ligar uma aba nova. Não é reescrever nada.

## 1. A sala (net)

O Noir já tem a `net`. Só falta o tipo de mensagem `"jam"` no `Telao/net/src/sfu.rs`, dentro do
`match` de `member_message`, ao lado do `"chat"`. Copie o braço `"jam" => { ... }` de
`Jam/net/src/sfu.rs`. Ele só repassa os pedidos, com limite de 300 KB, e o `to` manda pra uma
pessoa só.

Depois disso, publique o worker/servidor como de costume. O som da Jam vai na faixa **Voice**, que
o Noir já entrega pra todo mundo sem precisar "assistir".

## 2. Os arquivos (pasta própria, sem bater nome)

O Noir já tem `eq.rs`, `lyrics.rs`, `capture.rs` e `prefs.rs` com outras funções. Então tudo da Jam
vai pra `Telao/windows/src/musica/`:

| Da Jam (`app/src/`) | Vai pra (`windows/src/musica/`) | Observação |
|---|---|---|
| `room.rs` | `room.rs` | troque `jam_net::` por `crate::net::` (ou o nome da crate de rede do Noir) |
| `audio.rs`, `eq.rs` | `audio.rs`, `eq.rs` | o eq do Noir é de outra coisa: mantenha os dois |
| `capture.rs` | `captura.rs` | captura só o app de música (ScreenCaptureKit), Mac |
| `player.rs`, `search.rs`, `proto.rs` | iguais | AppleScript do Spotify/Music, busca, mensagens |
| `lyrics.rs`, `lyric_view.rs` | `letra.rs`, `letra_view.rs` | letra do LRCLIB e a tela de letra |
| `arts.rs` | `capas.rs` | capas e o fundo borrado |
| `app.rs` (a tela `jam`, `full_view`, `side`, `equalizer`) | `tela.rs` | vira `fn musica(&mut self, ui, rect)` |
| `prefs.rs` | **não copia** | junte os campos `spotify_id`, `spotify_secret`, `eq_*` no `Prefs` do Noir |
| `theme.rs`, `glass_gl.rs`, `log.rs` | **não copia** | use os do Noir (são a mesma origem) |

Crie `musica/mod.rs` com `pub mod room; pub mod audio; ...` e coloque `mod musica;` no `main.rs`
do Noir. Nos arquivos copiados, troque `crate::xxx` por `crate::musica::xxx`, menos nos de tema,
vidro e log, que continuam apontando pro Noir.

Dependências que o Noir talvez ainda não tenha no `Cargo.toml`: `audiopus` (Opus), `ureq` 2 e
`base64`. O `screencapturekit` e o `cpal` ele já usa. Compile com as mesmas variáveis da Jam:
`LIBOPUS_LIB_DIR=/opt/homebrew/opt/opus/lib LIBOPUS_STATIC=1`.

## 3. A aba

Desde 13/11 o dock tem só **Transmitir** e **Assistir**. Entra uma terceira, **Música**, que chama
`self.musica(ui, rect)`:

- Sem sala: o início da Jam ("Tocar pra galera" / "Entrar na Jam de um amigo").
- Com sala: a tela da Jam (capa com o vidro, fila, busca, letra, som).
- Tela cheia (F/Esc): usa a mesma janela do Noir.

O código da sala da Jam começa com `JAM-`. O Noir pode aceitar os dois no mesmo campo de
"Entrar".

Regras do Noir que valem aqui também:
- **Não mexer na ilha.**
- O fundo colorido da capa **só na aba Música**, porque foi pedido pra ela. O resto do Noir segue
  preto e cinza.

## 4. O que já foi testado (18/11, no Mac)

- O som chega inteiro: 5,0 s a cada 5 s, sem cortes. A folga agora começa em 600 ms, porque com
  250 ms quem entrava cortava 3 ou 4 vezes nos primeiros segundos. Com uma 3ª pessoa entrando no
  meio, deu 0 cortes.
- Controles testados no Apple Music com volume 0: tocar, pausar, pular, voltar, pular pra um ponto
  da música, fila (pôr, subir, tirar), tocar agora e passar pra próxima sozinho no fim.
- Equalizador: 4 testes medindo o som (`cargo test`).
- Letra: o parser tem testes e a busca no LRCLIB foi conferida. A tela de letra foi vista na
  prévia, não numa Jam de verdade.

## 5. Limites

- **Só o Mac toca pra galera.** O Windows só ouve, porque a captura do som de um app só existe no
  Mac.
- **Busca do Apple Music:** só na biblioteca. Tocar do catálogo precisa da conta paga de
  desenvolvedor da Apple.
- **Busca do Spotify:** precisa do Client ID e do Secret de um app grátis em developer.spotify.com,
  colados nos Ajustes de quem toca.
- **Letra:** só aparece se a música estiver no LRCLIB.
- **Cursor na tela cheia:** esconder o cursor (`CursorIcon::None`) travava o desenho no Mac. Não
  use.
