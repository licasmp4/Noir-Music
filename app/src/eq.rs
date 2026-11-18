//! Equalizador de quem ouve (cada um mexe só no seu): 10 faixas com filtros clássicos (biquad,
//! "Audio EQ Cookbook"), ganho de entrada que abre espaço quando alguma faixa sobe e um limitador
//! macio no fim, pra nunca estourar. Troca de modo sem estalo (o ganho anda aos poucos).
//! Também guarda os últimos 2048 valores que tocaram, pro desenho das frequências.

/// Centro de cada faixa (Hz). A primeira e a última são prateleiras (grave e brilho)
pub const BANDS: [f32; 10] = [32.0, 64.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0];
pub const LABELS: [&str; 10] = ["32", "64", "125", "250", "500", "1k", "2k", "4k", "8k", "16k"];
pub const N: usize = 10;
pub const MAX_DB: f32 = 12.0;

pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    pub gains: [f32; N],
}

const fn p(id: &'static str, name: &'static str, gains: [f32; N]) -> Preset {
    Preset { id, name, gains }
}

pub const PRESETS: &[Preset] = &[
    p("", "Original", [0.0; N]),
    p("grave", "Grave forte", [6.0, 6.0, 4.5, 2.0, 0.0, -0.5, 0.0, 0.5, 1.0, 1.0]),
    p("funk", "Funk e trap", [7.0, 6.5, 4.0, 0.5, -1.5, -1.0, 0.5, 1.5, 2.0, 2.0]),
    p("pop", "Pop", [1.0, 2.0, 2.5, 1.0, -0.5, -0.5, 0.5, 1.5, 2.5, 3.0]),
    p("voz", "Voz na frente", [-2.0, -2.0, -1.0, 0.0, 1.5, 2.5, 3.0, 2.5, 1.0, 0.0]),
    p("eletronica", "Eletrônica", [5.0, 4.5, 2.0, 0.0, -1.5, -1.0, 0.5, 2.0, 4.0, 4.5]),
    p("rock", "Rock", [4.0, 3.0, 1.5, -0.5, -1.5, -0.5, 1.5, 3.0, 3.5, 3.5]),
    p("acustica", "Acústica", [1.5, 1.5, 2.0, 1.5, 0.5, 0.5, 1.5, 2.0, 1.5, 1.0]),
    p("classica", "Clássica", [2.0, 1.5, 0.5, 0.0, 0.0, 0.0, -0.5, 0.5, 2.0, 2.5]),
    p("brilho", "Mais brilho", [0.0, 0.0, 0.0, 0.0, 0.0, 0.5, 1.5, 3.0, 4.5, 5.5]),
    p("noite", "Noite", [-4.0, -3.0, -1.5, 0.0, 1.0, 1.5, 1.5, 0.5, -1.0, -2.0]),
];

pub fn find(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

#[derive(Clone, Copy, Default)]
struct Coef {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Coef {
    /// kind: 0 prateleira de grave, 1 pico, 2 prateleira de agudo
    fn new(kind: u8, f: f32, db: f32, q: f32, sr: f32) -> Coef {
        let f = f.min(sr * 0.45);
        let a = 10f32.powf(db / 40.0);
        let w = std::f32::consts::TAU * f / sr;
        let (s, c) = w.sin_cos();
        let (b0, b1, b2, a0, a1, a2);
        match kind {
            1 => {
                let al = s / (2.0 * q);
                b0 = 1.0 + al * a;
                b1 = -2.0 * c;
                b2 = 1.0 - al * a;
                a0 = 1.0 + al / a;
                a1 = -2.0 * c;
                a2 = 1.0 - al / a;
            }
            _ => {
                let al = s / 2.0 * std::f32::consts::SQRT_2;
                let sq = 2.0 * a.sqrt() * al;
                let sign = if kind == 0 { 1.0 } else { -1.0 };
                b0 = a * ((a + 1.0) - sign * (a - 1.0) * c + sq);
                b1 = sign * 2.0 * a * ((a - 1.0) - sign * (a + 1.0) * c);
                b2 = a * ((a + 1.0) - sign * (a - 1.0) * c - sq);
                a0 = (a + 1.0) + sign * (a - 1.0) * c + sq;
                a1 = -sign * 2.0 * ((a - 1.0) + sign * (a + 1.0) * c);
                a2 = (a + 1.0) + sign * (a - 1.0) * c - sq;
            }
        }
        Coef { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
    }

    fn mag(&self, f: f32, sr: f32) -> f32 {
        let w = std::f32::consts::TAU * f / sr;
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = -(self.b1 * s1 + self.b2 * s2);
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = -(self.a1 * s1 + self.a2 * s2);
        ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt()
    }
}

fn coefs(gains: &[f32; N], sr: f32) -> [Coef; N] {
    std::array::from_fn(|i| {
        let kind = match i {
            0 => 0,
            9 => 2,
            _ => 1,
        };
        // faixas a uma oitava: Q ~1,4 cobre o espaço entre elas sem buraco nem morro
        Coef::new(kind, BANDS[i], gains[i], 1.41, sr)
    })
}

/// Abre espaço pela maior faixa que sobe (60 % dela): o volume quase não muda e não estoura
fn preamp(gains: &[f32; N]) -> f32 {
    let top = gains.iter().copied().fold(0.0f32, f32::max);
    10f32.powf(-top * 0.6 / 20.0)
}

/// A curva em dB de 20 Hz a 20 kHz (`n` pontos), pro desenho
pub fn curve(gains: &[f32; N], n: usize) -> Vec<f32> {
    let sr = 48_000.0;
    let c = coefs(gains, sr);
    (0..n).map(|i| 20.0 * c.iter().map(|k| k.mag(freq_at(i as f32 / (n - 1).max(1) as f32), sr)).product::<f32>().max(1e-6).log10()).collect()
}

/// 0..1 → frequência (escala de oitavas, 20 Hz a 20 kHz)
pub fn freq_at(t: f32) -> f32 {
    20.0 * 1000f32.powf(t)
}

/// frequência → 0..1
pub fn pos_of(f: f32) -> f32 {
    (f / 20.0).log(1000.0)
}

pub struct Eq {
    sr: f32,
    gains: [f32; N],
    target: [f32; N],
    on: bool,
    coef: [Coef; N],
    pre: f32,
    z: [[(f32, f32, f32, f32); 2]; N],
}

impl Eq {
    pub fn new(sr: f32) -> Eq {
        Eq { sr, gains: [0.0; N], target: [0.0; N], on: true, coef: coefs(&[0.0; N], sr), pre: 1.0, z: [[(0.0, 0.0, 0.0, 0.0); 2]; N] }
    }

    pub fn set(&mut self, gains: [f32; N], on: bool) {
        self.target = if on { gains } else { [0.0; N] };
        self.on = on;
    }

    /// Estéreo intercalado, no lugar
    pub fn process(&mut self, buf: &mut [f32]) {
        if self.gains.iter().chain(self.target.iter()).all(|g| *g == 0.0) {
            return;
        }
        if self.gains != self.target {
            for (g, t) in self.gains.iter_mut().zip(self.target) {
                *g += (t - *g).clamp(-0.75, 0.75);
            }
            self.coef = coefs(&self.gains, self.sr);
            self.pre = preamp(&self.gains);
        }
        for frame in buf.chunks_exact_mut(2) {
            for (ch, v) in frame.iter_mut().enumerate() {
                let mut x = *v * self.pre;
                for (b, c) in self.coef.iter().enumerate() {
                    let z = &mut self.z[b][ch];
                    let y = c.b0 * x + c.b1 * z.0 + c.b2 * z.1 - c.a1 * z.2 - c.a2 * z.3;
                    *z = (x, z.0, y, z.2);
                    x = y;
                }
                // limitador macio: até 0,9 passa igual, depois curva sem cortar seco
                *v = if x.abs() <= 0.9 { x } else { x.signum() * (0.9 + 0.1 * ((x.abs() - 0.9) / 0.1).tanh()) };
            }
        }
    }
}

// MARK: - Frequências pro desenho (FFT de 2048 pontos, raiz 2)

pub const FFT_N: usize = 2048;

/// Energia (dB) em `bins` faixas de oitava fracionária, de 20 Hz a 20 kHz
pub fn spectrum(samples: &[f32], sr: f32, bins: usize) -> Vec<f32> {
    let n = FFT_N.min(samples.len().next_power_of_two() / 2 * 2).max(64);
    if samples.len() < n {
        return vec![-90.0; bins];
    }
    let s = &samples[samples.len() - n..];
    let mut re: Vec<f32> = (0..n).map(|i| s[i] * (0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos())).collect();
    let mut im = vec![0.0f32; n];
    fft(&mut re, &mut im);
    let mag: Vec<f32> = (0..n / 2).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt() / n as f32 * 4.0).collect();
    (0..bins)
        .map(|b| {
            let f0 = freq_at(b as f32 / bins as f32);
            let f1 = freq_at((b + 1) as f32 / bins as f32);
            let k0 = ((f0 / sr * n as f32) as usize).min(n / 2 - 1);
            let k1 = ((f1 / sr * n as f32).ceil() as usize).clamp(k0 + 1, n / 2);
            let m = mag[k0..k1].iter().copied().fold(0.0f32, f32::max);
            20.0 * m.max(1e-5).log10()
        })
        .collect()
}

fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -std::f32::consts::TAU / len as f32;
        let (wr, wi) = (ang.cos(), ang.sin());
        for i in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (ar, ai) = (re[i + k], im[i + k]);
                let (br, bi) = (re[i + k + len / 2], im[i + k + len / 2]);
                let (tr, ti) = (br * cr - bi * ci, br * ci + bi * cr);
                re[i + k] = ar + tr;
                im[i + k] = ai + ti;
                re[i + k + len / 2] = ar - tr;
                im[i + k + len / 2] = ai - ti;
                let ncr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = ncr;
            }
        }
        len <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(f: f32, sr: f32, n: usize) -> Vec<f32> {
        (0..n).flat_map(|i| {
            let v = (std::f32::consts::TAU * f * i as f32 / sr).sin() * 0.25;
            [v, v]
        }).collect()
    }

    fn rms(b: &[f32]) -> f32 {
        (b.iter().map(|v| v * v).sum::<f32>() / b.len() as f32).sqrt()
    }

    /// Ganho medido de verdade (passando um tom pelo filtro), sem o pré-ganho
    fn measured_db(gains: [f32; N], f: f32) -> f32 {
        let sr = 44_100.0;
        let mut e = Eq::new(sr);
        e.set(gains, true);
        let mut warm = tone(f, sr, 8192);
        e.process(&mut warm);
        for _ in 0..40 {
            let mut w = tone(f, sr, 1024);
            e.process(&mut w);
        }
        let input = tone(f, sr, 16384);
        let mut out = input.clone();
        e.process(&mut out);
        20.0 * (rms(&out[4096..]) / rms(&input[4096..])).log10() - 20.0 * preamp(&gains).log10()
    }

    #[test]
    fn original_nao_mexe() {
        let input = tone(1000.0, 48_000.0, 4800);
        let mut out = input.clone();
        Eq::new(48_000.0).process(&mut out);
        assert_eq!(input, out);
    }

    #[test]
    fn cada_faixa_sobe_o_que_diz() {
        for i in 1..N - 1 {
            let mut g = [0.0; N];
            g[i] = 6.0;
            let db = measured_db(g, BANDS[i]);
            assert!((db - 6.0).abs() < 1.2, "faixa {} Hz deu {db:.2} dB", BANDS[i]);
        }
    }

    #[test]
    fn curva_bate_com_o_som() {
        let g = find("grave").unwrap().gains;
        let c = curve(&g, 1000);
        let at = |f: f32| c[(pos_of(f) * 999.0).round() as usize];
        for f in [64.0, 250.0, 1000.0, 4000.0] {
            let real = measured_db(g, f);
            assert!((real - at(f)).abs() < 1.0, "{f} Hz: desenho {:.2} x som {real:.2}", at(f));
        }
    }

    #[test]
    fn nunca_estoura() {
        let mut g = [MAX_DB; N];
        g[5] = MAX_DB;
        let mut e = Eq::new(48_000.0);
        e.set(g, true);
        let mut b: Vec<f32> = (0..48_000).flat_map(|i| { let v = ((i as f32) * 0.05).sin(); [v, v] }).collect();
        for c in b.chunks_mut(512) {
            e.process(c);
        }
        assert!(b.iter().all(|v| v.abs() <= 1.0));
    }
}
