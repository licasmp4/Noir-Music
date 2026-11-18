# Jam

Seus amigos ouvem **a mesma música que você, ao vivo**, sem precisar de Premium. Quem toca usa o
Spotify ou o Apple Music do próprio Mac; o Jam manda o som pra roda e todo mundo controla:
tocar/pausar, voltar, pular, adiantar, buscar e pôr na fila.

- **Som:** Opus estéreo 48 kHz a 256 kbps (transparente pro ouvido), cada pacote leva uma cópia do
  anterior (se um se perde na internet, o próximo traz ele de volta) e a folga cresce sozinha se a
  internet oscilar. O Jam pega o som **só do app de música**: aviso, Discord e o resto do computador
  ficam de fora. O volume do app é compensado (Spotify a 50 % não deixa os amigos ouvindo baixo).
- **Conexão:** a rede do [Noir](https://github.com/licasmp4/Noir-downloads): código `JAM-XXXX-XXXX`,
  direto entre os computadores e relay quando a internet não deixa.
- **Detecta sozinho** se a música está no Spotify ou no Apple Music.
- **Visual:** preto e cinza, capa em alta (2000 px) no palco e o player de vidro com a lente do
  Canopy 2 entortando a própria capa.

## Quem pode o quê

| | Mac | Windows |
|---|---|---|
| Ouvir e controlar | sim | sim |
| Tocar (abrir a Jam) | sim | ainda não |

| Busca | Como |
|---|---|
| Spotify | catálogo inteiro. Quem toca cria um app grátis em [developer.spotify.com](https://developer.spotify.com/dashboard) e cola o Client ID e o Secret nos ajustes do Jam |
| Apple Music | a biblioteca de quem toca (o catálogo inteiro exige a conta de desenvolvedor da Apple) |

## Baixar

- Windows: [Jam-Windows.zip](https://github.com/licasmp4/Jam-APP-SPO/releases/download/windows/Jam-Windows.zip), extraia e abra o `Jam.exe`.
- Mac: compile com `./mac/build-app.sh` (precisa de `brew install opus`). Na primeira vez o Mac pede
  permissão de **Gravação de Tela e Áudio do Sistema** (pro som) e de **Automação** (pra controlar o
  Spotify/Apple Music).

## Aviso

Retransmitir o som do Spotify ou do Apple Music vai contra os termos de uso deles. Use com o seu grupo
de amigos, por sua conta.

## Para quem mexe no código

```
app/src/
  room.rs      a Jam: sala, fila, pedidos dos amigos, quem toca
  audio.rs     Opus de música, cópia do pacote anterior, saída com folga
  capture.rs   som só do app de música (ScreenCaptureKit)
  player.rs    lê e controla Spotify/Apple Music (AppleScript, textos sempre como argumento)
  search.rs    busca do Spotify (API oficial) e capas do iTunes
  app.rs       as telas; glass_gl.rs = o vidro (mesmo do Noir)
net/           a rede do Noir (+ mensagem "jam" repassada pela sala)
```

Testes sem clicar: `JAM_DEMO=1` (Jam de mentira), `JAM_AUTO=host`, `JAM_AUTO=join:CODIGO`,
`JAM_VOLUME=0`, `JAM_SHOT=tela.png`. Registro em `~/Library/Application Support/Jam/log.txt`
(Windows: `%APPDATA%\Jam\log.txt`).
