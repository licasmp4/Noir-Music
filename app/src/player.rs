//! Quem toca: lê e controla o Spotify ou o Apple Music do Mac (AppleScript), o que estiver tocando.
//! Os textos dos amigos (busca, id da música) vão como argumento do script, nunca dentro dele.

use std::process::Command;

use crate::proto::Track;

#[derive(Clone, Debug, Default)]
pub struct Reading {
    pub source: String,
    pub track: Track,
    pub pos: f64,
    pub playing: bool,
    /// Volume do próprio app (0 a 100): o som capturado já vem com ele
    pub volume: f32,
}

/// Roda um AppleScript com argumentos (`on run argv`)
fn osa(script: &str, args: &[&str]) -> Result<String, String> {
    if !cfg!(target_os = "macos") {
        return Err("só no Mac".into());
    }
    let out = Command::new("/usr/bin/osascript").arg("-e").arg(script).args(args).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Os dois apps de uma vez, uma linha cada (só os que estão abertos; nunca abre nenhum).
/// Números em milissegundos inteiros: o AppleScript escreve decimal com vírgula no Mac em português.
const READ: &str = r#"
set out to ""
if application "Spotify" is running then
  try
    tell application "Spotify"
      set ps to player state as text
      if ps is not "stopped" then
        set t to current track
        set out to out & "spotify" & tab & ps & tab & (id of t) & tab & (name of t) & tab & (artist of t) & tab & (album of t) & tab & (duration of t) & tab & (round ((player position) * 1000)) & tab & (artwork url of t) & tab & (sound volume) & linefeed
      end if
    end tell
  end try
end if
if application "Music" is running then
  try
    tell application "Music"
      set ps to player state as text
      if ps is not "stopped" then
        set t to current track
        set out to out & "music" & tab & ps & tab & (persistent ID of t) & tab & (name of t) & tab & (artist of t) & tab & (album of t) & tab & (round ((duration of t) * 1000)) & tab & (round ((player position) * 1000)) & tab & "" & tab & (sound volume) & linefeed
      end if
    end tell
  end try
end if
return out
"#;

/// Tudo que está tocando ou pausado agora, nos dois apps
pub fn read() -> Vec<Reading> {
    let Ok(text) = osa(READ, &[]) else { return Vec::new() };
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 9 {
                return None;
            }
            let num = |s: &str| s.replace(',', ".").parse::<f64>().unwrap_or(0.0);
            Some(Reading {
                source: f[0].to_string(),
                playing: f[1] == "playing",
                track: Track { id: f[2].to_string(), title: f[3].to_string(), artist: f[4].to_string(), album: f[5].to_string(), art: f[8].to_string(), dur: num(f[6]) / 1000.0, by: String::new() },
                pos: num(f[7]) / 1000.0,
                volume: f.get(9).map(|v| num(v) as f32).unwrap_or(100.0),
            })
        })
        .collect()
}

/// Abertos agora (mesmo parados): pra saber de onde a busca vem quando nada toca
pub fn running() -> Vec<&'static str> {
    let s = osa(r#"set o to ""
if application "Spotify" is running then set o to o & "spotify "
if application "Music" is running then set o to o & "music"
return o"#, &[])
    .unwrap_or_default();
    let mut v = Vec::new();
    if s.contains("spotify") {
        v.push("spotify");
    }
    if s.contains("music") {
        v.push("music");
    }
    v
}

#[derive(Clone, Debug)]
pub enum Cmd {
    Toggle,
    Play,
    Pause,
    Next,
    Prev,
    Seek(f64),
    /// Tocar esta música agora
    Track(String),
}

pub fn send(source: &str, cmd: &Cmd) -> Result<(), String> {
    let app = match source {
        "spotify" => "Spotify",
        "music" => "Music",
        _ => return Err("nenhum app de música aberto".into()),
    };
    let r = match cmd {
        Cmd::Toggle => osa(&format!("tell application \"{app}\" to playpause"), &[]),
        Cmd::Play => osa(&format!("tell application \"{app}\" to play"), &[]),
        Cmd::Pause => osa(&format!("tell application \"{app}\" to pause"), &[]),
        Cmd::Next => osa(&format!("tell application \"{app}\" to next track"), &[]),
        Cmd::Prev => osa(&format!("tell application \"{app}\" to previous track"), &[]),
        Cmd::Seek(s) => osa(&format!("on run argv\ntell application \"{app}\" to set player position to (item 1 of argv as number)\nend run"), &[&format!("{}", s.max(0.0).round() as i64)]),
        Cmd::Track(id) if source == "spotify" => {
            if !id.starts_with("spotify:track:") {
                return Err("música inválida".into());
            }
            osa("on run argv\ntell application \"Spotify\" to play track (item 1 of argv)\nend run", &[id])
        }
        Cmd::Track(id) => osa(
            "on run argv\ntell application \"Music\"\nset t to (first track of library playlist 1 whose persistent ID is (item 1 of argv))\nplay t\nend tell\nend run",
            &[id],
        ),
    };
    r.map(|_| ()).map_err(|e| {
        crate::log::log(format!("controle {app} {cmd:?}: {e}"));
        e
    })
}

/// Busca na biblioteca do Apple Music (o catálogo inteiro precisa da conta de desenvolvedor da Apple)
pub fn search_music(q: &str) -> Vec<Track> {
    const S: &str = r#"on run argv
set out to ""
tell application "Music"
  set r to search library playlist 1 for (item 1 of argv)
  set n to 0
  repeat with t in r
    try
      if (media kind of t as text) is "song" or (media kind of t as text) is "música" then
        set out to out & (persistent ID of t) & tab & (name of t) & tab & (artist of t) & tab & (album of t) & tab & (round ((duration of t) * 1000)) & linefeed
        set n to n + 1
      end if
    end try
    if n ≥ 25 then exit repeat
  end repeat
end tell
return out
end run"#;
    let Ok(text) = osa(S, &[q]) else { return Vec::new() };
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (f.len() >= 5).then(|| Track {
                id: f[0].into(),
                title: f[1].into(),
                artist: f[2].into(),
                album: f[3].into(),
                dur: f[4].replace(',', ".").parse::<f64>().unwrap_or(0.0) / 1000.0,
                ..Default::default()
            })
        })
        .collect()
}
