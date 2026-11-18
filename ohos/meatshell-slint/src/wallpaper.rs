//! Wallpaper rendering + immersive colour extraction.
//!
//! Ported from upstream `src/wallpaper/impls/wallpaper.rs`: the built-ins are
//! drawn per-pixel in code (calm gradient pair + the synthwave "tech" one),
//! "miku"/"ms" are bundled JPEGs, and anything else is a user image path.
//! Each path yields a Slint image plus the palette the Theme tints itself with.

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

/// Render size for the procedural built-ins; `image-fit: cover` scales them.
const W: u32 = 1600;
const H: u32 = 1000;
/// Cap the long edge of a decoded wallpaper so a huge photo stays cheap.
const MAX_EDGE: u32 = 2560;

/// Colours derived from the image, pushed into `Theme` when immersive is on.
pub(crate) struct Palette {
    pub is_dark: bool,
    pub accent: (u8, u8, u8),
    pub tint: (u8, u8, u8),
}

/// Resolve a stored wallpaper id into an image + palette. `""` means none.
pub(crate) fn load(id: &str) -> Option<(Image, Palette)> {
    if id.is_empty() {
        return None;
    }
    let buf = match id {
        "builtin:light" => render_builtin(false),
        "builtin:dark" => render_builtin(true),
        "builtin:tech" => render_tech(),
        "builtin:miku" => decode_miku()?,
        "builtin:ms" => decode_ms()?,
        path => decode_custom(path)?,
    };
    let palette = derive_palette(buf.as_slice());
    Some((Image::from_rgba8(buf), palette))
}

pub(crate) fn is_builtin(id: &str) -> bool {
    matches!(
        id,
        "builtin:light" | "builtin:dark" | "builtin:tech" | "builtin:miku" | "builtin:ms"
    )
}

/// Wallpaper state that lives outside the engine's ConfigStore: the id (a
/// builtin tag or an absolute sandbox path) and the overlay opacity.
pub(crate) struct Prefs {
    pub id: String,
    pub overlay: Option<f32>,
}

fn prefs_file(name: &str) -> Option<std::path::PathBuf> {
    std::env::var("MEATSHELL_DATA_DIR")
        .ok()
        .filter(|d| !d.is_empty())
        .map(|d| std::path::Path::new(&d).join(name))
}

pub(crate) fn load_prefs() -> Prefs {
    let read = |name: &str| {
        prefs_file(name)
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default()
    };
    Prefs {
        id: read("wallpaper.txt").trim().to_string(),
        overlay: read("wallpaper-overlay.txt").trim().parse::<f32>().ok(),
    }
}

pub(crate) fn save_prefs(id: &str, overlay: Option<f32>) {
    if let Some(path) = prefs_file("wallpaper.txt") {
        if let Err(e) = std::fs::write(path, id) {
            eprintln!("meatshell-ohos: save wallpaper id: {e}");
        }
    }
    if let Some(overlay) = overlay {
        if let Some(path) = prefs_file("wallpaper-overlay.txt") {
            if let Err(e) = std::fs::write(path, format!("{overlay:.3}")) {
                eprintln!("meatshell-ohos: save wallpaper overlay: {e}");
            }
        }
    }
}

fn file_stem(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string()
}

/// Push a wallpaper id (and optionally an overlay) into the live window.
///
/// Built-in ids also flip light/dark — that is what the wallpaper swatches
/// promise — while a custom photo only recolours when immersive is on.
pub(crate) fn apply(ui: &crate::AppWindow, id: &str, overlay: Option<f32>) {
    match load(id) {
        Some((image, palette)) => {
            ui.set_wallpaper_img(image);
            ui.set_wallpaper_active(true);
            ui.set_wp_accent(slint::Color::from_rgb_u8(
                palette.accent.0,
                palette.accent.1,
                palette.accent.2,
            ));
            ui.set_wp_tint(slint::Color::from_rgb_u8(
                palette.tint.0,
                palette.tint.1,
                palette.tint.2,
            ));
            ui.set_current_wallpaper(id.into());
            ui.set_custom_wallpaper_name(if is_builtin(id) {
                Default::default()
            } else {
                file_stem(id).into()
            });
            if is_builtin(id) || ui.get_immersive_colors() {
                ui.set_dark_mode(palette.is_dark);
            }
        }
        None => {
            ui.set_wallpaper_active(false);
            ui.set_wallpaper_img(slint::Image::default());
            ui.set_current_wallpaper(Default::default());
            ui.set_custom_wallpaper_name(Default::default());
        }
    }
    if let Some(overlay) = overlay {
        ui.set_wallpaper_overlay(overlay);
    }
}

fn lerp(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).clamp(0.0, 255.0) as u8
}

fn blend(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 * (1.0 - t) + b as f32 * t).clamp(0.0, 255.0) as u8
}

fn render_builtin(dark: bool) -> SharedPixelBuffer<Rgba8Pixel> {
    let (c0, c1, glow) = if dark {
        ((0x10, 0x13, 0x1a), (0x1b, 0x22, 0x33), (0x4a, 0x6c, 0xe0))
    } else {
        ((0xef, 0xf3, 0xfb), (0xd5, 0xe1, 0xf2), (0x4a, 0x90, 0xe2))
    };
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(W, H);
    let px = buf.make_mut_slice();
    let glow_cx = W as f32 * 0.74;
    let glow_cy = H as f32 * 0.26;
    let glow_r = W as f32 * 0.6;
    let glow_strength = if dark { 0.38 } else { 0.22 };
    for y in 0..H {
        for x in 0..W {
            let t = ((x as f32 / W as f32) + (y as f32 / H as f32)) * 0.5;
            let mut r = lerp(c0.0, c1.0, t);
            let mut g = lerp(c0.1, c1.1, t);
            let mut b = lerp(c0.2, c1.2, t);
            let dx = x as f32 - glow_cx;
            let dy = y as f32 - glow_cy;
            let d = (dx * dx + dy * dy).sqrt() / glow_r;
            let gf = (1.0 - d).clamp(0.0, 1.0);
            let gf = gf * gf * glow_strength;
            r = blend(r, glow.0, gf);
            g = blend(g, glow.1, gf);
            b = blend(b, glow.2, gf);
            px[(y * W + x) as usize] = Rgba8Pixel { r, g, b, a: 255 };
        }
    }
    buf
}

/// "幻想 3048" — sci-fi sunset over a neon grid (upstream `render_tech`).
fn render_tech() -> SharedPixelBuffer<Rgba8Pixel> {
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(W, H);
    let px = buf.make_mut_slice();
    let wf = W as f32;
    let hf = H as f32;
    let horizon = hf * 0.60;
    let vp_x = wf * 0.5;
    let sun_cx = vp_x;
    let sun_cy = horizon - hf * 0.16;
    let sun_r = hf * 0.17;
    for y in 0..H {
        let yf = y as f32;
        for x in 0..W {
            let xf = x as f32;
            let (mut r, mut g, mut b) = if yf < horizon {
                let t = yf / horizon;
                let glow = t * t * t;
                (6.0 + 10.0 * glow, 9.0 + 40.0 * glow, 22.0 + 70.0 * glow)
            } else {
                let t = (yf - horizon) / (hf - horizon);
                (14.0 + 12.0 * t, 14.0 - 8.0 * t, 34.0 + 6.0 * t)
            };
            let sdx = xf - sun_cx;
            let sdy = yf - sun_cy;
            let sd = (sdx * sdx + sdy * sdy).sqrt() / sun_r;
            if sd < 1.0 && yf < horizon {
                let vt = ((yf - (sun_cy - sun_r)) / (2.0 * sun_r)).clamp(0.0, 1.0);
                let sr = 60.0 + 200.0 * vt;
                let sg = 220.0 - 150.0 * vt;
                let sb = 255.0 - 40.0 * vt;
                let gap = if vt > 0.5 && (yf / (hf * 0.022)).fract() < 0.45 {
                    0.0
                } else {
                    1.0
                };
                let m = (1.0 - sd).clamp(0.0, 1.0).sqrt() * gap;
                r = r * (1.0 - m) + sr * m;
                g = g * (1.0 - m) + sg * m;
                b = b * (1.0 - m) + sb * m;
            }
            if yf > horizon {
                let depth = yf - horizon;
                let hl = grid_line((hf * 7.0) / depth, 0.06);
                let vl = grid_line((xf - vp_x) / depth * 2.4, 0.05);
                let fade = 1.0 - (depth / (hf - horizon)).clamp(0.0, 1.0) * 0.5;
                let grid = hl.max(vl) * fade;
                r = r * (1.0 - grid) + 30.0 * grid;
                g = g * (1.0 - grid) + 230.0 * grid;
                b = b * (1.0 - grid) + 255.0 * grid;
            }
            let hglow = (1.0 - (yf - horizon).abs() / (hf * 0.05)).clamp(0.0, 1.0);
            let hglow = hglow * hglow;
            r += 40.0 * hglow;
            g += 200.0 * hglow;
            b += 230.0 * hglow;
            if yf < horizon * 0.92 && hash2(x, y) > 0.9985 {
                r += 200.0;
                g += 200.0;
                b += 200.0;
            }
            px[(y * W + x) as usize] = Rgba8Pixel {
                r: r.clamp(0.0, 255.0) as u8,
                g: g.clamp(0.0, 255.0) as u8,
                b: b.clamp(0.0, 255.0) as u8,
                a: 255,
            };
        }
    }
    buf
}

fn grid_line(t: f32, halfwidth: f32) -> f32 {
    let f = (t - t.round()).abs();
    (1.0 - f / halfwidth).clamp(0.0, 1.0)
}

fn hash2(x: u32, y: u32) -> f32 {
    let mut h = x
        .wrapping_mul(374761393)
        .wrapping_add(y.wrapping_mul(668265263));
    h = (h ^ (h >> 13)).wrapping_mul(1274126177);
    ((h ^ (h >> 16)) & 0x00ff_ffff) as f32 / 16777216.0
}

fn decode_custom(path: &str) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    Some(to_buffer(image::open(path).ok()?.to_rgba8()))
}

fn decode_miku() -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    const BYTES: &[u8] = include_bytes!("../../../assets/miku.jpg");
    Some(to_buffer(image::load_from_memory(BYTES).ok()?.to_rgba8()))
}

fn decode_ms() -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    const BYTES: &[u8] = include_bytes!("../../../assets/ms.jpg");
    Some(to_buffer(image::load_from_memory(BYTES).ok()?.to_rgba8()))
}

fn to_buffer(img: image::RgbaImage) -> SharedPixelBuffer<Rgba8Pixel> {
    let (w, h) = img.dimensions();
    let img = if w.max(h) > MAX_EDGE {
        let scale = MAX_EDGE as f32 / w.max(h) as f32;
        let nw = ((w as f32 * scale) as u32).max(1);
        let nh = ((h as f32 * scale) as u32).max(1);
        image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let (w, h) = img.dimensions();
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
    buf.make_mut_bytes().copy_from_slice(img.as_raw());
    buf
}

fn derive_palette(px: &[Rgba8Pixel]) -> Palette {
    let (mut sr, mut sg, mut sb, mut n) = (0u64, 0u64, 0u64, 0u64);
    let step = (px.len() / 4096).max(1);
    let mut i = 0;
    while i < px.len() {
        sr += px[i].r as u64;
        sg += px[i].g as u64;
        sb += px[i].b as u64;
        n += 1;
        i += step;
    }
    let n = n.max(1);
    let (ar, ag, ab) = ((sr / n) as u8, (sg / n) as u8, (sb / n) as u8);
    let lum = 0.299 * ar as f32 + 0.587 * ag as f32 + 0.114 * ab as f32;
    let is_dark = lum < 128.0;
    Palette {
        is_dark,
        accent: vivid_accent(ar, ag, ab, is_dark),
        tint: (ar, ag, ab),
    }
}

fn vivid_accent(r: u8, g: u8, b: u8, is_dark: bool) -> (u8, u8, u8) {
    let (h, s, _l) = rgb_to_hsl(r, g, b);
    let (h, s) = if s < 0.08 {
        (210.0 / 360.0, 0.70) // brand blue when the wallpaper is essentially grey
    } else {
        (h, s.max(0.55))
    };
    let l = if is_dark { 0.62 } else { 0.50 };
    hsl_to_rgb(h, s, l)
}

fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if (max - min).abs() < f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } / 6.0;
    (h, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    if s.abs() < f32::EPSILON {
        let v = (l * 255.0).round() as u8;
        return (v, v, v);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let to = |mut t: f32| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v * 255.0).round() as u8
    };
    (to(h + 1.0 / 3.0), to(h), to(h - 1.0 / 3.0))
}
