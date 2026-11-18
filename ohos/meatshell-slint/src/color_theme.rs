//! Colour theme presets (Settings › Wallpaper & theme).
//!
//! Ported from upstream `src/app/color_theme.rs`: the palettes are pure
//! computation over `ui/theme.slint`'s `ThemePalette`. The port has no detached
//! windows and no multi-window config registry, so only the main-window apply
//! step is kept.

use slint::{ModelRc, VecModel};

use meatshell_core::config::ConfigStore;

use crate::{AppWindow, ColorThemeItem, ThemePalette};

/// sRGB colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rgb(pub u8, pub u8, pub u8);

const WHITE: Rgb = Rgb(0xff, 0xff, 0xff);
const BLACK: Rgb = Rgb(0, 0, 0);

const fn hex(v: u32) -> Rgb {
    Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

impl Rgb {
    /// Linear blend: `t` = 0 keeps `self`, 1 gives `other`.
    fn mix(self, other: Rgb, t: f32) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        let ch = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Rgb(ch(self.0, other.0), ch(self.1, other.1), ch(self.2, other.2))
    }

    /// Parse `#RRGGBB` (the `#` is optional; surrounding spaces are ignored).
    pub(crate) fn parse(text: &str) -> Option<Rgb> {
        let digits = text.trim().trim_start_matches('#');
        if digits.len() != 6 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        u32::from_str_radix(digits, 16).ok().map(hex)
    }

    pub(crate) fn to_hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }

    pub(crate) fn to_slint(self) -> slint::Color {
        slint::Color::from_rgb_u8(self.0, self.1, self.2)
    }
}

/// One light or dark variant; mirrors `ThemePalette` in `theme.slint`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Palette {
    bg_root: Rgb,
    bg_panel: Rgb,
    bg_panel_alt: Rgb,
    bg_elevated: Rgb,
    bg_hover: Rgb,
    bg_active: Rgb,
    tab_active_bg: Rgb,
    tab_active_border: Rgb,
    border_subtle: Rgb,
    border_strong: Rgb,
    text_primary: Rgb,
    text_secondary: Rgb,
    text_muted: Rgb,
    accent: Rgb,
    accent_hover: Rgb,
    accent_pressed: Rgb,
    term_bg: Rgb,
}

impl Palette {
    fn to_slint(&self) -> ThemePalette {
        ThemePalette {
            bg_root: self.bg_root.to_slint(),
            bg_panel: self.bg_panel.to_slint(),
            bg_panel_alt: self.bg_panel_alt.to_slint(),
            bg_elevated: self.bg_elevated.to_slint(),
            bg_hover: self.bg_hover.to_slint(),
            bg_active: self.bg_active.to_slint(),
            tab_active_bg: self.tab_active_bg.to_slint(),
            tab_active_border: self.tab_active_border.to_slint(),
            border_subtle: self.border_subtle.to_slint(),
            border_strong: self.border_strong.to_slint(),
            text_primary: self.text_primary.to_slint(),
            text_secondary: self.text_secondary.to_slint(),
            text_muted: self.text_muted.to_slint(),
            accent: self.accent.to_slint(),
            accent_hover: self.accent_hover.to_slint(),
            accent_pressed: self.accent_pressed.to_slint(),
            term_bg: self.term_bg.to_slint(),
        }
    }
}

/// The original MeatShell dark palette (same values as `theme.slint`).
fn default_dark() -> Palette {
    Palette {
        bg_root: hex(0x1b1d23),
        bg_panel: hex(0x23262d),
        bg_panel_alt: hex(0x2a2d35),
        bg_elevated: hex(0x30333c),
        bg_hover: hex(0x373a44),
        bg_active: hex(0x3f4350),
        tab_active_bg: hex(0x171a20),
        tab_active_border: hex(0x06070a),
        border_subtle: hex(0x3a3d46),
        border_strong: hex(0x4a4e59),
        text_primary: hex(0xe6e8ee),
        text_secondary: hex(0xb4b9c4),
        text_muted: hex(0x9196a3),
        accent: hex(0x4a90e2),
        accent_hover: hex(0x5aa0f2),
        accent_pressed: hex(0x3a80d2),
        term_bg: hex(0x0e0f13),
    }
}

/// The original MeatShell light palette (same values as `theme.slint`).
fn default_light() -> Palette {
    Palette {
        bg_root: hex(0xf5f5f7),
        bg_panel: hex(0xffffff),
        bg_panel_alt: hex(0xf2f2f7),
        bg_elevated: hex(0xe8e8ed),
        bg_hover: hex(0xe0e0e6),
        bg_active: hex(0xd8d8de),
        tab_active_bg: hex(0xffffff),
        tab_active_border: hex(0xc7c7cc),
        border_subtle: hex(0xe2e2e8),
        border_strong: hex(0xc7c7cc),
        text_primary: hex(0x1d1d1f),
        text_secondary: hex(0x45454a),
        text_muted: hex(0x606066),
        accent: hex(0x0071e3),
        accent_hover: hex(0x0077ed),
        accent_pressed: hex(0x006adb),
        term_bg: hex(0xfafafa),
    }
}

/// The colours a derived palette is built from.
#[derive(Clone, Copy)]
struct Seeds {
    root: Rgb,
    panel: Rgb,
    text: Rgb,
    accent: Rgb,
    term: Rgb,
}

const fn seeds(root: u32, panel: u32, text: u32, accent: u32, term: u32) -> Seeds {
    Seeds {
        root: hex(root),
        panel: hex(panel),
        text: hex(text),
        accent: hex(accent),
        term: hex(term),
    }
}

/// Build a full palette from seeds, using the same steps the default palette
/// takes between its surfaces, borders and text shades.
fn derive(dark: bool, s: Seeds) -> Palette {
    if dark {
        let up = |t: f32| s.panel.mix(WHITE, t);
        Palette {
            bg_root: s.root,
            bg_panel: s.panel,
            bg_panel_alt: up(0.03),
            bg_elevated: up(0.055),
            bg_hover: up(0.085),
            bg_active: up(0.12),
            tab_active_bg: s.root.mix(BLACK, 0.15),
            tab_active_border: s.root.mix(BLACK, 0.75),
            border_subtle: up(0.10),
            border_strong: up(0.17),
            text_primary: s.text,
            text_secondary: s.text.mix(s.panel, 0.25),
            text_muted: s.text.mix(s.panel, 0.43),
            accent: s.accent,
            accent_hover: s.accent.mix(WHITE, 0.12),
            accent_pressed: s.accent.mix(BLACK, 0.12),
            term_bg: s.term,
        }
    } else {
        // Light surfaces step toward the text colour, keeping the hue.
        let down = |t: f32| s.panel.mix(s.text, t);
        Palette {
            bg_root: s.root,
            bg_panel: s.panel,
            bg_panel_alt: down(0.05),
            bg_elevated: down(0.09),
            bg_hover: down(0.12),
            bg_active: down(0.155),
            tab_active_bg: s.panel,
            tab_active_border: down(0.25),
            border_subtle: down(0.11),
            border_strong: down(0.25),
            text_primary: s.text,
            text_secondary: s.text.mix(s.panel, 0.18),
            text_muted: s.text.mix(s.panel, 0.30),
            accent: s.accent,
            accent_hover: s.accent.mix(WHITE, 0.06),
            accent_pressed: s.accent.mix(BLACK, 0.06),
            term_bg: s.term,
        }
    }
}

/// A seeded preset. Accents stay dark enough for the white text on buttons.
struct Preset {
    id: &'static str,
    name_en: &'static str,
    name_zh: &'static str,
    swatch: Rgb,
    dark: Seeds,
    light: Seeds,
}

pub(crate) const DEFAULT_ID: &str = "default";
pub(crate) const CUSTOM_ID: &str = "custom";
const DEFAULT_CUSTOM: Rgb = hex(0x4a90e2);

//                         root      panel     text      accent    term
const PRESETS: &[Preset] = &[
    Preset {
        id: "white",
        name_en: "White",
        name_zh: "纯白",
        swatch: hex(0xffffff),
        dark: seeds(0x1c1c1e, 0x232325, 0xececec, 0x6b7280, 0x111112),
        light: seeds(0xfafafa, 0xffffff, 0x1d1d1f, 0x3a3a3c, 0xffffff),
    },
    Preset {
        id: "mist-gray",
        name_en: "Mist Gray",
        name_zh: "雾灰",
        swatch: hex(0xd5dbe2),
        dark: seeds(0x1d2024, 0x24282d, 0xe4e8ed, 0x6f8196, 0x121417),
        light: seeds(0xf1f3f5, 0xf8f9fa, 0x1f2328, 0x55687d, 0xfbfcfd),
    },
    Preset {
        id: "titanium-gray",
        name_en: "Titanium Gray",
        name_zh: "钛灰",
        swatch: hex(0xc7c5c0),
        dark: seeds(0x1f1f1d, 0x272725, 0xe8e6e1, 0x827f77, 0x141412),
        light: seeds(0xefeeec, 0xf7f6f4, 0x22211f, 0x66635c, 0xfbfaf8),
    },
    Preset {
        id: "cobalt-blue",
        name_en: "Cobalt Blue",
        name_zh: "钴蓝",
        swatch: hex(0xc7d7f7),
        dark: seeds(0x141a26, 0x1a2130, 0xe3e9f5, 0x3d7eff, 0x0c111b),
        light: seeds(0xeef3fb, 0xf7faff, 0x172033, 0x1f5fd6, 0xfbfdff),
    },
    Preset {
        id: "sage-green",
        name_en: "Sage Green",
        name_zh: "鼠尾草绿",
        swatch: hex(0xcfe3d3),
        dark: seeds(0x171c18, 0x1e2520, 0xe2ebe4, 0x5a9169, 0x0f1310),
        light: seeds(0xeff4ef, 0xf7faf7, 0x1b261e, 0x467a55, 0xfbfdfb),
    },
    Preset {
        id: "warm-yellow",
        name_en: "Warm Yellow",
        name_zh: "暖黄",
        swatch: hex(0xf3dfae),
        dark: seeds(0x1f1c17, 0x26221c, 0xece4d6, 0xb8862b, 0x15130f),
        light: seeds(0xf7f2e8, 0xfcf9f3, 0x2a2418, 0x9c6c16, 0xfefcf8),
    },
    Preset {
        id: "sakura-pink",
        name_en: "Sakura Pink",
        name_zh: "樱花粉",
        swatch: hex(0xf7d3df),
        dark: seeds(0x201a1d, 0x282024, 0xf0e4ea, 0xc85a86, 0x161114),
        light: seeds(0xfbf1f4, 0xfff8fa, 0x2e1f26, 0xb84674, 0xfffbfc),
    },
    Preset {
        id: "vscode",
        name_en: "VS Code",
        name_zh: "VS Code",
        swatch: hex(0x007acc),
        dark: seeds(0x181818, 0x1f1f1f, 0xcccccc, 0x0078d4, 0x181818),
        light: seeds(0xf8f8f8, 0xffffff, 0x3b3b3b, 0x005fb8, 0xffffff),
    },
    Preset {
        id: "idea",
        name_en: "IDEA",
        name_zh: "IDEA",
        swatch: hex(0x5c6bc0),
        dark: seeds(0x2b2b2b, 0x3c3f41, 0xcbcbcb, 0x3d74c4, 0x2b2b2b),
        light: seeds(0xf2f2f2, 0xffffff, 0x1f1f1f, 0x2675bf, 0xffffff),
    },
    Preset {
        id: "xcode",
        name_en: "Xcode",
        name_zh: "Xcode",
        swatch: hex(0x1a8cff),
        dark: seeds(0x1f1f24, 0x292a30, 0xdfdfe0, 0x0a74e0, 0x1f1f24),
        light: seeds(0xf5f5f5, 0xffffff, 0x262626, 0x006fe6, 0xffffff),
    },
    Preset {
        id: "jetbrains",
        name_en: "JetBrains",
        name_zh: "JetBrains",
        swatch: hex(0x7b61ff),
        dark: seeds(0x1e1f22, 0x2b2d30, 0xdfe1e5, 0x7357e8, 0x1e1f22),
        light: seeds(0xf7f8fa, 0xffffff, 0x1e1f22, 0x6447d6, 0xffffff),
    },
    Preset {
        id: "cursor",
        name_en: "Cursor",
        name_zh: "Cursor",
        swatch: hex(0x6aa3ff),
        dark: seeds(0x141414, 0x1c1c1c, 0xd8d8d8, 0x3a7be0, 0x141414),
        light: seeds(0xf3f3f3, 0xfcfcfc, 0x1f1f1f, 0x2f6fd6, 0xffffff),
    },
    Preset {
        id: "claude-code",
        name_en: "Claude Code",
        name_zh: "Claude Code",
        swatch: hex(0xd97757),
        dark: seeds(0x262624, 0x30302e, 0xe8e6dc, 0xc2613f, 0x1f1e1d),
        light: seeds(0xf5f4ed, 0xfaf9f5, 0x3d3929, 0xb5552f, 0xfdfcf8),
    },
];

/// The custom theme: the default palette tinted toward `color`, which also
/// becomes the accent in both modes.
fn custom_palettes(color: Rgb) -> (Palette, Palette) {
    let d = default_dark();
    let l = default_light();
    let dark = derive(
        true,
        Seeds {
            root: d.bg_root.mix(color, 0.06),
            panel: d.bg_panel.mix(color, 0.06),
            text: d.text_primary,
            accent: color,
            term: d.term_bg.mix(color, 0.03),
        },
    );
    let light = derive(
        false,
        Seeds {
            root: l.bg_root.mix(color, 0.05),
            panel: l.bg_panel.mix(color, 0.03),
            text: l.text_primary,
            accent: color,
            term: l.term_bg,
        },
    );
    (dark, light)
}

/// A known preset id, or "default".
pub(crate) fn normalized_id(id: &str) -> &'static str {
    if id == CUSTOM_ID {
        return CUSTOM_ID;
    }
    PRESETS
        .iter()
        .find(|p| p.id == id)
        .map_or(DEFAULT_ID, |p| p.id)
}

/// Dark and light palettes for a theme id.
fn palettes(id: &str, custom: Rgb) -> (Palette, Palette) {
    match normalized_id(id) {
        CUSTOM_ID => custom_palettes(custom),
        DEFAULT_ID => (default_dark(), default_light()),
        id => {
            let preset = PRESETS.iter().find(|p| p.id == id).expect("normalized id");
            (derive(true, preset.dark), derive(false, preset.light))
        }
    }
}

/// Picker rows: default first, the presets, then custom.
fn items(custom: Rgb) -> Vec<ColorThemeItem> {
    let item = |id: &str, en: &str, zh: &str, swatch: Rgb| ColorThemeItem {
        id: id.into(),
        name_en: en.into(),
        name_zh: zh.into(),
        swatch: swatch.to_slint(),
    };
    let mut rows = vec![item(DEFAULT_ID, "Default", "默认", hex(0x4a90e2))];
    rows.extend(
        PRESETS
            .iter()
            .map(|p| item(p.id, p.name_en, p.name_zh, p.swatch)),
    );
    rows.push(item(CUSTOM_ID, "Custom", "自定义", custom));
    rows
}

pub(crate) fn custom_color(store: &ConfigStore) -> Rgb {
    Rgb::parse(store.color_theme_custom()).unwrap_or(DEFAULT_CUSTOM)
}

/// Push the saved colour theme and immersive switch into the main window.
pub(crate) fn apply_color_theme(window: &AppWindow, store: &ConfigStore) {
    let id = normalized_id(store.color_theme());
    let custom = custom_color(store);
    let (dark, light) = palettes(id, custom);
    window.set_theme_palette_dark(dark.to_slint());
    window.set_theme_palette_light(light.to_slint());
    window.set_color_theme(id.into());
    // Leave the field alone while it already holds this colour, so a hex being
    // typed is not rewritten under the caret.
    if Rgb::parse(window.get_color_theme_custom().as_str()) != Some(custom) {
        window.set_color_theme_custom(custom.to_hex().into());
    }
    window.set_color_themes(ModelRc::new(VecModel::from(items(custom))));
    window.set_immersive_colors(store.immersive_colors());
}
