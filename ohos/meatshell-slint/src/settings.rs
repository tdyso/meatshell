//! Settings page wiring (Settings › Interface, Terminal, Wallpaper & theme).
//!
//! The upstream `ui/app.slint` renders the whole settings card; this module
//! connects its callbacks to the upstream `ConfigStore` (verbatim engine
//! sources) so a change applies immediately and survives a restart. It mirrors
//! the relevant parts of upstream `src/app/settings_callbacks.rs` without the
//! desktop-only pieces (wallpapers, MCP, editors, detached windows).

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

use meatshell_core::config::ConfigStore;

use crate::AppWindow;

type Store = Option<Rc<RefCell<ConfigStore>>>;

/// Persist one preference through the store, logging a failed write.
macro_rules! persist {
    ($store:expr, |$s:ident| $body:expr) => {
        if let Some(store) = $store.as_ref() {
            let mut $s = store.borrow_mut();
            $body;
            if let Err(e) = $s.save() {
                eprintln!("meatshell-ohos: save settings: {e}");
            }
        }
    };
}

fn parse_hex_color(text: &str) -> Option<slint::Color> {
    let digits = text.trim().trim_start_matches('#');
    if digits.len() != 6 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(digits, 16).ok()?;
    Some(slint::Color::from_rgb_u8(
        (value >> 16) as u8,
        (value >> 8) as u8,
        value as u8,
    ))
}

/// Push every saved appearance / interface preference into a new window.
pub(crate) fn apply_saved_appearance(ui: &AppWindow, store: &ConfigStore) {
    // OHOS has no dark_light crate, so "system" follows the app default (dark).
    ui.set_dark_mode(store.theme_pref() != "light");

    let family = store.font_family().to_string();
    if !family.is_empty() {
        ui.set_term_font_family(family.into());
    }
    ui.set_term_font_size(store.font_size() as f32);
    ui.set_terminal_line_spacing(store.terminal_line_spacing());
    ui.set_term_font_bold(store.terminal_bold());
    ui.set_term_cursor_style(store.terminal_cursor_style().into());
    if let Some(color) = parse_hex_color(store.terminal_cursor_color()) {
        ui.set_term_cursor_color_hex(store.terminal_cursor_color().into());
        ui.set_term_cursor_color(color);
    }
    ui.set_session_log_enabled(store.session_log_enabled());
    ui.set_ui_scale(store.ui_scale() as f32 / 100.0);
    ui.set_panel_font(store.panel_font() as f32 / 100.0);
    // Only families that fontique actually registered can render; the embedded
    // UI font always exists, the system CJK family is registered at startup.
    ui.set_term_fonts(ModelRc::from(Rc::new(VecModel::from(vec![
        SharedString::from("Meatshell Mono"),
        SharedString::from("HarmonyOS Sans SC"),
    ]))));

    crate::color_theme::apply_color_theme(ui, store);

    ui.set_collapse_sidebar_default(store.collapse_sidebar_default());
    ui.set_collapse_sftp_default(store.collapse_sftp_default());
    ui.set_welcome_as_sidebar(store.welcome_as_sidebar());
    ui.set_quick_commands_as_sidebar(store.quick_commands_as_sidebar());
    ui.set_sftp_follow_cd(store.sftp_follow_cd());
    ui.set_download_always_ask(store.download_always_ask());
    ui.set_paste_confirm_enabled(store.paste_confirm_enabled());
    ui.set_extra_paste_shortcuts_enabled(store.extra_paste_shortcuts_enabled());
    ui.set_zen_mode(store.zen_mode());
    ui.set_ssh_keepalive_enabled(store.ssh_keepalive_enabled());
    ui.set_ssh_keepalive_interval(store.ssh_keepalive_interval() as i32);
    ui.set_ssh_keepalive_max(store.ssh_keepalive_max() as i32);

    // Remaining interface toggles mirrored by window properties. Renderer mode
    // is a launch-time choice on desktop; here it is only persisted so the
    // settings page round-trips.
    ui.set_json_format_output(store.json_format_output());
    ui.set_renderer_mode(SharedString::from(store.renderer_mode()));
    ui.set_cmd_bar_hidden(store.cmd_bar_hidden());
    ui.set_editor_embedded(store.editor_embedded());
    ui.set_sync_upload_enabled(store.sync_upload());
    ui.set_mcp_enabled(store.mcp_enabled());
    ui.set_mcp_use_saved_credentials(store.mcp_use_saved_credentials());
    ui.set_mcp_allow_commands(store.mcp_allow_commands());
    ui.set_mcp_allow_file_transfers(store.mcp_allow_file_transfers());

    // Output highlighting: the master switch, the built-in preset and the
    // custom rule list shown under Settings › Output.
    ui.set_output_highlight_enabled(store.output_highlight_enabled());
    ui.set_output_highlight_preset(SharedString::from(store.output_highlight_preset()));
    ui.set_output_highlight_rules(crate::output_highlight::model(store));
    ui.set_output_highlight_rule_status(SharedString::default());

    // Wallpaper + overlay are stored outside ConfigStore.
    let prefs = crate::wallpaper::load_prefs();
    crate::wallpaper::apply(ui, &prefs.id, prefs.overlay);
}

/// Recompile the renderer's rules after a setting change.
fn reload_rules(highlight: &Rc<RefCell<crate::output_highlight::Rules>>, store: &Store) {
    let Some(store) = store.as_ref() else { return };
    *highlight.borrow_mut() = crate::output_highlight::Rules::from_store(&store.borrow());
}

/// Connect the settings callbacks. Each one applies the value and saves it.
/// `highlight` is the renderer's compiled rule state; `repaint` rebuilds every
/// live terminal after a change that affects existing output.
pub(crate) fn register(
    ui: &AppWindow,
    store: Store,
    highlight: Rc<RefCell<crate::output_highlight::Rules>>,
    repaint: Rc<dyn Fn()>,
) {
    // Language: persisted only. The static @tr(...) strings come from the
    // embedded Chinese catalogue and Slint caches its bindings, so switching at
    // runtime cannot update them without a restart.
    {
        let store = store.clone();
        ui.on_set_language(move |lang: SharedString| {
            let lang = lang.to_string();
            persist!(store, |s| s.set_language(lang));
        });
    }

    // Dark / light switch (Settings and the toolbar toggle share it).
    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_toggle_theme(move || {
            let Some(store) = store.as_ref() else {
                return;
            };
            let dark = {
                let mut s = store.borrow_mut();
                let was_dark = s.theme_pref() != "light";
                s.set_theme_pref(if was_dark { "light" } else { "dark" }.to_string());
                if let Err(e) = s.save() {
                    eprintln!("meatshell-ohos: save settings: {e}");
                }
                !was_dark
            };
            if let Some(w) = weak.upgrade() {
                w.set_dark_mode(dark);
            }
        });
    }

    // Terminal font family / size / line spacing / bold / cursor.
    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_term_font(move |family: SharedString| {
            let family = family.to_string();
            if let Some(w) = weak.upgrade() {
                w.set_term_font_family(family.clone().into());
            }
            persist!(store, |s| s.set_font_family(family));
        });
    }
    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_term_font_size(move |size: i32| {
            let Some(store) = store.as_ref() else {
                return;
            };
            let size = {
                let mut s = store.borrow_mut();
                s.set_font_size(size.max(0) as u32);
                let size = s.font_size();
                if let Err(e) = s.save() {
                    eprintln!("meatshell-ohos: save settings: {e}");
                }
                size
            };
            if let Some(w) = weak.upgrade() {
                w.set_term_font_size(size as f32);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_terminal_line_spacing(move |spacing: f32| {
            let Some(store) = store.as_ref() else {
                return;
            };
            let spacing = {
                let mut s = store.borrow_mut();
                s.set_terminal_line_spacing(spacing);
                let spacing = s.terminal_line_spacing();
                if let Err(e) = s.save() {
                    eprintln!("meatshell-ohos: save settings: {e}");
                }
                spacing
            };
            if let Some(w) = weak.upgrade() {
                w.set_terminal_line_spacing(spacing);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_term_font_bold(move |bold: bool| {
            if let Some(w) = weak.upgrade() {
                w.set_term_font_bold(bold);
            }
            persist!(store, |s| s.set_terminal_bold(bold));
        });
    }
    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_term_cursor_style(move |style: SharedString| {
            let style = style.to_string();
            if let Some(w) = weak.upgrade() {
                w.set_term_cursor_style(style.clone().into());
            }
            persist!(store, |s| s.set_terminal_cursor_style(style));
        });
    }
    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_term_cursor_color(move |text: SharedString| -> bool {
            let Some(store) = store.as_ref() else {
                return false;
            };
            let Some(color) = parse_hex_color(text.as_str()) else {
                return false;
            };
            let normalized = {
                let mut s = store.borrow_mut();
                if !s.set_terminal_cursor_color(text.as_str()) {
                    return false;
                }
                let normalized = s.terminal_cursor_color().to_string();
                if let Err(e) = s.save() {
                    eprintln!("meatshell-ohos: save settings: {e}");
                }
                normalized
            };
            if let Some(w) = weak.upgrade() {
                w.set_term_cursor_color_hex(normalized.into());
                w.set_term_cursor_color(color);
            }
            true
        });
    }

    // UI zoom and settings-panel font scale (percent).
    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_ui_scale(move |percent: i32| {
            let Some(store) = store.as_ref() else {
                return;
            };
            let scale = {
                let mut s = store.borrow_mut();
                s.set_ui_scale(percent.max(0) as u32);
                let scale = s.ui_scale() as f32 / 100.0;
                if let Err(e) = s.save() {
                    eprintln!("meatshell-ohos: save settings: {e}");
                }
                scale
            };
            if let Some(w) = weak.upgrade() {
                w.set_ui_scale(scale);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_panel_font(move |percent: i32| {
            let Some(store) = store.as_ref() else {
                return;
            };
            let scale = {
                let mut s = store.borrow_mut();
                s.set_panel_font(percent.max(0) as u32);
                let scale = s.panel_font() as f32 / 100.0;
                if let Err(e) = s.save() {
                    eprintln!("meatshell-ohos: save settings: {e}");
                }
                scale
            };
            if let Some(w) = weak.upgrade() {
                w.set_panel_font(scale);
            }
        });
    }

    // Colour theme preset, custom colour and the immersive switch.
    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_color_theme(move |id: SharedString| {
            let Some(store) = store.as_ref() else {
                return;
            };
            let id = crate::color_theme::normalized_id(id.as_str());
            {
                let mut s = store.borrow_mut();
                s.set_color_theme(if id == crate::color_theme::DEFAULT_ID { "" } else { id });
                if let Err(e) = s.save() {
                    eprintln!("meatshell-ohos: save settings: {e}");
                }
            }
            if let Some(w) = weak.upgrade() {
                crate::color_theme::apply_color_theme(&w, &store.borrow());
            }
        });
    }
    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_color_theme_custom(move |text: SharedString| -> bool {
            let Some(store) = store.as_ref() else {
                return false;
            };
            let Some(color) = crate::color_theme::Rgb::parse(text.as_str()) else {
                return false;
            };
            {
                let mut s = store.borrow_mut();
                // Unchanged while already selected: keep the field's caret.
                if crate::color_theme::Rgb::parse(s.color_theme_custom()) == Some(color)
                    && s.color_theme() == crate::color_theme::CUSTOM_ID
                {
                    return true;
                }
                s.set_color_theme_custom(color.to_hex());
                s.set_color_theme(crate::color_theme::CUSTOM_ID);
                if let Err(e) = s.save() {
                    eprintln!("meatshell-ohos: save settings: {e}");
                }
            }
            if let Some(w) = weak.upgrade() {
                crate::color_theme::apply_color_theme(&w, &store.borrow());
            }
            true
        });
    }
    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_immersive_colors(move |on: bool| {
            if let Some(w) = weak.upgrade() {
                w.set_immersive_colors(on);
                // Turning immersive on should recolour with the current
                // wallpaper right away instead of waiting for the next pick.
                let prefs = crate::wallpaper::load_prefs();
                crate::wallpaper::apply(&w, &prefs.id, None);
            }
            persist!(store, |s| s.set_immersive_colors(on));
        });
    }

    // Wallpaper swatches, the custom-file picker and the overlay slider. The
    // picked id lives outside ConfigStore, so it is stored as two small files
    // in the sandbox (see `wallpaper::{load_prefs, save_prefs}`).
    {
        let weak = ui.as_weak();
        ui.on_set_wallpaper(move |id: SharedString| {
            let id = id.to_string();
            crate::wallpaper::save_prefs(&id, None);
            if let Some(w) = weak.upgrade() {
                crate::wallpaper::apply(&w, &id, None);
            }
        });
    }
    {
        ui.on_pick_wallpaper_file(|| crate::ohos_app::request_wallpaper_pick());
    }
    {
        let weak = ui.as_weak();
        ui.on_persist_wallpaper_overlay(move |alpha: f32| {
            if let Some(w) = weak.upgrade() {
                w.set_wallpaper_overlay(alpha);
            }
            let prefs = crate::wallpaper::load_prefs();
            crate::wallpaper::save_prefs(&prefs.id, Some(alpha));
        });
    }

    // Layout defaults and interface toggles.
    {
        let store = store.clone();
        ui.on_set_collapse_sidebar_default(move |on: bool| {
            persist!(store, |s| s.set_collapse_sidebar_default(on));
        });
    }
    {
        let store = store.clone();
        ui.on_set_collapse_sftp_default(move |on: bool| {
            persist!(store, |s| s.set_collapse_sftp_default(on));
        });
    }
    {
        let store = store.clone();
        ui.on_set_welcome_as_sidebar(move |on: bool| {
            persist!(store, |s| s.set_welcome_as_sidebar(on));
        });
    }
    {
        let store = store.clone();
        ui.on_set_quick_commands_as_sidebar(move |on: bool| {
            persist!(store, |s| s.set_quick_commands_as_sidebar(on));
        });
    }
    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_sftp_follow_cd(move |follow: bool| {
            if let Some(w) = weak.upgrade() {
                w.set_sftp_follow_cd(follow);
            }
            persist!(store, |s| s.set_sftp_follow_cd(follow));
        });
    }
    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_download_always_ask(move |ask: bool| {
            if let Some(w) = weak.upgrade() {
                w.set_download_always_ask(ask);
            }
            persist!(store, |s| s.set_download_always_ask(ask));
        });
    }
    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_paste_confirm_enabled(move |on: bool| {
            if let Some(w) = weak.upgrade() {
                w.set_paste_confirm_enabled(on);
            }
            persist!(store, |s| s.set_paste_confirm_enabled(on));
        });
    }
    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_extra_paste_shortcuts_enabled(move |on: bool| {
            if let Some(w) = weak.upgrade() {
                w.set_extra_paste_shortcuts_enabled(on);
            }
            persist!(store, |s| s.set_extra_paste_shortcuts_enabled(on));
        });
    }
    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_zen_mode(move |on: bool| {
            if let Some(w) = weak.upgrade() {
                w.set_zen_mode(on);
            }
            persist!(store, |s| s.set_zen_mode(on));
        });
    }
    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_session_log_enabled(move |on: bool| {
            if let Some(w) = weak.upgrade() {
                w.set_session_log_enabled(on);
            }
            persist!(store, |s| s.set_session_log_enabled(on));
        });
    }
    // SSH keepalive (#515): the store pushes the values into the SSH layer, so
    // echo back the clamped values it kept.
    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_ssh_keepalive(move |enabled: bool, interval: i32, max: i32| {
            let Some(store) = store.as_ref() else {
                return;
            };
            let (enabled, interval, max) = {
                let mut s = store.borrow_mut();
                s.set_ssh_keepalive(enabled, interval.max(0) as u32, max.max(0) as u32);
                let values = (
                    s.ssh_keepalive_enabled(),
                    s.ssh_keepalive_interval(),
                    s.ssh_keepalive_max(),
                );
                if let Err(e) = s.save() {
                    eprintln!("meatshell-ohos: save settings: {e}");
                }
                values
            };
            if let Some(w) = weak.upgrade() {
                w.set_ssh_keepalive_enabled(enabled);
                w.set_ssh_keepalive_interval(interval as i32);
                w.set_ssh_keepalive_max(max as i32);
            }
        });
    }

    // Output highlighting: master switch + built-in preset.
    {
        let weak = ui.as_weak();
        let store = store.clone();
        let highlight = highlight.clone();
        let repaint = repaint.clone();
        ui.on_set_output_highlight(move |enabled: bool, preset: SharedString| {
            let requested = preset.to_string();
            persist!(store, |s| {
                s.set_output_highlight_enabled(enabled);
                s.set_output_highlight_preset(requested);
            });
            reload_rules(&highlight, &store);
            let clamped = store
                .as_ref()
                .map(|s| s.borrow().output_highlight_preset().to_string())
                .unwrap_or_else(|| "log".to_string());
            if let Some(w) = weak.upgrade() {
                w.set_output_highlight_enabled(enabled);
                w.set_output_highlight_preset(SharedString::from(clamped));
            }
            repaint();
        });
    }
    {
        let weak = ui.as_weak();
        let store = store.clone();
        let highlight = highlight.clone();
        let repaint = repaint.clone();
        ui.on_add_output_highlight_rule(
            move |pattern: SharedString,
                  is_regex: bool,
                  case_sensitive: bool,
                  whole_line: bool,
                  color: SharedString|
                  -> bool {
                let pattern = pattern.trim().to_string();
                let Some(handle) = store.as_ref() else {
                    return false;
                };
                let Some(w) = weak.upgrade() else {
                    return false;
                };
                if let Err(message) =
                    crate::output_highlight::validate_rule(&pattern, is_regex, case_sensitive)
                {
                    w.set_output_highlight_rule_status(SharedString::from(message));
                    return false;
                }
                if crate::output_highlight::at_rule_limit(&handle.borrow()) {
                    w.set_output_highlight_rule_status(SharedString::from(
                        "自定义规则最多 128 条",
                    ));
                    return false;
                }
                {
                    let mut s = handle.borrow_mut();
                    s.add_output_highlight_rule(crate::output_highlight::rule_from_fields(
                        pattern,
                        is_regex,
                        case_sensitive,
                        whole_line,
                        color.to_string(),
                    ));
                    if let Err(e) = s.save() {
                        eprintln!("meatshell-ohos: save settings: {e}");
                    }
                    w.set_output_highlight_rules(crate::output_highlight::model(&s));
                }
                w.set_output_highlight_rule_status(SharedString::default());
                reload_rules(&highlight, &store);
                repaint();
                true
            },
        );
    }
    {
        let weak = ui.as_weak();
        let store = store.clone();
        let highlight = highlight.clone();
        let repaint = repaint.clone();
        ui.on_remove_output_highlight_rule(move |index: i32| {
            let Some(handle) = store.as_ref() else {
                return;
            };
            {
                let mut s = handle.borrow_mut();
                s.remove_output_highlight_rule(index.max(0) as usize);
                if let Err(e) = s.save() {
                    eprintln!("meatshell-ohos: save settings: {e}");
                }
                if let Some(w) = weak.upgrade() {
                    w.set_output_highlight_rules(crate::output_highlight::model(&s));
                    w.set_output_highlight_rule_status(SharedString::default());
                }
            }
            reload_rules(&highlight, &store);
            repaint();
        });
    }
    {
        let weak = ui.as_weak();
        let store = store.clone();
        let highlight = highlight.clone();
        let repaint = repaint.clone();
        ui.on_set_output_highlight_rule_enabled(move |index: i32, enabled: bool| {
            let Some(handle) = store.as_ref() else {
                return;
            };
            {
                let mut s = handle.borrow_mut();
                s.set_output_highlight_rule_enabled(index.max(0) as usize, enabled);
                if let Err(e) = s.save() {
                    eprintln!("meatshell-ohos: save settings: {e}");
                }
                if let Some(w) = weak.upgrade() {
                    w.set_output_highlight_rules(crate::output_highlight::model(&s));
                }
            }
            reload_rules(&highlight, &store);
            repaint();
        });
    }

    // ———————— remaining interface toggles ————————
    // Toolbar toggle: hide the quick-command bar. Persisted and echoed back to
    // the window property the bar binds to.
    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_cmd_bar_hidden(move |hidden: bool| {
            if let Some(w) = weak.upgrade() {
                w.set_cmd_bar_hidden(hidden);
            }
            persist!(store, |s| s.set_cmd_bar_hidden(hidden));
        });
    }
    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_json_format_output(move |enabled: bool| {
            if let Some(w) = weak.upgrade() {
                w.set_json_format_output(enabled);
            }
            persist!(store, |s| s.set_json_format_output(enabled));
        });
    }
    // Renderer choice is consumed before the first window on desktop, so only
    // the preference is stored here (applied on the next launch).
    {
        let store = store.clone();
        ui.on_set_renderer_mode(move |mode: SharedString| {
            let mode = mode.to_string();
            persist!(store, |s| s.set_renderer_mode(mode));
        });
    }
    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_editor_embedded(move |embedded: bool| {
            if let Some(w) = weak.upgrade() {
                w.set_editor_embedded(embedded);
            }
            persist!(store, |s| s.set_editor_embedded(embedded));
        });
    }
    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_sync_upload_enabled(move |enabled: bool| {
            if let Some(w) = weak.upgrade() {
                w.set_sync_upload_enabled(enabled);
            }
            persist!(store, |s| s.set_sync_upload(enabled));
        });
    }
    // Session-sync / broadcast input: hot-path flag read by the key handler.
    {
        ui.on_set_sync_input(|on: bool| crate::ohos_app::set_sync_input(on));
    }
    {
        let store = store.clone();
        ui.on_set_mcp_permissions(
            move |enabled: bool, credentials: bool, commands: bool, files: bool| {
                persist!(store, |s| {
                    s.set_mcp_enabled(enabled);
                    s.set_mcp_use_saved_credentials(credentials);
                    s.set_mcp_allow_commands(commands);
                    s.set_mcp_allow_file_transfers(files);
                });
            },
        );
    }
}
