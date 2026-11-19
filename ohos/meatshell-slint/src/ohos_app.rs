//! Real meatshell UI on the OpenHarmony software renderer.
//!
//! Instantiates the upstream `AppWindow` (compiled verbatim from
//! `../../ui/app.slint` by build.rs) and feeds it the saved session list from
//! the upstream config store (`meatshell_core::config`, unmodified upstream
//! sources). Desktop-only conveniences that have no OHOS counterpart yet
//! (local shells, agent CLIs, serial-port enumeration) are left out; the rest
//! renders exactly as upstream.

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::resource_stats::{ResourceState, StatsSample};
use crate::sftp_client::SftpClient;
use crate::ssh_session::{
    Auth, ConnectConfig, CredentialPrompt, CredentialReply, Event as SshEvent, MfaPrompt,
    Prompt as SshPrompt, PromptGate, SshSession,
};
use crate::terminal;
use crate::{
    register_system_font, AppWindow, PaneInfo, SessionInfo, SftpEntry, SftpTreeNode, TabInfo,
    TermMatch, TerminalState, TermSpan, TunnelInfo,
};
use meatshell_core::config::{self, AuthMethod, ConfigStore, PortForward, Session, SessionKind};
use meatshell_core::sftp_facade::{
    format_mtime, format_permissions, format_size, DownloadConflict, RemoteEntry, RemoteTreeNode,
    SftpCommand, SftpEvent,
};

/// Requests the ArkTS side has to act on (window controls), polled through
/// `ms_spike_take_action`. The Rust side never drives the OHOS window itself.
type PendingAction = Rc<RefCell<Option<String>>>;

/// One live terminal tab: the emulator state plus the SSH session feeding it.
struct TermTab {
    parser: vt100::Parser,
    /// Shared with the `TerminalState` row so updates reach the UI without
    /// rewriting the whole row model on every output chunk.
    spans: Rc<VecModel<TermSpan>>,
    session: Option<SshSession>,
    /// Connect-time question hand-off for this connection: host-key trust
    /// (`hostkey-prompt-open`), missing credentials (`cred-prompt-open`) and
    /// keyboard-interactive MFA (`mfa-prompt-open`). Answers travel back
    /// through this gate.
    gate: Arc<PromptGate>,
    is_dark: bool,
    connected: bool,
    rows_used: i32,
    /// `user@host:port`, used for the connected status line.
    label: String,
    /// SFTP worker for this tab. Started once the shell reports `Connected`,
    /// so the host key is already pinned and the credentials already accepted.
    sftp: Option<SftpClient>,
    /// Saved session config, cloned into the SFTP worker's own SSH connection.
    session_config: Session,
    /// Last remote directory reported by `SftpEntries`.
    sftp_path: String,
    /// Remote home directory, used to expand `~`.
    sftp_home: Option<String>,
    sftp_sort_key: String,
    sftp_sort_dir: i32,
    /// In-memory path bookmarks / recent folders (desktop persists these).
    bookmarks: Vec<String>,
    recent: Vec<String>,
    /// Local pane directory in the app sandbox (dual-pane view).
    local_path: std::path::PathBuf,
    /// Latest resource sample from this connection's stats channel, kept so
    /// switching back to the tab repaints the side panel without waiting for
    /// the next 2 s tick.
    last_stats: Option<StatsSample>,
    /// NIC picked in the resource panel's selector ("" = busiest NIC).
    selected_iface: String,
    /// Selection highlight rectangles, shared with the `TerminalState` row.
    selection: Rc<VecModel<TermMatch>>,
    /// Drag-selection anchor / focus in visible cell coordinates.
    sel_anchor: Option<(u16, u16)>,
    sel_focus: Option<(u16, u16)>,
    /// Multi-line paste waiting for the confirmation dialog.
    pending_paste: Option<String>,
    /// Last tap cell and time, for double-tap word selection. Touch presses
    /// inside the scroll area are delayed by Slint, so the built-in
    /// `double-clicked` callback never fires on OHOS (spec §25.9).
    last_tap: Option<(std::time::Instant, u16, u16)>,
    /// A double tap already selected and copied its word; the tap's release
    /// must not copy again or clear the highlight.
    skip_end_copy: bool,
    /// Highlighted find-bar matches for this tab.
    find_matches: Rc<VecModel<TermMatch>>,
}

/// Clipboard text produced by Rust and picked up by ArkTS (`slintClipboardTake`).
static CLIPBOARD_OUT: Mutex<Option<String>> = Mutex::new(None);

/// Last clipboard text this app has seen: either copied by the app itself or
/// handed over by ArkTS after a 粘贴. Slint's `TextInput` reads the platform
/// clipboard synchronously on Ctrl+V, while OHOS only exposes the pasteboard
/// through an async round trip (plus a security button when the read
/// permission is missing), so the platform getter serves this cache.
static CLIPBOARD_CACHE: Mutex<Option<String>> = Mutex::new(None);

/// Session-sync / broadcast input (the toolbar toggle). When on, every
/// keystroke is mirrored to the other live terminals, like upstream
/// `on-set-sync-input` (#78).
static SYNC_INPUT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_sync_input(on: bool) {
    SYNC_INPUT.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Set when the platform clipboard setter ran (Ctrl+C / Ctrl+X in a Slint
/// `TextInput`); `take_action` reports it as `clipboard-set`.
static CLIPBOARD_SET_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// Set when a `TextInput` paste asked for the system clipboard and nothing was
/// cached; `take_action` reports it as `clipboard-get`.
static CLIPBOARD_GET_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The pending clipboard read came from Ctrl+V in a `TextInput`; the payload
/// must be committed into the focused item instead of the terminal's PTY.
static PASTE_FOCUS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Terminal that asked for clipboard content (`clipboard-get` round trip).
static PASTE_TARGET: Mutex<Option<String>> = Mutex::new(None);

thread_local! {
    /// ArkTS → Rust clipboard payload handler, installed by `wire_callbacks`.
    static CLIPBOARD_IN: RefCell<Option<Box<dyn Fn(&str)>>> = const { RefCell::new(None) };
}

pub(crate) fn take_clipboard_out() -> Option<String> {
    CLIPBOARD_OUT.lock().ok().and_then(|mut slot| slot.take())
}

pub(crate) fn clipboard_text_from_arkts(text: &str) -> Result<(), String> {
    let handler = CLIPBOARD_IN.with(|slot| slot.borrow().as_ref().map(|_| ())).is_some();
    if !handler {
        return Err("clipboard handler not installed".to_string());
    }
    CLIPBOARD_IN.with(|slot| {
        if let Some(handle) = slot.borrow().as_ref() {
            handle(text);
        }
    });
    Ok(())
}

fn set_clipboard_text_handler(handler: Box<dyn Fn(&str)>) {
    CLIPBOARD_IN.with(|slot| *slot.borrow_mut() = Some(handler));
}

/// Hands `text` to ArkTS through the `clipboard-set` action.
fn queue_clipboard_copy(pending_action: &PendingAction, text: String) {
    if text.is_empty() {
        return;
    }
    remember_clipboard_text(&text);
    if let Ok(mut slot) = CLIPBOARD_OUT.lock() {
        *slot = Some(text);
    }
    *pending_action.borrow_mut() = Some("clipboard-set".to_string());
}

/// Caches clipboard text so a later Ctrl+V can paste it without a round trip.
fn remember_clipboard_text(text: &str) {
    if let Ok(mut slot) = CLIPBOARD_CACHE.lock() {
        *slot = Some(text.to_string());
    }
}

/// Serves `Platform::clipboard_text` (Ctrl+V inside a Slint `TextInput`).
///
/// Returns the cached text when there is one. Otherwise it asks ArkTS for the
/// system clipboard: reading it needs the security `PasteButton` grant, so the
/// chip appears and the payload arrives later through `slintClipboardText`.
pub(crate) fn clipboard_cache_text() -> Option<String> {
    if let Some(text) = CLIPBOARD_CACHE.lock().ok().and_then(|slot| slot.clone()) {
        if !text.is_empty() {
            return Some(text);
        }
    }
    PASTE_FOCUS.store(true, std::sync::atomic::Ordering::SeqCst);
    CLIPBOARD_GET_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
    None
}

/// Serves `Platform::set_clipboard_text` (Ctrl+C / Ctrl+X in a `TextInput`).
/// The system pasteboard is written by ArkTS on the `clipboard-set` action.
pub(crate) fn publish_clipboard_text(text: &str) {
    if text.is_empty() {
        return;
    }
    remember_clipboard_text(text);
    if let Ok(mut slot) = CLIPBOARD_OUT.lock() {
        *slot = Some(text.to_string());
    }
    CLIPBOARD_SET_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Committed text goes to whatever item holds focus: the command bar, the find
/// bar, or (as the fallback anchor) the terminal's hidden `TextInput`.
fn commit_text_to_focus(ui: &AppWindow, text: &str) {
    let mut event = i_slint_core::input::InternalKeyEvent::default();
    event.event_type = i_slint_core::input::KeyEventType::CommitComposition;
    event.key_event.text = SharedString::from(text);
    let _ = ui.window().dispatch_event(WindowEvent::internal(event));
    ui.window().request_redraw();
}

/// True when `(x, y)` lands inside the currently focused Slint text field.
///
/// The right-click 复制/剪切/粘贴 menu is only offered over an editable field:
/// the window-space rect is accumulated from the item tree so no layout
/// constants (dock widths, SFTP split, tab bar) have to be guessed here. The
/// terminal's keyboard anchor is a 0×0 `TextInput`, so right clicks on the
/// terminal body never match and keep the upstream context menu.
fn focus_text_field_at(ui: &AppWindow, x: f32, y: f32) -> bool {
    use i_slint_core::item_tree::ParentItemTraversalMode;
    use i_slint_core::items::TextInput;
    use i_slint_core::lengths::LogicalPoint;
    let window = ui.window();
    let inner = i_slint_core::window::WindowInner::from_pub(window);
    let Some(item) = inner.focus_item.borrow().upgrade() else { return false };
    if item.downcast::<TextInput>().is_none() {
        return false;
    }
    let own = item.geometry();
    if own.size.width <= 0.0 || own.size.height <= 0.0 {
        return false;
    }
    let mut origin = LogicalPoint::default();
    let mut node = Some(item);
    while let Some(current) = node {
        origin += current.geometry().origin.to_vector();
        node = current.parent_item(ParentItemTraversalMode::StopAtPopups);
    }
    x >= origin.x && x < origin.x + own.size.width && y >= origin.y && y < origin.y + own.size.height
}

/// Moves the drag-selection focus; `keep_anchor` extends an existing selection
/// (shift-click and drag updates), otherwise the anchor restarts at the cell.
fn move_selection(terms: &TermRegistry, id: &str, row: i32, col: i32, keep_anchor: bool) {
    let mut map = terms.borrow_mut();
    let Some(tab) = map.get_mut(id) else { return };
    let (rows, cols) = tab.parser.screen().size();
    let row = row.clamp(0, i32::from(rows.saturating_sub(1))) as u16;
    let col = col.clamp(0, i32::from(cols.saturating_sub(1))) as u16;
    if !keep_anchor || tab.sel_anchor.is_none() {
        tab.sel_anchor = Some((row, col));
    }
    tab.sel_focus = Some((row, col));
    refresh_selection(tab);
}

fn refresh_selection(tab: &TermTab) {
    let cols = tab.parser.screen().size().1;
    let rects = match (tab.sel_anchor, tab.sel_focus) {
        (Some(anchor), Some(focus)) => terminal::selection_rects(anchor, focus, cols),
        _ => Vec::new(),
    };
    tab.selection.set_vec(rects);
}

fn send_paste(tab: &mut TermTab, text: &str) {
    let Some(session) = tab.session.as_ref() else { return };
    let bytes = terminal::paste_bytes(text, tab.parser.screen().bracketed_paste());
    session.write(&bytes);
}

/// Clipboard payload returned by ArkTS for a pending 粘贴 request.
fn handle_pasted_text(ui: &AppWindow, terms: &TermRegistry, text: &str) {
    if text.is_empty() {
        return;
    }
    remember_clipboard_text(text);
    // The right-click 粘贴 menu records its tab in PASTE_TARGET first. The
    // long-press menu on OHOS has no way to read the system pasteboard
    // (READ_PASTEBOARD is a restricted permission), so ArkTS shows a
    // PasteButton chip instead; that path has no PASTE_TARGET and falls back
    // to the active tab.
    let target = PASTE_TARGET.lock().ok().and_then(|mut slot| slot.take());
    // Ctrl+V inside a Slint TextInput (command bar, find bar, editors) asked
    // for this payload: put it at the cursor of the focused item instead of
    // typing it into a shell.
    if PASTE_FOCUS.swap(false, std::sync::atomic::Ordering::SeqCst) && target.is_none() {
        commit_text_to_focus(ui, text);
        return;
    }
    let target = target.or_else(|| {
        let active = ui.get_active_tab_id().to_string();
        (!active.is_empty()).then_some(active)
    });
    let Some(target) = target else { return };
    let mut map = terms.borrow_mut();
    let Some(tab) = map.get_mut(&target) else { return };
    let multiline = text.contains('\n') || text.contains('\r');
    if multiline && ui.get_paste_confirm_enabled() {
        let preview: String = text.chars().take(600).collect();
        tab.pending_paste = Some(text.to_string());
        ui.set_paste_confirm_tab(SharedString::from(target));
        ui.set_paste_confirm_preview(SharedString::from(preview));
        ui.set_paste_confirm_large(text.chars().count() > 2000);
        ui.set_paste_confirm_open(true);
        return;
    }
    send_paste(tab, text);
}

type TermRegistry = Rc<RefCell<HashMap<String, TermTab>>>;

pub(crate) struct UiState {
    pub window: Rc<MinimalSoftwareWindow>,
    pub ui: AppWindow,
    pub width: usize,
    pub height: usize,
    #[allow(dead_code)] // read by ms_spike_take_action
    pending_action: PendingAction,
    /// Kept alive for the whole app: its callback drains SSH output every
    /// frame and repaints the visible terminals.
    #[allow(dead_code)]
    poll_timer: slint::Timer,
    diag_done: bool,
    /// Live terminal tabs. Kept so the public-download permission result can
    /// re-point local panes that were created while it was still pending.
    terms: TermRegistry,
    /// The same row model `ui.terminals` shows, used to refresh local panes.
    terminals: Rc<VecModel<TerminalState>>,
    /// Splitter being dragged, as `Some(vertical)` while a drag is in flight.
    ///
    /// The upstream splitter relies on `TouchArea.moved` plus `TouchArea.pressed`.
    /// ArkTS forwards a finger through `touch()` (Slint `Touch` events) and the
    /// synthetic press is dropped as soon as the gesture could turn into a
    /// scroll, so `pressed` never survives long enough for `moved` to fire and
    /// the splitter cannot be dragged. Tracking the gesture from the platform
    /// pointer stream keeps `ui/app.slint` untouched.
    split_drag: Option<bool>,
    /// Pane/splitter models the drag rewrites.
    panes: Rc<VecModel<PaneInfo>>,
    splitters: Rc<VecModel<crate::SplitterInfo>>,
    split_state: Rc<RefCell<Option<SplitState>>>,
    content_size: Rc<std::cell::Cell<(f32, f32)>>,
}

impl UiState {
    pub fn new(width: usize, height: usize, data_dir: Option<&str>) -> Result<Self, String> {
        // Must be set before set_platform: Slint's shared fontique collection
        // is built lazily when the platform creates its context, and a
        // SLINT_DEFAULT_FONT makes it register the font *and* populate the
        // SansSerif/SystemUi/UiSansSerif generic families. Without any generic
        // family an implicit family-less font query has no candidate and the
        // software renderer's `fallbackfont` unwrap aborts the process.
        if crate::FONT_CANDIDATES.iter().any(|path| std::path::Path::new(path).is_file()) {
            std::env::set_var(
                "SLINT_DEFAULT_FONT",
                crate::FONT_CANDIDATES
                    .iter()
                    .find(|path| std::path::Path::new(path).is_file())
                    .unwrap(),
            );
        }

        // NativeWindow hands out a rotating set of buffers, so the renderer must
        // assume the target contains stale pixels and repaint the whole frame.
        // ReusedBuffer would only repaint dirty regions and leave the rest of a
        // freshly dequeued buffer black.
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(crate::OhosPlatform { window: window.clone() }))
            .map_err(|_| "Slint platform was already installed".to_string())?;

        // Font registration needs Slint's global context, which set_platform
        // creates just above.
        register_system_font()?;

        // Point the upstream config store at the HAP sandbox directory passed
        // from ArkTS (context.filesDir). configure_profile() requires an
        // absolute path and creates the directory.
        if let Some(dir) = data_dir.filter(|d| !d.is_empty()) {
            std::env::set_var("MEATSHELL_DATA_DIR", dir);
        }
        if let Err(e) = config::configure_profile(&mut Vec::new()) {
            eprintln!("meatshell-ohos: configure_profile: {e}");
        }
        let store = match ConfigStore::load() {
            Ok(store) => Some(Rc::new(RefCell::new(store))),
            Err(e) => {
                eprintln!("meatshell-ohos: config load failed, starting with empty list: {e}");
                None
            }
        };

        window.set_size(slint::PhysicalSize::new(width as u32, height as u32));
        let ui = AppWindow::new().map_err(|e| format!("create AppWindow: {e}"))?;

        // Chinese UI text: install the embedded .po catalogue before the
        // window shows its first frame. `lang-en` drives the strings upstream
        // builds in Rust; the catalogs cover the static @tr(...) text.
        crate::i18n::install_chinese();
        ui.set_lang_en(false);

        // The UI embeds ui/fonts/MeatshellMono-*.ttf; those register during
        // AppWindow::new(), so this must come after it.
        crate::register_monospace_generic();

        // The desktop app resolves the UI font from user settings via fontdb;
        // here the only registered CJK-capable family is HarmonyOS Sans SC.
        // Theme.ui-font-family feeds AppWindow.default-font-family (#54).
        ui.set_ui_font_family(SharedString::from("HarmonyOS Sans SC"));

        // The vendored upstream sources are still v0.8.1; the layout follows the
        // v0.8.2 screenshot, so the footer shows the version the code actually
        // is rather than the screenshot's.
        ui.set_app_version(SharedString::from("0.8.1"));

        // Compliance pages shown inside the About card. The HAP ships the same
        // documents as rawfiles, but embedding them here keeps the in-app view
        // independent of the resource manager and works on every renderer.
        ui.set_privacy_text(SharedString::from(include_str!("../../../docs/PRIVACY.md")));
        ui.set_licenses_text(SharedString::from(concat!(
            include_str!("../../../LICENSE"),
            "\n\n",
            include_str!("../../../LICENSE-APACHE"),
            "\n\n",
            include_str!("../../../NOTICE"),
            "\n\n",
            include_str!("../../../THIRD_PARTY_NOTICES.md"),
        )));

        // Saved appearance first: theme, fonts, UI scale and the interface
        // toggles must be in place before the first frame. The dock layout
        // below still owns the panel geometry.
        if let Some(store) = store.as_ref() {
            crate::settings::apply_saved_appearance(&ui, &store.borrow());
        }

        let view = SessionsView::new(store.as_ref());
        ui.set_sessions(ModelRc::from(view.model.clone()));
        ui.set_sessions_revision(1);

        // 0.8.2 starts with no tab at all: the quick-connect list already lives
        // in the left sidebar, so the centre stays wallpaper-only until a
        // session is opened instead of showing the 0.8.1 welcome card.
        let tab_list = Rc::new(VecModel::<TabInfo>::default());
        ui.set_tabs(ModelRc::from(tab_list.clone()));
        ui.set_active_tab_id(SharedString::default());

        let panes = Rc::new(VecModel::from(vec![PaneInfo {
            id: 0,
            x: 0.0,
            y: 0.0,
            w: width as f32,
            h: content_height(height) as f32,
            active_id: SharedString::default(),
            focused: true,
            // Room for the floating top-right toolbar icons, matching upstream
            // `refresh_panes` for the pane touching the top-right corner.
            reserve_right: 240.0,
            tabs: ModelRc::from(Rc::new(VecModel::<TabInfo>::default())),
        }]));
        ui.set_panes(ModelRc::from(panes.clone()));
        let splitters = Rc::new(VecModel::<crate::SplitterInfo>::default());
        ui.set_splitters(ModelRc::from(splitters.clone()));
        // One vertical/horizontal split at most (two panes). `None` is the
        // unsplit start-up state.
        let split_state: Rc<RefCell<Option<SplitState>>> = Rc::new(RefCell::new(None));
        let content_size: Rc<std::cell::Cell<(f32, f32)>> =
            Rc::new(std::cell::Cell::new((width as f32, content_height(height) as f32)));

        // Three-column layout (#dock-stack): the welcome list sits on the left
        // and the resource monitor on the right; `rest` (the terminal area)
        // gets whatever remains. The upstream desktop app derives these rects
        // in its dock engine, which is not part of this port.
        crate::dock_layout::init(&ui);
        // Restore the panel sizes / collapsed flags saved by an earlier run.
        // Upstream's `sidebar_width` default is 220, which predates this port's
        // 280px resource column; treat it as "unset" so the aligned default
        // survives a fresh config.json.
        if let Some(store) = store.as_ref() {
            let store = store.borrow();
            let right = store.sidebar_width();
            if (right - 220.0).abs() > 0.5 {
                ui.set_sidebar_width(right.clamp(180.0, 420.0));
            }
            let left = store.welcome_sidebar_width();
            if (left - 240.0).abs() > 0.5 {
                ui.set_welcome_sidebar_width(left.clamp(160.0, 480.0));
            }
            ui.set_sidebar_collapsed(store.sidebar_collapsed().unwrap_or(false));
            ui.set_welcome_collapsed(store.welcome_collapsed().unwrap_or(false));
        }
        crate::dock_layout::apply_dock(&ui, width as f32, content_height(height) as f32);

        let pending_action: PendingAction = Rc::new(RefCell::new(None));
        let terminals = Rc::new(VecModel::<TerminalState>::default());
        ui.set_terminals(ModelRc::from(terminals.clone()));
        let terms: TermRegistry = Rc::new(RefCell::new(HashMap::new()));
        // Settings › Output highlighting: compiled once here, replaced in place
        // whenever a rule changes, and read by every terminal repaint.
        let highlight = Rc::new(RefCell::new(match store.as_ref() {
            Some(store) => crate::output_highlight::Rules::from_store(&store.borrow()),
            None => crate::output_highlight::Rules::off(),
        }));
        wire_callbacks(
            &ui,
            store.clone(),
            view.clone(),
            tab_list.clone(),
            panes.clone(),
            splitters.clone(),
            split_state.clone(),
            content_size.clone(),
            terminals.clone(),
            terms.clone(),
            highlight.clone(),
            pending_action.clone(),
            width as f32,
            content_height(height) as f32,
        );
        // The session editor owns the dialog properties and rebuilds `view`
        // after every save/delete, so it gets the same handles.
        crate::session_editor::register(&ui, store.clone(), view);
        // Settings page: apply + persist the appearance and interface prefs.
        // A highlight-rule change repaints every live terminal, since the
        // colours are applied while the row model is rebuilt.
        let repaint_all: Rc<dyn Fn()> = {
            let weak = ui.as_weak();
            let terms = terms.clone();
            let highlight = highlight.clone();
            Rc::new(move || {
                let rules = highlight.borrow();
                for tab in terms.borrow().values() {
                    terminal::rebuild(&tab.parser, tab.is_dark, &tab.spans, &rules);
                }
                drop(rules);
                if let Some(w) = weak.upgrade() {
                    w.window().request_redraw();
                }
            })
        };
        crate::settings::register(&ui, store, highlight.clone(), repaint_all);

        // The resource panel starts on this device's own numbers; connecting a
        // session switches it to that server's sample. One sample now avoids a
        // blank right column until the first poll tick 2 s later.
        let resource = Rc::new(RefCell::new(ResourceState::new()));
        resource.borrow_mut().tick_local(&ui);
        resource.borrow_mut().show_local(&ui);

        let poll_timer =
            start_poll_timer(&ui, tab_list, terminals.clone(), terms.clone(), resource, highlight);

        ui.show().map_err(|e| format!("show AppWindow: {e}"))?;

        Ok(Self {
            window,
            ui,
            width,
            height,
            pending_action,
            poll_timer,
            diag_done: false,
            terms,
            terminals,
            split_drag: None,
            panes,
            splitters,
            split_state,
            content_size,
        })
    }

    pub fn resize(&mut self, width: usize, height: usize) {
        if width == 0 || height == 0 {
            return;
        }
        self.width = width;
        self.height = height;
        self.window
            .set_size(slint::PhysicalSize::new(width as u32, height as u32));
        // Recompute the three-column split for the new window size; the exact
        // dock-area size arrives through `dock-area-resized` just after.
        crate::dock_layout::apply_dock(&self.ui, width as f32, content_height(height) as f32);
        // The real size comes from the `content-resized` callback below; this
        // keeps the first frame sane before Slint has laid out the dock area.
        self.ui.window().request_redraw();
    }

    pub fn request_redraw(&self) {
        self.ui.window().request_redraw();
    }

    /// ArkTS copied a wallpaper photo into the sandbox and reports its path.
    pub fn apply_wallpaper_path(&self, path: &str) {
        crate::wallpaper::save_prefs(path, None);
        crate::wallpaper::apply(&self.ui, path, None);
        self.request_redraw();
    }

    pub fn take_action(&self) -> Option<String> {
        if let Some(action) = self.pending_action.borrow_mut().take() {
            return Some(action);
        }
        if let Some(message) = PENDING_TOAST.lock().ok().and_then(|mut queue| queue.pop()) {
            return Some(format!("toast:{message}"));
        }
        if let Some(local) = PENDING_SAVE_EXPORT.lock().ok().and_then(|mut queue| queue.pop()) {
            return Some(format!("save-download:{}", local.to_string_lossy()));
        }
        if IMPORT_PICK_REQUESTED.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Some("pick-import".to_string());
        }
        if KEY_PICK_REQUESTED.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Some("pick-key".to_string());
        }
        if WALLPAPER_PICK_REQUESTED.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Some("pick-wallpaper".to_string());
        }
        // Ctrl+C / Ctrl+V inside a Slint TextInput: SystemClipboard on OHOS is
        // only reachable from ArkTS, so the platform getter/setter hands the
        // request over as an action.
        if CLIPBOARD_SET_REQUESTED.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Some("clipboard-set".to_string());
        }
        if CLIPBOARD_GET_REQUESTED.swap(false, std::sync::atomic::Ordering::SeqCst) {
            // Distinct action so ArkTS places the 粘贴 chip without a menu
            // anchor and Rust knows the payload goes to the focused input.
            return Some("clipboard-get-input".to_string());
        }
        // Taken out (not peeked) so the 100 ms ArkTS poll opens the picker once.
        let request = PENDING_UPLOAD_PICK.lock().ok().and_then(|mut slot| slot.take());
        let (tab, folder) = request?;
        if let Ok(mut slot) = UPLOAD_DEST_TAB.lock() {
            *slot = Some(tab);
        }
        Some(format!(
            "pick-upload:{}",
            if folder { "folder" } else { "file" }
        ))
    }

    /// Uploads files ArkTS picked through the system document picker. The paths
    /// are copies inside the HAP sandbox (`<filesDir>/upload-staging`): SFTP can
    /// only read real paths, and picker URIs outside Download are not readable
    /// by the worker thread. A path may be a directory — the engine recurses.
    pub fn upload_picked_files(&self, paths: &[std::path::PathBuf]) {
        if paths.is_empty() {
            return;
        }
        let tab_id = UPLOAD_DEST_TAB.lock().ok().and_then(|mut slot| slot.take());
        let Some(tab_id) = tab_id else { return };
        let remote_dir = self
            .terms
            .borrow()
            .get(&tab_id)
            .map(|tab| tab.sftp_path.clone())
            .unwrap_or_default();
        if remote_dir.is_empty() {
            return;
        }
        let mut count = 0;
        for local in paths {
            if sftp_send(
                &self.terms,
                &tab_id,
                SftpCommand::Upload {
                    local: local.clone(),
                    remote_dir: remote_dir.clone(),
                },
            ) {
                count += 1;
            }
        }
        if count == 0 {
            return;
        }
        update_sftp_row(&self.terminals, &tab_id, |row| {
            row.sftp_status = SharedString::from(format!("↑ 上传 {count} 项到 {remote_dir}…"));
        });
        self.ui.window().request_redraw();
    }

    /// The picker was dismissed without a selection: drop the pending upload
    /// target and clear the "opening picker" hint so no stale status remains.
    pub fn upload_pick_cancelled(&self) {
        let tab_id = UPLOAD_DEST_TAB.lock().ok().and_then(|mut slot| slot.take());
        let Some(tab_id) = tab_id else { return };
        update_sftp_row(&self.terminals, &tab_id, |row| {
            row.sftp_status = SharedString::default();
        });
        self.ui.window().request_redraw();
    }

    /// Mirrors the real OHOS window state into Slint so the custom title bar
    /// picks the maximize/restore glyph. The window itself is driven by ArkTS.
    pub fn set_maximized(&self, maximized: bool) {
        self.ui.set_window_maximized(maximized);
        self.ui.window().request_redraw();
    }

    /// Switches downloads (and the local dual-pane view) to the public Download
    /// directory ArkTS resolved after `READ_WRITE_DOWNLOAD_DIRECTORY` was
    /// granted. Tabs the user navigated away from keep their current folder;
    /// only panes still sitting in the previous download root follow along.
    pub fn set_download_dir(&self, dir: &str) {
        let dir = dir.trim();
        if dir.is_empty() {
            return;
        }
        let new_dir = std::path::PathBuf::from(dir);
        let old_dir = downloads_dir();
        set_public_download_dir(dir);
        if new_dir == old_dir {
            return;
        }
        let ids: Vec<String> = self.terms.borrow().keys().cloned().collect();
        let mut repaint = false;
        for id in ids {
            let mut delta = SftpDelta::default();
            {
                let mut map = self.terms.borrow_mut();
                let Some(tab) = map.get_mut(&id) else { continue };
                if tab.local_path != old_dir {
                    continue;
                }
                load_local_view(tab, new_dir.clone(), &mut delta);
            }
            if delta.dirty() {
                update_sftp_row(&self.terminals, &id, |row| delta.apply(row));
                repaint = true;
            }
        }
        if repaint {
            self.window.request_redraw();
        }
    }

    pub fn key(&mut self, text: &str, pressed: bool) {
        let event = if pressed {
            WindowEvent::KeyPressed { text: slint_key_text(text) }
        } else {
            WindowEvent::KeyReleased { text: slint_key_text(text) }
        };
        let _ = self.ui.window().dispatch_event(event);
        self.ui.window().request_redraw();
    }

    /// Text committed by the OHOS input method (finished Chinese composition).
    ///
    /// Delivered as a `CommitComposition` key event, the same shape Slint's
    /// winit backend uses for `Ime::Commit`. The hidden `TextInput` anchor in
    /// upstream `terminal_view.slint` receives it, inserts it into its `text`
    /// property and fires `edited`; the upstream guard there (preedit empty)
    /// forwards it to the PTY through `send-key`. Composition/preedit text is
    /// intentionally not forwarded: the IME renders its own candidate window
    /// and the terminal would otherwise paint half-finished pinyin.
    pub fn ime_commit(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        commit_text_to_focus(&self.ui, text);
    }

    /// Feed a platform gesture to the splitter drag. `kind` matches
    /// `pointer()` (0 press, 1 release, 2 move, 3 cancel).
    ///
    /// Returns true when the gesture belongs to a splitter and must not be
    /// forwarded to the UI: swallowing the press also stops the pane under the
    /// finger from starting a text selection.
    fn splitter_gesture(&mut self, x: f32, y: f32, kind: i32) -> bool {
        // `PaneInfo`/`SplitterInfo` coordinates are relative to the pane
        // container (`pane-area`), while the platform pointer stream is in
        // window coordinates. `content-origin-*` is that container's absolute
        // origin: it includes the title bar above the dock area, so
        // `dock-central-y` alone would be off by the title bar height.
        let ox = self.ui.get_content_origin_x();
        let oy = self.ui.get_content_origin_y();
        let (lx, ly) = (x - ox, y - oy);
        match kind {
            0 => {
                let pad = 6.0;
                let mut hit = None;
                for i in 0..self.splitters.row_count() {
                    let Some(sp) = self.splitters.row_data(i) else {
                        continue;
                    };
                    if lx >= sp.x - pad
                        && lx <= sp.x + sp.w + pad
                        && ly >= sp.y - pad
                        && ly <= sp.y + sp.h + pad
                    {
                        hit = Some(sp.vertical);
                        break;
                    }
                }
                match hit {
                    Some(vertical) => {
                        self.split_drag = Some(vertical);
                        true
                    }
                    None => false,
                }
            }
            1 | 3 => self.split_drag.take().is_some(),
            2 => {
                let Some(vertical) = self.split_drag else {
                    return false;
                };
                let Some(mut state) = *self.split_state.borrow() else {
                    return true;
                };
                let (w, h) = self.content_size.get();
                let extent = if vertical { w } else { h };
                if extent > 1.0 {
                    let pos = if vertical { lx } else { ly };
                    state.ratio = (pos / extent).clamp(0.15, 0.85);
                    *self.split_state.borrow_mut() = Some(state);
                    refresh_panes(&self.panes, &self.splitters, (w, h), Some(state));
                    self.ui.window().request_redraw();
                }
                true
            }
            _ => false,
        }
    }

    pub fn pointer(&mut self, x: f32, y: f32, kind: i32, button: i32) {
        let position = slint::LogicalPosition::new(x, y);
        let button = match button {
            1 => PointerEventButton::Right,
            2 => PointerEventButton::Middle,
            _ => PointerEventButton::Left,
        };
        if button == PointerEventButton::Left && self.splitter_gesture(x, y, kind) {
            return;
        }
        // Slint's built-in TextInput has no context menu and the OHOS
        // clipboard only exists on the ArkTS side, so a right click inside the
        // focused text field (command bar, find bar, editor fields) is handed
        // over as an action: ArkTS pops a native 全选/复制/剪切/粘贴 menu and
        // replays the matching Ctrl shortcut. Right clicks elsewhere keep the
        // upstream terminal menu (its focus is the 0×0 IME anchor).
        if kind == 0 && button == PointerEventButton::Right && focus_text_field_at(&self.ui, x, y) {
            *self.pending_action.borrow_mut() = Some("input-menu".to_string());
        }
        let event = match kind {
            0 => WindowEvent::PointerPressed { position, button },
            1 => WindowEvent::PointerReleased { position, button },
            2 => WindowEvent::PointerMoved { position },
            // Cancels whatever press is in flight (hover/grab dropped, TouchArea
            // clears its `pressed` flag). ArkTS emits this when a touch turns
            // into a long press, so the finger's eventual release cannot be
            // paired with the touch's synthetic Left press into a `clicked`.
            3 => WindowEvent::PointerExited,
            _ => return,
        };
        let _ = self.ui.window().dispatch_event(event);
        self.ui.window().request_redraw();
    }

    /// Forward a real touch contact. Slint only pans a `Flickable` (all std
    /// `ScrollView`s) on drag when the event comes from touch; forwarding the
    /// finger as a mouse pointer (as `pointer()` does) leaves the settings
    /// panel unscrollable (#24).
    pub fn touch(&mut self, x: f32, y: f32, phase: i32) {
        use i_slint_core::input::TouchPhase;
        let phase = match phase {
            0 => TouchPhase::Started,
            1 => TouchPhase::Moved,
            2 => TouchPhase::Ended,
            3 => TouchPhase::Cancelled,
            _ => return,
        };
        // Splitter drags are handled before Slint sees the touch, see
        // `splitter_gesture`.
        let gesture = match phase {
            TouchPhase::Started => 0,
            TouchPhase::Moved => 2,
            TouchPhase::Ended => 1,
            TouchPhase::Cancelled => 3,
        };
        if self.splitter_gesture(x, y, gesture) {
            return;
        }
        let event = WindowEvent::internal(i_slint_core::platform::InternalEvent::Touch {
            id: 0,
            position: i_slint_core::lengths::LogicalPoint::new(x, y),
            phase,
        });
        let _ = self.ui.window().dispatch_event(event);
        self.ui.window().request_redraw();
    }

    pub fn scroll(&mut self, x: f32, y: f32, delta_x: f32, delta_y: f32) {
        let _ = self.ui.window().dispatch_event(WindowEvent::PointerScrolled {
            position: slint::LogicalPosition::new(x, y),
            delta_x,
            delta_y,
        });
        self.ui.window().request_redraw();
    }

    pub fn render_to(&mut self, pixels: &mut [crate::PremultipliedRgbaColor], stride_pixels: usize) -> bool {
        // Drive Slint timers/animations, but only paint when Slint asked for a
        // repaint; otherwise the caller aborts the dequeued buffer and the
        // compositor keeps showing the last presented frame.
        slint::platform::update_timers_and_animations();

        let window = self.window.clone();
        let rendered = window.draw_if_needed(|renderer| {
            renderer.render(pixels, stride_pixels);
        });
        self.diag_if_needed(pixels, stride_pixels, rendered);
        rendered
    }

    /// One-shot debug dump, enabled with `MS_OHOS_DUMP_FRAME=1` in the app's
    /// environment: writes a pixel-statistics line and a PPM copy of the first
    /// rendered frame into `<data_dir>`. OHOS does not surface stderr, and a
    /// compositor screenshot is JPEG-compressed, so this is the reliable way to
    /// tell "nothing was drawn" from "drawn, but empty".
    fn diag_if_needed(
        &mut self,
        pixels: &[crate::PremultipliedRgbaColor],
        stride_pixels: usize,
        rendered: bool,
    ) {
        if self.diag_done {
            return;
        }
        self.diag_done = true;
        if std::env::var_os("MS_OHOS_DUMP_FRAME").is_none() {
            return;
        }
        let visible = self.ui.window().is_visible();
        let window_size = self.ui.window().size();
        let full_len = stride_pixels * self.height;
        let in_frame = pixels.len().min(full_len);
        let mut nonzero = 0usize;
        let mut opaque = 0usize;
        for row in 0..self.height.min(full_len / stride_pixels.max(1)) {
            let start = row * stride_pixels;
            for pixel in &pixels[start..(start + self.width).min(in_frame)] {
                if pixel.red != 0 || pixel.green != 0 || pixel.blue != 0 || pixel.alpha != 0 {
                    nonzero += 1;
                }
                if pixel.alpha == 255 {
                    opaque += 1;
                }
            }
        }
        let Some(dir) = crate::DATA_DIR.get() else {
            return;
        };
        let mut lines = String::new();
        lines.push_str(&format!(
            "rendered={rendered} visible={visible} slint_size={}x{} target={}x{} stride={stride_pixels}\n",
            window_size.width,
            window_size.height,
            self.width,
            self.height
        ));
        lines.push_str(&format!(
            "pixels_in_frame={in_frame} nonzero={nonzero} opaque={opaque} first8={:?}\n",
            &pixels[..8.min(in_frame)]
        ));
        let _ = std::fs::write(std::path::Path::new(dir).join("slint-diag.log"), lines);

        // Dump the frame itself: reading pixels on the host tells us far more
        // than a compositor screenshot (which JPEG-compresses and scales).
        let mut ppm = Vec::with_capacity(self.width * self.height * 3 + 32);
        ppm.extend_from_slice(frame_header(self.width, self.height).as_bytes());
        for row in 0..self.height {
            let start = row * stride_pixels;
            let end = (start + self.width).min(in_frame);
            if start >= in_frame || end <= start {
                break;
            }
            for pixel in &pixels[start..end] {
                ppm.push(pixel.red);
                ppm.push(pixel.green);
                ppm.push(pixel.blue);
            }
        }
        let _ = std::fs::write(std::path::Path::new(dir).join("slint-frame.ppm"), ppm);
    }
}

/// Wires the subset of AppWindow callbacks that work without a running SSH
/// session. Ports of the matching handlers in `src/app/*.rs`, simplified where
/// upstream depends on desktop-only state.
#[allow(clippy::too_many_arguments)]
fn wire_callbacks(
    ui: &AppWindow,
    store: Option<Rc<RefCell<ConfigStore>>>,
    view: SessionsView,
    tabs: Rc<VecModel<TabInfo>>,
    panes: Rc<VecModel<PaneInfo>>,
    splitters: Rc<VecModel<crate::SplitterInfo>>,
    split_state: Rc<RefCell<Option<SplitState>>>,
    content_size: Rc<std::cell::Cell<(f32, f32)>>,
    terminals: Rc<VecModel<TerminalState>>,
    terms: TermRegistry,
    highlight: Rc<RefCell<crate::output_highlight::Rules>>,
    pending_action: PendingAction,
    content_w: f32,
    content_h: f32,
) {
    // Case-insensitive name/host/user search over the saved sessions; upstream
    // also matches built-in shells and agent CLIs, which do not exist here.
    {
        let view = view.clone();
        ui.on_host_search_changed(move |query| {
            *view.query.borrow_mut() = query.to_string();
            apply_search(&view);
        });
    }

    // Collapse/expand a Quick Connect folder. Toggling flips `collapsed` on
    // every row of the group in place and persists it, like upstream
    // `on_toggle_group` in src/app/session_callbacks.rs.
    {
        let view = view.clone();
        let store = store.clone();
        let weak = ui.as_weak();
        ui.on_toggle_group(move |group: SharedString| {
            let sessions = &view.model;
            let target = group.to_string();
            let mut new_state = false;
            for i in 0..sessions.row_count() {
                if let Some(row) = sessions.row_data(i) {
                    if row.group.as_str() == target {
                        new_state = !row.collapsed;
                        break;
                    }
                }
            }
            for i in 0..sessions.row_count() {
                if let Some(mut row) = sessions.row_data(i) {
                    if row.group.as_str() == target {
                        row.collapsed = new_state;
                        sessions.set_row_data(i, row);
                    }
                }
            }
            // Keep the unfiltered copy in sync so clearing the search does not
            // resurrect the old collapse state.
            for row in view.all.borrow_mut().iter_mut() {
                if row.group.as_str() == target {
                    row.collapsed = new_state;
                }
            }
            if let Some(store) = &store {
                let mut store = store.borrow_mut();
                store.set_session_group_collapsed(&target, new_state);
                if let Err(err) = store.save() {
                    eprintln!("meatshell-ohos: save folder state: {err}");
                }
            }
            if let Some(ui) = weak.upgrade() {
                ui.set_sessions_revision(ui.get_sessions_revision().saturating_add(1));
            }
        });
    }

    // ———————— tabs ————————
    {
        let weak = ui.as_weak();
        let panes = panes.clone();
        ui.on_tab_selected(move |id: SharedString| {
            // Pane content visibility is driven by `active-id`, so switching to a
            // tab has to update its owning pane as well.
            for i in 0..panes.row_count() {
                if let Some(mut pane) = panes.row_data(i) {
                    let owns = (0..pane.tabs.row_count())
                        .any(|j| pane.tabs.row_data(j).map(|tab| tab.id == id).unwrap_or(false));
                    if owns {
                        if pane.active_id != id {
                            pane.active_id = id.clone();
                            panes.set_row_data(i, pane);
                        }
                        break;
                    }
                }
            }
            if let Some(ui) = weak.upgrade() {
                ui.set_active_tab_id(id);
            }
        });
    }

    {
        let weak = ui.as_weak();
        let tabs = tabs.clone();
        let panes = panes.clone();
        let splitters = splitters.clone();
        let split_state = split_state.clone();
        let content_size = content_size.clone();
        let terminals = terminals.clone();
        let terms = terms.clone();
        ui.on_tab_closed(move |id: SharedString| {
            remove_terminal(&terms, &terminals, id.as_str());
            close_tab(
                &tabs,
                &panes,
                &splitters,
                &split_state,
                content_size.get(),
                &id,
            );
            if let Some(ui) = weak.upgrade() {
                let pane_id = focused_pane_id(&panes).unwrap_or(0);
                if let Some(pane) = pane_row(&panes, pane_id) {
                    ui.set_active_tab_id(pane.active_id);
                }
            }
        });
    }

    {
        let weak = ui.as_weak();
        let tabs = tabs.clone();
        let panes = panes.clone();
        let splitters = splitters.clone();
        let split_state = split_state.clone();
        let content_size = content_size.clone();
        let terminals = terminals.clone();
        let terms = terms.clone();
        ui.on_pane_tab_closed(move |pane_id: i32, id: SharedString| {
            remove_terminal(&terms, &terminals, id.as_str());
            close_tab(
                &tabs,
                &panes,
                &splitters,
                &split_state,
                content_size.get(),
                &id,
            );
            update_pane_tabs(&panes, pane_id, &tabs, None);
            if let Some(ui) = weak.upgrade() {
                let pane_id = focused_pane_id(&panes).unwrap_or(0);
                if let Some(pane) = pane_row(&panes, pane_id) {
                    ui.set_active_tab_id(pane.active_id);
                }
            }
        });
    }

    {
        let weak = ui.as_weak();
        let tabs = tabs.clone();
        let panes = panes.clone();
        ui.on_new_tab_clicked(move || {
            if let Some(ui) = weak.upgrade() {
                let pane_id = focused_pane_id(&panes).unwrap_or(0);
                activate_welcome_tab(&ui, &tabs, &panes, pane_id);
            }
        });
    }

    {
        let weak = ui.as_weak();
        let tabs = tabs.clone();
        let panes = panes.clone();
        ui.on_pane_new_tab(move |pane_id: i32| {
            if let Some(ui) = weak.upgrade() {
                activate_welcome_tab(&ui, &tabs, &panes, pane_id);
            }
        });
    }

    {
        let weak = ui.as_weak();
        let panes = panes.clone();
        ui.on_pane_tab_selected(move |pane_id: i32, id: SharedString| {
            if let Some(mut pane) = pane_row(&panes, pane_id) {
                pane.active_id = id.clone();
                pane.focused = true;
                panes.set_row_data(pane_id as usize, pane);
            }
            if let Some(ui) = weak.upgrade() {
                ui.set_active_tab_id(id);
            }
        });
    }

    {
        let weak = ui.as_weak();
        let panes = panes.clone();
        ui.on_pane_focus(move |pane_id: i32| {
            for i in 0..panes.row_count() {
                if let Some(mut pane) = panes.row_data(i) {
                    pane.focused = i == pane_id as usize;
                    panes.set_row_data(i, pane);
                }
            }
            // The status strip and the side panels follow the focused pane.
            if let (Some(ui), Some(pane)) = (weak.upgrade(), pane_row(&panes, pane_id)) {
                if !pane.active_id.is_empty() {
                    ui.set_active_tab_id(pane.active_id);
                }
            }
        });
    }

    // Split the pane that owns `tab_id`: the tab moves to a new second pane,
    // either to the right or below (two panes at most, like the OHOS UI
    // supports comfortably on a 1366×768 screen).
    {
        let weak = ui.as_weak();
        let panes = panes.clone();
        let splitters = splitters.clone();
        let split_state = split_state.clone();
        let content_size = content_size.clone();
        ui.on_pane_split(move |_pane_id: i32, tab_id: SharedString, dir: SharedString| {
            if split_state.borrow().is_some() {
                return;
            }
            let Some(pane0) = pane_row(&panes, 0) else {
                return;
            };
            let mut rows0 = collect_tabs(&pane0.tabs);
            let Some(pos) = rows0.iter().position(|tab| tab.id == tab_id) else {
                return;
            };
            let moved = rows0.remove(pos);
            let vertical = dir.as_str() == "right";

            let mut p0 = pane0;
            p0.active_id = rows0.first().map(|tab| tab.id.clone()).unwrap_or_default();
            p0.focused = false;
            p0.tabs = ModelRc::from(Rc::new(VecModel::from(rows0)));

            let mut p1 = empty_pane(1);
            p1.focused = true;
            p1.active_id = moved.id.clone();
            p1.tabs = ModelRc::from(Rc::new(VecModel::from(vec![moved.clone()])));
            panes.set_vec(vec![p0, p1]);

            *split_state.borrow_mut() = Some(SplitState { vertical, ratio: 0.5 });
            refresh_panes(
                &panes,
                &splitters,
                content_size.get(),
                *split_state.borrow(),
            );
            if let Some(ui) = weak.upgrade() {
                ui.set_active_tab_id(moved.id);
                ui.window().request_redraw();
            }
        });
    }

    // Merge the split back into one pane; the merged pane keeps the first
    // pane's tabs and appends the second pane's.
    {
        let weak = ui.as_weak();
        let panes = panes.clone();
        let splitters = splitters.clone();
        let split_state = split_state.clone();
        let content_size = content_size.clone();
        ui.on_pane_merge(move |_pane_id: i32| {
            if split_state.borrow().is_none() {
                return;
            }
            merge_panes(&panes, &splitters, &split_state, content_size.get());
            if let Some(ui) = weak.upgrade() {
                ui.set_active_tab_id(panes.row_data(0).map(|p| p.active_id).unwrap_or_default());
                ui.window().request_redraw();
            }
        });
    }

    // Drag the divider: `pos` is the handle position along the split axis.
    {
        let weak = ui.as_weak();
        let panes = panes.clone();
        let splitters = splitters.clone();
        let split_state = split_state.clone();
        let content_size = content_size.clone();
        ui.on_splitter_drag(move |_split_id: i32, pos: f32, vertical: bool| {
            let Some(mut state) = *split_state.borrow() else {
                return;
            };
            let (w, h) = content_size.get();
            let extent = if vertical { w } else { h };
            if extent <= 1.0 {
                return;
            }
            state.ratio = (pos / extent).clamp(0.15, 0.85);
            *split_state.borrow_mut() = Some(state);
            refresh_panes(&panes, &splitters, (w, h), Some(state));
            if let Some(ui) = weak.upgrade() {
                ui.window().request_redraw();
            }
        });
    }

    // Ctrl+Tab / Ctrl+Shift+Tab: cycle the focused pane's tabs (#294).
    {
        let weak = ui.as_weak();
        let panes = panes.clone();
        ui.on_cycle_tab(move |reverse: bool| {
            let pane_id = focused_pane_id(&panes).unwrap_or(0);
            let Some(pane) = pane_row(&panes, pane_id) else {
                return;
            };
            let count = pane.tabs.row_count();
            if count < 2 {
                return;
            }
            let current = (0..count)
                .find(|&i| pane.tabs.row_data(i).map(|t| t.id == pane.active_id).unwrap_or(false))
                .unwrap_or(0);
            let next = if reverse {
                (current + count - 1) % count
            } else {
                (current + 1) % count
            };
            let Some(tab) = pane.tabs.row_data(next) else {
                return;
            };
            if let Some(mut pane) = pane_row(&panes, pane_id) {
                pane.active_id = tab.id.clone();
                panes.set_row_data(pane_id as usize, pane);
            }
            if let Some(ui) = weak.upgrade() {
                ui.set_active_tab_id(tab.id);
            }
        });
    }

    // Duplicate a tab: open a fresh connection to the same saved session, in
    // the same pane as the source (#v0.5).
    {
        let weak = ui.as_weak();
        let tabs = tabs.clone();
        let panes = panes.clone();
        let terminals = terminals.clone();
        let terms = terms.clone();
        ui.on_tab_duplicate(move |tab_id: SharedString| {
            let session = match terms.borrow().get(tab_id.as_str()) {
                Some(tab) => tab.session_config.clone(),
                None => return,
            };
            // The copy should land where the source tab lives.
            for i in 0..panes.row_count() {
                if let Some(mut pane) = panes.row_data(i) {
                    let owns = (0..pane.tabs.row_count()).any(|j| {
                        pane.tabs.row_data(j).map(|t| t.id == tab_id).unwrap_or(false)
                    });
                    if owns {
                        pane.focused = true;
                        panes.set_row_data(i, pane);
                        break;
                    }
                }
            }
            if let Some(ui) = weak.upgrade() {
                connect_session(&ui, &tabs, &panes, &terminals, &terms, &session);
            }
        });
    }

    // Rename dialog (tab context menu), pre-filled with the current title.
    {
        let weak = ui.as_weak();
        let tabs = tabs.clone();
        ui.on_tab_rename_request(move |tab_id: SharedString| {
            if tab_id.is_empty() || tab_id == "welcome" {
                return;
            }
            let title = (0..tabs.row_count())
                .find_map(|i| {
                    let row = tabs.row_data(i)?;
                    (row.id == tab_id).then(|| row.title.to_string())
                })
                .unwrap_or_default();
            if let Some(ui) = weak.upgrade() {
                ui.set_tab_rename_id(tab_id);
                ui.set_tab_rename_value(SharedString::from(title));
                ui.set_tab_rename_open(true);
            }
        });
    }

    // Apply a new display name. An empty name restores the session's own
    // name. Display-only: the saved session is untouched, like upstream.
    {
        let weak = ui.as_weak();
        let tabs = tabs.clone();
        let panes = panes.clone();
        let terms = terms.clone();
        ui.on_rename_tab(move |tab_id: SharedString, name: SharedString| {
            if let Some(ui) = weak.upgrade() {
                ui.set_tab_rename_open(false);
            }
            let name = name.trim().to_string();
            let title = if name.is_empty() {
                terms.borrow().get(tab_id.as_str()).map(|tab| {
                    let session = &tab.session_config;
                    if session.name.trim().is_empty() {
                        format!("{}@{}", session.user, session.host)
                    } else {
                        session.name.clone()
                    }
                })
            } else {
                Some(name)
            };
            let Some(title) = title else { return };
            set_tab_title(&tabs, &panes, tab_id.as_str(), &title);
        });
    }

    // Ctrl+= / Ctrl+- / Ctrl+0 zooms one session (per-tab px override);
    // Ctrl+Shift+= / - / 0 zooms the whole window and clears the per-tab
    // overrides. Mirrors upstream `on_zoom_term_font`.
    {
        let weak = ui.as_weak();
        let store = store.clone();
        let terminals = terminals.clone();
        ui.on_zoom_term_font(
            move |tab_id: SharedString, direction: i32, window_wide: bool| {
                let Some(ui) = weak.upgrade() else { return };
                let settings_size = store
                    .as_ref()
                    .map(|s| s.borrow().font_size() as i32)
                    .unwrap_or(16);
                if window_wide {
                    let next = if direction == 0 {
                        settings_size
                    } else {
                        ui.get_term_font_size() as i32 + direction
                    };
                    ui.set_term_font_size(next.clamp(8, 32) as f32);
                    for i in 0..terminals.row_count() {
                        if let Some(mut row) = terminals.row_data(i) {
                            if row.font_size > 0 {
                                row.font_size = 0;
                                terminals.set_row_data(i, row);
                            }
                        }
                    }
                    ui.window().request_redraw();
                    return;
                }
                if tab_id.is_empty() || tab_id == "welcome" {
                    return;
                }
                let Some(index) = terminal_index(&terminals, tab_id.as_str()) else {
                    return;
                };
                let Some(row) = terminals.row_data(index) else {
                    return;
                };
                let size = if direction == 0 {
                    settings_size.clamp(8, 32)
                } else {
                    let base = if row.font_size > 0 {
                        row.font_size
                    } else {
                        ui.get_term_font_size() as i32
                    };
                    (base + direction).clamp(8, 32)
                };
                let mut row = row;
                row.font_size = size;
                terminals.set_row_data(index, row);
                ui.window().request_redraw();
            },
        );
    }

    // Terminal full screen: hides the title bar and both side panels, leaving
    // only the terminal area (F11 / `fullscreen-key` in the UI).
    {
        let weak = ui.as_weak();
        ui.on_toggle_term_fullscreen(move || {
            if let Some(ui) = weak.upgrade() {
                let next = !ui.get_term_fullscreen();
                ui.set_term_fullscreen(next);
                ui.window().request_redraw();
            }
        });
    }
    // Leaving full screen when the active tab is not a terminal, mirroring
    // upstream `term-fullscreen-check`.
    {
        let weak = ui.as_weak();
        ui.on_term_fullscreen_check(move || {
            if let Some(ui) = weak.upgrade() {
                if ui.get_term_fullscreen() {
                    let id = ui.get_active_tab_id().to_string();
                    if !id.starts_with("term-") {
                        ui.set_term_fullscreen(false);
                        ui.window().request_redraw();
                    }
                }
            }
        });
    }

    // NIC selector: remember the choice for the active tab. The next stats
    // tick repaints the graph from that interface.
    {
        let weak = ui.as_weak();
        let terms = terms.clone();
        ui.on_select_net_iface(move |iface: SharedString| {
            let Some(ui) = weak.upgrade() else { return };
            let active = ui.get_active_tab_id().to_string();
            if let Some(tab) = terms.borrow_mut().get_mut(&active) {
                tab.selected_iface = iface.to_string();
            }
            ui.set_net_selected(iface);
            ui.window().request_redraw();
        });
    }

    // Slint reports the real dock-area size once layout is known; upstream
    // recomputes the pane tiling from it (refresh_panes). A single pane simply
    // covers the whole area.
    {
        let panes = panes.clone();
        let splitters = splitters.clone();
        let split_state = split_state.clone();
        let content_size = content_size.clone();
        ui.on_content_resized(move |w: f32, h: f32| {
            let w = w.max(1.0);
            let h = h.max(1.0);
            content_size.set((w, h));
            refresh_panes(&panes, &splitters, (w, h), *split_state.borrow());
        });
    }

    // The dock area reports its own size (window minus the custom title bar).
    // Recompute the two edge panels and the central terminal rect from it.
    {
        let weak = ui.as_weak();
        ui.on_dock_area_resized(move |w: f32, h: f32| {
            if let Some(w_ui) = weak.upgrade() {
                crate::dock_layout::apply_dock(&w_ui, w, h);
            }
        });
    }

    // Splitter drag on either edge panel. `panel-extent-drag` carries the new
    // width of panel `index`; dock-layout-changed follows after collapse /
    // dock-side changes, and both simply re-derive the geometry.
    {
        let weak = ui.as_weak();
        let store = store.clone();
        // The dock splitter only reports moves (`panel-extent-drag`); there is
        // no drag-end event, and `persist-*-width` belongs to the classic
        // handles this port does not render. Write the config once the splitter
        // has been idle briefly instead of on every pointer move.
        let save_timer = Rc::new(RefCell::new(slint::Timer::default()));
        ui.on_panel_extent_drag(move |index: i32, pos: f32| {
            let Some(w_ui) = weak.upgrade() else { return };
            crate::dock_layout::drag_panel(&w_ui, index, pos);
            let Some(store) = store.clone() else { return };
            save_timer.borrow().start(
                slint::TimerMode::SingleShot,
                Duration::from_millis(400),
                move || save_layout(&store, &w_ui),
            );
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_dock_layout_changed(move || {
            if let Some(w_ui) = weak.upgrade() {
                let (w, h) = crate::dock_layout::last_size();
                crate::dock_layout::apply_dock(&w_ui, w, h);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_sidebar_collapsed(move |collapsed: bool| {
            if let Some(w_ui) = weak.upgrade() {
                w_ui.set_sidebar_collapsed(collapsed);
                let (w, h) = crate::dock_layout::last_size();
                crate::dock_layout::apply_dock(&w_ui, w, h);
                if let Some(store) = store.as_ref() {
                    save_layout(store, &w_ui);
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_welcome_collapsed(move |collapsed: bool| {
            if let Some(w_ui) = weak.upgrade() {
                w_ui.set_welcome_collapsed(collapsed);
                let (w, h) = crate::dock_layout::last_size();
                crate::dock_layout::apply_dock(&w_ui, w, h);
                if let Some(store) = store.as_ref() {
                    save_layout(store, &w_ui);
                }
            }
        });
    }
    // Legacy drag handles (the ones these callbacks belong to) are not rendered
    // by this port; the dock splitter persists through `save_layout` above.
    // Keep them bound so the upgrade path stays warning-free.
    if let Some(store) = store.clone() {
        ui.on_persist_sidebar_width(move |width: f32| {
            let mut st = store.borrow_mut();
            st.set_sidebar_width(width.clamp(180.0, 420.0));
            let _ = st.save();
        });
    }
    if let Some(store) = store.clone() {
        ui.on_persist_welcome_sidebar_width(move |width: f32| {
            let mut st = store.borrow_mut();
            st.set_welcome_sidebar_width(width.clamp(160.0, 480.0));
            let _ = st.save();
        });
    }
    ui.on_persist_sidebar_dock(|_dock: SharedString| {});
    ui.on_persist_welcome_sidebar_dock(|_dock: SharedString| {});
    {
        // `changed sidebar-collapsed` asks for a refresh; the panel list is
        // already current, so nothing else is needed.
        let weak = ui.as_weak();
        ui.on_refresh_sidebar(move || {
            if let Some(w_ui) = weak.upgrade() {
                let (w, h) = crate::dock_layout::last_size();
                crate::dock_layout::apply_dock(&w_ui, w, h);
            }
        });
    }

    // ———————— SSH sessions ————————
    {
        let weak = ui.as_weak();
        let store = store.clone();
        let tabs = tabs.clone();
        let panes = panes.clone();
        let terminals = terminals.clone();
        let terms = terms.clone();
        ui.on_connect_session(move |id: SharedString| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(session) = store
                .as_ref()
                .and_then(|store| store.borrow().get(id.as_str()).cloned())
            else {
                return;
            };
            if session.kind != SessionKind::Ssh {
                ui.set_ssh_import_hint(SharedString::from(
                    "OHOS 版当前只支持 SSH 会话（串口 / Telnet / RDP 尚未接入）",
                ));
                return;
            }
            connect_session(&ui, &tabs, &panes, &terminals, &terms, &session);
        });
    }

    // Connect-time prompt dialogs: host-key trust (upstream #109-5), missing
    // credentials (#110) and keyboard-interactive MFA (#86-MFA). Each answer
    // unblocks the SSH thread parked on that connection's gate; the dialog is
    // closed here so it disappears the moment the user clicks.
    {
        let weak = ui.as_weak();
        let terms = terms.clone();
        ui.on_hostkey_accept(move || {
            if answer_hostkey_prompt(&terms, true).is_some() {
                if let Some(ui) = weak.upgrade() {
                    ui.set_hostkey_prompt_open(false);
                    ui.window().request_redraw();
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        let terms = terms.clone();
        ui.on_hostkey_reject(move || {
            if answer_hostkey_prompt(&terms, false).is_some() {
                if let Some(ui) = weak.upgrade() {
                    ui.set_hostkey_prompt_open(false);
                    ui.window().request_redraw();
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        let terms = terms.clone();
        let store = store.clone();
        let view = view.clone();
        ui.on_cred_accept(move || {
            let Some(ui) = weak.upgrade() else { return };
            let reply = CredentialReply {
                user: ui.get_cred_user().to_string(),
                password: ui.get_cred_password().to_string(),
                remember: ui.get_cred_remember(),
            };
            let remember = reply.remember;
            let entered_user = reply.user.clone();
            let entered_password = reply.password.clone();
            let Some(prompt) = answer_credentials_prompt(&terms, reply) else { return };
            ui.set_cred_password(SharedString::default());
            ui.set_cred_remember(false);
            ui.set_cred_prompt_open(false);
            // "Remember for this session" writes the typed fields back to the
            // saved session, like upstream `persist_credentials` (#110).
            if remember {
                if let Some(store) = &store {
                    {
                        let mut guard = store.borrow_mut();
                        if let Some(mut session) = guard.get(&prompt.session_id).cloned() {
                            if prompt.need_user && !entered_user.trim().is_empty() {
                                session.user = entered_user.trim().to_string();
                            }
                            if prompt.need_password {
                                session.password =
                                    meatshell_core::config::Secret::new(entered_password.clone());
                            }
                            guard.upsert(session);
                            if let Err(err) = guard.save() {
                                eprintln!("meatshell-ohos: 保存凭据失败: {err}");
                            }
                        }
                    }
                    refresh_sessions(&ui, store, &view);
                }
            }
            ui.window().request_redraw();
        });
    }
    {
        let weak = ui.as_weak();
        let terms = terms.clone();
        ui.on_cred_reject(move || {
            if cancel_credentials_prompt(&terms).is_some() {
                if let Some(ui) = weak.upgrade() {
                    ui.set_cred_password(SharedString::default());
                    ui.set_cred_remember(false);
                    ui.set_cred_prompt_open(false);
                    ui.window().request_redraw();
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        let terms = terms.clone();
        ui.on_mfa_submit(move || {
            let Some(ui) = weak.upgrade() else { return };
            let answer = ui.get_mfa_answer().to_string();
            if answer_mfa_prompt(&terms, answer).is_some() {
                ui.set_mfa_answer(SharedString::default());
                ui.set_mfa_prompt_open(false);
                ui.window().request_redraw();
            }
        });
    }
    {
        let weak = ui.as_weak();
        let terms = terms.clone();
        ui.on_mfa_cancel(move || {
            if cancel_mfa_prompt(&terms).is_some() {
                if let Some(ui) = weak.upgrade() {
                    ui.set_mfa_answer(SharedString::default());
                    ui.set_mfa_prompt_open(false);
                    ui.window().request_redraw();
                }
            }
        });
    }

    // Keys typed into the focused TerminalView go straight to the PTY.
    {
        let terms = terms.clone();
        ui.on_send_key(
            move |id: SharedString, key: SharedString, ctrl: bool, alt: bool, shift: bool| {
                let map = terms.borrow();
                let Some(tab) = map.get(id.as_str()) else { return };
                let Some(session) = tab.session.as_ref() else { return };
                let bytes = terminal::key_bytes(
                    key.as_str(),
                    ctrl,
                    alt,
                    shift,
                    terminal::app_cursor(&tab.parser),
                );
                session.write(&bytes);
                if SYNC_INPUT.load(std::sync::atomic::Ordering::Relaxed) {
                    for (other_id, other) in map.iter() {
                        if other_id == id.as_str() {
                            continue;
                        }
                        if let Some(session) = other.session.as_ref() {
                            session.write(&bytes);
                        }
                    }
                }
            },
        );
    }

    // TerminalView reports its grid size once it has been laid out. The remote
    // PTY is resized to match; the local parser follows.
    {
        let weak = ui.as_weak();
        let terminals = terminals.clone();
        let terms = terms.clone();
        let highlight = highlight.clone();
        ui.on_terminal_resize(move |id: SharedString, cols: f32, rows: f32| {
            let cols = (cols.round() as i64).clamp(1, 1000) as u16;
            let rows = (rows.round() as i64).clamp(1, 1000) as u16;
            let Some(ui) = weak.upgrade() else { return };
            let changed = {
                let mut map = terms.borrow_mut();
                match map.get_mut(id.as_str()) {
                    Some(tab) => {
                        let (current_rows, current_cols) = tab.parser.screen().size();
                        if current_rows == rows && current_cols == cols {
                            false
                        } else {
                            tab.parser.screen_mut().set_size(rows, cols);
                            if let Some(session) = &tab.session {
                                session.resize(cols, rows);
                            }
                            tab.rows_used = i32::from(rows);
                            let rules = highlight.borrow();
                            terminal::rebuild(&tab.parser, tab.is_dark, &tab.spans, &rules);
                            true
                        }
                    }
                    None => false,
                }
            };
            if changed {
                if let Some(index) = terminal_index(&terminals, id.as_str()) {
                    if let Some(mut row) = terminals.row_data(index) {
                        row.rows_used = i32::from(rows);
                        terminals.set_row_data(index, row);
                    }
                }
                ui.window().request_redraw();
            }
        });
    }

    // Mouse events for TUI apps that enabled a mouse protocol (btop, htop,
    // mc, vim `set mouse=a`). The UI only calls this when the terminal row
    // reports `mouse-tracked`, so a click elsewhere still starts a local
    // selection.
    {
        let terms = terms.clone();
        ui.on_terminal_mouse(
            move |tab_id: SharedString, kind: i32, button: i32, row: i32, col: i32| -> bool {
                let map = terms.borrow();
                let Some(tab) = map.get(tab_id.as_str()) else {
                    return false;
                };
                let Some(session) = tab.session.as_ref() else {
                    return false;
                };
                let screen = tab.parser.screen();
                if screen.mouse_protocol_mode() == vt100::MouseProtocolMode::None {
                    return false;
                }
                let (rows, cols) = screen.size();
                let (btn, release) = match kind {
                    1 => (button as u8, true),  // release
                    2 => (35, false),           // drag with a button held
                    _ => (button as u8, false), // press
                };
                let bytes = encode_mouse_event(
                    btn,
                    release,
                    col,
                    row,
                    cols,
                    rows,
                    screen.mouse_protocol_encoding(),
                );
                session.write(&bytes);
                true
            },
        );
    }

    // ———————— SFTP panel ————————
    {
        let terms = terms.clone();
        let terminals = terminals.clone();
        let weak = ui.as_weak();
        ui.on_sftp_navigate(move |id: SharedString, path: SharedString| {
            let resolved = {
                let map = terms.borrow();
                map.get(id.as_str()).map(|tab| {
                    resolve_remote_path(&tab.sftp_path, tab.sftp_home.as_deref(), path.as_str())
                })
            };
            let Some(resolved) = resolved else { return };
            if sftp_send(&terms, id.as_str(), SftpCommand::ListDir(resolved.clone())) {
                update_sftp_row(&terminals, id.as_str(), |row| {
                    row.sftp_loading = true;
                    row.sftp_status = SharedString::from(format!("加载 {resolved}…"));
                });
                if let Some(ui) = weak.upgrade() {
                    ui.window().request_redraw();
                }
            }
        });
    }
    {
        let terms = terms.clone();
        let terminals = terminals.clone();
        let weak = ui.as_weak();
        ui.on_sftp_refresh(move |id: SharedString, path: SharedString| {
            let target = if path.is_empty() {
                terms
                    .borrow()
                    .get(id.as_str())
                    .map(|tab| tab.sftp_path.clone())
                    .unwrap_or_default()
            } else {
                path.to_string()
            };
            if target.is_empty() {
                return;
            }
            if sftp_send(&terms, id.as_str(), SftpCommand::RefreshDir(target.clone())) {
                update_sftp_row(&terminals, id.as_str(), |row| {
                    row.sftp_loading = true;
                    row.sftp_status = SharedString::from(format!("刷新 {target}…"));
                });
                if let Some(ui) = weak.upgrade() {
                    ui.window().request_redraw();
                }
            }
        });
    }
    {
        let terms = terms.clone();
        ui.on_sftp_tree_expand(move |id: SharedString, path: SharedString| {
            // Upstream does both: toggle the node *and* navigate into it.
            sftp_send(&terms, id.as_str(), SftpCommand::ToggleTreeNode(path.to_string()));
            sftp_send(&terms, id.as_str(), SftpCommand::ListDir(path.to_string()));
        });
    }
    {
        let terminals = terminals.clone();
        let weak = ui.as_weak();
        ui.on_sftp_toggle_select(move |id: SharedString, row: i32| {
            if let Some(index) = terminal_index(&terminals, id.as_str()) {
                if let Some(mut data) = terminals.row_data(index) {
                    toggle_entry(&data.sftp_entries, row.max(0) as usize);
                    data.sftp_selected_count = selected_count(&data.sftp_entries);
                    terminals.set_row_data(index, data);
                }
            }
            if let Some(ui) = weak.upgrade() {
                ui.window().request_redraw();
            }
        });
    }
    {
        let terminals = terminals.clone();
        let weak = ui.as_weak();
        ui.on_sftp_select_range(move |id: SharedString, from: i32, to: i32| {
            if let Some(index) = terminal_index(&terminals, id.as_str()) {
                if let Some(mut data) = terminals.row_data(index) {
                    select_range_rows(&data.sftp_entries, from, to);
                    data.sftp_selected_count = selected_count(&data.sftp_entries);
                    terminals.set_row_data(index, data);
                }
            }
            if let Some(ui) = weak.upgrade() {
                ui.window().request_redraw();
            }
        });
    }
    {
        let terms = terms.clone();
        let terminals = terminals.clone();
        let weak = ui.as_weak();
        ui.on_sftp_sort_request(move |id: SharedString, key: SharedString| {
            let key = key.to_string();
            let (next_key, next_dir) = {
                let mut map = terms.borrow_mut();
                let Some(tab) = map.get_mut(id.as_str()) else { return };
                if tab.sftp_sort_key == key {
                    tab.sftp_sort_dir = if tab.sftp_sort_dir > 0 { -1 } else { 1 };
                } else {
                    tab.sftp_sort_key = key.clone();
                    tab.sftp_sort_dir = 1;
                }
                (tab.sftp_sort_key.clone(), tab.sftp_sort_dir)
            };
            if let Some(index) = terminal_index(&terminals, id.as_str()) {
                if let Some(mut data) = terminals.row_data(index) {
                    let mut entries = entry_rows(&data.sftp_entries);
                    sort_sftp_entries(&mut entries, &next_key, next_dir);
                    data.sftp_entries = ModelRc::from(Rc::new(VecModel::from(entries)));
                    data.sftp_sort_key = SharedString::from(next_key);
                    data.sftp_sort_dir = next_dir;
                    terminals.set_row_data(index, data);
                }
            }
            if let Some(ui) = weak.upgrade() {
                ui.window().request_redraw();
            }
        });
    }
    {
        let terms = terms.clone();
        let terminals = terminals.clone();
        let weak = ui.as_weak();
        ui.on_sftp_clear_sort(move |id: SharedString| {
            {
                let mut map = terms.borrow_mut();
                if let Some(tab) = map.get_mut(id.as_str()) {
                    tab.sftp_sort_key.clear();
                    tab.sftp_sort_dir = 0;
                }
            }
            if let Some(index) = terminal_index(&terminals, id.as_str()) {
                if let Some(mut data) = terminals.row_data(index) {
                    let mut entries = entry_rows(&data.sftp_entries);
                    sort_sftp_entries(&mut entries, "", 0);
                    data.sftp_entries = ModelRc::from(Rc::new(VecModel::from(entries)));
                    data.sftp_sort_key = SharedString::new();
                    data.sftp_sort_dir = 0;
                    terminals.set_row_data(index, data);
                }
            }
            if let Some(ui) = weak.upgrade() {
                ui.window().request_redraw();
            }
        });
    }
    {
        let terms = terms.clone();
        ui.on_sftp_delete(move |id: SharedString, path: SharedString| {
            sftp_send(&terms, id.as_str(), SftpCommand::Delete(path.to_string()));
        });
    }
    {
        let terms = terms.clone();
        let terminals = terminals.clone();
        ui.on_sftp_delete_selected(move |id: SharedString| {
            let paths: Vec<String> = terminal_index(&terminals, id.as_str())
                .and_then(|index| terminals.row_data(index))
                .map(|row| {
                    selected_entries(&row.sftp_entries)
                        .into_iter()
                        .map(|entry| entry.full_path.to_string())
                        .collect()
                })
                .unwrap_or_default();
            for path in paths {
                sftp_send(&terms, id.as_str(), SftpCommand::Delete(path));
            }
        });
    }
    {
        let terms = terms.clone();
        let terminals = terminals.clone();
        let weak = ui.as_weak();
        ui.on_sftp_download(move |id: SharedString, remote: SharedString| {
            let remote = remote.to_string();
            if sftp_send(
                &terms,
                id.as_str(),
                SftpCommand::Download {
                    remote: remote.clone(),
                    local_dir: downloads_dir().to_string_lossy().to_string(),
                    conflict: DownloadConflict::KeepBoth,
                },
            ) {
                update_sftp_row(&terminals, id.as_str(), |row| {
                    row.sftp_status = SharedString::from(format!("开始下载 {}…", name_of(&remote)));
                });
                if let Some(ui) = weak.upgrade() {
                    ui.window().request_redraw();
                }
            }
        });
    }
    {
        let terms = terms.clone();
        let terminals = terminals.clone();
        let weak = ui.as_weak();
        ui.on_sftp_download_selected(move |id: SharedString| {
            let (remote_dir, entries) = terminal_index(&terminals, id.as_str())
                .and_then(|index| terminals.row_data(index))
                .map(|row| {
                    (
                        terms
                            .borrow()
                            .get(id.as_str())
                            .map(|tab| tab.sftp_path.clone())
                            .unwrap_or_default(),
                        selected_entries(&row.sftp_entries),
                    )
                })
                .unwrap_or_default();
            if entries.is_empty() {
                return;
            }
            let local_dir = downloads_dir().to_string_lossy().to_string();
            let command = if entries.len() == 1 {
                SftpCommand::Download {
                    remote: entries[0].full_path.to_string(),
                    local_dir,
                    conflict: DownloadConflict::KeepBoth,
                }
            } else {
                SftpCommand::DownloadArchive {
                    remote_dir,
                    names: entries
                        .iter()
                        .map(|entry| entry.name.to_string())
                        .collect(),
                    local_dir,
                }
            };
            if sftp_send(&terms, id.as_str(), command) {
                update_sftp_row(&terminals, id.as_str(), |row| {
                    row.sftp_status = SharedString::from(format!(
                        "开始下载 {} 个条目…",
                        entries.len()
                    ));
                });
                if let Some(ui) = weak.upgrade() {
                    ui.window().request_redraw();
                }
            }
        });
    }
    {
        let terms = terms.clone();
        let terminals = terminals.clone();
        let weak = ui.as_weak();
        ui.on_sftp_open_file(move |id: SharedString, path: SharedString| {
            let path = path.to_string();
            if sftp_send(
                &terms,
                id.as_str(),
                SftpCommand::Download {
                    remote: path.clone(),
                    local_dir: downloads_dir().to_string_lossy().to_string(),
                    conflict: DownloadConflict::KeepBoth,
                },
            ) {
                update_sftp_row(&terminals, id.as_str(), |row| {
                    row.sftp_status = SharedString::from(format!(
                        "{} 已下载到下载目录（OHOS 版暂无内置编辑器）",
                        name_of(&path)
                    ));
                });
                if let Some(ui) = weak.upgrade() {
                    ui.window().request_redraw();
                }
            }
        });
    }
    {
        let terms = terms.clone();
        let terminals = terminals.clone();
        ui.on_sftp_view(move |id: SharedString, path: SharedString| {
            let path = path.to_string();
            if sftp_send(
                &terms,
                id.as_str(),
                SftpCommand::Download {
                    remote: path.clone(),
                    local_dir: downloads_dir().to_string_lossy().to_string(),
                    conflict: DownloadConflict::KeepBoth,
                },
            ) {
                update_sftp_row(&terminals, id.as_str(), |row| {
                    row.sftp_status =
                        SharedString::from(format!("{} 已下载到下载目录", name_of(&path)));
                });
            }
        });
    }
    {
        let terms = terms.clone();
        let terminals = terminals.clone();
        ui.on_sftp_edit(move |id: SharedString, path: SharedString| {
            let path = path.to_string();
            if sftp_send(
                &terms,
                id.as_str(),
                SftpCommand::Download {
                    remote: path.clone(),
                    local_dir: downloads_dir().to_string_lossy().to_string(),
                    conflict: DownloadConflict::KeepBoth,
                },
            ) {
                update_sftp_row(&terminals, id.as_str(), |row| {
                    row.sftp_status = SharedString::from(format!(
                        "{} 已下载到下载目录（暂无内置编辑器）",
                        name_of(&path)
                    ));
                });
            }
        });
    }
    {
        let terms = terms.clone();
        let terminals = terminals.clone();
        let weak = ui.as_weak();
        ui.on_sftp_prompt_submit(
            move |id: SharedString, kind: SharedString, target: SharedString, value: SharedString| {
                let value = value.to_string();
                if let Some(ui) = weak.upgrade() {
                    ui.set_sftp_prompt_open(false);
                    ui.set_sftp_prompt_value(SharedString::new());
                }
                if value.trim().is_empty() {
                    return;
                }
                let command = match kind.as_str() {
                    "rename" => SftpCommand::Rename {
                        from: target.to_string(),
                        to: join_remote(
                            target
                                .as_str()
                                .trim_end_matches('/')
                                .rsplit_once('/')
                                .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
                                .unwrap_or("/"),
                            value.trim(),
                        ),
                    },
                    "mkdir" => SftpCommand::MkDir(join_remote(target.as_str(), value.trim())),
                    "touch" => SftpCommand::TouchFile(join_remote(target.as_str(), value.trim())),
                    "chmod" => {
                        let Ok(mode) = u32::from_str_radix(value.trim(), 8) else {
                            return;
                        };
                        SftpCommand::Chmod {
                            path: target.to_string(),
                            mode,
                        }
                    }
                    _ => return,
                };
                if sftp_send(&terms, id.as_str(), command) {
                    update_sftp_row(&terminals, id.as_str(), |row| {
                        row.sftp_status = SharedString::from("已提交，等待刷新…");
                    });
                    if let Some(ui) = weak.upgrade() {
                        ui.window().request_redraw();
                    }
                }
            },
        );
    }
    {
        let weak = ui.as_weak();
        ui.on_sftp_chmod_open(
            move |_id: SharedString, path: SharedString, name: SharedString, mode: i32| {
                let Some(ui_handle) = weak.upgrade() else { return };
                let mode = mode.max(0) as u32;
                ui_handle.set_chmod_open(true);
                ui_handle.set_chmod_path(path);
                ui_handle.set_chmod_name(name);
                ui_handle.set_chmod_or(mode & 0o400 != 0);
                ui_handle.set_chmod_ow(mode & 0o200 != 0);
                ui_handle.set_chmod_ox(mode & 0o100 != 0);
                ui_handle.set_chmod_gr(mode & 0o040 != 0);
                ui_handle.set_chmod_gw(mode & 0o020 != 0);
                ui_handle.set_chmod_gx(mode & 0o010 != 0);
                ui_handle.set_chmod_tr(mode & 0o004 != 0);
                ui_handle.set_chmod_tw(mode & 0o002 != 0);
                ui_handle.set_chmod_tx(mode & 0o001 != 0);
                let tab = _id.to_string();
                ui_handle.set_chmod_tab(SharedString::from(tab));
            },
        );
        let terms = terms.clone();
        let terminals = terminals.clone();
        let weak = ui.as_weak();
        ui.on_sftp_chmod_apply(move || {
            let Some(ui) = weak.upgrade() else { return };
            let mode = u32::from(ui.get_chmod_or()) << 8
                | u32::from(ui.get_chmod_ow()) << 7
                | u32::from(ui.get_chmod_ox()) << 6
                | u32::from(ui.get_chmod_gr()) << 5
                | u32::from(ui.get_chmod_gw()) << 4
                | u32::from(ui.get_chmod_gx()) << 3
                | u32::from(ui.get_chmod_tr()) << 2
                | u32::from(ui.get_chmod_tw()) << 1
                | u32::from(ui.get_chmod_tx());
            let tab = ui.get_chmod_tab().to_string();
            let path = ui.get_chmod_path().to_string();
            ui.set_chmod_open(false);
            if sftp_send(&terms, &tab, SftpCommand::Chmod { path, mode }) {
                update_sftp_row(&terminals, &tab, |row| {
                    row.sftp_status = SharedString::from("已修改权限，等待刷新…");
                });
                ui.window().request_redraw();
            }
        });
    }
    {
        let terms = terms.clone();
        let terminals = terminals.clone();
        let weak = ui.as_weak();
        ui.on_sftp_toggle_dual(move |id: SharedString| {
            let mut enabled = false;
            if let Some(index) = terminal_index(&terminals, id.as_str()) {
                if let Some(mut row) = terminals.row_data(index) {
                    enabled = !row.sftp_dual;
                    row.sftp_dual = enabled;
                    terminals.set_row_data(index, row);
                }
            }
            if !enabled {
                if let Some(ui) = weak.upgrade() {
                    ui.window().request_redraw();
                }
                return;
            }
            let mut delta = SftpDelta::default();
            {
                let mut map = terms.borrow_mut();
                if let Some(tab) = map.get_mut(id.as_str()) {
                    let path = tab.local_path.clone();
                    load_local_view(tab, path, &mut delta);
                }
            }
            update_sftp_row(&terminals, id.as_str(), |row| delta.apply(row));
            if let Some(ui) = weak.upgrade() {
                ui.window().request_redraw();
            }
        });
    }
    {
        let terms = terms.clone();
        let terminals = terminals.clone();
        let weak = ui.as_weak();
        let handler = move |id: SharedString, path: String| {
            let target = terms
                .borrow()
                .get(id.as_str())
                .map(|tab| resolve_local_path(&tab.local_path, &path));
            let Some(target) = target else { return };
            let mut delta = SftpDelta::default();
            {
                let mut map = terms.borrow_mut();
                if let Some(tab) = map.get_mut(id.as_str()) {
                    load_local_view(tab, target, &mut delta);
                }
            }
            update_sftp_row(&terminals, id.as_str(), |row| delta.apply(row));
            if let Some(ui) = weak.upgrade() {
                ui.window().request_redraw();
            }
        };
        let navigate = handler.clone();
        ui.on_sftp_local_navigate(move |id: SharedString, path: SharedString| {
            navigate(id, path.to_string());
        });
        let navigate_up = handler.clone();
        ui.on_sftp_local_up(move |id: SharedString| {
            navigate_up(id, "..".to_string());
        });
        let navigate_home = handler.clone();
        ui.on_sftp_local_home(move |id: SharedString| {
            navigate_home(id, "~".to_string());
        });
        let navigate_refresh = handler;
        ui.on_sftp_local_refresh(move |id: SharedString| {
            navigate_refresh(id, String::new());
        });
    }
    {
        let terminals = terminals.clone();
        let weak = ui.as_weak();
        let terminals_toggle = terminals.clone();
        let weak_toggle = weak.clone();
        ui.on_sftp_local_toggle_select(move |id: SharedString, row: i32| {
            if let Some(index) = terminal_index(&terminals_toggle, id.as_str()) {
                if let Some(mut data) = terminals_toggle.row_data(index) {
                    toggle_entry(&data.sftp_local_entries, row.max(0) as usize);
                    data.sftp_local_selected_count = selected_count(&data.sftp_local_entries);
                    terminals_toggle.set_row_data(index, data);
                }
            }
            if let Some(ui) = weak_toggle.upgrade() {
                ui.window().request_redraw();
            }
        });
        ui.on_sftp_local_select_range(move |id: SharedString, from: i32, to: i32| {
            if let Some(index) = terminal_index(&terminals, id.as_str()) {
                if let Some(mut data) = terminals.row_data(index) {
                    select_range_rows(&data.sftp_local_entries, from, to);
                    data.sftp_local_selected_count = selected_count(&data.sftp_local_entries);
                    terminals.set_row_data(index, data);
                }
            }
            if let Some(ui) = weak.upgrade() {
                ui.window().request_redraw();
            }
        });
    }
    {
        let terms = terms.clone();
        let terminals = terminals.clone();
        let weak = ui.as_weak();
        let terms_single = terms.clone();
        let terminals_single = terminals.clone();
        let weak_single = weak.clone();
        ui.on_sftp_local_upload(move |id: SharedString, local: SharedString| {
            let remote_dir = terms_single
                .borrow()
                .get(id.as_str())
                .map(|tab| tab.sftp_path.clone())
                .unwrap_or_default();
            if remote_dir.is_empty() {
                return;
            }
            let local = std::path::PathBuf::from(local.to_string());
            if sftp_send(
                &terms_single,
                id.as_str(),
                SftpCommand::Upload {
                    local: local.clone(),
                    remote_dir: remote_dir.clone(),
                },
            ) {
                update_sftp_row(&terminals_single, id.as_str(), |row| {
                    row.sftp_status = SharedString::from(format!(
                        "↑ 上传 {} 到 {remote_dir}…",
                        local
                            .file_name()
                            .map(|name| name.to_string_lossy().to_string())
                            .unwrap_or_default()
                    ));
                });
                if let Some(ui) = weak_single.upgrade() {
                    ui.window().request_redraw();
                }
            }
        });
        ui.on_sftp_local_upload_selected(move |id: SharedString| {
            let (remote_dir, entries) = terms
                .borrow()
                .get(id.as_str())
                .map(|tab| tab.sftp_path.clone())
                .map(|dir| {
                    (
                        dir,
                        terminal_index(&terminals, id.as_str())
                            .and_then(|index| terminals.row_data(index))
                            .map(|row| selected_entries(&row.sftp_local_entries))
                            .unwrap_or_default(),
                    )
                })
                .unwrap_or_default();
            if remote_dir.is_empty() || entries.is_empty() {
                return;
            }
            let mut count = 0;
            for entry in entries {
                if entry.is_dir {
                    continue;
                }
                if sftp_send(
                    &terms,
                    id.as_str(),
                    SftpCommand::Upload {
                        local: std::path::PathBuf::from(entry.full_path.to_string()),
                        remote_dir: remote_dir.clone(),
                    },
                ) {
                    count += 1;
                }
            }
            update_sftp_row(&terminals, id.as_str(), |row| {
                row.sftp_status = SharedString::from(format!("↑ 已加入 {count} 个上传任务"));
            });
            if let Some(ui) = weak.upgrade() {
                ui.window().request_redraw();
            }
        });
    }
    {
        let terms = terms.clone();
        let terminals = terminals.clone();
        let weak = ui.as_weak();
        let terms_single = terms.clone();
        let terminals_single = terminals.clone();
        let weak_single = weak.clone();
        ui.on_sftp_download_to_local(move |id: SharedString, remote: SharedString| {
            let local_dir = terms_single
                .borrow()
                .get(id.as_str())
                .map(|tab| tab.local_path.to_string_lossy().to_string())
                .unwrap_or_else(|| downloads_dir().to_string_lossy().to_string());
            let remote = remote.to_string();
            if sftp_send(
                &terms_single,
                id.as_str(),
                SftpCommand::Download {
                    remote: remote.clone(),
                    local_dir,
                    conflict: DownloadConflict::KeepBoth,
                },
            ) {
                update_sftp_row(&terminals_single, id.as_str(), |row| {
                    row.sftp_status = SharedString::from(format!("↓ 下载 {}…", name_of(&remote)));
                });
                if let Some(ui) = weak_single.upgrade() {
                    ui.window().request_redraw();
                }
            }
        });
        ui.on_sftp_download_selected_to_local(move |id: SharedString| {
            let local_dir = terms
                .borrow()
                .get(id.as_str())
                .map(|tab| tab.local_path.to_string_lossy().to_string())
                .unwrap_or_else(|| downloads_dir().to_string_lossy().to_string());
            let entries = terminal_index(&terminals, id.as_str())
                .and_then(|index| terminals.row_data(index))
                .map(|row| selected_entries(&row.sftp_entries))
                .unwrap_or_default();
            if entries.is_empty() {
                return;
            }
            if sftp_send(
                &terms,
                id.as_str(),
                SftpCommand::DownloadInto {
                    remotes: entries
                        .iter()
                        .map(|entry| entry.full_path.to_string())
                        .collect(),
                    local_dir,
                    conflict: DownloadConflict::KeepBoth,
                },
            ) {
                update_sftp_row(&terminals, id.as_str(), |row| {
                    row.sftp_status =
                        SharedString::from(format!("↓ 下载 {} 个条目…", entries.len()));
                });
                if let Some(ui) = weak.upgrade() {
                    ui.window().request_redraw();
                }
            }
        });
    }
    {
        let terms = terms.clone();
        let terminals = terminals.clone();
        ui.on_sftp_record_visit(move |id: SharedString, path: SharedString| {
            let recent = {
                let mut map = terms.borrow_mut();
                let Some(tab) = map.get_mut(id.as_str()) else { return };
                tab.recent.retain(|item| item != path.as_str());
                tab.recent.insert(0, path.to_string());
                tab.recent.truncate(20);
                tab.recent.clone()
            };
            update_sftp_row(&terminals, id.as_str(), |row| {
                row.sftp_recent = shared_list(&recent);
            });
        });
    }
    {
        let terms = terms.clone();
        let terminals = terminals.clone();
        let weak = ui.as_weak();
        ui.on_sftp_toggle_bookmark(move |id: SharedString, path: SharedString| {
            let (bookmarks, bookmarked) = {
                let mut map = terms.borrow_mut();
                let Some(tab) = map.get_mut(id.as_str()) else { return };
                if let Some(pos) = tab.bookmarks.iter().position(|item| item == path.as_str()) {
                    tab.bookmarks.remove(pos);
                } else {
                    tab.bookmarks.push(path.to_string());
                }
                (
                    tab.bookmarks.clone(),
                    tab.bookmarks.iter().any(|item| item == path.as_str()),
                )
            };
            update_sftp_row(&terminals, id.as_str(), |row| {
                row.sftp_bookmarks = shared_list(&bookmarks);
                row.sftp_bookmarked = bookmarked;
            });
            if let Some(ui) = weak.upgrade() {
                ui.window().request_redraw();
            }
        });
    }
    {
        let terms = terms.clone();
        let terminals = terminals.clone();
        let weak = ui.as_weak();
        ui.on_sftp_remove_bookmark(move |id: SharedString, path: SharedString| {
            let bookmarks = {
                let mut map = terms.borrow_mut();
                let Some(tab) = map.get_mut(id.as_str()) else { return };
                tab.bookmarks.retain(|item| item != path.as_str());
                tab.bookmarks.clone()
            };
            update_sftp_row(&terminals, id.as_str(), |row| {
                row.sftp_bookmarks = shared_list(&bookmarks);
                row.sftp_bookmarked = false;
            });
            if let Some(ui) = weak.upgrade() {
                ui.window().request_redraw();
            }
        });
    }
    {
        let terms = terms.clone();
        let terminals = terminals.clone();
        ui.on_sftp_clear_recent(move |id: SharedString| {
            if let Some(tab) = terms.borrow_mut().get_mut(id.as_str()) {
                tab.recent.clear();
            }
            update_sftp_row(&terminals, id.as_str(), |row| {
                row.sftp_recent = empty_model::<SharedString>();
            });
        });
    }
    {
        let terms = terms.clone();
        let terminals = terminals.clone();
        ui.on_sftp_places_refresh(move |id: SharedString| {
            let (bookmarks, recent) = terms
                .borrow()
                .get(id.as_str())
                .map(|tab| (tab.bookmarks.clone(), tab.recent.clone()))
                .unwrap_or_default();
            update_sftp_row(&terminals, id.as_str(), |row| {
                row.sftp_bookmarks = shared_list(&bookmarks);
                row.sftp_recent = shared_list(&recent);
            });
        });
    }
    {
        let terms = terms.clone();
        let terminals = terminals.clone();
        ui.on_sftp_copy_to_target(
            move |id: SharedString, remote: SharedString, target: SharedString| {
                let target_info = terms
                    .borrow()
                    .get(target.as_str())
                    .and_then(|tab| {
                        tab.sftp
                            .as_ref()
                            .map(|client| (client.target(), tab.sftp_path.clone()))
                    });
                let Some((sender, target_dir)) = target_info else {
                    update_sftp_row(&terminals, id.as_str(), |row| {
                        row.sftp_status = SharedString::from("目标标签页没有 SFTP 连接");
                    });
                    return;
                };
                sftp_send(
                    &terms,
                    id.as_str(),
                    SftpCommand::CopyTo {
                        remotes: vec![remote.to_string()],
                        target: sender,
                        target_dir,
                    },
                );
            },
        );
    }
    {
        let terms = terms.clone();
        let terminals = terminals.clone();
        ui.on_sftp_copy_selected_to_target(move |id: SharedString, target: SharedString| {
            let target_info = terms
                .borrow()
                .get(target.as_str())
                .and_then(|tab| {
                    tab.sftp
                        .as_ref()
                        .map(|client| (client.target(), tab.sftp_path.clone()))
                });
            let Some((sender, target_dir)) = target_info else {
                update_sftp_row(&terminals, id.as_str(), |row| {
                    row.sftp_status = SharedString::from("目标标签页没有 SFTP 连接");
                });
                return;
            };
            let entries = terminal_index(&terminals, id.as_str())
                .and_then(|index| terminals.row_data(index))
                .map(|row| selected_entries(&row.sftp_entries))
                .unwrap_or_default();
            if entries.is_empty() {
                return;
            }
            sftp_send(
                &terms,
                id.as_str(),
                SftpCommand::CopyTo {
                    remotes: entries
                        .iter()
                        .map(|entry| entry.full_path.to_string())
                        .collect(),
                    target: sender,
                    target_dir,
                },
            );
        });
    }
    {
        let terminals = terminals.clone();
        ui.on_persist_sftp_local_pane_width(move |id: SharedString, width: f32| {
            update_sftp_row(&terminals, id.as_str(), |row| {
                row.sftp_local_pane_width = width;
            });
        });
    }
    // Panel geometry is per tab in `TerminalState`; Slint drives these while the
    // divider / collapse button is dragged, exactly like upstream's Rust host.
    {
        let terminals = terminals.clone();
        ui.on_set_pane_sftp_height(move |id: SharedString, height: f32| {
            update_sftp_row(&terminals, id.as_str(), |row| {
                row.sftp_panel_height = height;
            });
        });
    }
    {
        let terminals = terminals.clone();
        ui.on_set_pane_sftp_width(move |id: SharedString, width: f32| {
            update_sftp_row(&terminals, id.as_str(), |row| {
                row.sftp_panel_width = width;
            });
        });
    }
    {
        let terminals = terminals.clone();
        ui.on_set_pane_sftp_collapsed(move |id: SharedString, collapsed: bool| {
            update_sftp_row(&terminals, id.as_str(), |row| {
                row.sftp_collapsed = collapsed;
            });
        });
    }
    {
        let terminals = terminals.clone();
        ui.on_set_pane_sftp_saved_height(move |id: SharedString, height: f32| {
            update_sftp_row(&terminals, id.as_str(), |row| {
                row.sftp_saved_height = height;
            });
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_persist_sftp_tree_width(move |width: f32| {
            if let Some(ui) = weak.upgrade() {
                ui.set_sftp_tree_width(width);
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_toggle_sftp_column(move |key: SharedString| {
            let Some(ui) = weak.upgrade() else { return };
            match key.as_str() {
                "type" => ui.set_sftp_show_type(!ui.get_sftp_show_type()),
                "size" => ui.set_sftp_show_size(!ui.get_sftp_show_size()),
                "modified" => ui.set_sftp_show_modified(!ui.get_sftp_show_modified()),
                "permissions" => ui.set_sftp_show_permissions(!ui.get_sftp_show_permissions()),
                "owner" => ui.set_sftp_show_owner(!ui.get_sftp_show_owner()),
                "group" => ui.set_sftp_show_group(!ui.get_sftp_show_group()),
                _ => {}
            }
        });
    }
    {
        let terminals = terminals.clone();
        let terms_upload = terms.clone();
        ui.on_sftp_upload_clicked({
            let terminals = terminals.clone();
            move |id: SharedString, _remote_dir: SharedString, folder: bool| {
                if terms_upload.borrow().get(id.as_str()).is_none() {
                    set_sftp_status_all(&terminals, "请先连接会话再上传文件");
                    return;
                }
                // ArkTS opens the system document picker on the next poll and
                // hands the staged paths back through `slintUploadFiles`.
                if let Ok(mut slot) = PENDING_UPLOAD_PICK.lock() {
                    *slot = Some((id.to_string(), folder));
                }
                update_sftp_row(&terminals, id.as_str(), |row| {
                    row.sftp_status = SharedString::from("正在打开文件选择器…");
                });
            }
        });
        ui.on_sftp_paste_upload({
            let terminals = terminals.clone();
            move |_, _| set_sftp_status_all(&terminals, "OHOS 版暂不支持剪贴板上传")
        });
        ui.on_sftp_copy_path({
            let terminals = terminals.clone();
            move |_| set_sftp_status_all(&terminals, "OHOS 版暂不支持系统剪贴板")
        });
        ui.on_sftp_local_copy_path({
            let terminals = terminals.clone();
            move |_| set_sftp_status_all(&terminals, "OHOS 版暂不支持系统剪贴板")
        });
        ui.on_sftp_open_external({
            let terminals = terminals.clone();
            move |_, _| set_sftp_status_all(&terminals, "OHOS 版暂无外部程序打开")
        });
        ui.on_sftp_edit_external({
            let terminals = terminals.clone();
            move |_, _| set_sftp_status_all(&terminals, "OHOS 版暂无外部编辑器")
        });
        ui.on_sftp_local_open({
            let terminals = terminals.clone();
            move |_, _| set_sftp_status_all(&terminals, "OHOS 版暂无默认程序打开")
        });
        ui.on_sftp_drag_out({
            let terminals = terminals.clone();
            move |_, _| set_sftp_status_all(&terminals, "OHOS 版暂不支持拖拽到文件管理器")
        });
    }

    // ———————— runtime SSH tunnels (#206) ————————
    // Form values arrive as strings; validation mirrors upstream
    // `tunnel_callbacks.rs` (kind, bind port, target host/port).
    {
        let terms = terms.clone();
        let store = store.clone();
        ui.on_tunnel_add(
            move |tab_id: SharedString,
                  name: SharedString,
                  kind: SharedString,
                  bind: SharedString,
                  bind_port: SharedString,
                  host: SharedString,
                  host_port: SharedString,
                  save: bool,
                  auto_start: bool| {
                let kind = kind.to_string();
                if !matches!(kind.as_str(), "local" | "remote" | "dynamic") {
                    return;
                }
                let Ok(bind_port) = bind_port.trim().parse::<u16>() else { return };
                if bind_port == 0 {
                    return;
                }
                let host_port = if kind == "dynamic" {
                    0
                } else {
                    match host_port.trim().parse::<u16>() {
                        Ok(port) if port > 0 => port,
                        _ => return,
                    }
                };
                if kind != "dynamic" && host.trim().is_empty() {
                    return;
                }
                let forward = PortForward {
                    id: uuid::Uuid::new_v4().to_string(),
                    kind: kind.clone(),
                    auto_start,
                    name: name.trim().to_string(),
                    bind_addr: bind.trim().to_string(),
                    bind_port,
                    host: host.trim().to_string(),
                    host_port,
                };
                let session_id = match terms.borrow().get(tab_id.as_str()) {
                    Some(tab) => tab.session_config.id.clone(),
                    None => return,
                };
                let mut stored = false;
                if save {
                    if let Some(store) = &store {
                        let mut guard = store.borrow_mut();
                        if let Some(mut session) = guard.get(&session_id).cloned() {
                            session.forwards.push(forward.clone());
                            guard.upsert(session);
                            if let Err(err) = guard.save() {
                                eprintln!("meatshell-ohos: 保存端口转发失败: {err}");
                            }
                            stored = true;
                        }
                    }
                }
                let tunnel_id = if stored {
                    format!("saved-{}", forward.id)
                } else {
                    format!("runtime-{}", forward.id)
                };
                let mut map = terms.borrow_mut();
                let Some(tab) = map.get_mut(tab_id.as_str()) else { return };
                if stored {
                    tab.session_config.forwards.push(forward.clone());
                }
                if let Some(session) = &tab.session {
                    session.add_tunnel(tunnel_id, forward);
                }
            },
        );
    }
    {
        let terms = terms.clone();
        ui.on_tunnel_stop(move |tab_id: SharedString, tunnel_id: SharedString| {
            if let Some(tab) = terms.borrow().get(tab_id.as_str()) {
                if let Some(session) = &tab.session {
                    session.stop_tunnel(tunnel_id.to_string());
                }
            }
        });
    }
    {
        let terms = terms.clone();
        ui.on_tunnel_start(move |tab_id: SharedString, tunnel_id: SharedString| {
            if let Some(tab) = terms.borrow().get(tab_id.as_str()) {
                if let Some(session) = &tab.session {
                    session.start_tunnel(tunnel_id.to_string());
                }
            }
        });
    }
    {
        let terms = terms.clone();
        let store = store.clone();
        ui.on_tunnel_delete(move |tab_id: SharedString, tunnel_id: SharedString| {
            let mut map = terms.borrow_mut();
            let Some(tab) = map.get_mut(tab_id.as_str()) else { return };
            let saved_id = tunnel_id.strip_prefix("saved-");
            if let Some(saved_id) = saved_id {
                tab.session_config.forwards.retain(|f| f.id != saved_id);
                if let Some(store) = &store {
                    let mut guard = store.borrow_mut();
                    if let Some(mut session) = guard.get(&tab.session_config.id).cloned() {
                        session.forwards.retain(|f| f.id != saved_id);
                        guard.upsert(session);
                        if let Err(err) = guard.save() {
                            eprintln!("meatshell-ohos: 删除端口转发失败: {err}");
                        }
                    }
                }
            }
            if let Some(session) = &tab.session {
                session.delete_tunnel(tunnel_id.to_string());
            }
        });
    }

    // ———————— command bar: quick commands + history ————————
    {
        let terms = terms.clone();
        let send: crate::quick_commands::SendBytes = Rc::new(move |tab_id, bytes| {
            let map = terms.borrow();
            match tab_id {
                Some(id) => {
                    if let Some(session) = map.get(id).and_then(|tab| tab.session.as_ref()) {
                        session.write(bytes);
                    }
                }
                None => {
                    for tab in map.values() {
                        if let Some(session) = tab.session.as_ref() {
                            session.write(bytes);
                        }
                    }
                }
            }
        });
        crate::quick_commands::register(ui, store.clone(), send);
    }

    // ———————— terminal selection / clipboard ————————
    // The terminal view reports drag and double-click selection in visible
    // cell coordinates; the highlight model and the copy path are local here
    // (upstream also spans scrollback, which this port does not render).
    {
        let terms = terms.clone();
        let pending = pending_action.clone();
        ui.on_term_select_start(move |id: SharedString, row: i32, col: i32, _ctrl: bool, shift: bool| {
            // Double tap = local word selection. Slint's own double-click never
            // fires here: touch presses inside the scroll area are delayed, so
            // the click counter never advances (spec §25.9).
            if !shift {
                let now = std::time::Instant::now();
                let word = {
                    let mut map = terms.borrow_mut();
                    let Some(tab) = map.get_mut(id.as_str()) else { return };
                    let (row, col) = (row.max(0) as u16, col.max(0) as u16);
                    let repeat = matches!(
                        tab.last_tap,
                        Some((then, r, c)) if (r, c) == (row, col)
                            && now.duration_since(then) < std::time::Duration::from_millis(400)
                    );
                    tab.skip_end_copy = false;
                    tab.last_tap = if repeat { None } else { Some((now, row, col)) };
                    if repeat {
                        if let Some((anchor, focus)) = terminal::word_bounds(&tab.parser, row, col) {
                            tab.sel_anchor = Some(anchor);
                            tab.sel_focus = Some(focus);
                            refresh_selection(tab);
                            tab.skip_end_copy = true;
                            Some(terminal::selected_text(&tab.parser, anchor, focus))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                };
                if let Some(text) = word {
                    queue_clipboard_copy(&pending, text);
                    return;
                }
            }
            move_selection(&terms, id.as_str(), row, col, shift);
        });
    }
    {
        let terms = terms.clone();
        ui.on_term_select_update(move |id: SharedString, row: i32, col: i32| {
            move_selection(&terms, id.as_str(), row, col, true);
        });
    }
    {
        let terms = terms.clone();
        let pending = pending_action.clone();
        ui.on_term_select_end(move |id: SharedString| {
            let text = {
                let mut map = terms.borrow_mut();
                let Some(tab) = map.get_mut(id.as_str()) else { return };
                // Double-tap word selection already copied and must stay highlighted.
                if tab.skip_end_copy {
                    tab.skip_end_copy = false;
                    return;
                }
                match (tab.sel_anchor, tab.sel_focus) {
                    (Some(anchor), Some(focus)) if anchor != focus => {
                        terminal::selected_text(&tab.parser, anchor, focus)
                    }
                    // A plain click clears the previous highlight.
                    _ => {
                        tab.sel_anchor = None;
                        tab.sel_focus = None;
                        refresh_selection(tab);
                        String::new()
                    }
                }
            };
            queue_clipboard_copy(&pending, text);
        });
    }
    {
        let terms = terms.clone();
        let pending = pending_action.clone();
        ui.on_term_select_word(move |id: SharedString, row: i32, col: i32| {
            let text = {
                let mut map = terms.borrow_mut();
                let Some(tab) = map.get_mut(id.as_str()) else { return };
                let (col, row) = (col.max(0) as u16, row.max(0) as u16);
                let Some((anchor, focus)) = terminal::word_bounds(&tab.parser, row, col) else { return };
                tab.sel_anchor = Some(anchor);
                tab.sel_focus = Some(focus);
                refresh_selection(tab);
                terminal::selected_text(&tab.parser, anchor, focus)
            };
            queue_clipboard_copy(&pending, text);
        });
    }
    {
        let terms = terms.clone();
        ui.on_clear_terminal(move |id: SharedString| {
            let mut map = terms.borrow_mut();
            let Some(tab) = map.get_mut(id.as_str()) else { return };
            // Wipe the local grid right away, then ask the shell to redraw
            // (Ctrl+L), mirroring the desktop "Clear buffer" behaviour.
            tab.spans.set_vec(Vec::new());
            tab.sel_anchor = None;
            tab.sel_focus = None;
            tab.selection.set_vec(Vec::new());
            tab.find_matches.set_vec(Vec::new());
            if let Some(session) = tab.session.as_ref() {
                session.write(&[0x0c]);
            }
        });
    }
    {
        let terms = terms.clone();
        ui.on_find_query_changed(move |id: SharedString, query: SharedString| {
            let mut map = terms.borrow_mut();
            let Some(tab) = map.get_mut(id.as_str()) else { return };
            let matches = terminal::find_matches(&tab.parser, query.as_str());
            tab.find_matches.set_vec(
                matches
                    .into_iter()
                    .map(|(row, col, len)| TermMatch {
                        row: i32::from(row),
                        col: i32::from(col),
                        len: i32::from(len),
                    })
                    .collect::<Vec<_>>(),
            );
        });
    }
    {
        let terms = terms.clone();
        let pending = pending_action.clone();
        ui.on_copy_terminal_text(move |id: SharedString| {
            let text = {
                let map = terms.borrow();
                let Some(tab) = map.get(id.as_str()) else { return };
                match (tab.sel_anchor, tab.sel_focus) {
                    (Some(anchor), Some(focus)) if anchor != focus => {
                        terminal::selected_text(&tab.parser, anchor, focus)
                    }
                    _ => terminal::visible_text(&tab.parser),
                }
            };
            queue_clipboard_copy(&pending, text);
        });
    }
    {
        // ArkTS reads the system pasteboard and hands the text back through
        // `ms_spike_clipboard_text` once it is available.
        let pending = pending_action.clone();
        ui.on_paste_from_clipboard(move |id: SharedString| {
            if let Ok(mut slot) = PASTE_TARGET.lock() {
                *slot = Some(id.to_string());
            } else {
                return;
            }
            *pending.borrow_mut() = Some("clipboard-get".to_string());
        });
    }
    {
        let weak = ui.as_weak();
        let terms = terms.clone();
        ui.on_paste_confirmed(move |id: SharedString| {
            let text = terms
                .borrow_mut()
                .get_mut(id.as_str())
                .and_then(|tab| tab.pending_paste.take());
            if let Some(ui) = weak.upgrade() {
                ui.set_paste_confirm_open(false);
            }
            if let Some(text) = text {
                if let Some(tab) = terms.borrow_mut().get_mut(id.as_str()) {
                    send_paste(tab, &text);
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        let terms = terms.clone();
        ui.on_paste_confirm_cancelled(move || {
            for tab in terms.borrow_mut().values_mut() {
                tab.pending_paste = None;
            }
            if let Some(ui) = weak.upgrade() {
                ui.set_paste_confirm_open(false);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let terms = terms.clone();
        set_clipboard_text_handler(Box::new(move |text: &str| {
            if let Some(ui) = weak.upgrade() {
                handle_pasted_text(&ui, &terms, text);
            }
        }));
    }
    {
        let pending = pending_action.clone();
        ui.on_copy_text(move |text: SharedString| {
            queue_clipboard_copy(&pending, text.to_string());
        });
    }

    // ———————— window controls (handed to ArkTS) ————————
    for (cb, action) in [
        ("minimize", "minimize"),
        ("maximize", "maximize-toggle"),
        ("close", "close"),
    ] {
        let pending = pending_action.clone();
        let action = action.to_string();
        let handler = move || {
            *pending.borrow_mut() = Some(action.clone());
        };
        match cb {
            "minimize" => ui.on_win_minimize(handler),
            "maximize" => ui.on_win_maximize_toggle(handler),
            _ => ui.on_win_close(handler),
        }
    }
    // Drag has no desktop counterpart to emulate: OHOS starts an OS-level
    // window move (window.startMoving) from the same request.
    {
        let pending = pending_action.clone();
        ui.on_win_drag(move || {
            *pending.borrow_mut() = Some("drag".to_string());
        });
    }

    // Initial pane size, overridden by the first content-resized callback.
    let _ = (content_w, content_h);
}

/// Persists the current three-column layout into the upstream config store, so
/// a restart restores the widths and collapse flags the user left behind.
fn save_layout(store: &Rc<RefCell<ConfigStore>>, ui: &AppWindow) {
    let mut st = store.borrow_mut();
    st.set_welcome_sidebar_width(ui.get_welcome_sidebar_width().clamp(160.0, 480.0));
    st.set_sidebar_width(ui.get_sidebar_width().clamp(180.0, 420.0));
    st.set_welcome_collapsed(ui.get_welcome_collapsed());
    st.set_sidebar_collapsed(ui.get_sidebar_collapsed());
    if let Err(e) = st.save() {
        eprintln!("meatshell-ohos: save layout: {e}");
    }
}

fn pane_row(panes: &Rc<VecModel<PaneInfo>>, pane_id: i32) -> Option<PaneInfo> {
    usize::try_from(pane_id).ok().and_then(|i| panes.row_data(i))
}

/// Current split geometry: at most two panes, divided either side by side
/// (`vertical`) or stacked, with a draggable ratio.
#[derive(Clone, Copy)]
struct SplitState {
    vertical: bool,
    ratio: f32,
}

fn empty_pane(id: i32) -> PaneInfo {
    PaneInfo {
        id,
        x: 0.0,
        y: 0.0,
        w: 0.0,
        h: 0.0,
        active_id: SharedString::default(),
        focused: id == 0,
        reserve_right: 0.0,
        tabs: ModelRc::from(Rc::new(VecModel::<TabInfo>::default())),
    }
}

/// Recompute both pane rects and the splitter bar from the current content
/// size, mirroring upstream `refresh_panes`. Tab ownership and active ids are
/// preserved; only geometry is rewritten.
fn refresh_panes(
    panes: &Rc<VecModel<PaneInfo>>,
    splitters: &Rc<VecModel<crate::SplitterInfo>>,
    content: (f32, f32),
    split: Option<SplitState>,
) {
    let (width, height) = (content.0.max(1.0), content.1.max(1.0));
    let gap = 4.0;
    let mut p0 = pane_row(panes, 0).unwrap_or_else(|| empty_pane(0));
    p0.id = 0;
    p0.x = 0.0;
    p0.y = 0.0;
    match split {
        None => {
            p0.w = width;
            p0.h = height;
            p0.focused = true;
            p0.reserve_right = 240.0;
            panes.set_vec(vec![p0]);
            splitters.set_vec(Vec::new());
        }
        Some(state) => {
            let mut p1 = pane_row(panes, 1).unwrap_or_else(|| empty_pane(1));
            p1.id = 1;
            if state.vertical {
                let usable = (width - gap).max(2.0);
                let first = (usable * state.ratio).clamp(usable * 0.15, usable * 0.85);
                p0.w = first;
                p0.h = height;
                p0.reserve_right = 0.0;
                p1.x = first + gap;
                p1.y = 0.0;
                p1.w = width - first - gap;
                p1.h = height;
                p1.reserve_right = 240.0;
                splitters.set_vec(vec![crate::SplitterInfo {
                    split_id: 0,
                    x: first,
                    y: 0.0,
                    w: gap,
                    h: height,
                    vertical: true,
                }]);
            } else {
                let usable = (height - gap).max(2.0);
                let first = (usable * state.ratio).clamp(usable * 0.15, usable * 0.85);
                p0.w = width;
                p0.h = first;
                p0.reserve_right = 240.0;
                p1.x = 0.0;
                p1.y = first + gap;
                p1.w = width;
                p1.h = height - first - gap;
                p1.reserve_right = 0.0;
                splitters.set_vec(vec![crate::SplitterInfo {
                    split_id: 0,
                    x: 0.0,
                    y: first,
                    w: width,
                    h: gap,
                    vertical: false,
                }]);
            }
            if !p0.focused && !p1.focused {
                p0.focused = true;
            }
            panes.set_vec(vec![p0, p1]);
        }
    }
}

/// Fold the second pane's tabs back into the first and drop the split.
fn merge_panes(
    panes: &Rc<VecModel<PaneInfo>>,
    splitters: &Rc<VecModel<crate::SplitterInfo>>,
    split_state: &Rc<RefCell<Option<SplitState>>>,
    content: (f32, f32),
) {
    if split_state.borrow_mut().take().is_none() {
        return;
    }
    let Some(mut p0) = pane_row(panes, 0) else {
        return;
    };
    let mut rows: Vec<TabInfo> = collect_tabs(&p0.tabs);
    if let Some(p1) = pane_row(panes, 1) {
        for tab in collect_tabs(&p1.tabs) {
            if !rows.iter().any(|row| row.id == tab.id) {
                rows.push(tab);
            }
        }
    }
    p0.tabs = ModelRc::from(Rc::new(VecModel::from(rows.clone())));
    if !rows.iter().any(|tab| tab.id == p0.active_id) {
        p0.active_id = rows.first().map(|tab| tab.id.clone()).unwrap_or_default();
    }
    p0.focused = true;
    panes.set_vec(vec![p0]);
    refresh_panes(panes, splitters, content, None);
}

/// Removes one tab from the shared tab model. Closing the last one leaves the
/// pane empty (wallpaper only), matching the 0.8.2 startup state. In a split,
/// a pane that loses its last tab is folded back into the other one.
fn close_tab(
    tabs: &Rc<VecModel<TabInfo>>,
    panes: &Rc<VecModel<PaneInfo>>,
    splitters: &Rc<VecModel<crate::SplitterInfo>>,
    split_state: &Rc<RefCell<Option<SplitState>>>,
    content: (f32, f32),
    id: &SharedString,
) {
    if let Some(index) = (0..tabs.row_count())
        .find(|&i| tabs.row_data(i).map(|tab| &tab.id == id).unwrap_or(false))
    {
        tabs.remove(index);
    }
    let split = panes.row_count() > 1;
    for i in 0..panes.row_count() {
        if let Some(mut pane) = panes.row_data(i) {
            if split {
                // Keep pane ownership: only drop the closed row from the strip.
                let mut rows = collect_tabs(&pane.tabs);
                rows.retain(|tab| tab.id != *id);
                pane.tabs = ModelRc::from(Rc::new(VecModel::from(rows)));
            } else {
                pane.tabs = ModelRc::from(Rc::new(VecModel::from(visible_tabs(tabs))));
            }
            if pane.active_id == *id {
                pane.active_id = pane.tabs.row_data(0).map(|tab| tab.id).unwrap_or_default();
            }
            panes.set_row_data(i, pane);
        }
    }
    if split {
        let empty = |index: i32| {
            pane_row(panes, index)
                .map(|pane| pane.tabs.row_count() == 0)
                .unwrap_or(false)
        };
        if empty(0) || empty(1) {
            merge_panes(panes, splitters, split_state, content);
        }
    }
}

fn visible_tabs(tabs: &Rc<VecModel<TabInfo>>) -> Vec<TabInfo> {
    (0..tabs.row_count()).filter_map(|i| tabs.row_data(i)).collect()
}

/// Same as [`visible_tabs`] for a pane snapshot, which is a `ModelRc`.
fn collect_tabs(tabs: &ModelRc<TabInfo>) -> Vec<TabInfo> {
    (0..tabs.row_count()).filter_map(|i| tabs.row_data(i)).collect()
}

fn update_pane_tabs(
    panes: &Rc<VecModel<PaneInfo>>,
    pane_id: i32,
    tabs: &Rc<VecModel<TabInfo>>,
    active: Option<&SharedString>,
) {
    if let Some(mut pane) = pane_row(panes, pane_id) {
        let split = panes.row_count() > 1;
        let mut rows = if split {
            // Split: the pane owns its tabs. Only a tab that was just created
            // (the `active` id) still has to be appended.
            collect_tabs(&pane.tabs)
        } else {
            visible_tabs(tabs)
        };
        if split {
            if let Some(active) = active {
                if !rows.iter().any(|tab| tab.id == *active) {
                    if let Some(tab) = visible_tabs(tabs)
                        .into_iter()
                        .find(|tab| tab.id == *active)
                    {
                        rows.push(tab);
                    }
                }
            }
        }
        pane.active_id = active
            .cloned()
            .or_else(|| {
                rows.iter()
                    .find(|tab| tab.id == pane.active_id)
                    .map(|tab| tab.id.clone())
            })
            .or_else(|| rows.first().map(|tab| tab.id.clone()))
            .unwrap_or_default();
        pane.tabs = ModelRc::from(Rc::new(VecModel::from(rows)));
        panes.set_row_data(pane_id as usize, pane);
    }
}

/// Rename one tab in place, both in the shared tab model and in every pane's
/// snapshot sub-model (the tab strips render the snapshots, not `tabs`).
fn set_tab_title(
    tabs: &Rc<VecModel<TabInfo>>,
    panes: &Rc<VecModel<PaneInfo>>,
    tab_id: &str,
    title: &str,
) {
    for i in 0..tabs.row_count() {
        if let Some(mut row) = tabs.row_data(i) {
            if row.id == tab_id {
                row.title_len = tab_title_len(title);
                row.title = SharedString::from(title);
                tabs.set_row_data(i, row);
                break;
            }
        }
    }
    for i in 0..panes.row_count() {
        if let Some(pane) = panes.row_data(i) {
            for j in 0..pane.tabs.row_count() {
                if let Some(mut tab) = pane.tabs.row_data(j) {
                    if tab.id == tab_id {
                        tab.title_len = tab_title_len(title);
                        tab.title = SharedString::from(title);
                        pane.tabs.set_row_data(j, tab);
                    }
                }
            }
        }
    }
}

/// Encode a mouse report for the PTY (X10 or SGR), mirroring upstream
/// `terminal::encode_mouse_event`: coordinates are 1-based and clamped to the
/// screen, which is what xterm-compatible applications expect.
fn encode_mouse_event(
    btn: u8,
    release: bool,
    col: i32,
    row: i32,
    cols: u16,
    rows: u16,
    encoding: vt100::MouseProtocolEncoding,
) -> Vec<u8> {
    let c = (col.clamp(0, cols.saturating_sub(1) as i32) as u16 + 1).clamp(1, 223);
    let r = (row.clamp(0, rows.saturating_sub(1) as i32) as u16 + 1).clamp(1, 223);
    match encoding {
        vt100::MouseProtocolEncoding::Sgr => {
            let final_byte = if release { b'm' } else { b'M' };
            format!("\x1b[<{btn};{c};{r}{}", final_byte as char).into_bytes()
        }
        _ => {
            let cb = btn as u16 + if release { 3 } else { 0 } + 32;
            vec![0x1b, b'[', b'M', cb as u8, (c + 32) as u8, (r + 32) as u8]
        }
    }
}

/// The pane that received the last focus event; new tabs open in it, like
/// upstream `active-pane`.
fn focused_pane_id(panes: &Rc<VecModel<PaneInfo>>) -> Option<i32> {
    (0..panes.row_count()).find(|&i| panes.row_data(i).map(|pane| pane.focused).unwrap_or(false))
        .map(|i| i as i32)
}

/// Until real SSH sessions are implemented, "+" can only offer the welcome
/// page. app.slint renders that view solely for a tab whose id is "welcome", so
/// re-activate the existing one instead of leaving an invisible duplicate tab
/// (and an empty pane) behind.
fn activate_welcome_tab(
    ui: &AppWindow,
    tabs: &Rc<VecModel<TabInfo>>,
    panes: &Rc<VecModel<PaneInfo>>,
    pane_id: i32,
) {
    let existing = (0..tabs.row_count())
        .filter_map(|i| tabs.row_data(i))
        .find(|tab| tab.kind.as_str() == "welcome")
        .map(|tab| tab.id);
    let id = match existing {
        Some(id) => id,
        None => {
            let tab = welcome_tab("新标签页");
            tabs.push(tab.clone());
            tab.id
        }
    };
    update_pane_tabs(panes, pane_id, tabs, Some(&id));
    ui.set_active_tab_id(id);
}

// ———————— keyboard mapping ————————
//
// ArkTS sends symbolic names for non-character keys ("Control", "UpArrow", …);
// Slint's special keys are encoded as private-use code points, so they have to
// be converted here. Plain characters pass through untouched.
fn slint_key_text(raw: &str) -> SharedString {
    use slint::platform::Key;
    let key = match raw {
        "Backspace" => Some(Key::Backspace),
        "Tab" => Some(Key::Tab),
        "Return" => Some(Key::Return),
        "Escape" => Some(Key::Escape),
        "Delete" => Some(Key::Delete),
        "Shift" => Some(Key::Shift),
        "ShiftR" => Some(Key::ShiftR),
        "Control" => Some(Key::Control),
        "ControlR" => Some(Key::ControlR),
        "Alt" => Some(Key::Alt),
        "AltGr" => Some(Key::AltGr),
        "Meta" => Some(Key::Meta),
        "MetaR" => Some(Key::MetaR),
        "CapsLock" => Some(Key::CapsLock),
        "Space" => Some(Key::Space),
        "UpArrow" => Some(Key::UpArrow),
        "DownArrow" => Some(Key::DownArrow),
        "LeftArrow" => Some(Key::LeftArrow),
        "RightArrow" => Some(Key::RightArrow),
        "Home" => Some(Key::Home),
        "End" => Some(Key::End),
        "PageUp" => Some(Key::PageUp),
        "PageDown" => Some(Key::PageDown),
        "Insert" => Some(Key::Insert),
        "F1" => Some(Key::F1),
        "F2" => Some(Key::F2),
        "F3" => Some(Key::F3),
        "F4" => Some(Key::F4),
        "F5" => Some(Key::F5),
        "F6" => Some(Key::F6),
        "F7" => Some(Key::F7),
        "F8" => Some(Key::F8),
        "F9" => Some(Key::F9),
        "F10" => Some(Key::F10),
        "F11" => Some(Key::F11),
        "F12" => Some(Key::F12),
        _ => None,
    };
    match key {
        Some(key) => SharedString::from(key),
        None => SharedString::from(raw),
    }
}

// ———————— live SSH terminals ————————

fn empty_model<T: Clone + 'static>() -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::<T>::default()))
}

/// Key auth without an explicit key: OHOS has no file picker, so look for the
/// conventional identity files in the app sandbox (`hdc file send` can drop
/// them there).
fn default_identity_key() -> Auth {
    let directory = std::env::var("MEATSHELL_DATA_DIR").unwrap_or_default();
    if directory.is_empty() {
        return Auth::Key(String::new());
    }
    for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
        if let Ok(pem) = std::fs::read_to_string(std::path::Path::new(&directory).join(name)) {
            if !pem.trim().is_empty() {
                return Auth::Key(pem);
            }
        }
    }
    Auth::Key(String::new())
}

/// Saved authentication fields → what the SSH engine expects. Shared by the
/// real terminal connection and the session editor's "test connection", so a
/// test exercises exactly the same credentials a later login would use.
pub(crate) fn auth_for_session(session: &Session) -> Auth {
    match &session.auth {
        AuthMethod::Password => Auth::Password(session.password.as_str().to_string()),
        AuthMethod::KeyboardInteractive => {
            Auth::KeyboardInteractive(session.password.as_str().to_string())
        }
        AuthMethod::Key => {
            let inline = session.private_key_inline.as_str();
            if !inline.is_empty() {
                Auth::Key(inline.to_string())
            } else if !session.private_key_path.is_empty() {
                // The path is honoured in case it points inside the app sandbox
                // or an already-mounted dir.
                // 注意：`/data/app/el2/.../files` 这类绝对路径在应用命名空间里
                // 不可见，直接读只得到空串，最终会被误报成「未保存私钥」。
                // 所以读失败时按文件名回落到应用自己的数据目录再试一次。
                let path = std::path::Path::new(&session.private_key_path);
                let mut pem = std::fs::read_to_string(path).ok();
                if pem.is_none() {
                    if let Some(name) = path.file_name() {
                        let dir = std::env::var("MEATSHELL_DATA_DIR").unwrap_or_default();
                        if !dir.is_empty() {
                            pem = std::fs::read_to_string(std::path::Path::new(&dir).join(name)).ok();
                        }
                    }
                }
                Auth::Key(pem.unwrap_or_default())
            } else {
                // No explicit path: fall back to the conventional identity files
                // under the app sandbox (opkg-style install).
                default_identity_key()
            }
        }
    }
}

/// Opens a terminal tab for a saved SSH session and starts the remote shell.
fn connect_session(
    ui: &AppWindow,
    tabs: &Rc<VecModel<TabInfo>>,
    panes: &Rc<VecModel<PaneInfo>>,
    terminals: &Rc<VecModel<TerminalState>>,
    terms: &TermRegistry,
    session: &Session,
) {
    let title = if session.name.trim().is_empty() {
        format!("{}@{}", session.user, session.host)
    } else {
        session.name.clone()
    };
    let tab_id = format!("term-{}", uuid::Uuid::new_v4());
    tabs.push(TabInfo {
        id: SharedString::from(tab_id.clone()),
        title: SharedString::from(title.clone()),
        title_len: tab_title_len(&title),
        kind: SharedString::from("terminal"),
        connected: false,
        local: false,
    });
    let pane_id = focused_pane_id(panes).unwrap_or(0);
    update_pane_tabs(panes, pane_id, tabs, Some(&SharedString::from(tab_id.clone())));
    ui.set_active_tab_id(SharedString::from(tab_id.clone()));

    // 80×24 until TerminalView reports the real grid; the queued resize reaches
    // the far side right after the shell starts.
    let parser = vt100::Parser::new(24, 80, 0);
    let spans = Rc::new(VecModel::<TermSpan>::default());
    let selection = Rc::new(VecModel::<TermMatch>::default());
    let find_matches = Rc::new(VecModel::<TermMatch>::default());
    let label = format!("{}@{}:{}", session.user, session.host, session.port);
    let status = format!("正在连接 {label}…");
    let is_dark = ui.get_dark_mode();

    terminals.push(TerminalState {
        id: SharedString::from(tab_id.clone()),
        status: SharedString::from(status.clone()),
        spans: ModelRc::from(spans.clone()),
        cursor_row: -1,
        cursor_col: -1,
        rows_used: 24,
        scroll_max: 0,
        scroll_offset: 0,
        is_alt_screen: false,
        mouse_tracked: false,
        find_matches: ModelRc::from(find_matches.clone()),
        selection: ModelRc::from(selection.clone()),
        sftp_path: SharedString::new(),
        sftp_entries: empty_model::<SftpEntry>(),
        sftp_status: SharedString::from("正在连接 SFTP…"),
        sftp_loading: false,
        sftp_tree_nodes: empty_model::<SftpTreeNode>(),
        sftp_selected_count: 0,
        sftp_sort_key: SharedString::new(),
        sftp_sort_dir: 0,
        sftp_available: true,
        sftp_bookmarks: empty_model::<SharedString>(),
        sftp_recent: empty_model::<SharedString>(),
        sftp_bookmarked: false,
        sftp_dual: false,
        sftp_local_path: SharedString::from(downloads_dir().to_string_lossy().to_string()),
        sftp_local_entries: empty_model::<SftpEntry>(),
        sftp_local_status: SharedString::new(),
        sftp_local_loading: false,
        sftp_local_selected_count: 0,
        sftp_local_pane_width: 0.0,
        font_size: 0,
        tunnels: empty_model::<TunnelInfo>(),
        sftp_collapsed: false,
        sftp_panel_height: 220.0,
        sftp_panel_width: 0.0,
        sftp_saved_height: 220.0,
    });

    let auth = auth_for_session(session);
    let gate = PromptGate::new();
    let ssh = SshSession::spawn(
        ConnectConfig {
            session_id: session.id.clone(),
            host: session.host.clone(),
            port: session.port,
            user: session.user.clone(),
            auth,
            cols: 80,
            rows: 24,
            forwards: session.forwards.clone(),
        },
        Arc::clone(&gate),
    );
    terms.borrow_mut().insert(
        tab_id,
        TermTab {
            parser,
            spans,
            session: Some(ssh),
            gate,
            is_dark,
            connected: false,
            rows_used: 24,
            label,
            sftp: None,
            session_config: session.clone(),
            sftp_path: String::new(),
            sftp_home: None,
            sftp_sort_key: String::new(),
            sftp_sort_dir: 0,
            bookmarks: Vec::new(),
            recent: Vec::new(),
            local_path: downloads_dir(),
            last_stats: None,
            selected_iface: String::new(),
            selection,
            sel_anchor: None,
            sel_focus: None,
            pending_paste: None,
            last_tap: None,
            skip_end_copy: false,
            find_matches,
        },
    );
    ui.window().request_redraw();
}

/// Repeated Slint timer that drains SSH events on the render thread. Slint
/// timers fire from `update_timers_and_animations()`, which `render_to` calls
/// every frame, so output arrives without any extra thread touching the UI.
fn start_poll_timer(
    ui: &AppWindow,
    tabs: Rc<VecModel<TabInfo>>,
    terminals: Rc<VecModel<TerminalState>>,
    terms: TermRegistry,
    resource: Rc<RefCell<ResourceState>>,
    highlight: Rc<RefCell<crate::output_highlight::Rules>>,
) -> slint::Timer {
    let timer = slint::Timer::default();
    let weak = ui.as_weak();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(16),
        move || {
            if let Some(ui) = weak.upgrade() {
                poll_sessions(&ui, &tabs, &terminals, &terms, &resource, &highlight);
            }
        },
    );
    timer
}

struct TerminalPoll {
    status: Option<String>,
    connected: bool,
    connected_changed: bool,
    cursor: (i32, i32),
    rows_used: i32,
    repaint: bool,
    tunnels: Option<Vec<TunnelInfo>>,
    mouse_tracked: bool,
    alt_screen: bool,
}

fn poll_sessions(
    ui: &AppWindow,
    tabs: &Rc<VecModel<TabInfo>>,
    terminals: &Rc<VecModel<TerminalState>>,
    terms: &TermRegistry,
    resource: &Rc<RefCell<ResourceState>>,
    highlight: &Rc<RefCell<crate::output_highlight::Rules>>,
) {
    // Connection-test results arrive from an SSH worker thread; the dialog is
    // not tied to a tab, so publish them before walking the live terminals.
    if let Some(status) = take_dialog_status() {
        ui.set_dialog_test_status(status.into());
        ui.window().request_redraw();
    }
    let ids: Vec<String> = terms.borrow().keys().cloned().collect();
    let mut repaint = false;
    for id in ids {
        let (poll, sftp) = {
            let mut map = terms.borrow_mut();
            let Some(tab) = map.get_mut(&id) else { continue };
            let events = tab.session.as_ref().map(|session| session.drain()).unwrap_or_default();
            let sftp_events = tab
                .sftp
                .as_ref()
                .map(|client| client.drain())
                .unwrap_or_default();
            if events.is_empty() && sftp_events.is_empty() {
                continue;
            }
            let was_connected = tab.connected;
            let mut painted = false;
            let mut status: Option<String> = None;
            let mut connected_changed = false;
            let mut sftp_disconnected = false;
            let mut tunnels_items: Option<Vec<TunnelInfo>> = None;
            for event in events {
                match event {
                    SshEvent::Tunnels(rows) => {
                        tunnels_items = Some(
                            rows.into_iter()
                                .map(|row| TunnelInfo {
                                    id: SharedString::from(row.id),
                                    name: SharedString::from(row.name),
                                    kind: SharedString::from(row.kind.clone()),
                                    bind: SharedString::from(format!(
                                        "{}:{}",
                                        row.bind_addr, row.bind_port
                                    )),
                                    target: if row.kind == "dynamic" {
                                        SharedString::from("SOCKS5")
                                    } else if row.host.is_empty() {
                                        SharedString::default()
                                    } else {
                                        SharedString::from(format!(
                                            "{}:{}",
                                            row.host, row.host_port
                                        ))
                                    },
                                    status: SharedString::from(row.status),
                                    active: row.active,
                                })
                                .collect(),
                        );
                    }
                    SshEvent::Stats(sample) => {
                        // Cache for instant replay on tab switch, and push to
                        // the side panel when this is the visible tab.
                        let is_active = ui.get_active_tab_id().as_str() == id;
                        let host = tab.session_config.host.clone();
                        tab.last_stats = Some(sample.clone());
                        if is_active && tab.connected {
                            resource.borrow_mut().show_remote(
                                ui,
                                &sample,
                                &host,
                                &id,
                                &tab.selected_iface,
                            );
                            repaint = true;
                        }
                    }
                    SshEvent::Data(bytes) => {
                        tab.parser.process(&bytes);
                        painted = true;
                    }
                    SshEvent::Connected => {
                        connected_changed |= !tab.connected;
                        tab.connected = true;
                        status = Some(format!("已连接 {}", tab.label));
                    }
                    SshEvent::Closed => {
                        connected_changed |= tab.connected;
                        tab.connected = false;
                        tab.session = None;
                        sftp_disconnected = true;
                        tunnels_items = Some(Vec::new());
                        status = Some("连接已断开".to_string());
                    }
                    SshEvent::Failed(err) => {
                        connected_changed |= tab.connected;
                        tab.connected = false;
                        tab.session = None;
                        sftp_disconnected = true;
                        tunnels_items = Some(Vec::new());
                        tab.parser
                            .process(format!("\r\n\x1b[31m{err}\x1b[0m\r\n").as_bytes());
                        status = Some(format!("连接失败: {err}"));
                        painted = true;
                    }
                }
            }
            let mut sftp = SftpDelta::default();
            if sftp_disconnected {
                tab.sftp = None;
                sftp.status = Some("SFTP 已断开".to_string());
                sftp.loading = Some(false);
            }
            // Start SFTP only after the shell authenticated: by then the host
            // key is pinned in known_hosts and the credentials are accepted, so
            // the SFTP connection opens silently instead of prompting twice.
            if tab.connected && !was_connected && tab.sftp.is_none() {
                match SftpClient::spawn(tab.session_config.clone()) {
                    Ok(client) => {
                        tab.sftp = Some(client);
                        sftp.status = Some("SFTP 已连接，正在加载目录…".to_string());
                        sftp.loading = Some(true);
                    }
                    Err(err) => {
                        sftp.status = Some(format!("SFTP 启动失败: {err}"));
                        sftp.loading = Some(false);
                    }
                }
            }
            if !sftp_events.is_empty() {
                apply_sftp_events(tab, sftp_events, &mut sftp);
            }
            if painted {
                let rules = highlight.borrow();
                terminal::rebuild(&tab.parser, tab.is_dark, &tab.spans, &rules);
            }
            (
                TerminalPoll {
                    status,
                    connected: tab.connected,
                    connected_changed,
                    cursor: terminal::cursor(&tab.parser),
                    rows_used: tab.rows_used,
                    repaint: painted,
                    tunnels: tunnels_items,
                    mouse_tracked: tab.parser.screen().mouse_protocol_mode()
                        != vt100::MouseProtocolMode::None,
                    alt_screen: tab.parser.screen().alternate_screen(),
                },
                sftp,
            )
        };
        repaint |= poll.repaint || sftp.dirty();
        if let Some(index) = terminal_index(terminals, &id) {
            if let Some(mut row) = terminals.row_data(index) {
                if let Some(status) = poll.status {
                    row.status = SharedString::from(status);
                }
                row.cursor_row = poll.cursor.0;
                row.cursor_col = poll.cursor.1;
                row.rows_used = poll.rows_used;
                row.mouse_tracked = poll.mouse_tracked;
                row.is_alt_screen = poll.alt_screen;
                if let Some(items) = poll.tunnels {
                    row.tunnels = ModelRc::from(Rc::new(VecModel::from(items)));
                }
                sftp.apply(&mut row);
                terminals.set_row_data(index, row);
            }
        }
        if poll.connected_changed {
            set_tab_connected(tabs, &id, poll.connected);
        }
    }
    // ———————— resource panel ————————
    // A connected remote tab owns the top of the side panel; everything else
    // (welcome tab, connecting, disconnected) falls back to this device.
    {
        let active_id = ui.get_active_tab_id().to_string();
        let cached = {
            let map = terms.borrow();
            map.get(&active_id).and_then(|tab| {
                if tab.connected {
                    tab.last_stats.as_ref().map(|sample| {
                        (
                            sample.clone(),
                            tab.session_config.host.clone(),
                            tab.selected_iface.clone(),
                        )
                    })
                } else {
                    None
                }
            })
        };
        let mut res = resource.borrow_mut();
        let ticked = res.tick_local(ui);
        match cached {
            Some((sample, host, preferred)) => {
                let effective = sample
                    .net
                    .iter()
                    .find(|(name, ..)| name.as_str() == preferred)
                    .or_else(|| sample.net.first())
                    .map(|(name, ..)| name.as_str())
                    .unwrap_or("");
                let already = res.active() == active_id.as_str()
                    && ui.get_net_selected().as_str() == effective;
                if !already {
                    res.show_remote(ui, &sample, &host, &active_id, &preferred);
                }
            }
            None => {
                if !res.active().is_empty() || ticked {
                    res.show_local(ui);
                }
            }
        }
    }

    surface_prompts(ui, terms);
    if repaint {
        ui.window().request_redraw();
    }
}

/// Row updates produced by one frame of SFTP events. Applied on the UI thread
/// after the `terms` borrow is released.
#[derive(Default)]
struct SftpDelta {
    status: Option<String>,
    loading: Option<bool>,
    path: Option<String>,
    entries: Option<ModelRc<SftpEntry>>,
    tree: Option<ModelRc<SftpTreeNode>>,
    selected: Option<i32>,
    bookmarks: Option<ModelRc<SharedString>>,
    recent: Option<ModelRc<SharedString>>,
    bookmarked: Option<bool>,
    local_path: Option<String>,
    local_entries: Option<ModelRc<SftpEntry>>,
    local_status: Option<String>,
    local_loading: Option<bool>,
    local_selected: Option<i32>,
}

impl SftpDelta {
    fn dirty(&self) -> bool {
        self.status.is_some()
            || self.loading.is_some()
            || self.path.is_some()
            || self.entries.is_some()
            || self.tree.is_some()
            || self.selected.is_some()
            || self.bookmarks.is_some()
            || self.recent.is_some()
            || self.bookmarked.is_some()
            || self.local_path.is_some()
            || self.local_entries.is_some()
            || self.local_status.is_some()
            || self.local_loading.is_some()
            || self.local_selected.is_some()
    }

    fn apply(&self, row: &mut TerminalState) {
        if let Some(status) = &self.status {
            row.sftp_status = SharedString::from(status.as_str());
        }
        if let Some(loading) = self.loading {
            row.sftp_loading = loading;
        }
        if let Some(path) = &self.path {
            row.sftp_path = SharedString::from(path.as_str());
        }
        if let Some(entries) = &self.entries {
            row.sftp_entries = entries.clone();
        }
        if let Some(tree) = &self.tree {
            row.sftp_tree_nodes = tree.clone();
        }
        if let Some(count) = self.selected {
            row.sftp_selected_count = count;
        }
        if let Some(bookmarks) = &self.bookmarks {
            row.sftp_bookmarks = bookmarks.clone();
        }
        if let Some(recent) = &self.recent {
            row.sftp_recent = recent.clone();
        }
        if let Some(bookmarked) = self.bookmarked {
            row.sftp_bookmarked = bookmarked;
        }
        if let Some(path) = &self.local_path {
            row.sftp_local_path = SharedString::from(path.as_str());
        }
        if let Some(entries) = &self.local_entries {
            row.sftp_local_entries = entries.clone();
        }
        if let Some(status) = &self.local_status {
            row.sftp_local_status = SharedString::from(status.as_str());
        }
        if let Some(loading) = self.local_loading {
            row.sftp_local_loading = loading;
        }
        if let Some(count) = self.local_selected {
            row.sftp_local_selected_count = count;
        }
    }
}

/// Translates upstream `RemoteEntry`s exactly like the desktop app does, so
/// sizes, timestamps and permission strings match the original byte for byte.
fn sftp_entries(entries: &[RemoteEntry], sort_key: &str, sort_dir: i32) -> Vec<SftpEntry> {
    let mut mapped: Vec<SftpEntry> = entries
        .iter()
        .map(|entry| SftpEntry {
            name: entry.name.clone().into(),
            full_path: entry.full_path.clone().into(),
            is_dir: entry.is_dir,
            size: if entry.is_dir {
                SharedString::new()
            } else {
                SharedString::from(format_size(entry.size))
            },
            size_bytes: entry.size as f32,
            modified: SharedString::from(format_mtime(entry.modified)),
            modified_ts: entry.modified as f32,
            mode: (entry.mode & 0o7777) as i32,
            file_type: SharedString::from(match entry.file_type.as_str() {
                "directory" => "文件夹",
                "symlink" => "符号链接",
                "socket" => "套接字",
                "block-device" => "块设备",
                "character-device" => "字符设备",
                "fifo" => "管道",
                _ => "文件",
            }),
            permissions: SharedString::from(format_permissions(entry.permissions_mode)),
            permissions_mode: entry.permissions_mode as i32,
            owner: entry.owner.clone().unwrap_or_default().into(),
            group: entry.group.clone().unwrap_or_default().into(),
            selected: false,
        })
        .collect();
    sort_sftp_entries(&mut mapped, sort_key, sort_dir);
    mapped
}

fn sftp_tree(nodes: &[RemoteTreeNode]) -> Vec<SftpTreeNode> {
    nodes
        .iter()
        .map(|node| SftpTreeNode {
            path: node.path.clone().into(),
            name: node.name.clone().into(),
            depth: node.depth as i32,
            expanded: node.expanded,
            has_children: node.has_children,
        })
        .collect()
}

fn sort_sftp_entries(entries: &mut [SftpEntry], key: &str, dir: i32) {
    use std::cmp::Ordering;

    let name_cmp = |a: &SftpEntry, b: &SftpEntry| natural_name_cmp(&a.name, &b.name);
    let default_cmp = |a: &SftpEntry, b: &SftpEntry| match (a.is_dir, b.is_dir) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => name_cmp(a, b),
    };

    if dir == 0 || key.is_empty() {
        entries.sort_by(default_cmp);
        return;
    }

    entries.sort_by(|a, b| {
        let group = match (a.is_dir, b.is_dir) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => Ordering::Equal,
        };
        if group != Ordering::Equal {
            return group;
        }
        let ord = match key {
            "size" => a
                .size_bytes
                .partial_cmp(&b.size_bytes)
                .unwrap_or(Ordering::Equal)
                .then_with(|| default_cmp(a, b)),
            "modified" => a
                .modified_ts
                .partial_cmp(&b.modified_ts)
                .unwrap_or(Ordering::Equal)
                .then_with(|| default_cmp(a, b)),
            "permissions" => a
                .permissions_mode
                .cmp(&b.permissions_mode)
                .then_with(|| default_cmp(a, b)),
            "owner" => a.owner.cmp(&b.owner).then_with(|| default_cmp(a, b)),
            "group" => a.group.cmp(&b.group).then_with(|| default_cmp(a, b)),
            "type" => a
                .file_type
                .cmp(&b.file_type)
                .then_with(|| default_cmp(a, b)),
            _ => name_cmp(a, b).then_with(|| default_cmp(a, b)),
        };
        if dir < 0 {
            ord.reverse()
        } else {
            ord
        }
    });
}

/// Natural ("file2" before "file10") case-insensitive comparison, copied from
/// the upstream sorter so the panel order is identical.
fn natural_name_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    natural_ascii_cmp(&a.to_lowercase(), &b.to_lowercase()).then_with(|| a.cmp(b))
}

fn natural_ascii_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let ab = a.as_bytes();
    let bb = b.as_bytes();
    let mut ai = 0;
    let mut bi = 0;
    while ai < ab.len() && bi < bb.len() {
        let ad = ab[ai].is_ascii_digit();
        let bd = bb[bi].is_ascii_digit();
        if ad && bd {
            let a_start = ai;
            let b_start = bi;
            while ai < ab.len() && ab[ai].is_ascii_digit() {
                ai += 1;
            }
            while bi < bb.len() && bb[bi].is_ascii_digit() {
                bi += 1;
            }
            let mut a_sig = a_start;
            let mut b_sig = b_start;
            while a_sig < ai && ab[a_sig] == b'0' {
                a_sig += 1;
            }
            while b_sig < bi && bb[b_sig] == b'0' {
                b_sig += 1;
            }
            let ord = (ai - a_sig)
                .cmp(&(bi - b_sig))
                .then_with(|| ab[a_sig..ai].cmp(&bb[b_sig..bi]))
                .then_with(|| (ai - a_start).cmp(&(bi - b_start)));
            if ord != Ordering::Equal {
                return ord;
            }
            continue;
        }
        let ord = ab[ai].cmp(&bb[bi]);
        if ord != Ordering::Equal {
            return ord;
        }
        ai += 1;
        bi += 1;
    }
    ab.len().cmp(&bb.len())
}

fn apply_sftp_events(tab: &mut TermTab, events: Vec<SftpEvent>, delta: &mut SftpDelta) {
    for event in events {
        match event {
            SftpEvent::SftpEntries { path, entries } => {
                tab.sftp_path = path.clone();
                if tab.sftp_home.is_none() {
                    tab.sftp_home = Some(path.clone());
                }
                if tab.recent.first().map(String::as_str) != Some(path.as_str()) {
                    tab.recent.retain(|item| item != &path);
                    tab.recent.insert(0, path.clone());
                    tab.recent.truncate(20);
                    delta.recent = Some(shared_list(&tab.recent));
                }
                delta.path = Some(path.clone());
                delta.bookmarked = Some(tab.bookmarks.contains(&path));
                delta.entries = Some(ModelRc::from(Rc::new(VecModel::from(sftp_entries(
                    &entries,
                    &tab.sftp_sort_key,
                    tab.sftp_sort_dir,
                )))));
                delta.loading = Some(false);
                delta.selected = Some(0);
            }
            SftpEvent::SftpStatus(text) => {
                delta.loading = Some(text.contains("加载") || text.contains("连接中"));
                delta.status = Some(text);
            }
            SftpEvent::SftpError(text) => {
                delta.loading = Some(false);
                delta.status = Some(text);
            }
            SftpEvent::SftpTreeUpdate(nodes) => {
                delta.tree = Some(ModelRc::from(Rc::new(VecModel::from(sftp_tree(&nodes)))));
            }
            SftpEvent::SftpTransfer {
                name,
                is_upload,
                transferred,
                total,
                state,
                msg,
                ..
            } => {
                let direction = if is_upload { "↑" } else { "↓" };
                let percent = if total > 0 {
                    format!(" {:.0}%", transferred as f64 * 100.0 / total as f64)
                } else {
                    String::new()
                };
                let mark = match state {
                    1 => "✓ ",
                    2 => "✗ ",
                    _ => "",
                };
                delta.loading = Some(state == 0);
                delta.status = Some(format!(
                    "{mark}{direction} {name}{percent}  {}/{}  {msg}",
                    format_size(transferred),
                    format_size(total)
                ));
                // 没有公共下载目录时，文件先落在沙箱；交给 ArkTS 用系统保存
                // 对话框导出到用户选定的位置（picker 兜底）。
                if state == 1 && !is_upload && !public_download_ready() {
                    let dir = downloads_dir();
                    if let Some(local) = finished_local_path(&dir, &name) {
                        if let Ok(mut queue) = PENDING_SAVE_EXPORT.lock() {
                            queue.push(local);
                        }
                    }
                }
            }
            SftpEvent::SftpDownloadInstead { path } => {
                if let Some(client) = tab.sftp.as_ref() {
                    client.send(SftpCommand::Download {
                        remote: path.clone(),
                        local_dir: downloads_dir().to_string_lossy().to_string(),
                        conflict: DownloadConflict::KeepBoth,
                    });
                    delta.status = Some(format!(
                        "{} 已改为下载到下载目录",
                        name_of(&path)
                    ));
                }
            }
            SftpEvent::SftpFileText { name, error, .. } => {
                delta.status = Some(if error.is_empty() {
                    format!("已读取 {name}（OHOS 版暂无内置编辑器）")
                } else {
                    format!("读取 {name} 失败: {error}")
                });
            }
            SftpEvent::LocalFilesChanged { path } => {
                if tab.local_path == std::path::PathBuf::from(&path) {
                    load_local_view(tab, tab.local_path.clone(), delta);
                }
            }
            SftpEvent::HostKeyPrompt { host, .. } => {
                delta.status = Some(format!(
                    "SFTP 需要确认 {host} 的主机密钥（请先在终端完成一次连接）"
                ));
            }
            SftpEvent::CredentialPrompt { host, .. } => {
                delta.status = Some(format!("SFTP 需要 {host} 的登录凭据"));
            }
            SftpEvent::MfaPrompt { host, .. } => {
                delta.status = Some(format!(
                    "SFTP 需要 {host} 的双重验证（OHOS 版暂不支持第二次验证码）"
                ));
            }
        }
    }
}

fn shared_list(items: &[String]) -> ModelRc<SharedString> {
    ModelRc::from(Rc::new(VecModel::from(
        items
            .iter()
            .map(|item| SharedString::from(item.as_str()))
            .collect::<Vec<_>>(),
    )))
}

fn name_of(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
}

/// Public Download directory handed over by ArkTS
/// (`environment.getUserDownloadDir`, e.g. `/storage/Users/currentUser/Download`)
/// once `ohos.permission.READ_WRITE_DOWNLOAD_DIRECTORY` is granted. Unset means
/// "permission not granted yet / denied": downloads fall back to the sandbox.
static PUBLIC_DOWNLOAD_DIR: std::sync::Mutex<Option<std::path::PathBuf>> =
    std::sync::Mutex::new(None);

/// Records the public Download directory. Called before the UI exists too, so
/// the first `UiState` already starts its local pane there.
pub(crate) fn set_public_download_dir(dir: &str) {
    let dir = dir.trim();
    if dir.is_empty() {
        return;
    }
    if let Ok(mut slot) = PUBLIC_DOWNLOAD_DIR.lock() {
        *slot = Some(std::path::PathBuf::from(dir));
    }
}

/// Downloads finished into the sandbox while no public directory is available
/// (permission denied). `take_action` hands them to ArkTS, which copies each
/// one out through the system save dialog — the picker fallback for ROMs whose
/// permission dialog cannot grant `READ_WRITE_DOWNLOAD_DIRECTORY`.
static PENDING_SAVE_EXPORT: std::sync::Mutex<Vec<std::path::PathBuf>> =
    std::sync::Mutex::new(Vec::new());

/// Tab whose SFTP panel asked for "Upload file / Upload folder" and is waiting
/// for the ArkTS document picker: `(tab id, folder mode)`. `take_action` hands
/// `pick-upload:file|folder` to ArkTS exactly once and remembers the tab in
/// `UPLOAD_DEST_TAB`, since the picker result arrives asynchronously and the
/// active tab may change meanwhile.
static PENDING_UPLOAD_PICK: std::sync::Mutex<Option<(String, bool)>> = std::sync::Mutex::new(None);

/// Where the paths picked by the system file picker should be uploaded.
static UPLOAD_DEST_TAB: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Set when 设置 → 导入连接 needs the system file picker; ArkTS polls it
/// through `take_action` (`pick-import`) and hands the chosen JSON back with
/// `ms_spike_import_picked`.
static IMPORT_PICK_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Short messages for ArkTS to show as a toast (`toast:<text>` action). OHOS
/// has no status line outside the SFTP panel, and the settings menu closes
/// before import/export finish, so a toast is the only visible feedback.
static PENDING_TOAST: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Set when the session editor's key page needs the system file picker; ArkTS
/// polls it through `take_action` (`pick-key`) and answers with
/// `ms_spike_key_picked`.
static KEY_PICK_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Set when 设置 → 壁纸 wants a photo from the system picker; ArkTS polls it
/// through `take_action` (`pick-wallpaper`) and answers with
/// `ms_spike_wallpaper_picked`.
static WALLPAPER_PICK_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Latest connection-test result produced by an SSH worker thread. The render
/// thread drains it in `poll_sessions` and writes `dialog-test-status`.
static PENDING_DIALOG_STATUS: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

thread_local! {
    /// Set by `session_editor::register_actions`; runs the merge when ArkTS
    /// hands back the picked import file.
    static IMPORT_PICKED_HANDLER: std::cell::RefCell<Option<Box<dyn Fn(&std::path::Path)>>> =
        const { std::cell::RefCell::new(None) };
    /// Set by the session editor's key page; receives the picked private key.
    static KEY_PICKED_HANDLER: std::cell::RefCell<Option<Box<dyn Fn(&std::path::Path)>>> =
        const { std::cell::RefCell::new(None) };
}

pub(crate) fn queue_toast(message: &str) {
    if let Ok(mut queue) = PENDING_TOAST.lock() {
        queue.push(message.to_string());
    }
}

/// Hands a sandbox file to ArkTS's system save dialog (`save-download:`),
/// used when the public download directory is not available.
pub(crate) fn queue_save_export(path: std::path::PathBuf) {
    if let Ok(mut queue) = PENDING_SAVE_EXPORT.lock() {
        queue.push(path);
    }
}

pub(crate) fn request_import_pick() {
    IMPORT_PICK_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

pub(crate) fn request_wallpaper_pick() {
    WALLPAPER_PICK_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

pub(crate) fn set_import_picked_handler(handler: Box<dyn Fn(&std::path::Path)>) {
    IMPORT_PICKED_HANDLER.with(|slot| {
        *slot.borrow_mut() = Some(handler);
    });
}

/// ArkTS copied the picked JSON into the sandbox and reports its path.
pub fn import_picked_file(path: &std::path::Path) -> Result<(), String> {
    IMPORT_PICKED_HANDLER.with(|slot| match slot.borrow().as_ref() {
        Some(handler) => {
            handler(path);
            Ok(())
        }
        None => Err("import handler not registered".to_string()),
    })
}

pub(crate) fn request_key_pick() {
    KEY_PICK_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

pub(crate) fn set_key_picked_handler(handler: Box<dyn Fn(&std::path::Path)>) {
    KEY_PICKED_HANDLER.with(|slot| {
        *slot.borrow_mut() = Some(handler);
    });
}

/// ArkTS copied the picked private key into `<filesDir>/keys` and reports it.
pub fn key_picked_file(path: &std::path::Path) -> Result<(), String> {
    KEY_PICKED_HANDLER.with(|slot| match slot.borrow().as_ref() {
        Some(handler) => {
            handler(path);
            Ok(())
        }
        None => Err("key handler not registered".to_string()),
    })
}

/// Connection-test result from an SSH worker thread; drained by
/// `poll_sessions` and written to the dialog's status line.
pub(crate) fn queue_dialog_status(message: &str) {
    if let Ok(mut slot) = PENDING_DIALOG_STATUS.lock() {
        *slot = Some(message.to_string());
    }
}

fn take_dialog_status() -> Option<String> {
    PENDING_DIALOG_STATUS
        .lock()
        .ok()
        .and_then(|mut slot| slot.take())
}

/// True when downloads land in the public Download directory and need no export.
pub(crate) fn public_download_ready() -> bool {
    PUBLIC_DOWNLOAD_DIR
        .lock()
        .map(|slot| slot.is_some())
        .unwrap_or(false)
}

/// Resolves the file a finished download wrote: `KeepBoth` renames conflicts to
/// `stem (n).ext`, so fall back to the newest sibling sharing the stem.
fn finished_local_path(dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    let direct = dir.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    let stem = name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name);
    let prefix = format!("{stem} (");
    let mut newest: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let file_name = entry.file_name().to_string_lossy().to_string();
        if !file_name.starts_with(&prefix) {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) else {
            continue;
        };
        if newest.as_ref().map(|(time, _)| modified > *time).unwrap_or(true) {
            newest = Some((modified, entry.path()));
        }
    }
    newest.map(|(_, path)| path)
}

/// Directory that receives downloads and backs the local dual-pane view.
pub(crate) fn downloads_dir() -> std::path::PathBuf {
    if let Ok(slot) = PUBLIC_DOWNLOAD_DIR.lock() {
        if let Some(dir) = slot.as_ref() {
            let _ = std::fs::create_dir_all(dir);
            return dir.clone();
        }
    }
    let root = std::env::var("MEATSHELL_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let dir = root.join("downloads");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn local_entries(dir: &std::path::Path) -> Result<Vec<SftpEntry>, String> {
    use std::os::unix::fs::PermissionsExt;

    let read = std::fs::read_dir(dir).map_err(|err| format!("读取本地目录失败: {err}"))?;
    let mut entries = Vec::new();
    for item in read.flatten() {
        let path = item.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let permissions_mode = meta.permissions().mode();
        let modified = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|dur| dur.as_secs().min(u32::MAX as u64) as u32)
            .unwrap_or(0);
        let is_dir = meta.is_dir();
        let file_type = if is_dir {
            "文件夹"
        } else if meta.file_type().is_symlink() {
            "符号链接"
        } else {
            "文件"
        };
        entries.push(SftpEntry {
            name: item.file_name().to_string_lossy().to_string().into(),
            full_path: path.to_string_lossy().to_string().into(),
            is_dir,
            size: if is_dir {
                SharedString::new()
            } else {
                SharedString::from(format_size(meta.len()))
            },
            size_bytes: meta.len() as f32,
            modified: SharedString::from(format_mtime(modified)),
            modified_ts: modified as f32,
            mode: (permissions_mode & 0o7777) as i32,
            file_type: SharedString::from(file_type),
            permissions: SharedString::from(format_permissions(permissions_mode)),
            permissions_mode: permissions_mode as i32,
            owner: SharedString::new(),
            group: SharedString::new(),
            selected: false,
        });
    }
    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => natural_name_cmp(&a.name, &b.name),
    });
    Ok(entries)
}

fn load_local_view(tab: &mut TermTab, path: std::path::PathBuf, delta: &mut SftpDelta) {
    let target = if path.as_os_str().is_empty() {
        downloads_dir()
    } else {
        path
    };
    match local_entries(&target) {
        Ok(entries) => {
            tab.local_path = target.clone();
            delta.local_path = Some(target.to_string_lossy().to_string());
            delta.local_entries = Some(ModelRc::from(Rc::new(VecModel::from(entries))));
            delta.local_status = Some(format!("本地: {}", target.to_string_lossy()));
            delta.local_loading = Some(false);
            delta.local_selected = Some(0);
        }
        Err(err) => {
            delta.local_status = Some(err);
            delta.local_loading = Some(false);
        }
    }
}

fn resolve_local_path(current: &std::path::Path, path: &str) -> std::path::PathBuf {
    let home = downloads_dir();
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return current.to_path_buf();
    }
    if trimmed == "~" {
        return home;
    }
    if trimmed == ".." {
        return current
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or(home);
    }
    let candidate = std::path::PathBuf::from(trimmed);
    if candidate.is_absolute() {
        candidate
    } else {
        current.join(candidate)
    }
}

fn resolve_remote_path(current: &str, home: Option<&str>, path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return current.to_string();
    }
    if trimmed == "~" {
        return home.unwrap_or("/").to_string();
    }
    if trimmed == ".." {
        let base = current.trim_end_matches('/');
        if base.is_empty() {
            return "/".to_string();
        }
        if let Some((parent, _)) = base.rsplit_once('/') {
            return if parent.is_empty() {
                "/".to_string()
            } else {
                parent.to_string()
            };
        }
        return "/".to_string();
    }
    if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("{}/{}", current.trim_end_matches('/'), trimmed)
    }
}

/// Opens whichever connect-time dialog the parked connection needs: host-key
/// trust, missing credentials or MFA. The dialog stays open until the user
/// answers; the SSH thread is blocked on that connection's gate meanwhile.
fn surface_prompts(ui: &AppWindow, terms: &TermRegistry) {
    let pending = terms.borrow().values().find_map(|tab| tab.gate.pending());
    match pending {
        Some(SshPrompt::HostKey(prompt)) => {
            if ui.get_hostkey_prompt_open() {
                return;
            }
            let target = format!("{}:{}", prompt.host, prompt.port);
            if prompt.changed {
                ui.set_hostkey_title(SharedString::from("主机密钥已变更"));
                ui.set_hostkey_message(SharedString::from(format!(
                    "警告：{target} 提供的主机密钥与本地记录不一致，可能存在中间人攻击。\
                     仅在确认服务器已重装或密钥已更换时继续。"
                )));
                ui.set_hostkey_confirm_label(SharedString::from("仍要连接"));
            } else {
                ui.set_hostkey_title(SharedString::from("未知主机密钥"));
                ui.set_hostkey_message(SharedString::from(format!(
                    "首次连接 {target}，请核对指纹后再决定是否信任。信任后将写入本地记录。"
                )));
                ui.set_hostkey_confirm_label(SharedString::from("信任并继续"));
            }
            ui.set_hostkey_detail(SharedString::from(format!(
                "主机: {target}\n密钥类型: {}\n指纹: {}",
                prompt.key_type, prompt.fingerprint
            )));
            ui.set_hostkey_changed(prompt.changed);
            ui.set_hostkey_prompt_open(true);
            ui.window().request_redraw();
        }
        Some(SshPrompt::Credentials(prompt)) => {
            if ui.get_cred_prompt_open() {
                return;
            }
            ui.set_cred_host(SharedString::from(format!("{}:{}", prompt.host, prompt.port)));
            ui.set_cred_need_user(prompt.need_user);
            ui.set_cred_need_password(prompt.need_password);
            ui.set_cred_user(SharedString::from(prompt.user.clone()));
            ui.set_cred_password(SharedString::default());
            ui.set_cred_remember(false);
            ui.set_cred_prompt_open(true);
            ui.window().request_redraw();
        }
        Some(SshPrompt::Mfa(prompt)) => {
            if ui.get_mfa_prompt_open() {
                return;
            }
            ui.set_mfa_host(SharedString::from(prompt.host.clone()));
            ui.set_mfa_prompt(SharedString::from(prompt.prompt.clone()));
            ui.set_mfa_echo(prompt.echo);
            ui.set_mfa_answer(SharedString::default());
            ui.set_mfa_prompt_open(true);
            ui.window().request_redraw();
        }
        None => {
            if ui.get_hostkey_prompt_open() {
                ui.set_hostkey_prompt_open(false);
            }
            if ui.get_cred_prompt_open() {
                ui.set_cred_prompt_open(false);
            }
            if ui.get_mfa_prompt_open() {
                ui.set_mfa_prompt_open(false);
            }
        }
    }
}

/// Forwards the host-key answer to whichever connection is waiting.
fn answer_hostkey_prompt(
    terms: &TermRegistry,
    accept: bool,
) -> Option<crate::ssh_session::HostKeyPrompt> {
    let map = terms.borrow();
    for tab in map.values() {
        if matches!(tab.gate.pending(), Some(SshPrompt::HostKey(_))) {
            return tab.gate.answer_host_key(accept);
        }
    }
    None
}

fn answer_credentials_prompt(
    terms: &TermRegistry,
    reply: CredentialReply,
) -> Option<CredentialPrompt> {
    let map = terms.borrow();
    for tab in map.values() {
        if matches!(tab.gate.pending(), Some(SshPrompt::Credentials(_))) {
            return tab.gate.answer_credentials(reply);
        }
    }
    None
}

fn cancel_credentials_prompt(terms: &TermRegistry) -> Option<CredentialPrompt> {
    let map = terms.borrow();
    for tab in map.values() {
        if matches!(tab.gate.pending(), Some(SshPrompt::Credentials(_))) {
            return tab.gate.cancel_credentials();
        }
    }
    None
}

fn answer_mfa_prompt(terms: &TermRegistry, answer: String) -> Option<MfaPrompt> {
    let map = terms.borrow();
    for tab in map.values() {
        if matches!(tab.gate.pending(), Some(SshPrompt::Mfa(_))) {
            return tab.gate.answer_mfa(answer);
        }
    }
    None
}

fn cancel_mfa_prompt(terms: &TermRegistry) -> Option<MfaPrompt> {
    let map = terms.borrow();
    for tab in map.values() {
        if matches!(tab.gate.pending(), Some(SshPrompt::Mfa(_))) {
            return tab.gate.cancel_mfa();
        }
    }
    None
}

fn sftp_send(terms: &TermRegistry, tab_id: &str, command: SftpCommand) -> bool {
    let map = terms.borrow();
    match map.get(tab_id).and_then(|tab| tab.sftp.as_ref()) {
        Some(client) => {
            client.send(command);
            true
        }
        None => false,
    }
}

fn update_sftp_row(
    terminals: &Rc<VecModel<TerminalState>>,
    tab_id: &str,
    update: impl FnOnce(&mut TerminalState),
) {
    if let Some(index) = terminal_index(terminals, tab_id) {
        if let Some(mut row) = terminals.row_data(index) {
            update(&mut row);
            terminals.set_row_data(index, row);
        }
    }
}

/// Writes the same status line into every tab, for callbacks whose Slint
/// signature does not identify the invoking tab.
fn set_sftp_status_all(terminals: &Rc<VecModel<TerminalState>>, message: &str) {
    for index in 0..terminals.row_count() {
        if let Some(mut row) = terminals.row_data(index) {
            row.sftp_status = SharedString::from(message);
            terminals.set_row_data(index, row);
        }
    }
}

fn entry_rows(model: &ModelRc<SftpEntry>) -> Vec<SftpEntry> {
    (0..model.row_count())
        .filter_map(|row| model.row_data(row))
        .collect()
}

fn selected_count(model: &ModelRc<SftpEntry>) -> i32 {
    (0..model.row_count())
        .filter_map(|row| model.row_data(row))
        .filter(|entry| entry.selected)
        .count() as i32
}

fn selected_entries(model: &ModelRc<SftpEntry>) -> Vec<SftpEntry> {
    (0..model.row_count())
        .filter_map(|row| model.row_data(row))
        .filter(|entry| entry.selected)
        .collect()
}

fn toggle_entry(model: &ModelRc<SftpEntry>, row: usize) {
    if let Some(mut entry) = model.row_data(row) {
        entry.selected = !entry.selected;
        model.set_row_data(row, entry);
    }
}

fn select_range_rows(model: &ModelRc<SftpEntry>, from: i32, to: i32) {
    let low = from.min(to).max(0) as usize;
    let high = from.max(to).max(0) as usize;
    for row in low..=high {
        if let Some(mut entry) = model.row_data(row) {
            entry.selected = true;
            model.set_row_data(row, entry);
        }
    }
}

fn join_remote(directory: &str, name: &str) -> String {
    format!("{}/{}", directory.trim_end_matches('/'), name)
}

fn terminal_index(terminals: &Rc<VecModel<TerminalState>>, id: &str) -> Option<usize> {
    (0..terminals.row_count()).find(|&i| {
        terminals
            .row_data(i)
            .map(|row| row.id.as_str() == id)
            .unwrap_or(false)
    })
}

fn set_tab_connected(tabs: &Rc<VecModel<TabInfo>>, id: &str, connected: bool) {
    for i in 0..tabs.row_count() {
        if let Some(mut tab) = tabs.row_data(i) {
            if tab.id.as_str() == id && tab.connected != connected {
                tab.connected = connected;
                tabs.set_row_data(i, tab);
            }
        }
    }
}

fn remove_terminal(terms: &TermRegistry, terminals: &Rc<VecModel<TerminalState>>, id: &str) {
    terms.borrow_mut().remove(id);
    if let Some(index) = terminal_index(terminals, id) {
        terminals.remove(index);
    }
}

fn frame_header(width: usize, height: usize) -> String {
    format!("P6\n{width} {height}\n255\n")
}

/// Custom title bar height reserved above the pane area (`app.slint` draws a
/// 38px immersive bar when `custom-titlebar` is on, which is its default).
const TITLEBAR_HEIGHT: usize = 38;

fn content_height(height: usize) -> usize {
    height.saturating_sub(TITLEBAR_HEIGHT)
}

/// Same shape upstream pushes into `tabs` at startup (`src/app/open_window.rs`).
/// The id is the literal "welcome" because app.slint only renders the welcome
/// view for a pane whose active tab id is exactly that.
fn welcome_tab(title: &str) -> TabInfo {
    TabInfo {
        id: SharedString::from("welcome"),
        title: SharedString::from(title),
        title_len: tab_title_len(title),
        kind: SharedString::from("welcome"),
        connected: false,
        local: false,
    }
}

/// Width estimate upstream uses for tab strips: ASCII counts as one column,
/// everything else as two (`tab_title_len` in src/app.rs).
fn tab_title_len(title: &str) -> i32 {
    title
        .chars()
        .map(|ch| if ch.is_ascii() { 1usize } else { 2usize })
        .sum::<usize>()
        .min(i32::MAX as usize) as i32
}

// ———————— session list model (port of upstream src/app/session_models.rs) ————————
//
// Simplified port of `build_session_rows`: no built-in local shells, no agent
// CLIs. Group ordering, the empty-folder placeholder row and collapse state
// follow upstream exactly.

/// The saved-session list as the UI sees it: `all` is the unfiltered,
/// group-ordered row set; `model` is what `AppWindow.sessions` renders (search
/// applied); `query` keeps the active search so a refresh after save/delete
/// does not drop the filter.
#[derive(Clone)]
pub(crate) struct SessionsView {
    pub model: Rc<VecModel<SessionInfo>>,
    pub all: Rc<RefCell<Vec<SessionInfo>>>,
    pub query: Rc<RefCell<String>>,
}

impl SessionsView {
    pub fn new(store: Option<&Rc<RefCell<ConfigStore>>>) -> Self {
        let rows = store.map(|store| build_session_rows(&store.borrow())).unwrap_or_default();
        Self {
            model: Rc::new(VecModel::from(rows.clone())),
            all: Rc::new(RefCell::new(rows)),
            query: Rc::new(RefCell::new(String::new())),
        }
    }
}

fn apply_search(view: &SessionsView) {
    let needle = view.query.borrow().trim().to_lowercase();
    let rows: Vec<SessionInfo> = if needle.is_empty() {
        view.all.borrow().clone()
    } else {
        view.all
            .borrow()
            .iter()
            .filter(|row| {
                row.name.to_lowercase().contains(&needle)
                    || row.host.to_lowercase().contains(&needle)
                    || row.user.to_lowercase().contains(&needle)
            })
            .cloned()
            .collect()
    };
    // Update rows in place when the count is unchanged: a full `set_vec`
    // rebuild recreates the rows and drops the pointer grab, which would end a
    // drag-to-reorder after a single hop.
    if view.model.row_count() == rows.len() {
        for (i, row) in rows.into_iter().enumerate() {
            view.model.set_row_data(i, row);
        }
    } else {
        view.model.set_vec(rows);
    }
}

/// Rebuild the row set from the store after a save/delete and re-apply the
/// active search, like upstream `sync_sessions_for_window`.
pub(crate) fn refresh_sessions(
    ui: &AppWindow,
    store: &Rc<RefCell<ConfigStore>>,
    view: &SessionsView,
) {
    *view.all.borrow_mut() = build_session_rows(&store.borrow());
    apply_search(view);
    ui.set_sessions_revision(ui.get_sessions_revision().saturating_add(1));
}

pub(crate) fn is_reserved_session_group(name: &str) -> bool {
    name.eq_ignore_ascii_case("default") || name.eq_ignore_ascii_case("system")
}

pub(crate) fn named_display_groups(explicit: &[String], sessions: &[Session]) -> Vec<String> {
    let mut named: Vec<String> = explicit
        .iter()
        .filter(|group| !is_reserved_session_group(group.trim()))
        .cloned()
        .chain(
            sessions
                .iter()
                .filter(|session| {
                    !session.group.is_empty() && !is_reserved_session_group(session.group.trim())
                })
                .map(|session| session.group.clone()),
        )
        .collect();
    named.sort_by_key(|group| group.to_lowercase());
    named.dedup();
    named
}

fn serial_session_detail(session: &Session) -> String {
    if session.kind != SessionKind::Serial {
        return String::new();
    }
    let parity = match session.parity.as_str() {
        "odd" => "O",
        "even" => "E",
        _ => "N",
    };
    format!(
        "{} · {} baud · {}{}{}",
        session.serial_port, session.baud_rate, session.data_bits, parity, session.stop_bits
    )
}

fn build_session_rows(store: &ConfigStore) -> Vec<SessionInfo> {
    let sessions = store.sessions();
    let collapsed = store.collapsed_session_groups();
    let group_is_collapsed = |group: &str| {
        collapsed
            .map(|groups| groups.iter().any(|collapsed| collapsed == group))
            .unwrap_or(true)
    };

    let has_default = sessions
        .iter()
        .any(|session| session.group.is_empty() || is_reserved_session_group(session.group.trim()));
    let mut named = named_display_groups(store.groups(), sessions);
    let mut display_groups = Vec::new();
    if has_default {
        display_groups.push("default".to_string());
    }
    display_groups.append(&mut named);

    // Placeholder row for an empty folder; id == "" marks it as a group header
    // with no session (used by the UI to gate the "delete group" action).
    let blank = |group: &str| SessionInfo {
        id: "".into(),
        name: "".into(),
        host: "".into(),
        serial_detail: "".into(),
        port: 0,
        user: "".into(),
        auth: "".into(),
        last_used: "".into(),
        group: group.into(),
        group_header: group.into(),
        collapsed: group_is_collapsed(group),
        builtin: false,
    };

    // No alphabetical sort inside a group: the stored Vec order is the user's
    // manual order, maintained by drag-to-reorder (same convention as upstream).
    let mut rows: Vec<SessionInfo> = Vec::new();
    for group in &display_groups {
        let group_sessions: Vec<&Session> = if group == "default" {
            sessions
                .iter()
                .filter(|session| {
                    session.group.is_empty() || is_reserved_session_group(session.group.trim())
                })
                .collect()
        } else {
            sessions.iter().filter(|session| &session.group == group).collect()
        };
        if group_sessions.is_empty() {
            rows.push(blank(group));
        } else {
            for (i, s) in group_sessions.into_iter().enumerate() {
                rows.push(SessionInfo {
                    id: s.id.clone().into(),
                    name: s.name.clone().into(),
                    host: s.host.clone().into(),
                    serial_detail: serial_session_detail(s).into(),
                    port: i32::from(s.port),
                    user: s.user.clone().into(),
                    auth: s.auth.as_str().into(),
                    last_used: s
                        .last_used
                        .clone()
                        .unwrap_or_else(|| "never".to_string())
                        .into(),
                    group: group.clone().into(),
                    group_header: if i == 0 { group.clone().into() } else { "".into() },
                    collapsed: group_is_collapsed(group),
                    builtin: false,
                });
            }
        }
    }
    rows
}
