//! OHOS port of the upstream session editor.
//!
//! Mirrors `src/app/session_editor.rs` (jump-row editing, secret reveal) and the
//! dialog half of `src/app/session_callbacks.rs` (new / edit / duplicate /
//! remove / move-to-group / submit) against the *unmodified* upstream
//! `SessionDraft` and `AppWindow` definitions.
//!
//! Deliberate reductions for this port:
//! * No local shells or agent CLIs, so `SessionKind::Serial` ports cannot be
//!   enumerated and the dialog's port picker stays empty.
//! * No RDP engine yet; RDP drafts are still saved faithfully.
//! * Port forwards and triggers are editable through the same row callbacks
//!   as upstream (`add/update/delete-forward|trigger`); validation mirrors
//!   `src/app/port_forward.rs` / `src/app/session_trigger.rs`.
//! * Connection test reuses the real SSH engine on a worker thread (12 s
//!   budget); unknown host keys are reported, never trusted silently.
//! * Key-file picking goes through the ArkTS document picker and stores the
//!   copy under `<filesDir>/keys`.

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use meatshell_core::config::{
    normalize_term_type, AuthMethod, ConfigStore, PortForward, Secret, Session, SessionKind,
    SessionLogMode, SessionTrigger, DEFAULT_TERM_TYPE,
};

use crate::ohos_app::{
    auth_for_session, is_reserved_session_group, named_display_groups, refresh_sessions,
    SessionsView,
};
use crate::ssh_session::{
    ConnectConfig, Event as SshEvent, Prompt as SshPrompt, PromptGate, SshSession,
};
use crate::{AppWindow, JumpHop, PortFwd, SessionDraft, TriggerDraft};

/// Mirrors upstream `MAX_JUMP_HOPS` (16 hops).
const MAX_JUMP_HOPS: usize = 16;
const RDP_RESOLUTION_FULLSCREEN: &str = "fullscreen";
const RDP_RESOLUTION_CUSTOM: &str = "custom";
const RDP_RESOLUTION_DEFAULT: &str = "1280x720";
const RDP_MIN_SIDE: i32 = 200;
const RDP_MAX_SIDE: i32 = 8192;
const RDP_DEFAULT_WIDTH: u16 = 1280;
const RDP_DEFAULT_HEIGHT: u16 = 720;

pub(crate) fn register(
    ui: &AppWindow,
    store: Option<Rc<RefCell<ConfigStore>>>,
    view: SessionsView,
) {
    // Editable rows for the open dialog. Both the dialog callbacks (row edits)
    // and the submit callback (validation) work on this same set, exactly like
    // upstream `edit_forwards` / `edit_triggers` / `edit_trigger_secrets`.
    let rows = DialogRows::new();
    register_dialog(ui, store.clone(), rows.clone());
    register_actions(ui, store, view, rows);
}

#[derive(Clone)]
struct DialogRows {
    forwards: Rc<RefCell<Vec<PortFwd>>>,
    triggers: Rc<RefCell<Vec<TriggerDraft>>>,
    /// Saved (masked) responses, index-aligned with `triggers`; a blank
    /// response field keeps the stored secret on save (#212).
    trigger_secrets: Rc<RefCell<Vec<Secret>>>,
}

impl DialogRows {
    fn new() -> Self {
        Self {
            forwards: Rc::new(RefCell::new(vec![blank_forward_draft()])),
            triggers: Rc::new(RefCell::new(vec![blank_trigger_draft()])),
            trigger_secrets: Rc::new(RefCell::new(vec![Secret::default()])),
        }
    }

    fn load(&self, session: &Session) {
        *self.forwards.borrow_mut() = forward_drafts(&session.forwards);
        if self.forwards.borrow().is_empty() {
            self.forwards.borrow_mut().push(blank_forward_draft());
        }
        *self.triggers.borrow_mut() = trigger_drafts(&session.triggers);
        *self.trigger_secrets.borrow_mut() =
            session.triggers.iter().map(|t| t.response.clone()).collect();
        if self.triggers.borrow().is_empty() {
            self.triggers.borrow_mut().push(blank_trigger_draft());
            self.trigger_secrets.borrow_mut().push(Secret::default());
        }
    }

    fn reset(&self) {
        *self.forwards.borrow_mut() = vec![blank_forward_draft()];
        *self.triggers.borrow_mut() = vec![blank_trigger_draft()];
        *self.trigger_secrets.borrow_mut() = vec![Secret::default()];
    }
}

/// Cancellation handle for the connection test currently in flight.
type TestCancel = Rc<RefCell<Option<Arc<AtomicBool>>>>;

/// How long a "test connection" may take before it reports a timeout.
const TEST_TIMEOUT: Duration = Duration::from_secs(12);

fn cancel_running_test(slot: &TestCancel) {
    if let Some(flag) = slot.borrow_mut().take() {
        flag.store(true, Ordering::SeqCst);
    }
}

/// Answers (or rejects) whatever prompt the test connection is parked on, so
/// the SSH worker can exit instead of waiting for the 2-minute human timeout.
fn reject_pending(gate: &PromptGate) {
    match gate.pending() {
        Some(SshPrompt::HostKey(_)) => {
            gate.answer_host_key(false);
        }
        Some(SshPrompt::Credentials(_)) => {
            gate.cancel_credentials();
        }
        Some(SshPrompt::Mfa(_)) => {
            gate.cancel_mfa();
        }
        None => {}
    }
}

/// Runs the dialog's "test connection": same credentials a real login would
/// use, no tab, no port forwards, and results are pushed back through
/// `queue_dialog_status` for the render thread to publish.
fn start_connection_test(
    w: &AppWindow,
    draft: &SessionDraft,
    existing: Option<Session>,
    slot: &TestCancel,
) {
    cancel_running_test(slot);
    // Empty password / inline key keep the stored secret of an edited session;
    // forwards are left out so a test never binds listeners.
    let session = session_from_draft(draft, existing.as_ref(), Vec::new(), Vec::new());
    if session.kind != SessionKind::Ssh {
        w.set_dialog_test_status("仅 SSH 会话支持连接测试".into());
        return;
    }
    if session.host.trim().is_empty() {
        w.set_dialog_test_status("请输入主机地址".into());
        return;
    }
    let auth = auth_for_session(&session);
    let host = session.host.clone();
    let port = session.port;
    let user = session.user.clone();
    let session_id = session.id.clone();
    let target = if user.is_empty() { host.clone() } else { format!("{user}@{host}") };

    let cancel = Arc::new(AtomicBool::new(false));
    *slot.borrow_mut() = Some(Arc::clone(&cancel));
    w.set_dialog_test_status("正在测试连接…".into());

    std::thread::spawn(move || {
        let gate = PromptGate::new();
        let ssh = SshSession::spawn(
            ConnectConfig {
                session_id,
                host: host.clone(),
                port,
                user: user.clone(),
                auth,
                cols: 80,
                rows: 24,
                forwards: Vec::new(),
            },
            Arc::clone(&gate),
        );
        let deadline = Instant::now() + TEST_TIMEOUT;
        let mut result: Option<String> = None;
        loop {
            if cancel.load(Ordering::SeqCst) {
                reject_pending(&gate);
                return;
            }
            if let Some(prompt) = gate.pending() {
                let message = match prompt {
                    SshPrompt::HostKey(_) => {
                        "目标主机指纹未信任，请先打开该会话完成确认".to_string()
                    }
                    SshPrompt::Credentials(prompt) => {
                        if prompt.need_user {
                            "请填写用户名和密码".to_string()
                        } else {
                            "请填写密码".to_string()
                        }
                    }
                    SshPrompt::Mfa(_) => {
                        "该主机需要交互式验证码，请打开会话登录后再测试".to_string()
                    }
                };
                reject_pending(&gate);
                result = Some(message);
                break;
            }
            let mut settled = false;
            for event in ssh.drain() {
                match event {
                    SshEvent::Connected => {
                        result = Some("连接正常".to_string());
                        settled = true;
                        break;
                    }
                    SshEvent::Failed(err) => {
                        result = Some(format!("连接失败: {err}"));
                        settled = true;
                        break;
                    }
                    _ => {}
                }
            }
            if settled {
                break;
            }
            if Instant::now() >= deadline {
                result = Some(format!("连接超时: {target}:{port}"));
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if let Some(message) = result {
            if !cancel.load(Ordering::SeqCst) {
                crate::ohos_app::queue_dialog_status(&message);
            }
        }
        // Dropping `ssh` sends Command::Close, tearing the test connection down
        // before this thread exits.
    });
}

// ———————— port-forward / trigger row helpers (ports of upstream app modules) ————————

fn blank_forward_draft() -> PortFwd {
    PortFwd {
        id: "".into(),
        kind: "local".into(),
        auto_start: true,
        name: "".into(),
        bind_addr: "127.0.0.1".into(),
        bind_port: "".into(),
        host: "".into(),
        host_port: "".into(),
    }
}

fn forward_drafts(forwards: &[PortForward]) -> Vec<PortFwd> {
    forwards
        .iter()
        .map(|forward| PortFwd {
            id: forward.id.clone().into(),
            kind: forward.kind.clone().into(),
            auto_start: forward.auto_start,
            name: forward.name.clone().into(),
            bind_addr: if forward.bind_addr.trim().is_empty() {
                "127.0.0.1".into()
            } else {
                forward.bind_addr.trim().into()
            },
            bind_port: forward.bind_port.to_string().into(),
            host: forward.host.clone().into(),
            host_port: if forward.kind == "dynamic" {
                "".into()
            } else {
                forward.host_port.to_string().into()
            },
        })
        .collect()
}

fn forward_model(forwards: &[PortFwd]) -> ModelRc<PortFwd> {
    ModelRc::from(Rc::new(VecModel::from(forwards.to_vec())))
}

fn validated_port_forwards(drafts: &[PortFwd]) -> Result<Vec<PortForward>, String> {
    let mut forwards = Vec::new();
    for draft in drafts {
        let is_blank = draft.name.trim().is_empty()
            && draft.bind_port.trim().is_empty()
            && draft.host.trim().is_empty()
            && draft.host_port.trim().is_empty();
        if is_blank {
            continue;
        }
        let bind_port = draft
            .bind_port
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|port| *port > 0)
            .ok_or("请输入有效的监听端口（1-65535）")?;
        let kind = draft.kind.as_str();
        let (host, host_port) = if kind == "dynamic" {
            (String::new(), 0)
        } else {
            let host = draft.host.trim();
            let host_port = draft.host_port.trim().parse::<u16>().ok().filter(|port| *port > 0);
            if host.is_empty() || host_port.is_none() {
                return Err("请输入目标主机和有效的目标端口（1-65535）".into());
            }
            (host.to_string(), host_port.unwrap())
        };
        forwards.push(PortForward {
            id: draft.id.to_string(),
            kind: kind.to_string(),
            auto_start: draft.auto_start,
            name: draft.name.trim().to_string(),
            bind_addr: if draft.bind_addr.trim().is_empty() {
                "127.0.0.1".to_string()
            } else {
                draft.bind_addr.trim().to_string()
            },
            bind_port,
            host,
            host_port,
        });
    }
    Ok(forwards)
}

fn blank_trigger_draft() -> TriggerDraft {
    TriggerDraft { expect: "".into(), response: "".into(), append_enter: true, repeat: false }
}

fn trigger_drafts(triggers: &[SessionTrigger]) -> Vec<TriggerDraft> {
    triggers
        .iter()
        .map(|trigger| TriggerDraft {
            expect: trigger.expect.clone().into(),
            response: "".into(),
            append_enter: trigger.append_enter,
            repeat: trigger.repeat,
        })
        .collect()
}

fn trigger_model(triggers: &[TriggerDraft]) -> ModelRc<TriggerDraft> {
    ModelRc::from(Rc::new(VecModel::from(triggers.to_vec())))
}

fn validated_triggers(
    drafts: &[TriggerDraft],
    saved_responses: &[Secret],
) -> Result<Vec<SessionTrigger>, String> {
    let mut out = Vec::new();
    for (index, draft) in drafts.iter().enumerate() {
        if draft.expect.trim().is_empty() && draft.response.is_empty() {
            continue;
        }
        if draft.expect.trim().is_empty() {
            return Err("请输入触发器的期望文本".into());
        }
        let response = if draft.response.is_empty() {
            saved_responses.get(index).cloned().unwrap_or_default()
        } else {
            Secret::new(draft.response.to_string())
        };
        if response.is_empty() {
            return Err("请输入触发器的回复内容".into());
        }
        out.push(SessionTrigger {
            expect: draft.expect.trim().to_string(),
            response,
            append_enter: draft.append_enter,
            repeat: draft.repeat,
        });
    }
    Ok(out)
}

// ———————— dialog internals (port of src/app/session_editor.rs) ————————

fn register_dialog(ui: &AppWindow, store: Option<Rc<RefCell<ConfigStore>>>, rows: DialogRows) {
    {
        let weak = ui.as_weak();
        ui.on_add_jump(move || {
            if let Some(w) = weak.upgrade() {
                update_rows(&w, |rows| {
                    if rows.len() < MAX_JUMP_HOPS {
                        rows.push(JumpHop { placeholder: true, ..JumpHop::default() });
                    }
                });
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_update_jump(move |index: i32, choice: i32| {
            let Some(w) = weak.upgrade() else {
                return;
            };
            let Some(id) = w.get_jump_ids().row_data(choice.max(0) as usize) else {
                return;
            };
            let Ok(index) = usize::try_from(index) else {
                return;
            };
            // In-place update: replacing the model would destroy the row whose
            // dropdown popup is still closing (#509 upstream).
            let rows = w.get_dialog_jumps();
            if let Some(row) = rows.row_data(index) {
                let placeholder = row.placeholder && id.trim().is_empty();
                rows.set_row_data(index, JumpHop { id, choice, placeholder });
                w.set_dialog_test_status("".into());
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_delete_jump(move |index: i32| {
            if let Some(w) = weak.upgrade() {
                update_rows(&w, |rows| {
                    if index >= 0 && (index as usize) < rows.len() {
                        rows.remove(index as usize);
                    }
                });
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_move_jump(move |from: i32, to: i32| {
            if let Some(w) = weak.upgrade() {
                update_rows(&w, |rows| move_row(rows, from, to));
            }
        });
    }
    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_reveal_session_secret(move |private_key: bool| -> SharedString {
            let Some(w) = weak.upgrade() else {
                return SharedString::new();
            };
            // A reveal request must come from an open editor after explicit
            // opt-in (same guard as upstream).
            if !w.get_dialog_open() || !w.get_dialog_allow_secret_reveal() {
                return SharedString::new();
            }
            let Some(store) = store.as_ref() else {
                return SharedString::new();
            };
            let store = store.borrow();
            let Some(session) = store.get(w.get_dialog_id().as_str()) else {
                return SharedString::new();
            };
            if private_key {
                session.private_key_inline.as_str().into()
            } else {
                session.password.as_str().into()
            }
        });
    }
    // Port-forward editor (#56, #277).
    {
        let weak = ui.as_weak();
        let rows = rows.clone();
        ui.on_add_forward(move || {
            rows.forwards.borrow_mut().push(blank_forward_draft());
            if let Some(w) = weak.upgrade() {
                w.set_dialog_forwards(forward_model(&rows.forwards.borrow()));
            }
        });
    }
    {
        let rows = rows.clone();
        ui.on_update_forward(move |index: i32, forward: PortFwd| {
            let i = index.max(0) as usize;
            let mut forwards = rows.forwards.borrow_mut();
            if i < forwards.len() {
                forwards[i] = forward;
            }
        });
    }
    {
        let weak = ui.as_weak();
        let rows = rows.clone();
        ui.on_delete_forward(move |index: i32| {
            let i = index.max(0) as usize;
            {
                let mut forwards = rows.forwards.borrow_mut();
                if i < forwards.len() {
                    forwards.remove(i);
                }
                if forwards.is_empty() {
                    forwards.push(blank_forward_draft());
                }
            }
            if let Some(w) = weak.upgrade() {
                w.set_dialog_forwards(forward_model(&rows.forwards.borrow()));
            }
        });
    }
    // Expect/send trigger editor (#212).
    {
        let weak = ui.as_weak();
        let rows = rows.clone();
        ui.on_add_trigger(move || {
            rows.triggers.borrow_mut().push(blank_trigger_draft());
            rows.trigger_secrets.borrow_mut().push(Secret::default());
            if let Some(w) = weak.upgrade() {
                w.set_dialog_triggers(trigger_model(&rows.triggers.borrow()));
            }
        });
    }
    {
        let rows = rows.clone();
        ui.on_update_trigger(move |index: i32, trigger: TriggerDraft| {
            let i = index.max(0) as usize;
            let mut triggers = rows.triggers.borrow_mut();
            if i < triggers.len() {
                triggers[i] = trigger;
            }
        });
    }
    {
        let weak = ui.as_weak();
        let rows = rows.clone();
        ui.on_delete_trigger(move |index: i32| {
            let i = index.max(0) as usize;
            {
                let mut triggers = rows.triggers.borrow_mut();
                let mut secrets = rows.trigger_secrets.borrow_mut();
                if i < triggers.len() {
                    triggers.remove(i);
                }
                if i < secrets.len() {
                    secrets.remove(i);
                }
                if triggers.is_empty() {
                    triggers.push(blank_trigger_draft());
                    secrets.push(Secret::default());
                }
            }
            if let Some(w) = weak.upgrade() {
                w.set_dialog_triggers(trigger_model(&rows.triggers.borrow()));
            }
        });
    }
}

fn update_rows(window: &AppWindow, update: impl FnOnce(&mut Vec<JumpHop>)) {
    let mut rows = window.get_dialog_jumps().iter().collect::<Vec<_>>();
    update(&mut rows);
    window.set_dialog_jumps(ModelRc::from(Rc::new(VecModel::from(rows))));
    window.set_dialog_test_status("".into());
}

fn move_row<T>(rows: &mut Vec<T>, from: i32, to: i32) {
    if from >= 0 && to >= 0 && (from as usize) < rows.len() && (to as usize) < rows.len() {
        let row = rows.remove(from as usize);
        rows.insert(to as usize, row);
    }
}

// ———————— open / submit / list actions ————————

fn register_actions(
    ui: &AppWindow,
    store: Option<Rc<RefCell<ConfigStore>>>,
    view: SessionsView,
    rows: DialogRows,
) {
    // Shared by the test / stop-test / draft-changed / close / submit handlers:
    // any of them may be the one that abandons the in-flight connection test.
    let test_cancel: TestCancel = Rc::new(RefCell::new(None));
    {
        let weak = ui.as_weak();
        let store = store.clone();
        let rows = rows.clone();
        ui.on_new_session_clicked(move || {
            let Some(w) = weak.upgrade() else {
                return;
            };
            let Some(store) = store.as_ref() else {
                w.set_dialog_test_status("配置不可用，无法新建会话".into());
                return;
            };
            fill_new_session(&w, &store.borrow(), &rows);
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        let rows = rows.clone();
        ui.on_edit_session(move |id: SharedString| {
            let Some(w) = weak.upgrade() else {
                return;
            };
            let Some(store) = store.as_ref() else {
                return;
            };
            let store = store.borrow();
            let Some(session) = store.get(id.as_str()) else {
                return;
            };
            fill_existing_session(&w, &store, session, &rows);
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        let view = view.clone();
        ui.on_duplicate_session(move |id: SharedString| {
            let Some(store) = store.as_ref() else {
                return;
            };
            let mut duplicated = false;
            {
                let mut s = store.borrow_mut();
                if let Some(orig) = s.get(id.as_str()).cloned() {
                    let mut copy = orig;
                    copy.id = new_id();
                    copy.name = format!("{} (copy)", copy.name);
                    copy.last_used = None;
                    s.upsert(copy);
                    if let Err(e) = s.save() {
                        eprintln!("meatshell-ohos: save config: {e}");
                    }
                    duplicated = true;
                }
            }
            if duplicated {
                if let Some(w) = weak.upgrade() {
                    refresh_sessions(&w, store, &view);
                }
            }
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        let view = view.clone();
        ui.on_remove_session(move |id: SharedString| {
            let Some(store) = store.as_ref() else {
                return;
            };
            {
                let mut s = store.borrow_mut();
                s.remove(id.as_str());
                if let Err(e) = s.save() {
                    eprintln!("meatshell-ohos: save config: {e}");
                }
            }
            if let Some(w) = weak.upgrade() {
                refresh_sessions(&w, store, &view);
            }
        });
    }

    {
        let store = store.clone();
        let view = view.clone();
        let weak = ui.as_weak();
        ui.on_move_session(move |id: SharedString, group: SharedString| {
            let Some(store) = store.as_ref() else {
                return;
            };
            let mut moved = false;
            {
                let mut s = store.borrow_mut();
                if let Some(orig) = s.get(id.as_str()).cloned() {
                    let mut target = orig;
                    // "default" is the display label for ungrouped → store empty.
                    target.group = if group.as_str().eq_ignore_ascii_case("default") {
                        String::new()
                    } else if is_reserved_session_group(group.as_str().trim()) {
                        // `system` belongs exclusively to built-in local shells.
                        return;
                    } else {
                        group.to_string()
                    };
                    s.upsert(target);
                    if let Err(e) = s.save() {
                        eprintln!("meatshell-ohos: save config: {e}");
                    }
                    moved = true;
                }
            }
            if moved {
                if let Some(w) = weak.upgrade() {
                    refresh_sessions(&w, store, &view);
                }
            }
        });
    }

    // Drag-to-reorder inside the welcome list. Rows are updated in place
    // (refresh_sessions → apply_search) so the dragged card keeps its grab;
    // persisting waits for `reorder-session-end` on pointer release, exactly
    // like upstream.
    {
        let weak = ui.as_weak();
        let store = store.clone();
        let view = view.clone();
        ui.on_reorder_session(move |id: SharedString, dir: i32| -> bool {
            let Some(w) = weak.upgrade() else {
                return false;
            };
            if !w.get_host_search_query().trim().is_empty() {
                return false;
            }
            let Some(store) = store.as_ref() else {
                return false;
            };
            let moved = store.borrow_mut().reorder_session(id.as_str(), dir as isize);
            if moved {
                refresh_sessions(&w, store, &view);
            }
            moved
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        let view = view.clone();
        ui.on_reorder_session_end(move || {
            let (Some(w), Some(store)) = (weak.upgrade(), store.clone()) else {
                return;
            };
            if let Err(e) = store.borrow_mut().save() {
                eprintln!("meatshell-ohos: save config: {e}");
            }
            refresh_sessions(&w, &store, &view);
        });
    }

    // Group create / rename (#41). The empty string is the dialog's error
    // channel: anything non-empty keeps the dialog open with that message.
    {
        let weak = ui.as_weak();
        let store = store.clone();
        let view = view.clone();
        ui.on_submit_group(move |orig: SharedString, name: SharedString| -> SharedString {
            let Some(w) = weak.upgrade() else {
                return SharedString::new();
            };
            let Some(store) = store.as_ref() else {
                return "配置不可用".into();
            };
            let trimmed = name.trim();
            let error = {
                let s = store.borrow();
                if trimmed.is_empty() {
                    Some("请输入分组名称")
                } else if is_reserved_session_group(trimmed) {
                    Some("该名称为系统保留分组")
                } else if (orig.is_empty() || !trimmed.eq_ignore_ascii_case(orig.as_str()))
                    && s.session_group_exists(trimmed)
                {
                    Some("分组已存在")
                } else {
                    None
                }
            };
            if let Some(message) = error {
                return message.into();
            }
            {
                let mut s = store.borrow_mut();
                if orig.is_empty() {
                    s.add_group(trimmed.to_string());
                } else {
                    s.rename_group(orig.as_str(), trimmed.to_string());
                }
                if let Err(e) = s.save() {
                    eprintln!("meatshell-ohos: save config: {e}");
                }
            }
            refresh_sessions(&w, store, &view);
            SharedString::new()
        });
    }

    // Group delete (#41); the UI only offers it on empty groups.
    {
        let weak = ui.as_weak();
        let store = store.clone();
        let view = view.clone();
        ui.on_delete_group(move |name: SharedString| {
            let (Some(w), Some(store)) = (weak.upgrade(), store.as_ref()) else {
                return;
            };
            {
                let mut s = store.borrow_mut();
                s.remove_group(&name.to_string());
                if let Err(e) = s.save() {
                    eprintln!("meatshell-ohos: save config: {e}");
                }
            }
            refresh_sessions(&w, store, &view);
        });
    }

    // Import hosts from an `ssh_config` the user pushed into the sandbox. OHOS
    // has no file picker, so the file is read from `import/ssh_config` (or
    // `import/.ssh/config`) instead of `~/.ssh/config`.
    {
        let weak = ui.as_weak();
        let store = store.clone();
        let view = view.clone();
        ui.on_import_ssh_config(move || {
            let Some(w) = weak.upgrade() else {
                return;
            };
            let Some(store) = store.as_ref() else {
                w.set_ssh_import_hint("配置不可用".into());
                return;
            };
            let dir = import_dir();
            let found = [dir.join("ssh_config"), dir.join(".ssh").join("config")]
                .iter()
                .find_map(|path| std::fs::read_to_string(path).ok().map(|text| (path.clone(), text)));
            let Some((path, text)) = found else {
                w.set_ssh_import_hint(
                    format!(
                        "未找到 ssh_config，请先推送到 {}/ssh_config",
                        dir.display()
                    )
                    .into(),
                );
                return;
            };
            let hosts = parse_ssh_config(&text);
            if hosts.is_empty() {
                w.set_ssh_import_hint("ssh_config 中没有可导入的 Host".into());
                return;
            }
            let mut added = 0usize;
            {
                let mut s = store.borrow_mut();
                for host in hosts {
                    // Same rule as upstream: skip an existing alias or the same
                    // host + user pair.
                    let dup = s
                        .sessions()
                        .iter()
                        .any(|x| x.name == host.alias || (x.host == host.hostname && x.user == host.user));
                    if dup {
                        continue;
                    }
                    let auth = if host.identity_file.is_empty() {
                        AuthMethod::Password
                    } else {
                        AuthMethod::Key
                    };
                    s.upsert(Session {
                        name: host.alias,
                        host: host.hostname,
                        port: host.port,
                        user: if host.user.is_empty() {
                            "root".into()
                        } else {
                            host.user
                        },
                        auth,
                        private_key_path: host.identity_file,
                        ..Session::new_empty()
                    });
                    added += 1;
                }
                if added > 0 {
                    if let Err(e) = s.save() {
                        eprintln!("meatshell-ohos: save config: {e}");
                    }
                }
            }
            refresh_sessions(&w, store, &view);
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            w.set_ssh_import_hint(format!("已从 {name} 导入 {added} 个主机").into());
        });
    }

    // Export all connections to a JSON file in the public Download directory
    // (or the sandbox fallback) and report the result with a toast. The hint is
    // deliberately not set: only the settings menu (already closed) and the
    // welcome header show it, so it would linger as truncated text.
    {
        let store = store.clone();
        ui.on_export_sessions(move || {
            let Some(store) = store.as_ref() else {
                crate::ohos_app::queue_toast("配置不可用");
                return;
            };
            let dir = crate::ohos_app::downloads_dir();
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let file = dir.join(format!("meatshell-connections-{stamp}.json"));
            let public = crate::ohos_app::public_download_ready();
            let hint = match store.borrow().export_to(&file) {
                Ok(count) => {
                    if !public {
                        // Sandbox-only: let ArkTS copy it out through the system
                        // save dialog, which is also where the user picks the name.
                        crate::ohos_app::queue_save_export(file.clone());
                    }
                    format!(
                        "已导出 {count} 个连接到 {}",
                        file.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    )
                }
                Err(e) => format!("导出失败：{e}"),
            };
            crate::ohos_app::queue_toast(&hint);
        });
    }

    // Import connections: the settings menu asks for the system file picker
    // (`pick-import`); ArkTS copies the chosen JSON into the sandbox and calls
    // `ms_spike_import_picked`, which lands in the handler below.
    {
        let weak = ui.as_weak();
        let store = store.clone();
        let view = view.clone();
        ui.on_import_sessions(move || {
            let Some(w) = weak.upgrade() else {
                return;
            };
            let Some(store) = store.as_ref() else {
                crate::ohos_app::queue_toast("配置不可用");
                return;
            };
            let store = store.clone();
            let view = view.clone();
            crate::ohos_app::set_import_picked_handler(Box::new(move |path: &std::path::Path| {
                let result = store.borrow_mut().import_from(path);
                let hint = match result {
                    Ok((added, skipped)) => {
                        if added > 0 {
                            if let Err(e) = store.borrow_mut().save() {
                                eprintln!("meatshell-ohos: save config: {e}");
                            }
                        }
                        format!("已导入 {added} 个会话（跳过 {skipped}）")
                    }
                    Err(e) => format!("导入失败：{e}"),
                };
                refresh_sessions(&w, &store, &view);
                crate::ohos_app::queue_toast(&hint);
            }));
            crate::ohos_app::request_import_pick();
        });
    }

    // Text batch import (#150): `host|port|user|password|name` per line.
    {
        let weak = ui.as_weak();
        let store = store.clone();
        let view = view.clone();
        ui.on_batch_import_confirm(move |text: SharedString| {
            let Some(w) = weak.upgrade() else {
                return;
            };
            let Some(store) = store.as_ref() else {
                return;
            };
            let mut added = 0usize;
            {
                let mut s = store.borrow_mut();
                for session in parse_batch_import(&text) {
                    let dup = s.sessions().iter().any(|x| {
                        x.name == session.name || (x.host == session.host && x.user == session.user)
                    });
                    if dup {
                        continue;
                    }
                    s.upsert(session);
                    added += 1;
                }
                if added > 0 {
                    if let Err(e) = s.save() {
                        eprintln!("meatshell-ohos: save config: {e}");
                    }
                }
            }
            w.set_batch_import_open(false);
            refresh_sessions(&w, store, &view);
            w.set_ssh_import_hint(format!("批量导入 {added} 个会话").into());
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        let view = view.clone();
        let rows = rows.clone();
        let test_cancel = test_cancel.clone();
        ui.on_session_dialog_submit(move |draft: SessionDraft| {
            cancel_running_test(&test_cancel);
            let Some(store) = store.as_ref() else {
                return;
            };
            let Some(w) = weak.upgrade() else {
                return;
            };
            // Validate the editable rows exactly like upstream: an empty row is
            // ignored, a partially filled one reports the first error in the
            // dialog's status line instead of saving.
            let forwards = match validated_port_forwards(&rows.forwards.borrow()) {
                Ok(forwards) => forwards,
                Err(message) => {
                    w.set_dialog_test_status(message.into());
                    return;
                }
            };
            let triggers = match validated_triggers(
                &rows.triggers.borrow(),
                &rows.trigger_secrets.borrow(),
            ) {
                Ok(triggers) => triggers,
                Err(message) => {
                    w.set_dialog_test_status(message.into());
                    return;
                }
            };
            let id = draft.id.to_string();
            let existing = store.borrow().get(&id).cloned();
            let new_session = session_from_draft(&draft, existing.as_ref(), forwards, triggers);
            if let Err(err) = store.borrow().resolve_jump_chain(&new_session) {
                w.set_dialog_test_status(err.to_string().into());
                return;
            }
            {
                let mut s = store.borrow_mut();
                s.upsert(new_session);
                if let Err(err) = s.save() {
                    eprintln!("meatshell-ohos: save config: {err}");
                    w.set_dialog_test_status(err.to_string().into());
                    return;
                }
            }
            refresh_sessions(&w, store, &view);
            w.set_dialog_open(false);
        });
    }

    {
        let weak = ui.as_weak();
        let test_cancel = test_cancel.clone();
        ui.on_session_dialog_cancel(move || {
            cancel_running_test(&test_cancel);
            if let Some(w) = weak.upgrade() {
                w.set_dialog_open(false);
            }
        });
    }

    {
        let weak = ui.as_weak();
        let test_cancel = test_cancel.clone();
        ui.on_session_dialog_closed(move || {
            cancel_running_test(&test_cancel);
            if let Some(w) = weak.upgrade() {
                w.set_dialog_open(false);
            }
        });
    }

    {
        let weak = ui.as_weak();
        let test_cancel = test_cancel.clone();
        ui.on_session_dialog_changed(move || {
            cancel_running_test(&test_cancel);
            if let Some(w) = weak.upgrade() {
                w.set_dialog_test_status("".into());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        let test_cancel = test_cancel.clone();
        ui.on_session_dialog_test(move |draft: SessionDraft| {
            let Some(w) = weak.upgrade() else {
                return;
            };
            let existing = store
                .as_ref()
                .and_then(|store| store.borrow().get(draft.id.as_str()).cloned());
            start_connection_test(&w, &draft, existing, &test_cancel);
        });
    }
    {
        let weak = ui.as_weak();
        let test_cancel = test_cancel.clone();
        ui.on_session_dialog_stop_test(move || {
            cancel_running_test(&test_cancel);
            // Drop a result that may have been queued just before the click.
            crate::ohos_app::queue_dialog_status("");
            if let Some(w) = weak.upgrade() {
                w.set_dialog_test_status("".into());
            }
        });
    }
    // Browse… copies the chosen private key into `<filesDir>/keys` through the
    // ArkTS document picker, then reports the sandbox path back here.
    {
        let weak = ui.as_weak();
        let test_cancel = test_cancel.clone();
        ui.on_session_dialog_pick_key(move || {
            let Some(w) = weak.upgrade() else {
                return;
            };
            cancel_running_test(&test_cancel);
            w.set_dialog_test_status("".into());
            let weak = w.as_weak();
            crate::ohos_app::set_key_picked_handler(Box::new(move |path: &std::path::Path| {
                if let Some(w) = weak.upgrade() {
                    w.set_dialog_key_path(path.to_string_lossy().to_string().into());
                    w.set_dialog_test_status("".into());
                }
            }));
            crate::ohos_app::request_key_pick();
        });
    }
    // No RDP engine on OHOS yet, so the preview would only repeat the args.
    {
        ui.on_session_dialog_preview_rdp(|_draft: SessionDraft| SharedString::new());
    }
}

fn fill_new_session(w: &AppWindow, store: &ConfigStore, rows: &DialogRows) {
    let empty = Session::new_empty();
    w.set_session_groups(session_groups_model(store));
    // No local shells here, so the port picker has nothing to list.
    w.set_session_serial_ports(ModelRc::from(Rc::new(VecModel::<SharedString>::default())));
    rows.reset();
    w.set_dialog_forwards(forward_model(&rows.forwards.borrow()));
    w.set_dialog_triggers(trigger_model(&rows.triggers.borrow()));
    let (jump_labels, jump_ids) = jump_candidates(store, &empty.id);
    w.set_jump_choices(jump_labels);
    w.set_jump_ids(jump_ids);
    w.set_dialog_jumps(ModelRc::default());
    w.set_dialog_allow_secret_reveal(false);
    w.set_dialog_allow_mcp_access(true);
    w.set_dialog_id(empty.id.into());
    w.set_dialog_name("".into());
    w.set_dialog_host("".into());
    w.set_dialog_port("22".into());
    // No default username (#110): leaving it blank makes the connect-time
    // prompt ask for it, Xshell-style.
    w.set_dialog_user("".into());
    w.set_dialog_auth("password".into());
    w.set_dialog_password("".into());
    w.set_dialog_key_path("".into());
    w.set_dialog_key_inline("".into());
    w.set_dialog_key_inline_mode(false);
    w.set_dialog_test_status("".into());
    w.set_dialog_proxy_type("none".into());
    w.set_dialog_proxy_hostport("".into());
    w.set_dialog_group("".into());
    w.set_dialog_kind("ssh".into());
    w.set_dialog_serial_port("".into());
    w.set_dialog_baud("115200".into());
    w.set_dialog_data_bits("8".into());
    w.set_dialog_stop_bits("1".into());
    w.set_dialog_parity("none".into());
    w.set_dialog_flow("none".into());
    w.set_dialog_rdp_domain("".into());
    w.set_dialog_rdp_resolution(RDP_RESOLUTION_DEFAULT.into());
    w.set_dialog_rdp_width("1280".into());
    w.set_dialog_rdp_height("720".into());
    w.set_dialog_rdp_legacy_tls(false);
    w.set_dialog_rdp_admin(false);
    w.set_dialog_rdp_smart_sizing(false);
    w.set_dialog_rdp_cert_policy("ignore".into());
    w.set_dialog_rdp_network("auto".into());
    w.set_dialog_rdp_sound(false);
    w.set_dialog_rdp_clipboard(true);
    w.set_dialog_rdp_gfx(false);
    w.set_dialog_rdp_extra_args("".into());
    w.set_dialog_rdp_show_advanced(false);
    w.set_dialog_rdp_preview("".into());
    w.set_dialog_encoding("UTF-8".into());
    w.set_dialog_vt100_drawing(false);
    w.set_dialog_term_type(DEFAULT_TERM_TYPE.into());
    w.set_dialog_session_log("default".into());
    w.set_dialog_disable_shell_integration(false);
    w.set_dialog_sftp_mode("".into());
    w.set_dialog_sftp_command("".into());
    w.set_dialog_note("".into());
    w.set_dialog_editing(false);
    w.set_dialog_open(true);
}

fn fill_existing_session(
    w: &AppWindow,
    store: &ConfigStore,
    session: &Session,
    rows: &DialogRows,
) {
    w.set_session_groups(session_groups_model(store));
    w.set_session_serial_ports(ModelRc::from(Rc::new(VecModel::<SharedString>::default())));
    rows.load(session);
    w.set_dialog_forwards(forward_model(&rows.forwards.borrow()));
    w.set_dialog_triggers(trigger_model(&rows.triggers.borrow()));
    w.set_dialog_id(session.id.clone().into());
    w.set_dialog_name(session.name.clone().into());
    w.set_dialog_host(session.host.clone().into());
    w.set_dialog_port(session.port.to_string().into());
    w.set_dialog_user(session.user.clone().into());
    w.set_dialog_auth(session.auth.as_str().into());
    // Start blank; the dialog only loads saved credentials after opt-in,
    // initially masked. Blank on save still retains them (#10).
    w.set_dialog_password("".into());
    w.set_dialog_key_path(session.private_key_path.clone().into());
    w.set_dialog_key_inline("".into());
    w.set_dialog_key_inline_mode(!session.private_key_inline.is_empty());
    w.set_dialog_test_status("".into());
    let (proxy_type, proxy_hostport) = split_proxy(&session.proxy);
    w.set_dialog_proxy_type(proxy_type.into());
    w.set_dialog_proxy_hostport(proxy_hostport.into());
    let (jump_labels, jump_ids) = jump_candidates(store, &session.id);
    let ids = match store.resolve_jump_chain(session) {
        Ok(hops) => hops.into_iter().rev().map(|hop| hop.id).collect(),
        Err(err) => {
            w.set_dialog_test_status(err.to_string().into());
            if session.jump_session_ids.is_empty() {
                // Require an explicit repair: flattening only the immediate hop
                // would silently discard a broken inherited route.
                vec![String::new(), session.jump_session_id.clone()]
            } else {
                session.jump_session_ids.clone()
            }
        }
    };
    w.set_dialog_jumps(jump_rows(ids, &jump_ids));
    w.set_dialog_allow_secret_reveal(session.allow_secret_reveal);
    w.set_dialog_allow_mcp_access(session.mcp_access);
    w.set_jump_choices(jump_labels);
    w.set_jump_ids(jump_ids);
    w.set_dialog_group(session.group.clone().into());
    w.set_dialog_kind(session.kind.as_str().into());
    w.set_dialog_serial_port(session.serial_port.clone().into());
    w.set_dialog_baud(session.baud_rate.to_string().into());
    w.set_dialog_data_bits(session.data_bits.to_string().into());
    w.set_dialog_stop_bits(session.stop_bits.to_string().into());
    w.set_dialog_parity(session.parity.clone().into());
    w.set_dialog_flow(session.flow_control.clone().into());
    w.set_dialog_rdp_domain(session.rdp_domain.clone().into());
    w.set_dialog_rdp_resolution(
        rdp_resolution_choice(session.rdp_fullscreen, session.rdp_width, session.rdp_height).into(),
    );
    w.set_dialog_rdp_width(session.rdp_width.to_string().into());
    w.set_dialog_rdp_height(session.rdp_height.to_string().into());
    w.set_dialog_rdp_legacy_tls(session.rdp_legacy_tls);
    w.set_dialog_rdp_admin(session.rdp_admin);
    w.set_dialog_rdp_smart_sizing(session.rdp_smart_sizing);
    w.set_dialog_rdp_cert_policy(session.rdp_cert_policy.clone().into());
    w.set_dialog_rdp_network(session.rdp_network.clone().into());
    w.set_dialog_rdp_sound(session.rdp_sound);
    w.set_dialog_rdp_clipboard(session.rdp_clipboard);
    w.set_dialog_rdp_gfx(session.rdp_gfx);
    w.set_dialog_rdp_extra_args(session.rdp_extra_args.clone().into());
    w.set_dialog_rdp_show_advanced(!session.rdp_extra_args.is_empty());
    w.set_dialog_rdp_preview("".into());
    w.set_dialog_encoding(session.encoding.clone().into());
    w.set_dialog_vt100_drawing(session.vt100_drawing);
    w.set_dialog_term_type(session.effective_term_type().into());
    w.set_dialog_session_log(session.session_log.as_str().into());
    w.set_dialog_disable_shell_integration(session.disable_shell_integration);
    w.set_dialog_sftp_mode(session.sftp_mode.clone().into());
    w.set_dialog_sftp_command(session.sftp_command.clone().into());
    w.set_dialog_note(session.note.clone().into());
    w.set_dialog_editing(true);
    w.set_dialog_open(true);
}

// ———————— model helpers (port of src/app/session_models.rs) ————————

fn session_groups_model(store: &ConfigStore) -> ModelRc<SharedString> {
    let named = named_display_groups(store.groups(), store.sessions());
    ModelRc::from(Rc::new(VecModel::from(
        named.into_iter().map(SharedString::from).collect::<Vec<_>>(),
    )))
}

fn jump_candidates(
    store: &ConfigStore,
    exclude_id: &str,
) -> (ModelRc<SharedString>, ModelRc<SharedString>) {
    let mut labels: Vec<SharedString> = vec!["请选择跳板机".into()];
    let mut ids: Vec<SharedString> = vec!["".into()];
    for s in store.sessions() {
        if s.kind != SessionKind::Ssh || s.id == exclude_id {
            continue;
        }
        let label = if s.name.trim().is_empty() {
            if s.user.trim().is_empty() {
                s.host.clone()
            } else {
                format!("{}@{}", s.user, s.host)
            }
        } else {
            format!("{} ({}@{}:{})", s.name, s.user, s.host, s.port)
        };
        labels.push(label.into());
        ids.push(s.id.clone().into());
    }
    (
        ModelRc::from(Rc::new(VecModel::from(labels))),
        ModelRc::from(Rc::new(VecModel::from(ids))),
    )
}

fn jump_rows(ids: Vec<String>, choices: &ModelRc<SharedString>) -> ModelRc<JumpHop> {
    ModelRc::from(Rc::new(VecModel::from(
        ids.into_iter()
            .map(|id| {
                let choice = choices
                    .iter()
                    .position(|value| value.as_str() == id)
                    .map(|index| index as i32)
                    .unwrap_or(0);
                JumpHop { id: id.into(), choice, placeholder: false }
            })
            .collect::<Vec<_>>(),
    )))
}

fn split_proxy(url: &str) -> (String, String) {
    let s = url.trim();
    if s.is_empty() {
        return ("none".to_string(), String::new());
    }
    let lower = s.to_ascii_lowercase();
    for p in ["http://", "https://"] {
        if lower.starts_with(p) {
            return ("http".to_string(), s[p.len()..].trim_end_matches('/').to_string());
        }
    }
    for p in ["socks5h://", "socks5://", "socks://"] {
        if lower.starts_with(p) {
            return ("socks5".to_string(), s[p.len()..].trim_end_matches('/').to_string());
        }
    }
    ("socks5".to_string(), s.trim_end_matches('/').to_string())
}

fn rdp_display_settings(
    resolution: &str,
    custom_width: i32,
    custom_height: i32,
) -> (bool, u16, u16) {
    // Positive values are clamped so a typo like "99999" still gives a usable
    // desktop; empty/non-numeric input (<= 0) keeps the default.
    let side = |value: i32, fallback: u16| {
        if value <= 0 {
            fallback
        } else {
            value.clamp(RDP_MIN_SIDE, RDP_MAX_SIDE) as u16
        }
    };
    let custom = || {
        (
            side(custom_width, RDP_DEFAULT_WIDTH),
            side(custom_height, RDP_DEFAULT_HEIGHT),
        )
    };
    match resolution {
        RDP_RESOLUTION_FULLSCREEN => {
            let (width, height) = custom();
            (true, width, height)
        }
        RDP_RESOLUTION_CUSTOM => {
            let (width, height) = custom();
            (false, width, height)
        }
        // A preset looks like "1920x1080".
        preset => match preset.split_once(['x', 'X', '*']) {
            Some((width, height)) => (
                false,
                side(width.trim().parse().unwrap_or(0), RDP_DEFAULT_WIDTH),
                side(height.trim().parse().unwrap_or(0), RDP_DEFAULT_HEIGHT),
            ),
            None => (true, RDP_DEFAULT_WIDTH, RDP_DEFAULT_HEIGHT),
        },
    }
}

fn rdp_resolution_choice(fullscreen: bool, width: u16, height: u16) -> String {
    if fullscreen {
        return RDP_RESOLUTION_FULLSCREEN.to_string();
    }
    const PRESETS: [&str; 6] = [
        "1920x1080", "1680x1050", "1600x900", "1440x900", "1366x768", "1280x720",
    ];
    let size = format!("{width}x{height}");
    PRESETS
        .into_iter()
        .find(|preset| *preset == size)
        .unwrap_or(RDP_RESOLUTION_CUSTOM)
        .to_string()
}

fn normalized_sftp_mode(mode: &str, command: &str) -> String {
    match mode {
        "sudo" => "sudo".to_string(),
        "custom" if !command.trim().is_empty() => "custom".to_string(),
        _ => String::new(),
    }
}

fn draft_jump_ids(rows: impl IntoIterator<Item = (String, bool)>) -> Vec<String> {
    rows.into_iter()
        .filter(|(id, placeholder)| !(*placeholder && id.trim().is_empty()))
        .map(|(id, _)| id)
        .collect()
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ———————— draft → Session (port of upstream session_from_draft) ————————

fn session_from_draft(
    draft: &SessionDraft,
    existing: Option<&Session>,
    forwards: Vec<meatshell_core::config::PortForward>,
    triggers: Vec<meatshell_core::config::SessionTrigger>,
) -> Session {
    let password = if draft.password.is_empty() {
        existing.map(|s| s.password.clone()).unwrap_or_default()
    } else {
        meatshell_core::config::Secret::new(draft.password.to_string())
    };
    let private_key_inline = if draft.private_key_inline_mode {
        if draft.private_key_inline.is_empty() {
            existing.map(|s| s.private_key_inline.clone()).unwrap_or_default()
        } else {
            meatshell_core::config::Secret::new(draft.private_key_inline.to_string())
        }
    } else {
        meatshell_core::config::Secret::default()
    };
    // Store key paths with forward slashes uniformly; inline mode clears the
    // path.
    let private_key_path = if draft.private_key_inline_mode {
        String::new()
    } else {
        draft.private_key_path.to_string().replace('\\', "/")
    };
    let kind = SessionKind::from_str(&draft.kind.to_string());
    // Auto-name: serial uses the device; otherwise user@host or just host (#110).
    let auto_name = match kind {
        SessionKind::Serial => format!("{} @{}", draft.serial_port, draft.baud_rate),
        _ if draft.user.trim().is_empty() => draft.host.to_string(),
        _ => format!("{}@{}", draft.user, draft.host),
    };
    // Telnet defaults to 23, RDP to 3389, SSH to 22; serial ignores the port.
    let default_port = match kind {
        SessionKind::Telnet => 23,
        SessionKind::Rdp => 3389,
        _ => 22,
    };
    let (rdp_fullscreen, rdp_width, rdp_height) =
        rdp_display_settings(&draft.rdp_resolution.to_string(), draft.rdp_width, draft.rdp_height);
    Session {
        id: draft.id.to_string(),
        name: if draft.name.is_empty() { auto_name } else { draft.name.to_string() },
        host: draft.host.to_string(),
        port: if draft.port <= 0 { default_port } else { draft.port as u16 },
        user: draft.user.to_string(),
        auth: AuthMethod::from_str(&draft.auth.to_string()),
        password,
        private_key_path,
        private_key_inline,
        proxy: draft.proxy.to_string(),
        last_used: None,
        group: draft.group.to_string(),
        kind,
        local_distribution: String::new(),
        local_working_dir: String::new(),
        local_command: String::new(),
        serial_port: draft.serial_port.to_string(),
        baud_rate: if draft.baud_rate <= 0 { 115_200 } else { draft.baud_rate as u32 },
        data_bits: draft.data_bits as u8,
        stop_bits: draft.stop_bits as u8,
        parity: draft.parity.to_string(),
        flow_control: draft.flow_control.to_string(),
        encoding: draft.encoding.to_string(),
        vt100_drawing: draft.vt100_drawing,
        term_type: normalize_term_type(&draft.term_type).to_string(),
        session_log: SessionLogMode::from_str(draft.session_log.as_str()),
        forwards,
        triggers,
        disable_shell_integration: draft.disable_shell_integration,
        sftp_mode: if kind == SessionKind::Ssh {
            normalized_sftp_mode(&draft.sftp_mode, &draft.sftp_command)
        } else {
            String::new()
        },
        sftp_command: if kind == SessionKind::Ssh {
            draft.sftp_command.trim().to_string()
        } else {
            String::new()
        },
        note: draft.note.to_string(),
        jump_session_id: String::new(),
        jump_session_ids: if kind == SessionKind::Ssh {
            draft_jump_ids(draft.jumps.iter().map(|hop| (hop.id.to_string(), hop.placeholder)))
        } else {
            Vec::new()
        },
        allow_secret_reveal: draft.allow_secret_reveal,
        mcp_access: draft.allow_mcp_access,
        rdp_domain: draft.rdp_domain.to_string(),
        rdp_fullscreen,
        rdp_width,
        rdp_height,
        rdp_legacy_tls: draft.rdp_legacy_tls,
        rdp_admin: draft.rdp_admin,
        rdp_smart_sizing: draft.rdp_smart_sizing,
        rdp_cert_policy: draft.rdp_cert_policy.to_string(),
        rdp_network: draft.rdp_network.to_string(),
        rdp_sound: draft.rdp_sound,
        rdp_clipboard: draft.rdp_clipboard,
        rdp_gfx: draft.rdp_gfx,
        rdp_extra_args: draft.rdp_extra_args.to_string(),
    }
}

// ── Import / export helpers (no file picker on OHOS) ─────────────────────────

/// Sandbox folder the user pushes import files into (`hdc file send`).
fn import_dir() -> std::path::PathBuf {
    meatshell_core::config::data_dir().join("import")
}

/// One `Host` block of an ssh_config, reduced to what a session needs.
struct SshHost {
    alias: String,
    hostname: String,
    port: u16,
    user: String,
    identity_file: String,
}

fn flush_host(cur: &mut Option<SshHost>, out: &mut Vec<SshHost>) {
    let Some(mut host) = cur.take() else {
        return;
    };
    if host.hostname.is_empty() {
        host.hostname = host.alias.clone();
    }
    // Wildcard blocks (`Host *`) are defaults, not connectable sessions.
    if host.alias.is_empty() || host.hostname.contains('*') || host.hostname.contains('?') {
        return;
    }
    if host.port == 0 {
        host.port = 22;
    }
    out.push(host);
}

/// Minimal `~/.ssh/config` reader: `Host` / `HostName` / `Port` / `User` /
/// `IdentityFile`. Upstream's parser is crate-private inside the engine, so the
/// port keeps its own copy of the few directives a session actually uses.
fn parse_ssh_config(text: &str) -> Vec<SshHost> {
    let mut out = Vec::new();
    let mut cur: Option<SshHost> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(2, char::is_whitespace);
        let key = parts.next().unwrap_or("").to_ascii_lowercase();
        let value = parts.next().unwrap_or("").trim();
        match key.as_str() {
            "host" => {
                flush_host(&mut cur, &mut out);
                let alias = value
                    .split_whitespace()
                    .find(|alias| !alias.contains('*') && !alias.contains('?'))
                    .unwrap_or("");
                cur = Some(SshHost {
                    alias: unquote(alias),
                    hostname: String::new(),
                    port: 0,
                    user: String::new(),
                    identity_file: String::new(),
                });
            }
            // A `Match` block ends the current host; its options are conditional.
            "match" => flush_host(&mut cur, &mut out),
            "hostname" => {
                if let Some(host) = cur.as_mut() {
                    host.hostname = unquote(value);
                }
            }
            "port" => {
                if let Some(host) = cur.as_mut() {
                    host.port = value.parse().unwrap_or(0);
                }
            }
            "user" => {
                if let Some(host) = cur.as_mut() {
                    host.user = unquote(value);
                }
            }
            "identityfile" => {
                if let Some(host) = cur.as_mut() {
                    host.identity_file = unquote(value);
                }
            }
            _ => {}
        }
    }
    flush_host(&mut cur, &mut out);
    out
}

fn unquote(value: &str) -> String {
    value.trim_matches('"').trim_matches('\'').to_string()
}

/// #150 batch import: one `host|port|user|password|name` per line; trailing
/// fields are optional and a `host|port|username|...` header is skipped.
fn parse_batch_import(text: &str) -> Vec<Session> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.splitn(5, '|').map(str::trim).collect();
        let host = parts.first().copied().unwrap_or("");
        if host.is_empty() || host.eq_ignore_ascii_case("host") {
            continue;
        }
        let port = parts
            .get(1)
            .and_then(|p| p.parse::<u16>().ok())
            .filter(|&p| p > 0)
            .unwrap_or(22);
        let user = parts
            .get(2)
            .copied()
            .filter(|s| !s.is_empty())
            .unwrap_or("root");
        let password = parts.get(3).copied().unwrap_or("");
        let name = parts
            .get(4)
            .copied()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{user}@{host}"));
        let mut session = Session {
            name,
            host: host.to_string(),
            port,
            user: user.to_string(),
            auth: AuthMethod::Password,
            ..Session::new_empty()
        };
        if !password.is_empty() {
            session.password = Secret::new(password.to_string());
        }
        out.push(session);
    }
    out
}
