//! O vidro do Clawdeck, na placa de vídeo, do jeito que a Apple faz.
//!
//! O egui não sabe desfocar nem entortar o que está atrás. Aqui, no meio do desenho, a peça de vidro
//! copia o pedaço da tela que já foi pintado atrás dela (fundo, cartões, a própria transmissão) e
//! desenha por cima com a conta do Clawdeck:
//! - `vidrogl.js` (painéis, folhas, botões): na beira o fundo entorta pra dentro numa faixa larga
//!   e macia, cada cor um tiquinho diferente (a franja de um vidro de verdade); no meio fica fosco,
//!   mais saturado, com uma luz larga e fraca descendo do topo;
//! - `ilhalente.js` (a ilha aberta, como a do Canopy 2): o fundo aparece nítido, entorta forte
//!   numa faixa de 16 px que corta seco (vira fiapos), com o fiapo vermelho e azul; o topo quase
//!   não entorta, as laterais e a base entortam inteiro. Fumê preto em cima (onde está o texto),
//!   abrindo na base, e um fio de luz que acende nos cantos de baixo.
//! A sombra também sai daqui (desenhada antes, ela escureceria o próprio vidro).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use eframe::egui::{self, Color32, Rect};
use eframe::egui_glow::{self, glow};
use glow::HasContext;

/// Como é o vidro
#[derive(Clone, Copy, Debug)]
pub struct Glass {
    /// Raio de cada canto (pontos): cima-esq, cima-dir, baixo-dir, baixo-esq
    pub radius: [f32; 4],
    /// Fumê: opacidade em 0 %, 60 %, 80 % e 100 % da altura
    pub smoke: [f32; 4],
    /// Altura (0 a 1) dos dois valores do meio do fumê
    pub smoke_at: [f32; 2],
    pub smoke_rgb: [f32; 3],
    /// 0 = fundo nítido; 1 = fosco no meio (o desfoque do Clawdeck, ~7 px)
    pub frost: f32,
    pub saturation: f32,
    /// Quanto entorta (pontos) e a largura da faixa que entorta
    pub force: f32,
    pub band: f32,
    /// Separação das cores na beira (pontos)
    pub chroma: f32,
    /// true = lente da ilha (Canopy): perfil que corta seco, topo fraco
    pub lens: bool,
    /// Cor da capa tingindo o vidro (rgb, quanto)
    pub tint: Option<(Color32, f32)>,
    /// Luz na borda
    pub rim: f32,
    /// Sombra: opacidade, desfoque e deslocamento pra baixo (pontos)
    pub shadow: (f32, f32, f32),
    /// Opacidade do vidro inteiro (entrar/sair)
    pub opacity: f32,
}

impl Glass {
    /// Painel grande e folhas: fosco, saturado, entorta na beira (vidrogl.js). `fill` = quão fechado
    pub fn panel(radius: [f32; 4], fill: f32) -> Glass {
        let f = fill.clamp(0.0, 1.0);
        Glass {
            radius,
            smoke: [f * 0.62; 4],
            smoke_at: [0.6, 0.8],
            smoke_rgb: [0.055, 0.05, 0.065],
            frost: 1.0,
            saturation: 1.35,
            force: 0.0, // vem da faixa (abaixo)
            band: 0.0,
            chroma: 1.0,
            lens: false,
            tint: None,
            rim: 0.05,
            shadow: (0.0, 0.0, 0.0),
            opacity: 1.0,
        }
    }

    /// A ilha aberta (Canopy 2 com liquidGlassTransparency)
    pub fn island(radius: f32) -> Glass {
        Glass {
            radius: [radius; 4],
            // medido na gravação do Canopy 2, comparando com o fundo sem a ilha: quase preto em cima,
            // já deixa ver metade na altura do tempo e fica aberto dos botões pra baixo
            smoke: [0.99, 0.80, 0.25, 0.30],
            smoke_at: [0.30, 0.68],
            smoke_rgb: [0.0; 3],
            frost: 0.0,
            saturation: 1.25,
            force: 60.0,
            band: 36.0,
            chroma: 2.5,
            lens: true,
            tint: None,
            rim: 0.16,
            shadow: (0.65, 40.0, 18.0),
            opacity: 1.0,
        }
    }

    /// A beira de vidro da ilha por cima de um vídeo: entorta a imagem na borda, sem fumê no meio
    pub fn pip(radius: f32) -> Glass {
        Glass {
            radius: [radius; 4],
            smoke: [0.0; 4],
            smoke_at: [0.30, 0.68],
            smoke_rgb: [0.0; 3],
            frost: 0.0,
            saturation: 1.0,
            // quase imperceptível (pedido: a borda de vidro na câmera ficava feia)
            force: 2.0,
            band: 5.0,
            chroma: 0.0,
            lens: true,
            tint: None,
            rim: 0.04,
            shadow: (0.0, 0.0, 0.0),
            opacity: 1.0,
        }
    }

    /// Cartão, painel e botão no mesmo vidro da ilha (Canopy 2): o que está atrás nítido, entortando
    /// numa faixa na borda com o fiapo vermelho e azul, fumê que abre de cima pra baixo e o fio de luz
    /// acendendo nos cantos de baixo. Um pouco mais fechado que a ilha, porque tem texto até embaixo.
    pub fn card(radius: [f32; 4]) -> Glass {
        Glass {
            radius,
            smoke: [0.44, 0.34, 0.24, 0.16],
            smoke_at: [0.6, 0.8],
            smoke_rgb: [0.0; 3],
            frost: 0.0,
            saturation: 1.2,
            // a mesma lente da ilha (pedido: "o glass igual o da dynamic island, aquela distorção toda")
            force: 60.0,
            band: 36.0,
            chroma: 3.0,
            lens: true,
            tint: None,
            rim: 0.42,
            shadow: (0.55, 40.0, 16.0),
            opacity: 1.0,
        }
    }

    /// Aviso da ilha: mais fechado (só a base deixa ver o que está atrás)
    pub fn alert(radius: f32) -> Glass {
        // antes quase preto de cima a baixo (0,98 → 0,8: "design antigo, não tá glass"). Agora o mesmo
        // jeito da ilha aberta: escuro onde fica o título, abrindo embaixo; um pouco mais fechado
        // porque o texto do aviso desce até a segunda linha
        Glass { smoke: [0.93, 0.74, 0.42, 0.40], smoke_at: [0.40, 0.85], ..Glass::island(radius) }
    }

    pub fn with_shadow(mut self, alpha: f32, blur: f32, dy: f32) -> Glass {
        self.shadow = (alpha, blur, dy);
        self
    }

    pub fn with_tint(mut self, tint: Option<Color32>, amount: f32) -> Glass {
        self.tint = tint.map(|c| (c, amount));
        self
    }

    pub fn with_opacity(mut self, a: f32) -> Glass {
        self.opacity = a;
        self
    }
}

struct Gl {
    program: glow::Program,
    vao: glow::VertexArray,
    loc: HashMap<&'static str, Option<glow::UniformLocation>>,
    /// Texturas pra cópia do fundo, por tamanho (várias peças por quadro)
    tex: HashMap<(i32, i32), (glow::Texture, u64)>,
    tick: u64,
    /// Pra fazer os níveis borrados com cópias encolhidas (blit) em vez do glGenerateMipmap, que no
    /// Mac (OpenGL em cima do Metal) faz a CPU parar esperando a placa a cada vidro. None = não deu, usa o antigo
    fbo: Option<Option<(glow::Framebuffer, glow::Framebuffer)>>,
    failed: bool,
}

// Os objetos do OpenGL são números; só são usados na thread que desenha.
unsafe impl Send for Gl {}

static NO_GL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn state() -> &'static Mutex<Option<Gl>> {
    static S: OnceLock<Mutex<Option<Gl>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

const UNIFORMS: [&str; 16] = [
    "u_bg", "u_org", "u_tsize", "u_vsize", "u_valid", "u_rect", "u_rad", "u_smoke", "u_smoke_at", "u_smoke_rgb", "u_tint", "u_lens", "u_look", "u_shadow", "u_ppp", "u_frost",
];

/// Desenha o vidro em `rect` (pontos). Entra na fila do painter como as outras formas.
pub fn paint(painter: &egui::Painter, rect: Rect, g: Glass) {
    painter.add(shape(rect, g));
}

/// A forma do vidro (pra guardar o lugar e trocar depois, como o `glass_shapes`)
pub fn shape(rect: Rect, g: Glass) -> egui::Shape {
    let margin = if g.shadow.0 > 0.0 { g.shadow.1 + g.shadow.2.abs() + 2.0 } else { 1.0 };
    let area = rect.expand(margin);
    let cb = egui_glow::CallbackFn::new(move |info, p| {
        let gl = p.gl();
        let mut st = state().lock().unwrap_or_else(|e| e.into_inner());
        if st.is_none() {
            if NO_GL.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            let v = egui_glow::ShaderVersion::get(gl);
            *st = unsafe { create(gl, v.version_declaration(), v.is_new_shader_interface()) };
        }
        let Some(s) = st.as_mut() else { return };
        if s.failed {
            return;
        }
        // com a janela animando (winmotion) a camada vem escalada: leva o vidro junto
        let vp = info.viewport;
        let k = vp.width() / area.width().max(1.0);
        let rect = if (k - 1.0).abs() > 0.001 || vp.min != area.min {
            Rect::from_min_size(vp.min + (rect.min - area.min) * k, rect.size() * k)
        } else {
            rect
        };
        unsafe { draw(gl, s, &info, rect, &g) };
    });
    egui::Shape::Callback(egui::PaintCallback { rect: area, callback: Arc::new(cb) })
}

unsafe fn draw(gl: &glow::Context, s: &mut Gl, info: &egui::PaintCallbackInfo, rect: Rect, g: &Glass) {
    let vp = info.viewport_in_pixels();
    let (w, h) = (vp.width_px, vp.height_px);
    if w <= 0 || h <= 0 {
        return;
    }
    let ppp = info.pixels_per_point;
    let [sw, sh] = info.screen_size_px;
    // o pedaço que existe na tela (o resto da textura fica fora do alcance)
    let x0 = vp.left_px.max(0);
    let y0 = vp.from_bottom_px.max(0);
    let x1 = (vp.left_px + w).min(sw as i32);
    let y1 = (vp.from_bottom_px + h).min(sh as i32);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    s.tick += 1;
    let tick = s.tick;
    // a textura vai em degraus de 128 px: a folha abrindo muda de tamanho a cada quadro, e criar
    // uma textura nova (com 5 níveis) por quadro derrubava a animação pra ~30 quadros/s
    let (tw, th) = ((w + 127) / 128 * 128, (h + 127) / 128 * 128);
    let key = (tw, th);
    unsafe {
        let tex = match s.tex.get_mut(&key) {
            Some(t) => {
                t.1 = tick;
                t.0
            }
            None => {
                if s.tex.len() >= 24 {
                    // tira a mais antiga
                    if let Some((&k, &(t, _))) = s.tex.iter().min_by_key(|(_, v)| v.1) {
                        gl.delete_texture(t);
                        s.tex.remove(&k);
                    }
                }
                let Ok(t) = gl.create_texture() else { return };
                gl.bind_texture(glow::TEXTURE_2D, Some(t));
                // os níveis menores já com lugar reservado (o fosco usa até o nível ~3,4)
                for l in 0..=LEVELS {
                    gl.tex_image_2d(glow::TEXTURE_2D, l, glow::RGBA8 as i32, (tw >> l).max(1), (th >> l).max(1), 0, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(None));
                }
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_BASE_LEVEL, 0);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAX_LEVEL, LEVELS);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
                s.tex.insert(key, (t, tick));
                t
            }
        };
        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        // copia o que já está pintado atrás (a cópia não liga pro recorte)
        gl.copy_tex_sub_image_2d(glow::TEXTURE_2D, 0, x0 - vp.left_px, y0 - vp.from_bottom_px, x0, y0, x1 - x0, y1 - y0);
        let frost = g.frost > 0.01;
        if frost {
            if s.fbo.is_none() {
                s.fbo = Some(match (gl.create_framebuffer(), gl.create_framebuffer()) {
                    (Ok(a), Ok(b)) => Some((a, b)),
                    _ => None,
                });
            }
            let blitted = match s.fbo.flatten() {
                Some((rd, dr)) => shrink(gl, tex, w, h, rd, dr),
                None => false,
            };
            if !blitted {
                gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                gl.generate_mipmap(glow::TEXTURE_2D);
            }
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR_MIPMAP_LINEAR as i32);
        } else {
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
        }

        gl.use_program(Some(s.program));
        gl.bind_vertex_array(Some(s.vao));
        let u = |n: &str| s.loc.get(n).and_then(|l| l.as_ref());
        gl.uniform_1_i32(u("u_bg"), 0);
        gl.uniform_2_f32(u("u_org"), vp.left_px as f32, vp.from_bottom_px as f32);
        gl.uniform_2_f32(u("u_tsize"), tw as f32, th as f32);
        gl.uniform_2_f32(u("u_vsize"), w as f32, h as f32);
        // área válida em px, de cima pra baixo
        let (vx0, vx1) = ((x0 - vp.left_px) as f32 + 0.5, (x1 - vp.left_px) as f32 - 0.5);
        let (vyb0, vyb1) = ((y0 - vp.from_bottom_px) as f32, (y1 - vp.from_bottom_px) as f32);
        gl.uniform_4_f32(u("u_valid"), vx0, h as f32 - vyb1 + 0.5, vx1, h as f32 - vyb0 - 0.5);
        let c = rect.center();
        gl.uniform_4_f32(u("u_rect"), c.x * ppp - vp.left_px as f32, c.y * ppp - vp.top_px as f32, rect.width() * ppp / 2.0, rect.height() * ppp / 2.0);
        let max_r = rect.width().min(rect.height()) / 2.0;
        let r = g.radius.map(|x| x.clamp(0.0, max_r) * ppp);
        gl.uniform_4_f32(u("u_rad"), r[0], r[1], r[2], r[3]);
        gl.uniform_4_f32(u("u_smoke"), g.smoke[0], g.smoke[1], g.smoke[2], g.smoke[3]);
        gl.uniform_2_f32(u("u_smoke_at"), g.smoke_at[0], g.smoke_at[1]);
        gl.uniform_3_f32(u("u_smoke_rgb"), g.smoke_rgb[0], g.smoke_rgb[1], g.smoke_rgb[2]);
        match g.tint {
            Some((t, a)) => gl.uniform_4_f32(u("u_tint"), t.r() as f32 / 255.0, t.g() as f32 / 255.0, t.b() as f32 / 255.0, a),
            None => gl.uniform_4_f32(u("u_tint"), 0.0, 0.0, 0.0, 0.0),
        }
        // painel: a faixa vem do tamanho (como no vidrogl: 85 % do lado menor, entre 7 e 30)
        let (force, band) = if g.lens {
            (g.force, g.band)
        } else {
            let band = (rect.width().min(rect.height()) / 2.0 * 0.85).clamp(7.0, 30.0);
            (if g.force > 0.0 { g.force } else { band * 0.62 }, band)
        };
        gl.uniform_4_f32(u("u_lens"), force * ppp, band * ppp, g.chroma * ppp, if g.lens { 1.0 } else { 0.0 });
        gl.uniform_4_f32(u("u_look"), g.frost, g.saturation, g.rim, 0.0);
        gl.uniform_4_f32(u("u_shadow"), g.shadow.0, g.shadow.1 * ppp, g.shadow.2 * ppp, g.opacity.clamp(0.0, 1.0));
        gl.uniform_1_f32(u("u_ppp"), ppp);
        gl.uniform_1_f32(u("u_frost"), if frost { 1.0 } else { 0.0 });
        gl.enable(glow::BLEND);
        // a peça já mistura com o que estava atrás (inclusive o alfa da janela transparente): substitui o pixel
        gl.blend_func(glow::ONE, glow::ZERO);
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        gl.blend_func_separate(glow::ONE, glow::ONE_MINUS_SRC_ALPHA, glow::ONE_MINUS_DST_ALPHA, glow::ONE);
        gl.bind_texture(glow::TEXTURE_2D, None);
    }
}

/// Quantos níveis menores a cópia do fundo tem (cada um com metade do lado)
const LEVELS: i32 = 4;

/// Faz os níveis 1..LEVELS da cópia encolhendo o anterior pela metade, tudo na placa (sem a CPU
/// esperar). Devolve false se o framebuffer não aceitou (aí quem chama usa o glGenerateMipmap)
unsafe fn shrink(gl: &glow::Context, tex: glow::Texture, w: i32, h: i32, rd: glow::Framebuffer, dr: glow::Framebuffer) -> bool {
    let fb = |v: i32| std::num::NonZeroU32::new(v as u32).map(glow::NativeFramebuffer);
    unsafe {
        let (old_rd, old_dr) = (gl.get_parameter_i32(glow::READ_FRAMEBUFFER_BINDING), gl.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING));
        gl.bind_texture(glow::TEXTURE_2D, None);
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(rd));
        gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(dr));
        let scissor = gl.is_enabled(glow::SCISSOR_TEST);
        gl.disable(glow::SCISSOR_TEST);
        let mut ok = true;
        for l in 0..LEVELS {
            gl.framebuffer_texture_2d(glow::READ_FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(tex), l);
            gl.framebuffer_texture_2d(glow::DRAW_FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(tex), l + 1);
            if l == 0 && (gl.check_framebuffer_status(glow::READ_FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE || gl.check_framebuffer_status(glow::DRAW_FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE) {
                ok = false;
                break;
            }
            let (sw, sh, dw, dh) = ((w >> l).max(1), (h >> l).max(1), (w >> (l + 1)).max(1), (h >> (l + 1)).max(1));
            gl.blit_framebuffer(0, 0, sw, sh, 0, 0, dw, dh, glow::COLOR_BUFFER_BIT, glow::LINEAR);
        }
        gl.framebuffer_texture_2d(glow::READ_FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, None, 0);
        gl.framebuffer_texture_2d(glow::DRAW_FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, None, 0);
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, fb(old_rd));
        gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, fb(old_dr));
        if scissor {
            gl.enable(glow::SCISSOR_TEST);
        }
        ok
    }
}

unsafe fn create(gl: &glow::Context, version: &str, new: bool) -> Option<Gl> {
    let (vout, fin, frag_out, tex, out) =
        if new { ("out", "in", "out vec4 frag;", "texture", "frag") } else { ("varying", "varying", "", "texture2D", "gl_FragColor") };
    let vs = format!(
        "{version}
        {vout} vec2 v_p;
        void main() {{
            vec2 p = vec2(float(gl_VertexID & 1), float((gl_VertexID >> 1) & 1));
            v_p = p;
            gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
        }}"
    );
    let fs = format!(
        "{version}
        precision highp float;
        {fin} vec2 v_p;
        {frag_out}
        uniform sampler2D u_bg;
        uniform vec2 u_org;
        uniform vec2 u_tsize;
        uniform vec2 u_vsize;
        uniform vec4 u_valid;
        uniform vec4 u_rect;
        uniform vec4 u_rad;
        uniform vec4 u_smoke;
        uniform vec2 u_smoke_at;
        uniform vec3 u_smoke_rgb;
        uniform vec4 u_tint;
        uniform vec4 u_lens;
        uniform vec4 u_look;
        uniform vec4 u_shadow;
        uniform float u_ppp;
        uniform float u_frost;

        // retângulo arredondado, um raio por canto (y pra baixo): cima-esq, cima-dir, baixo-dir, baixo-esq
        float caixa(vec2 q, vec2 m, vec4 r) {{
            float rr = q.x > 0.0 ? (q.y > 0.0 ? r.z : r.y) : (q.y > 0.0 ? r.w : r.x);
            vec2 d = abs(q) - m + rr;
            return length(max(d, 0.0)) + min(max(d.x, d.y), 0.0) - rr;
        }}
        vec2 uv(vec2 p) {{
            p = clamp(p, u_valid.xy, u_valid.zw);
            return vec2(p.x, u_vsize.y - p.y) / u_tsize;
        }}
        vec3 nitido(vec2 p) {{ return {tex}(u_bg, uv(p)).rgb; }}
        // a cópia do fundo vem pré-multiplicada; alfa < 1 onde a janela é transparente (vidro do sistema atrás)
        float opaco(vec2 p) {{ return {tex}(u_bg, uv(p)).a; }}
        // fosco: 9 amostras num nível mais borrado (~7 px, o blur(7px) do vidrogl)
        vec3 fosco(vec2 p) {{
            float s = 5.0 * u_ppp;
            float lod = 2.4 + log2(max(u_ppp, 1.0));
            vec3 c = vec3(0.0);
            for (int i = -1; i <= 1; i++) {{
                for (int j = -1; j <= 1; j++) {{
                    float w = (i == 0 && j == 0) ? 0.2 : ((i == 0 || j == 0) ? 0.12 : 0.08);
                    c += {tex}(u_bg, uv(p + vec2(float(i), float(j)) * s), lod).rgb * w;
                }}
            }}
            return c;
        }}

        void main() {{
            vec2 fp = gl_FragCoord.xy - u_org;
            vec2 f = vec2(fp.x, u_vsize.y - fp.y);
            vec2 m = u_rect.zw;
            vec2 o = f - u_rect.xy;
            float d = caixa(o, m, u_rad);
            float cobre = clamp(0.5 - d, 0.0, 1.0);
            float alpha = u_shadow.w;

            // sombra (fora do vidro)
            float sombra = 0.0;
            if (u_shadow.x > 0.0) {{
                float ds = caixa(o - vec2(0.0, u_shadow.z), m, u_rad);
                float t = clamp((ds + u_shadow.y * 0.35) / (u_shadow.y * 1.35), 0.0, 1.0);
                sombra = u_shadow.x * (1.0 - t) * (1.0 - t);
            }}
            // o que já estava pintado aqui (a peça substitui o pixel inteiro: blend ONE, ZERO)
            vec4 antes = {tex}(u_bg, uv(f));
            vec4 fora = vec4(antes.rgb * (1.0 - sombra), antes.a + sombra * (1.0 - antes.a));
            if (cobre <= 0.0) {{
                {out} = mix(antes, fora, alpha);
                return;
            }}

            vec2 e = vec2(0.5, 0.0);
            vec2 nor = normalize(vec2(caixa(o + e.xy, m, u_rad) - caixa(o - e.xy, m, u_rad),
                                      caixa(o + e.yx, m, u_rad) - caixa(o - e.yx, m, u_rad)) + 1e-5);
            float prof = max(-d, 0.0);
            float faixa = max(u_lens.y, 1.0);
            float t = clamp(prof / faixa, 0.0, 1.0);
            float y01 = clamp((o.y + m.y) / (2.0 * m.y), 0.0, 1.0);
            vec3 cor;
            float fundo_a = antes.a;
            if (u_lens.w > 0.5) {{
                // lente do Canopy 2, ajustada contra um quadro da gravação: o campo vem de uma pílula (canto do tamanho
                // da meia altura), por isso o que está atrás vira oval no meio e faz ondas nos cantos de baixo
                float rn = min(m.x, m.y);
                vec4 rr = vec4(rn);
                float dn = caixa(o, m, rr);
                vec2 nr = normalize(vec2(caixa(o + e.xy, m, rr) - caixa(o - e.xy, m, rr),
                                         caixa(o + e.yx, m, rr) - caixa(o - e.yx, m, rr)) + 1e-5);
                float fb = max(min(u_lens.y, 0.41 * rn), 1.0);
                float fo = min(u_lens.x, 0.68 * rn);
                float tn = clamp(max(-dn, 0.0) / fb, 0.0, 1.0);
                float q = (1.0 - tn) * (1.0 - tn);
                float peso = 0.22 + 0.78 * clamp((nr.y + 0.6) / 0.6, 0.0, 1.0);
                vec2 base = f - nr * fo * q * peso;
                vec2 ab = nr * u_lens.z * q * peso;
                cor = vec3(nitido(base + ab).r, nitido(base).g, nitido(base - ab).b);
                if (u_frost > 0.5) {{
                    // fosco no vidro todo: o que passa por baixo vira cor, e a beira continua entortando
                    vec3 fs = vec3(fosco(base + ab).r, fosco(base).g, fosco(base - ab).b);
                    cor = mix(cor, fs, u_look.x);
                }}
                fundo_a = opaco(base);
            }} else {{
                // painel (vidrogl.js): faixa larga e macia, meio fosco
                float q = 1.0 - t, q3 = q * q * q;
                vec2 base = f - nor * u_lens.x * q3;
                vec2 ab = nor * u_lens.z * (0.3 + q3 * 1.6) * q3;
                vec3 ni = vec3(nitido(base + ab).r, nitido(base).g, nitido(base - ab).b);
                if (u_frost > 0.5) {{
                    vec3 fo = vec3(fosco(base + ab).r, fosco(base).g, fosco(base - ab).b);
                    cor = mix(fo, ni, mix(1.0 - u_look.x, 1.0, q3 * 0.65));
                }} else {{
                    cor = ni;
                }}
            }}
            float l = dot(cor, vec3(0.299, 0.587, 0.114));
            cor = clamp(mix(vec3(l), cor, u_look.y), 0.0, 1.0);

            // fumê: preto em cima, abrindo pra base
            float fume = y01 < u_smoke_at.x ? mix(u_smoke.x, u_smoke.y, y01 / u_smoke_at.x)
                       : (y01 < u_smoke_at.y ? mix(u_smoke.y, u_smoke.z, (y01 - u_smoke_at.x) / (u_smoke_at.y - u_smoke_at.x)) : mix(u_smoke.z, u_smoke.w, (y01 - u_smoke_at.y) / (1.0 - u_smoke_at.y)));
            cor = cor * (1.0 - fume) + u_smoke_rgb * fume;
            float va = fundo_a * (1.0 - fume) + fume;
            cor = mix(cor, u_tint.rgb * 0.75, u_tint.a);
            va = mix(va, 1.0, u_tint.a);

            // luz na borda
            float fio = 1.0 - smoothstep(0.0, 1.2 * u_ppp, prof);
            if (u_lens.w > 0.5) {{
                // Canopy: some no topo e acende nos cantos de baixo
                float luz = y01 < 0.4 ? mix(0.06, 0.07, y01 / 0.4) : (y01 < 0.88 ? mix(0.07, 0.3, (y01 - 0.4) / 0.48) : mix(0.3, 0.42, (y01 - 0.88) / 0.12));
                float acende = fio * luz * (u_look.z / 0.42);
                cor += vec3(acende);
                va += acende * (1.0 - va);
            }} else {{
                // vidrogl: sem linha forte; luz larga e fraca que desce do topo + fio fino aceso em cima
                float aro = exp(-prof / u_ppp * 0.09) * u_look.z * max(0.0, -nor.y);
                float acende = aro + fio * mix(0.16, 0.03, y01);
                cor += vec3(acende);
                va = clamp(va + acende * (1.0 - va), 0.0, 1.0);
            }}
            va = clamp(va, 0.0, 1.0);
            cor = clamp(cor, 0.0, va);
            vec4 dentro = vec4(cor, va);
            {out} = mix(antes, mix(fora, dentro, cobre), alpha);
        }}"
    );
    // placa sem OpenGL de verdade: sem vidro novo, mas o app segue (desenha o vidro antigo)
    let (Ok(program), Ok(vao)) = (unsafe { gl.create_program() }, unsafe { gl.create_vertex_array() }) else {
        NO_GL.store(true, std::sync::atomic::Ordering::Relaxed);
        crate::log::log("vidro: a placa não criou o programa; usando o vidro antigo".to_string());
        return None;
    };
    let failed = Gl { program, vao, loc: HashMap::new(), tex: HashMap::new(), tick: 0, fbo: None, failed: true };
    unsafe {
        let program = failed.program;
        let mut shaders = Vec::new();
        for (kind, src) in [(glow::VERTEX_SHADER, &vs), (glow::FRAGMENT_SHADER, &fs)] {
            let Ok(sh) = gl.create_shader(kind) else { return Some(failed) };
            gl.shader_source(sh, src);
            gl.compile_shader(sh);
            if !gl.get_shader_compile_status(sh) {
                crate::log::log(format!("vidro: shader não compilou: {}", gl.get_shader_info_log(sh)));
                return Some(failed);
            }
            gl.attach_shader(program, sh);
            shaders.push(sh);
        }
        gl.link_program(program);
        for sh in shaders {
            gl.detach_shader(program, sh);
            gl.delete_shader(sh);
        }
        if !gl.get_program_link_status(program) {
            crate::log::log(format!("vidro: shader não ligou: {}", gl.get_program_info_log(program)));
            return Some(failed);
        }
        crate::log::log(format!("vidro: ok ({})", gl.get_parameter_string(glow::RENDERER)));
        let loc = UNIFORMS.iter().map(|n| (*n, gl.get_uniform_location(program, n))).collect();
        Some(Gl { loc, failed: false, ..failed })
    }
}

/// Funciona? (sem shader, quem chama desenha o vidro antigo)
pub fn broken() -> bool {
    // medição: TELAO_SEM_VIDRO=1 desenha o vidro antigo, pra comparar o custo
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var_os("TELAO_SEM_VIDRO").is_some())
        || NO_GL.load(std::sync::atomic::Ordering::Relaxed) || state().lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|s| s.failed)
}
