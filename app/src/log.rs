//! Registro em arquivo (pra entender o que aconteceu quando um amigo diz que não funcionou).
//! Mac: ~/Library/Application Support/Noir Music/log.txt · Windows: %APPDATA%\Noir Music\log.txt

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

static FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

pub fn dir() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
    } else {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support")).unwrap_or_else(std::env::temp_dir)
    };
    let d = base.join("Noir Music");
    if !d.exists() {
        // antes chamava Jam: traz as preferências (nome, Spotify, equalizador)
        let old = base.join("Jam").join("prefs.json");
        let _ = std::fs::create_dir_all(&d);
        if old.exists() {
            let _ = std::fs::copy(old, d.join("prefs.json"));
        }
    }
    d
}

pub fn path() -> PathBuf {
    dir().join("log.txt")
}

pub fn log(msg: impl AsRef<str>) {
    let msg = msg.as_ref();
    eprintln!("{msg}");
    let mut f = FILE.lock().unwrap_or_else(|e| e.into_inner());
    if f.is_none() {
        // começa de novo quando passa de 2 MB
        let p = path();
        if std::fs::metadata(&p).map(|m| m.len() > 2_000_000).unwrap_or(false) {
            let _ = std::fs::remove_file(&p);
        }
        *f = std::fs::OpenOptions::new().create(true).append(true).open(p).ok();
    }
    if let Some(f) = f.as_mut() {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = writeln!(f, "{t} {msg}");
    }
}
