//! Letras sincronizadas pelo LRCLIB (lrclib.net: grátis, sem chave). Cada um busca a sua (quem
//! ouve também), então não passa nada pela sala. Guarda o que já achou.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value;

#[derive(Clone, Debug, Default)]
pub struct Line {
    /// Começo e fim em segundos (o fim é o começo da próxima)
    pub start: f32,
    pub end: f32,
    pub text: String,
    /// Palavra por palavra (LRC "enhanced": <mm:ss.xx>), quando a letra tiver
    pub words: Vec<(f32, String)>,
}

#[derive(Clone, Debug, Default)]
pub struct Lyrics {
    pub lines: Vec<Line>,
    /// false = só o texto, sem tempo (mostra parada, sem seguir a música)
    pub synced: bool,
    pub instrumental: bool,
}

#[derive(Clone)]
pub enum State {
    Loading,
    Ready(std::sync::Arc<Lyrics>),
    None,
}

static CACHE: Mutex<Option<HashMap<String, State>>> = Mutex::new(None);

fn key(title: &str, artist: &str) -> String {
    format!("{title}\u{1}{artist}").to_lowercase()
}

/// A letra dessa música: devolve na hora o que tem e, se nunca buscou, busca numa thread e chama
/// `done` quando chegar
pub fn get(title: &str, artist: &str, album: &str, dur: f32, done: impl Fn() + Send + 'static) -> State {
    if title.trim().is_empty() {
        return State::None;
    }
    let k = key(title, artist);
    {
        let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        let map = c.get_or_insert_with(HashMap::new);
        if let Some(s) = map.get(&k) {
            return s.clone();
        }
        map.insert(k.clone(), State::Loading);
    }
    let (t, a, al) = (title.to_string(), artist.to_string(), album.to_string());
    std::thread::spawn(move || {
        let found = fetch(&t, &a, &al, dur);
        crate::log::log(format!("letra {}: {t} — {a}", match &found {
            Some(l) if l.synced => "sincronizada",
            Some(_) => "sem tempo",
            None => "não achou",
        }));
        let st = found.map_or(State::None, |l| State::Ready(std::sync::Arc::new(l)));
        CACHE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(k, st);
        done();
    });
    State::Loading
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(Duration::from_secs(10)).user_agent(concat!("NoirMusic/", env!("CARGO_PKG_VERSION"))).build()
}

/// "(feat. X)", "- Remastered 2011", "[Explicit]" atrapalham achar
fn clean(s: &str) -> String {
    let mut s = s.split(['(', '[']).next().unwrap_or(s).to_string();
    for sep in [" - ", " – "] {
        if let Some(i) = s.find(sep) {
            let tail = s[i..].to_lowercase();
            if ["remaster", "live", "version", "mix", "edit", "mono", "stereo", "ao vivo"].iter().any(|w| tail.contains(w)) {
                s.truncate(i);
            }
        }
    }
    s.trim().to_string()
}

fn fetch(title: &str, artist: &str, album: &str, dur: f32) -> Option<Lyrics> {
    let first_artist = artist.split([',', '&']).next().unwrap_or(artist).trim();
    // 1) exato (título, artista, álbum e duração ±2 s)
    if dur > 0.0 {
        let r = agent()
            .get("https://lrclib.net/api/get")
            .query("track_name", title)
            .query("artist_name", first_artist)
            .query("album_name", album)
            .query("duration", &format!("{}", dur.round() as i64))
            .call();
        if let Some(v) = r.ok().and_then(|r| r.into_json::<Value>().ok()) {
            if let Some(l) = from_json(&v) {
                return Some(l);
            }
        }
    }
    // 2) busca: escolhe a de duração mais perto, preferindo a sincronizada
    let v: Value = agent()
        .get("https://lrclib.net/api/search")
        .query("track_name", &clean(title))
        .query("artist_name", first_artist)
        .call()
        .ok()?
        .into_json()
        .ok()?;
    let mut best: Option<(f32, Lyrics)> = None;
    for item in v.as_array()?.iter().take(20) {
        let d = item["duration"].as_f64().unwrap_or(0.0) as f32;
        let diff = if dur > 0.0 && d > 0.0 { (d - dur).abs() } else { 0.0 };
        if diff > 4.0 {
            continue;
        }
        if let Some(l) = from_json(item) {
            let score = diff + if l.synced { 0.0 } else { 100.0 };
            if best.as_ref().is_none_or(|(s, _)| score < *s) {
                best = Some((score, l));
            }
        }
    }
    best.map(|(_, l)| l)
}

fn from_json(v: &Value) -> Option<Lyrics> {
    if v["instrumental"].as_bool() == Some(true) {
        return Some(Lyrics { lines: Vec::new(), synced: false, instrumental: true });
    }
    if let Some(lrc) = v["syncedLyrics"].as_str().filter(|s| !s.trim().is_empty()) {
        let l = parse_lrc(lrc);
        if !l.lines.is_empty() {
            return Some(l);
        }
    }
    let plain = v["plainLyrics"].as_str().filter(|s| !s.trim().is_empty())?;
    Some(Lyrics { lines: plain.lines().map(|t| Line { text: t.trim().to_string(), ..Default::default() }).collect(), synced: false, instrumental: false })
}

/// "[01:23.45]" ou "<01:23.45>" → segundos
fn stamp(s: &str) -> Option<f32> {
    let (m, rest) = s.split_once(':')?;
    let m: f32 = m.trim().parse().ok()?;
    let sec: f32 = rest.trim().replace(',', ".").parse().ok()?;
    Some(m * 60.0 + sec)
}

pub fn parse_lrc(lrc: &str) -> Lyrics {
    let mut offset = 0.0f32;
    let mut lines: Vec<Line> = Vec::new();
    for raw in lrc.lines() {
        let mut rest = raw.trim();
        let mut times = Vec::new();
        while let Some(r) = rest.strip_prefix('[') {
            let Some(end) = r.find(']') else { break };
            let tag = &r[..end];
            if let Some(o) = tag.strip_prefix("offset:") {
                offset = o.trim().parse::<f32>().unwrap_or(0.0) / 1000.0;
            } else if let Some(t) = stamp(tag) {
                times.push(t);
            }
            rest = &r[end + 1..];
        }
        if times.is_empty() {
            continue;
        }
        // palavras: "<00:01.00> Oi <00:01.50> tudo"
        let mut words = Vec::new();
        let mut text = String::new();
        if rest.contains('<') {
            for part in rest.split('<').filter(|p| !p.is_empty()) {
                if let Some((t, w)) = part.split_once('>') {
                    if let Some(t) = stamp(t) {
                        if !w.trim().is_empty() {
                            words.push((t, w.to_string()));
                        }
                        text.push_str(w);
                        continue;
                    }
                }
                text.push_str(part);
            }
        } else {
            text = rest.to_string();
        }
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        for t in times {
            lines.push(Line { start: (t - offset).max(0.0), end: 0.0, text: text.clone(), words: words.clone() });
        }
    }
    lines.sort_by(|a, b| a.start.total_cmp(&b.start));
    // linha vazia no meio = pausa instrumental (vira os três pontinhos); várias vazias seguidas viram uma
    lines.dedup_by(|b, a| a.text.is_empty() && b.text.is_empty());
    for i in 0..lines.len() {
        lines[i].end = lines.get(i + 1).map_or(lines[i].start + 6.0, |n| n.start);
    }
    // introdução longa sem letra: os três pontinhos até a primeira linha
    if lines.first().is_some_and(|l| l.start > 4.0 && !l.text.is_empty()) {
        let first = lines[0].start;
        lines.insert(0, Line { start: 0.0, end: first, text: String::new(), words: Vec::new() });
    }
    // vazia no fim não serve pra nada
    while lines.last().is_some_and(|l| l.text.is_empty()) {
        lines.pop();
    }
    Lyrics { synced: !lines.is_empty(), lines, instrumental: false }
}

/// A linha que está tocando agora (ou a última que passou)
pub fn current(l: &Lyrics, pos: f32) -> Option<usize> {
    if !l.synced {
        return None;
    }
    let i = l.lines.partition_point(|x| x.start <= pos);
    i.checked_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_lrc() {
        let l = parse_lrc("[ar:X]\n[00:01.00]Oi\n[00:03.50]\n[00:05.00][00:20.00]Refrão\n[00:08.00]<00:08.00>uma <00:08.40>palavra");
        let t: Vec<_> = l.lines.iter().map(|x| (x.start, x.text.as_str())).collect();
        assert_eq!(t, vec![(1.0, "Oi"), (3.5, ""), (5.0, "Refrão"), (8.0, "uma palavra"), (20.0, "Refrão")]);
        assert_eq!(l.lines[3].words.len(), 2);
        assert_eq!(current(&l, 6.0), Some(2));
        assert_eq!(current(&l, 0.5), None);
    }

    #[test]
    fn limpa_titulo() {
        assert_eq!(clean("Song (feat. Y) - Remastered 2011"), "Song");
        assert_eq!(clean("Love - Live"), "Love");
        assert_eq!(clean("Day - Night"), "Day - Night");
    }
}
