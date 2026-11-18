//! meatshell on OpenHarmony: the real upstream UI (AppWindow) driven by the
//! Slint software renderer.
//!
//! This crate depends on Slint (software renderer, parley text layout) and
//! the engine (`meatshell-core`, upstream sources verbatim). OHOS window /
//! buffer handling lives in the C++ NAPI layer; the renderer writes
//! premultiplied RGBA pixels into a caller-owned buffer.
//!
//! The UI glue lives in `ohos_app`: it instantiates `AppWindow` (compiled
//! from the upstream `ui/app.slint` by build.rs) and feeds it the saved
//! session list from the upstream config store.

mod color_theme;
mod dock_layout;
mod i18n;
mod ohos_app;
mod output_highlight;
mod quick_commands;
mod resource_stats;
mod session_editor;
mod settings;
mod sftp_client;
mod ssh_session;
mod terminal;
mod tunnels;
mod wallpaper;

use slint::platform::software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor};
use slint::platform::{Clipboard, Platform, WindowAdapter};
use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::rc::Rc;
use std::sync::{Mutex, OnceLock};

slint::include_modules!();

use ohos_app::UiState;

// The C++ bridge (`napi_init.cpp`) calls the engine FFI entry points directly.
// Release builds use thin LTO, which drops code that is unreachable from an
// externally visible symbol, so this export references every engine entry
// point.  It is never called; it only keeps those symbols inside the single
// Rust staticlib.
#[no_mangle]
pub extern "C" fn ms_engine_link_keepalive() -> usize {
    let entries: [usize; 7] = [
        meatshell_core::ms_version as *const () as usize,
        meatshell_core::ms_selftest as *const () as usize,
        meatshell_core::ms_selftest_detail as *const () as usize,
        meatshell_core::ms_tcp_probe as *const () as usize,
        meatshell_core::ms_last_error as *const () as usize,
        meatshell_core::ms_ssh_exec as *const () as usize,
        meatshell_core::ms_ssh_output as *const () as usize,
    ];
    entries.iter().sum()
}

const FONT_CANDIDATES: &[&str] = &[
    "/system/fonts/HarmonyOS_Sans_SC.ttf",
    "/system/fonts/HarmonyOS_Sans_SC_Regular.ttf",
    "/system/fonts/NotoSansCJK-Regular.ttc",
];

static LAST_ERROR: Mutex<Option<CString>> = Mutex::new(None);

/// HAP sandbox data directory for the upstream config store, handed over by
/// ArkTS (`context.filesDir`) through the C++ bridge before first init.
static DATA_DIR: OnceLock<String> = OnceLock::new();

thread_local! {
    static UI: RefCell<Option<UiState>> = const { RefCell::new(None) };
}

pub(crate) struct OhosPlatform {
    pub window: Rc<MinimalSoftwareWindow>,
}

impl Platform for OhosPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> std::time::Duration {
        // Must be a monotonically advancing clock anchored at process start:
        // Slint computes timer deadlines against this value, so returning
        // `Instant::now().elapsed()` (always ~0) froze every Timer and
        // animation forever.
        static START: OnceLock<std::time::Instant> = OnceLock::new();
        START.get_or_init(std::time::Instant::now).elapsed()
    }

    /// Ctrl+V inside a Slint `TextInput` (command bar, find bar, editors).
    ///
    /// OHOS has no synchronous pasteboard read: without READ_PASTEBOARD the
    /// system returns an empty `PasteData`, and the only grant path is the
    /// security PasteButton. So this serves the last text the app has seen and,
    /// when there is none, asks ArkTS for a real read through the chip — the
    /// payload then arrives via `slintClipboardText` and is committed into the
    /// focused item.
    fn clipboard_text(&self, _clipboard: Clipboard) -> Option<String> {
        ohos_app::clipboard_cache_text()
    }

    /// Ctrl+C / Ctrl+X inside a Slint `TextInput`; ArkTS writes the system
    /// pasteboard when it polls the `clipboard-set` action.
    fn set_clipboard_text(&self, text: &str, _clipboard: Clipboard) {
        ohos_app::publish_clipboard_text(text);
    }
}

/// Registers a CJK system font and fills in fontique's *generic families*.
///
/// OHOS has no fontconfig, so `fontique::Collection::new(system_fonts: true)`
/// discovers no families and leaves SansSerif/SystemUi/UiSansSerif empty.
/// Anything that queries a font without an explicit family then gets no
/// candidate: `i_slint_renderer_software::fonts::systemfonts::fallbackfont`
/// does `query_fontique(...).unwrap()`, which aborts the process (observed
/// while instantiating a TextInput). Pointing the generic families at the
/// registered family makes those implicit requests resolvable again.
pub(crate) fn register_system_font() -> Result<(), String> {
    use slint::fontique_011::fontique::{FallbackKey, GenericFamily, Script};
    for path in FONT_CANDIDATES {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let blob = slint::fontique_011::fontique::Blob::new(std::sync::Arc::new(bytes));
        let mut collection = slint::fontique_011::shared_collection();
        let registered = collection.register_fonts(blob, None);
        if registered.is_empty() {
            continue;
        }
        let families: Vec<_> = registered.iter().map(|(family_id, _)| *family_id).collect();
        // `Collection` is created with `shared: true`, so these mutations are
        // visible through the clone the platform's FontContext holds.
        for generic in [
            GenericFamily::SansSerif,
            GenericFamily::SystemUi,
            GenericFamily::UiSansSerif,
            GenericFamily::Serif,
            GenericFamily::Cursive,
            GenericFamily::Fantasy,
        ] {
            collection.set_generic_families(generic, families.iter().copied());
        }
        // OHOS has no fontconfig, so fontique's system backend cannot answer
        // `fallback(selector)`; `fallback_families(script)` stays empty and any
        // glyph missing from the explicit family (the terminal uses
        // "Meatshell Mono", which has no CJK coverage) is shaped as .notdef and
        // drawn as `?`. Point the CJK scripts at the family registered above.
        for script in [
            Script::from_bytes(*b"Hani"), // Han ideographs
            Script::from_bytes(*b"Hang"), // Hangul
            Script::from_bytes(*b"Hira"), // Hiragana
            Script::from_bytes(*b"Kana"), // Katakana
            Script::from_bytes(*b"Bopo"), // Bopomofo
        ] {
            let _ =
                collection.set_fallbacks(FallbackKey::new(script, None), families.iter().copied());
        }
        return Ok(());
    }
    Err(format!(
        "no usable system font found (tried {})",
        FONT_CANDIDATES.join(", ")
    ))
}

/// The terminal/editor font is embedded by the UI (`ui/fonts/MeatshellMono-*`),
/// so it only exists after `AppWindow::new()` has run the generated font
/// registration. Append it to the Monospace generic family afterwards.
pub(crate) fn register_monospace_generic() {
    use slint::fontique_011::fontique::GenericFamily;
    let mut collection = slint::fontique_011::shared_collection();
    let ids: Vec<_> = ["Meatshell Mono"]
        .iter()
        .filter_map(|name| collection.family_by_name(name).map(|family| family.id()))
        .collect();
    if !ids.is_empty() {
        collection.append_generic_families(GenericFamily::Monospace, ids.iter().copied());
    }
}

fn set_last_error(message: impl Into<String>) {
    let message = message.into();
    let c_string = CString::new(message).unwrap_or_else(|_| CString::new("unknown").unwrap());
    *LAST_ERROR.lock().unwrap() = Some(c_string);
}

fn clear_last_error() {
    *LAST_ERROR.lock().unwrap() = None;
}

fn with_ui<T>(width: i32, height: i32, f: impl FnOnce(&mut UiState) -> T) -> Result<T, String> {
    if width <= 0 || height <= 0 {
        return Err(format!("invalid size {width}x{height}"));
    }
    UI.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(UiState::new(
                width as usize,
                height as usize,
                DATA_DIR.get().map(|s| s.as_str()),
            )?);
        }
        let state = slot.as_mut().unwrap();
        if state.width != width as usize || state.height != height as usize {
            state.resize(width as usize, height as usize);
        }
        Ok(f(state))
    })
}

fn write_ppm(
    path: &str,
    pixels: &[PremultipliedRgbaColor],
    width: usize,
    height: usize,
) -> Result<(), String> {
    let mut out = Vec::with_capacity(width * height * 3 + 32);
    out.extend_from_slice(format!("P6\n{width} {height}\n255\n").as_bytes());
    for pixel in pixels.iter().take(width * height) {
        out.push(pixel.red);
        out.push(pixel.green);
        out.push(pixel.blue);
    }
    std::fs::write(path, out).map_err(|e| format!("write {path}: {e}"))
}

/// Diagnostic: render one frame into a PPM file.  Used to inspect the real
/// AppWindow without the HAP display stack.
#[no_mangle]
pub extern "C" fn ms_spike_render_ppm(
    path: *const c_char,
    width: i32,
    height: i32,
) -> i32 {
    if path.is_null() {
        set_last_error("null path");
        return -1;
    }
    let path = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(path) => path,
        Err(e) => {
            set_last_error(format!("invalid path: {e}"));
            return -1;
        }
    };
    let result = with_ui(width, height, |state| {
        // Rendering is event-driven, so force a repaint before dumping a frame.
        state.request_redraw();
        let mut pixels = vec![PremultipliedRgbaColor::default(); width as usize * height as usize];
        state.render_to(&mut pixels, width as usize);
        write_ppm(path, &pixels, width as usize, height as usize)
    });
    match result {
        Ok(Ok(())) => {
            clear_last_error();
            0
        }
        Ok(Err(e)) => {
            set_last_error(e);
            -1
        }
        Err(e) => {
            set_last_error(e);
            -1
        }
    }
}

/// Prepare the real AppWindow on the calling thread.  The HAP render thread
/// calls this once; subsequent keyboard/pointer events must come from the
/// same thread.
///
/// `data_dir` is the HAP sandbox files directory (from ArkTS
/// `context.filesDir`); the upstream config store keeps sessions.json there.
#[no_mangle]
pub extern "C" fn ms_spike_init(width: i32, height: i32, data_dir: *const c_char) -> i32 {
    if !data_dir.is_null() {
        if let Ok(dir) = unsafe { CStr::from_ptr(data_dir) }.to_str() {
            let _ = DATA_DIR.set(dir.to_string());
        }
    }
    match with_ui(width, height, |_| ()) {
        Ok(()) => {
            clear_last_error();
            0
        }
        Err(e) => {
            set_last_error(e);
            -1
        }
    }
}

/// Resize the Slint window.  Must be called on the render thread.
#[no_mangle]
pub extern "C" fn ms_spike_resize(width: i32, height: i32) -> i32 {
    match with_ui(width, height, |state| state.resize(width as usize, height as usize)) {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(e);
            -1
        }
    }
}

/// Render one frame into a caller-owned RGBA buffer.  `stride_pixels` is the
/// number of 4-byte pixels in one row of `pixels` (may include padding).
/// Returns 1 if Slint produced a frame, 0 if no redraw was needed, -1 on error.
#[no_mangle]
pub extern "C" fn ms_spike_render_to(
    pixels: *mut u8,
    width: i32,
    height: i32,
    stride_pixels: i32,
) -> i32 {
    if pixels.is_null() || width <= 0 || height <= 0 || stride_pixels < width {
        set_last_error(format!(
            "invalid render target pixels={pixels:p} {width}x{height} stride={stride_pixels}"
        ));
        return -1;
    }
    let stride = stride_pixels as usize;
    let length = stride * height as usize;
    let slice = unsafe { std::slice::from_raw_parts_mut(pixels as *mut PremultipliedRgbaColor, length) };
    match with_ui(width, height, |state| state.render_to(slice, stride)) {
        Ok(rendered) => {
            clear_last_error();
            if rendered { 1 } else { 0 }
        }
        Err(e) => {
            set_last_error(e);
            -1
        }
    }
}

/// Feed a key to Slint.  Must be called from the render thread.
#[no_mangle]
pub extern "C" fn ms_spike_key(text: *const c_char, pressed: i32) -> i32 {
    let text = if text.is_null() {
        String::new()
    } else {
        match unsafe { CStr::from_ptr(text) }.to_str() {
            Ok(text) => text.to_string(),
            Err(e) => {
                set_last_error(format!("invalid key text: {e}"));
                return -1;
            }
        }
    };
    let result = UI.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(state) = slot.as_mut() else {
            return Err("ms_spike_key called before ms_spike_init".to_string());
        };
        state.key(&text, pressed != 0);
        Ok(())
    });
    match result {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(e);
            -1
        }
    }
}

/// Text committed by the OHOS input method (Chinese IME).  Must be called
/// from the render thread.
#[no_mangle]
pub extern "C" fn ms_spike_ime_commit(text: *const c_char) -> i32 {
    if text.is_null() {
        set_last_error("null ime text");
        return -1;
    }
    let text = match unsafe { CStr::from_ptr(text) }.to_str() {
        Ok(text) => text.to_string(),
        Err(e) => {
            set_last_error(format!("invalid ime text: {e}"));
            return -1;
        }
    };
    let result = UI.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(state) = slot.as_mut() else {
            return Err("ms_spike_ime_commit called before ms_spike_init".to_string());
        };
        state.ime_commit(&text);
        Ok(())
    });
    match result {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(e);
            -1
        }
    }
}

/// Pointer event: 0=press, 1=release, 2=move, 3=exit (cancel the press in
/// flight).  `button` is 0=left, 1=right, 2=middle (matches ArkTS
/// `MouseButton`).  Must be called from the render thread.
#[no_mangle]
pub extern "C" fn ms_spike_pointer(x: f32, y: f32, kind: i32, button: i32) -> i32 {
    let result = UI.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(state) = slot.as_mut() else {
            return Err("ms_spike_pointer called before ms_spike_init".to_string());
        };
        state.pointer(x, y, kind, button);
        Ok(())
    });
    match result {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(e);
            -1
        }
    }
}

/// Touch contact at a logical position. `phase` is 0=Started, 1=Moved,
/// 2=Ended, 3=Cancelled. Must be called from the render thread.
#[no_mangle]
pub extern "C" fn ms_spike_touch(x: f32, y: f32, phase: i32) -> i32 {
    let result = UI.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(state) = slot.as_mut() else {
            return Err("ms_spike_touch called before ms_spike_init".to_string());
        };
        state.touch(x, y, phase);
        Ok(())
    });
    match result {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(e);
            -1
        }
    }
}

/// Mouse wheel / trackpad scroll at a logical position.  Must be called from
/// the render thread.
#[no_mangle]
pub extern "C" fn ms_spike_scroll(x: f32, y: f32, delta_x: f32, delta_y: f32) -> i32 {
    let result = UI.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(state) = slot.as_mut() else {
            return Err("ms_spike_scroll called before ms_spike_init".to_string());
        };
        state.scroll(x, y, delta_x, delta_y);
        Ok(())
    });
    match result {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(e);
            -1
        }
    }
}

thread_local! {
    /// Keeps the last action string alive until the next poll: ArkTS reads the
    /// pointer immediately after `ms_spike_take_action` returns.
    static LAST_ACTION: RefCell<Option<CString>> = const { RefCell::new(None) };
}

/// Hands the next pending window action ("minimize" / "maximize-toggle" /
/// "close") to the ArkTS side. Returns NULL when there is nothing to do.
/// Must be called from the render thread.
#[no_mangle]
pub extern "C" fn ms_spike_take_action() -> *const c_char {
    let action = UI.with(|slot| {
        slot.borrow_mut().as_mut().and_then(|state| state.take_action())
    });
    let Some(action) = action else {
        return std::ptr::null();
    };
    let action = CString::new(action).unwrap_or_default();
    LAST_ACTION.with(|slot| {
        let mut slot = slot.borrow_mut();
        *slot = Some(action);
        slot.as_ref().map(|action| action.as_ptr()).unwrap_or(std::ptr::null())
    })
}

/// Pending clipboard text produced by a copy action. ArkTS reads it right
/// after `ms_spike_take_action` returns "clipboard-set" and must free the
/// pointer with `ms_spike_free_string`. Returns NULL when nothing is pending.
#[no_mangle]
pub extern "C" fn ms_spike_clipboard_take() -> *mut c_char {
    match ohos_app::take_clipboard_out() {
        Some(text) => CString::new(text)
            .map(CString::into_raw)
            .unwrap_or(std::ptr::null_mut()),
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn ms_spike_free_string(ptr: *mut c_char) {
    if !ptr.is_null() {
        unsafe { drop(CString::from_raw(ptr)) };
    }
}

/// Clipboard payload read by ArkTS, completing a pending 粘贴 request.
/// Must be called from the render thread.
#[no_mangle]
pub extern "C" fn ms_spike_clipboard_text(text: *const c_char) -> i32 {
    if text.is_null() {
        set_last_error("null clipboard text");
        return -1;
    }
    let text = match unsafe { CStr::from_ptr(text) }.to_str() {
        Ok(text) => text,
        Err(e) => {
            set_last_error(format!("invalid clipboard text: {e}"));
            return -1;
        }
    };
    match ohos_app::clipboard_text_from_arkts(text) {
        Ok(()) => {
            clear_last_error();
            0
        }
        Err(e) => {
            set_last_error(e);
            -1
        }
    }
}

/// Mirrors the OHOS window's maximize state into Slint's custom title bar.
/// Must be called from the render thread.
#[no_mangle]
pub extern "C" fn ms_spike_set_maximized(maximized: i32) -> i32 {
    let result = UI.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(state) = slot.as_mut() else {
            return Err("ms_spike_set_maximized called before ms_spike_init".to_string());
        };
        state.set_maximized(maximized != 0);
        Ok(())
    });
    match result {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(e);
            -1
        }
    }
}

/// Sets the public Download directory resolved by ArkTS
/// (`environment.getUserDownloadDir`) after the download-directory permission
/// was granted. Must be called from the render thread.
#[no_mangle]
pub extern "C" fn ms_spike_set_download_dir(dir: *const c_char) -> i32 {
    if dir.is_null() {
        set_last_error("null download dir");
        return -1;
    }
    let dir = match unsafe { CStr::from_ptr(dir) }.to_str() {
        Ok(dir) => dir.to_string(),
        Err(e) => {
            set_last_error(format!("invalid download dir: {e}"));
            return -1;
        }
    };
    UI.with(|slot| match slot.borrow_mut().as_mut() {
        Some(state) => state.set_download_dir(&dir),
        // Queued before the first frame: remember it so the first UI starts there.
        None => ohos_app::set_public_download_dir(&dir),
    });
    0
}

/// Uploads the files ArkTS picked through the system document picker.
/// `paths` is a `\n`-separated list of sandbox copies (the picker URIs were
/// copied into `<filesDir>/upload-staging` by ArkTS); a directory entry is
/// uploaded recursively by the engine. Must be called from the render thread.
#[no_mangle]
pub extern "C" fn ms_spike_upload_files(paths: *const c_char) -> i32 {
    if paths.is_null() {
        set_last_error("null upload paths");
        return -1;
    }
    let paths = match unsafe { CStr::from_ptr(paths) }.to_str() {
        Ok(paths) => paths,
        Err(e) => {
            set_last_error(format!("invalid upload paths: {e}"));
            return -1;
        }
    };
    let files: Vec<std::path::PathBuf> = paths
        .split('\n')
        .filter(|path| !path.is_empty())
        .map(std::path::PathBuf::from)
        .collect();
    let result = UI.with(|slot| match slot.borrow_mut().as_mut() {
        Some(state) => {
            state.upload_picked_files(&files);
            Ok(())
        }
        None => Err("ms_spike_upload_files called before ms_spike_init".to_string()),
    });
    match result {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(e);
            -1
        }
    }
}

/// The ArkTS picker was dismissed without choosing anything.
#[no_mangle]
pub extern "C" fn ms_spike_upload_cancel() -> i32 {
    UI.with(|slot| match slot.borrow_mut().as_mut() {
        Some(state) => {
            state.upload_pick_cancelled();
            0
        }
        None => {
            set_last_error("ms_spike_upload_cancel called before ms_spike_init".to_string());
            -1
        }
    })
}

/// ArkTS copied the JSON picked for 设置 → 导入连接 into the sandbox.
#[no_mangle]
pub extern "C" fn ms_spike_import_picked(path: *const c_char) -> i32 {
    if path.is_null() {
        set_last_error("null import path".to_string());
        return -1;
    }
    let path = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(p) => p,
        Err(e) => {
            set_last_error(format!("invalid import path: {e}"));
            return -1;
        }
    };
    match ohos_app::import_picked_file(std::path::Path::new(path)) {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(e);
            -1
        }
    }
}

/// ArkTS copied the private key picked in the session editor into the sandbox.
#[no_mangle]
pub extern "C" fn ms_spike_key_picked(path: *const c_char) -> i32 {
    if path.is_null() {
        set_last_error("null key path".to_string());
        return -1;
    }
    let path = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(p) => p,
        Err(e) => {
            set_last_error(format!("invalid key path: {e}"));
            return -1;
        }
    };
    match ohos_app::key_picked_file(std::path::Path::new(path)) {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(e);
            -1
        }
    }
}

/// ArkTS copied the wallpaper photo picked in 设置 → 壁纸 into the sandbox.
#[no_mangle]
pub extern "C" fn ms_spike_wallpaper_picked(path: *const c_char) -> i32 {
    if path.is_null() {
        set_last_error("null wallpaper path".to_string());
        return -1;
    }
    let path = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(p) => p,
        Err(e) => {
            set_last_error(format!("invalid wallpaper path: {e}"));
            return -1;
        }
    };
    UI.with(|slot| match slot.borrow_mut().as_mut() {
        Some(state) => {
            state.apply_wallpaper_path(path);
            0
        }
        None => {
            set_last_error("ms_spike_wallpaper_picked called before ms_spike_init".to_string());
            -1
        }
    })
}

/// Legacy spike hook: the real AppWindow has no status line; keep the entry
/// point for the existing bridge and just trigger a repaint.
#[no_mangle]
pub extern "C" fn ms_spike_set_status(_text: *const c_char) -> i32 {
    let _ = UI.with(|slot| {
        if let Some(state) = slot.borrow_mut().as_mut() {
            state.request_redraw();
        }
    });
    0
}

#[no_mangle]
pub extern "C" fn ms_spike_last_error() -> *const c_char {
    LAST_ERROR
        .lock()
        .unwrap()
        .as_ref()
        .map(|s| s.as_ptr())
        .unwrap_or(std::ptr::null())
}

#[no_mangle]
pub extern "C" fn ms_spike_version() -> *const c_char {
    static VERSION: &[u8] = b"meatshell-slint-p2\0";
    VERSION.as_ptr() as *const c_char
}
