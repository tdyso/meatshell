//! Minimal interactive SSH session: connect, authenticate, open a PTY shell.
//!
//! One session runs on its own thread with a current-thread tokio runtime; the
//! UI thread polls the shared event queue from a Slint timer (see `ohos_app`).
//! That keeps the render thread free of any blocking network work while the
//! engine stays on the same thread that owns the Slint window.
//!
//! Host keys are verified against an OpenSSH-format `known_hosts` file in the
//! app data dir. An unknown or changed key is *not* accepted silently: the
//! verifier parks the connection and surfaces a prompt through `PromptGate`,
//! and only the user's answer (upstream's `hostkey-accept` / `hostkey-reject`
//! dialog) decides whether the connection continues.
//!
//! The same gate carries the other two connect-time questions upstream asks:
//! missing credentials (`cred-prompt-*`, #110) and keyboard-interactive MFA
//! codes (`mfa-prompt-*`, #86-MFA). Desktop-only upstream features that still
//! have no OHOS counterpart: jump hosts, proxies, shell integration. Only the
//! plain PTY shell is requested; port forwarding lives in `tunnels.rs`.

use std::sync::{Arc, Mutex};

use meatshell_core::config::PortForward;
use russh::client::{self, Config};
use russh::ChannelMsg;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::tunnels::{
    self, RemoteTargets, RuntimeForward, SharedHandle, TunnelRow,
};

pub enum Auth {
    Password(String),
    KeyboardInteractive(String),
    /// PEM / OpenSSH private key text, exactly as stored by the session editor.
    Key(String),
}

pub struct ConnectConfig {
    /// Saved session id, used to persist "remember credentials" (#110).
    pub session_id: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: Auth,
    pub cols: u16,
    pub rows: u16,
    /// Saved port forwards (`session.forwards`), started after authentication
    /// when `auto_start` is set.
    pub forwards: Vec<PortForward>,
}

pub enum Event {
    /// Raw bytes from the remote PTY (stdout + stderr).
    Data(Vec<u8>),
    /// Shell channel is up; the terminal is ready for input.
    Connected,
    /// The remote side closed the channel / the connection ended.
    Closed,
    /// Something failed; the string is user-facing Chinese text.
    Failed(String),
    /// Runtime tunnel rows for the Tunnels tab (#206).
    Tunnels(Vec<TunnelRow>),
    /// Resource monitor sample for the "运行状态" side panel.
    Stats(crate::resource_stats::StatsSample),
}

enum Command {
    Data(Vec<u8>),
    Resize(u16, u16),
    AddTunnel { id: String, forward: PortForward },
    StopTunnel(String),
    StartTunnel(String),
    DeleteTunnel(String),
    Close,
}

/// What the UI shows when a server presents an unverified host key.
#[derive(Clone)]
pub struct HostKeyPrompt {
    pub host: String,
    pub port: u16,
    /// `true` when the key differs from the recorded one (possible MITM).
    pub changed: bool,
    pub key_type: String,
    pub fingerprint: String,
}

/// Login fields the session is missing (#110): upstream asks before connecting
/// instead of failing with an opaque authentication error.
#[derive(Clone)]
pub struct CredentialPrompt {
    /// Saved session id; the UI persists the answer when "remember" is checked.
    pub session_id: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub need_user: bool,
    pub need_password: bool,
}

/// What the user typed into the credential dialog.
pub struct CredentialReply {
    pub user: String,
    pub password: String,
    pub remember: bool,
}

/// One keyboard-interactive challenge that is not the saved password — an MFA /
/// verification code, a bastion's one-time token (#86-MFA).
#[derive(Clone)]
pub struct MfaPrompt {
    pub host: String,
    pub prompt: String,
    /// Server's echo flag: `false` hides the typed code.
    pub echo: bool,
}

/// Everything the UI may have to ask about one connection, tagged with the
/// answer channel for the SSH thread parked on it.
pub enum Prompt {
    HostKey(HostKeyPrompt),
    Credentials(CredentialPrompt),
    Mfa(MfaPrompt),
}

enum Pending {
    HostKey(HostKeyPrompt, std::sync::mpsc::Sender<Option<bool>>),
    Credentials(CredentialPrompt, std::sync::mpsc::Sender<Option<CredentialReply>>),
    Mfa(MfaPrompt, std::sync::mpsc::Sender<Option<String>>),
}

/// Hand-off point between the SSH thread (which blocks inside
/// `check_server_key` / authentication) and the UI thread (which polls every
/// frame).
///
/// Upstream asks the user through the same UI (`hostkey-prompt-open`,
/// `cred-prompt-open`, `mfa-prompt-open`); this is the OHOS equivalent. It is
/// deliberately a plain mutex instead of a channel so the UI can *inspect*
/// pending prompts without consuming them.
pub struct PromptGate {
    pending: Mutex<Option<Pending>>,
}

impl PromptGate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { pending: Mutex::new(None) })
    }

    /// The prompt waiting for an answer, if any. Cheap: called per frame.
    pub fn pending(&self) -> Option<Prompt> {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|pending| match pending {
                Pending::HostKey(prompt, _) => Prompt::HostKey(prompt.clone()),
                Pending::Credentials(prompt, _) => Prompt::Credentials(prompt.clone()),
                Pending::Mfa(prompt, _) => Prompt::Mfa(prompt.clone()),
            })
    }

    /// Answers the pending host-key question. `None` means nothing of that kind
    /// was waiting (the connection already timed out or was cancelled), so the
    /// caller can ignore stray clicks.
    pub fn answer_host_key(&self, accept: bool) -> Option<HostKeyPrompt> {
        match self.take()? {
            Pending::HostKey(prompt, answer) => {
                let _ = answer.send(Some(accept));
                Some(prompt)
            }
            other => {
                self.restore(other);
                None
            }
        }
    }

    pub fn answer_credentials(&self, reply: CredentialReply) -> Option<CredentialPrompt> {
        match self.take()? {
            Pending::Credentials(prompt, answer) => {
                let _ = answer.send(Some(reply));
                Some(prompt)
            }
            other => {
                self.restore(other);
                None
            }
        }
    }

    pub fn answer_mfa(&self, answer_text: String) -> Option<MfaPrompt> {
        match self.take()? {
            Pending::Mfa(prompt, answer) => {
                let _ = answer.send(Some(answer_text));
                Some(prompt)
            }
            other => {
                self.restore(other);
                None
            }
        }
    }

    /// Dialog closed without typing anything: tell the SSH thread to abort.
    pub fn cancel_credentials(&self) -> Option<CredentialPrompt> {
        match self.take()? {
            Pending::Credentials(prompt, answer) => {
                let _ = answer.send(None);
                Some(prompt)
            }
            other => {
                self.restore(other);
                None
            }
        }
    }

    pub fn cancel_mfa(&self) -> Option<MfaPrompt> {
        match self.take()? {
            Pending::Mfa(prompt, answer) => {
                let _ = answer.send(None);
                Some(prompt)
            }
            other => {
                self.restore(other);
                None
            }
        }
    }

    fn take(&self) -> Option<Pending> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    fn restore(&self, pending: Pending) {
        let mut slot = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(pending);
        }
    }

    /// Blocks the SSH thread until the user answers or the wait expires.
    /// `None` means "no answer" (timeout / connection abandoned).
    fn ask<T>(
        &self,
        wrap: impl FnOnce(std::sync::mpsc::Sender<Option<T>>) -> Pending,
    ) -> Option<T> {
        let (tx, rx) = std::sync::mpsc::channel();
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(wrap(tx));
        // The UI polls every 16 ms, so this only waits for a human. Bound it
        // anyway: an abandoned prompt must not pin the SSH thread forever.
        let answer = rx.recv_timeout(std::time::Duration::from_secs(120)).ok().flatten();
        if answer.is_none() {
            // Timed out: drop the slot so the dialog does not linger over an
            // already-abandoned connection. `take()` is harmless when the UI
            // answered at the same instant (the slot is already empty).
            self.pending.lock().unwrap_or_else(|e| e.into_inner()).take();
        }
        answer
    }

    fn ask_host_key(&self, prompt: HostKeyPrompt) -> Option<bool> {
        self.ask(|tx| Pending::HostKey(prompt, tx))
    }

    fn ask_credentials(&self, prompt: CredentialPrompt) -> Option<CredentialReply> {
        self.ask(|tx| Pending::Credentials(prompt, tx))
    }

    fn ask_mfa(&self, prompt: MfaPrompt) -> Option<String> {
        self.ask(|tx| Pending::Mfa(prompt, tx))
    }
}

/// Verifies the server host key against an OpenSSH-format `known_hosts` file.
///
/// A known-and-matching key passes silently. Anything else (unknown host, or
/// a key that differs from the recorded one) becomes a user question; only an
/// explicit "trust" writes the file, so an unwanted prompt can never pin a
/// malicious key on its own.
pub(crate) struct HostKeyVerifier {
    host: String,
    port: u16,
    gate: Arc<PromptGate>,
    /// Set when verification fails, so `shell_loop` can show the real reason
    /// instead of russh's opaque `UnknownKey` error.
    failure: Arc<Mutex<Option<String>>>,
    /// Set with the fingerprint when a new key was pinned (first connect).
    learned: Arc<Mutex<Option<String>>>,
    /// `-R` mappings: remote listen port → local target.
    remote_forwards: RemoteTargets,
    /// Event sink for remote-forward connection failures / notices.
    events: EventQueue,
}

#[async_trait::async_trait]
impl client::Handler for HostKeyVerifier {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        let fingerprint = key.fingerprint(russh::keys::HashAlg::Sha256).to_string();
        let key_type = key.algorithm().to_string();
        let target = format!("{}:{}", self.host, self.port);
        let fail = |message: String| {
            *self.failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(message);
        };
        // Shared with the SFTP worker, which opens its own SSH connection and
        // verifies the same key: both read/write upstream's `host:port` store.
        let status = meatshell_core::sftp_facade::host_key_state(&self.host, self.port, key);
        let changed = match status {
            meatshell_core::sftp_facade::HostKeyState::Match => return Ok(true),
            meatshell_core::sftp_facade::HostKeyState::Changed => true,
            meatshell_core::sftp_facade::HostKeyState::Unknown => false,
        };

        // Unknown or changed key: park the connection and let the user decide
        // (upstream's host-key confirmation dialog, #109-5).
        let accepted = self.gate.ask_host_key(HostKeyPrompt {
            host: self.host.clone(),
            port: self.port,
            changed,
            key_type,
            fingerprint: fingerprint.clone(),
        });
        match accepted {
            Some(true) => {
                match meatshell_core::sftp_facade::remember_host_key(&self.host, self.port, key) {
                    Ok(()) => {
                        *self.learned.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some(fingerprint);
                        Ok(true)
                    }
                    Err(err) => {
                        fail(format!("记录主机密钥失败（{fingerprint}）: {err}"));
                        Ok(false)
                    }
                }
            }
            Some(false) => {
                fail(if changed {
                    format!(
                        "主机密钥校验失败：{target} 的主机密钥与 known_hosts 记录不一致\
                         （{fingerprint}），已按用户选择取消连接。",
                    )
                } else {
                    format!("已取消连接：未信任 {target} 的主机密钥（{fingerprint}）。")
                });
                Ok(false)
            }
            None => {
                fail(format!("等待确认 {target} 的主机密钥超时，连接已取消。"));
                Ok(false)
            }
        }
    }

    /// Remote forward (-R): the server opened a channel for a connection that
    /// arrived on a port we asked it to listen on. Connect to the configured
    /// local target and splice the two together (#56).
    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<russh::client::Msg>,
        connected_address: &str,
        connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let target = self
            .remote_forwards
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&connected_port)
            .cloned();
        let events = self.events.clone();
        let bind = connected_address.to_string();
        tokio::spawn(async move {
            let Some((host, port)) = target else {
                eprintln!("forwarded-tcpip on {bind}:{connected_port} with no mapping");
                return;
            };
            match tokio::net::TcpStream::connect((host.as_str(), port)).await {
                Ok(mut tcp) => {
                    let mut stream = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut tcp, &mut stream).await;
                }
                Err(err) => push_event(
                    &events,
                    Event::Data(
                        format!("\r\n[meatshell] -R {host}:{port} 连接失败 / connect failed: {err}\r\n")
                            .into_bytes(),
                    ),
                ),
            }
        });
        Ok(())
    }
}

pub(crate) type EventQueue = Arc<Mutex<Vec<Event>>>;

type Queue = EventQueue;

pub(crate) fn push_event(queue: &EventQueue, event: Event) {
    queue.lock().unwrap_or_else(|e| e.into_inner()).push(event);
}

fn push(queue: &Queue, event: Event) {
    push_event(queue, event);
}

/// One `/proc` snapshot in the upstream monitor wire format.
const STATS_CMD: &str = "PATH=/usr/bin:/bin:/usr/sbin:/sbin; export PATH; \
awk '/^cpu /{print}' /proc/stat; \
awk '/^(MemTotal|MemAvailable|SwapTotal|SwapFree):/{print}' /proc/meminfo; \
cat /proc/net/dev; echo __DF__; df -kP 2>/dev/null";

/// Query the remote host for one resource sample. Runs on its own channel so a
/// slow or missing `df` cannot stall the shell channel.
async fn sample_remote(handle: &SharedHandle) -> Option<String> {
    let guard = handle.lock().await;
    let mut channel = guard.channel_open_session().await.ok()?;
    if channel.exec(true, STATS_CMD).await.is_err() {
        return None;
    }
    drop(guard);
    let mut out = Vec::new();
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(3), channel.wait()).await {
            Ok(Some(ChannelMsg::Data { data })) => out.extend_from_slice(&data),
            Ok(Some(ChannelMsg::Eof)) | Ok(Some(ChannelMsg::Close)) | Ok(None) => break,
            Ok(Some(_)) => {}
            Err(_) => break,
        }
    }
    String::from_utf8(out).ok()
}

/// Poll the remote host every 2 s for as long as the shell lives.
fn spawn_stats_sampler(handle: SharedHandle, events: EventQueue) {
    tokio::spawn(async move {
        let mut prev = crate::resource_stats::PrevState::default();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let Some(raw) = sample_remote(&handle).await else {
                continue;
            };
            if let Some(sample) = crate::resource_stats::parse_block(&raw, &mut prev) {
                push_event(&events, Event::Stats(sample));
            }
        }
    });
}

/// Handle owned by the UI thread. Dropping it closes the remote session.
pub struct SshSession {
    commands: UnboundedSender<Command>,
    events: Queue,
}

impl SshSession {
    pub fn spawn(config: ConnectConfig, gate: Arc<PromptGate>) -> Self {
        let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
        let events: Queue = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let spawned = std::thread::Builder::new()
            .name("meatshell-ssh".into())
            .spawn(move || {
                let runtime =
                    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                        Ok(runtime) => runtime,
                        Err(err) => {
                            push(&sink, Event::Failed(format!("创建 SSH 运行时失败: {err}")));
                            return;
                        }
                    };
                runtime.block_on(run(config, gate, receiver, Arc::clone(&sink)));
            });
        if let Err(err) = spawned {
            push(&events, Event::Failed(format!("启动 SSH 线程失败: {err}")));
        }
        Self { commands, events }
    }

    /// Takes every event produced since the last poll.
    pub fn drain(&self) -> Vec<Event> {
        std::mem::take(&mut *self.events.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn write(&self, bytes: &[u8]) {
        if !bytes.is_empty() {
            let _ = self.commands.send(Command::Data(bytes.to_vec()));
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.commands.send(Command::Resize(cols, rows));
    }

    pub fn add_tunnel(&self, id: String, forward: PortForward) {
        let _ = self.commands.send(Command::AddTunnel { id, forward });
    }

    pub fn stop_tunnel(&self, id: String) {
        let _ = self.commands.send(Command::StopTunnel(id));
    }

    pub fn start_tunnel(&self, id: String) {
        let _ = self.commands.send(Command::StartTunnel(id));
    }

    pub fn delete_tunnel(&self, id: String) {
        let _ = self.commands.send(Command::DeleteTunnel(id));
    }
}

impl Drop for SshSession {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Close);
    }
}

async fn run(
    config: ConnectConfig,
    gate: Arc<PromptGate>,
    receiver: UnboundedReceiver<Command>,
    events: Queue,
) {
    match shell_loop(&config, gate, receiver, &events).await {
        Ok(()) => push(&events, Event::Closed),
        Err(err) => push(&events, Event::Failed(err)),
    }
}

async fn shell_loop(
    config: &ConnectConfig,
    gate: Arc<PromptGate>,
    mut receiver: UnboundedReceiver<Command>,
    events: &Queue,
) -> Result<(), String> {
    let target = format!("{}:{}", config.host, config.port);

    // Upstream resolves the session's credentials before authenticating and
    // asks the UI for whatever is missing (#110). Without this an empty
    // username fails with an opaque server error instead of a dialog.
    let mut user = config.user.trim().to_string();
    let mut password = match &config.auth {
        Auth::Password(password) | Auth::KeyboardInteractive(password) => password.clone(),
        Auth::Key(_) => String::new(),
    };
    let need_user = user.is_empty();
    // Only plain password auth prompts up front; keyboard-interactive asks
    // through its own challenge flow below (which is how bastions work).
    let need_password = matches!(&config.auth, Auth::Password(_)) && password.is_empty();
    if need_user || need_password {
        let prompt = CredentialPrompt {
            session_id: config.session_id.clone(),
            host: config.host.clone(),
            port: config.port,
            user: user.clone(),
            need_user,
            need_password,
        };
        match gate.ask_credentials(prompt) {
            Some(reply) => {
                if need_user {
                    user = reply.user.trim().to_string();
                }
                if need_password {
                    password = reply.password;
                }
            }
            None => return Err(format!("已取消连接：未提供 {target} 的登录凭据。")),
        }
    }
    if user.is_empty() {
        return Err(format!("{target} 未提供用户名，连接已取消。"));
    }
    let failure: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let learned: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    // `-R` mappings are registered by the UI thread's tunnel commands and read
    // by the handler when the server opens a forwarded channel.
    let remote_forwards: RemoteTargets = Arc::new(Mutex::new(std::collections::HashMap::new()));
    let mut session = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        client::connect(
            Arc::new(Config {
                keepalive_interval: Some(std::time::Duration::from_secs(30)),
                ..Default::default()
            }),
            (config.host.as_str(), config.port),
            HostKeyVerifier {
                host: config.host.clone(),
                port: config.port,
                gate: Arc::clone(&gate),
                failure: Arc::clone(&failure),
                learned: Arc::clone(&learned),
                remote_forwards: Arc::clone(&remote_forwards),
                events: Arc::clone(events),
            },
        ),
    )
    .await
    .map_err(|_| format!("连接 {target} 超时"))?
    .map_err(|err| {
        // A host-key rejection surfaces as `UnknownKey`; prefer the concrete
        // reason the verifier recorded (mismatch line, write failure, ...).
        failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_else(|| format!("连接 {target} 失败: {err}"))
    })?;

    if let Some(fingerprint) = learned.lock().unwrap_or_else(|e| e.into_inner()).take() {
        push(
            events,
            Event::Data(format!("[meatshell] 已记录 {target} 的主机密钥 {fingerprint}\r\n").into_bytes()),
        );
    }

    authenticate(&mut session, config, &user, &password, &target, &gate).await?;

    // The shell channel and the tunnel tasks share one handle: `direct-tcpip`
    // and `tcpip_forward` both need `&mut self`, and the shell loop only uses
    // the handle for the channel setup below and `disconnect`.
    let handle: SharedHandle = Arc::new(tokio::sync::Mutex::new(session));
    let mut channel = handle
        .lock()
        .await
        .channel_open_session()
        .await
        .map_err(|err| format!("打开会话通道失败: {err}"))?;
    // OpenSSH clients forward LANG/LC_* by default (`SendEnv LANG LC_*`).
    // Without it the remote shell runs in the C locale, where `ls` replaces
    // every non-ASCII byte with `?` before the bytes ever reach us — no
    // amount of font fallback can recover that. sshd only honours the request
    // when `AcceptEnv` allows it, so a rejection is not fatal.
    for name in ["LANG", "LC_CTYPE"] {
        if let Err(err) = channel.set_env(true, name, "en_US.UTF-8").await {
            eprintln!("meatshell-ohos: set_env {name} rejected: {err}");
        }
    }
    channel
        .request_pty(
            true,
            "xterm-256color",
            u32::from(config.cols),
            u32::from(config.rows),
            0,
            0,
            &[],
        )
        .await
        .map_err(|err| format!("申请 PTY 失败: {err}"))?;
    channel
        .request_shell(true)
        .await
        .map_err(|err| format!("启动远程 shell 失败: {err}"))?;
    push(events, Event::Connected);

    // Saved forwards from the session editor: `auto_start` ones come up now,
    // the rest are listed as stopped so the Tunnels tab can start them.
    let mut runtime_forwards: std::collections::HashMap<String, RuntimeForward> =
        std::collections::HashMap::new();
    for (index, forward) in config.forwards.iter().enumerate() {
        let id = if forward.id.is_empty() {
            format!("config-{index}")
        } else {
            format!("saved-{}", forward.id)
        };
        let entry = if !forward.auto_start {
            tunnels::stopped_entry(id.clone(), forward.clone())
        } else if forward.kind == "remote" {
            tunnels::start_remote_forward(
                &handle,
                &remote_forwards,
                id.clone(),
                forward.clone(),
                events,
            )
            .await
        } else {
            tunnels::start_runtime_forward(handle.clone(), id.clone(), forward.clone(), events)
        };
        runtime_forwards.insert(id, entry);
    }
    tunnels::emit_tunnel_update(&runtime_forwards, events);

    // Resource monitor: an independent exec channel every 2 s feeds the
    // right-hand "运行状态" panel without touching the shell channel.
    spawn_stats_sampler(handle.clone(), events.clone());

    loop {
        tokio::select! {
            command = receiver.recv() => {
                match command {
                    Some(Command::Data(bytes)) => {
                        if let Err(err) = channel.data(&bytes[..]).await {
                            return Err(format!("写入失败: {err}"));
                        }
                    }
                    Some(Command::Resize(cols, rows)) => {
                        let _ = channel
                            .window_change(u32::from(cols), u32::from(rows), 0, 0)
                            .await;
                    }
                    Some(Command::AddTunnel { id, forward }) => {
                        let entry = if forward.kind == "remote" {
                            tunnels::start_remote_forward(
                                &handle,
                                &remote_forwards,
                                id.clone(),
                                forward,
                                events,
                            )
                            .await
                        } else {
                            tunnels::start_runtime_forward(handle.clone(), id.clone(), forward, events)
                        };
                        runtime_forwards.insert(id, entry);
                        tunnels::emit_tunnel_update(&runtime_forwards, events);
                    }
                    Some(Command::StopTunnel(id)) => {
                        if let Some(entry) = runtime_forwards.get_mut(&id) {
                            tunnels::stop_forward(entry, &handle, &remote_forwards, events).await;
                            tunnels::emit_tunnel_update(&runtime_forwards, events);
                        }
                    }
                    Some(Command::StartTunnel(id)) => {
                        if let Some(entry) = runtime_forwards.get(&id) {
                            if entry.info.active {
                                continue;
                            }
                            let forward = entry.forward.clone();
                            let entry = if forward.kind == "remote" {
                                tunnels::start_remote_forward(
                                    &handle,
                                    &remote_forwards,
                                    id.clone(),
                                    forward,
                                    events,
                                )
                                .await
                            } else {
                                tunnels::start_runtime_forward(handle.clone(), id.clone(), forward, events)
                            };
                            runtime_forwards.insert(id, entry);
                            tunnels::emit_tunnel_update(&runtime_forwards, events);
                        }
                    }
                    Some(Command::DeleteTunnel(id)) => {
                        if let Some(entry) = runtime_forwards.get_mut(&id) {
                            if tunnels::stop_forward(entry, &handle, &remote_forwards, events).await {
                                runtime_forwards.remove(&id);
                            }
                            tunnels::emit_tunnel_update(&runtime_forwards, events);
                        }
                    }
                    // Channel closed or the UI dropped the handle.
                    Some(Command::Close) | None => {
                        let _ = channel.close().await;
                        let _ = handle
                            .lock()
                            .await
                            .disconnect(russh::Disconnect::ByApplication, "", "en")
                            .await;
                        return Ok(());
                    }
                }
            }
            message = channel.wait() => {
                match message {
                    Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                        push(events, Event::Data(data.to_vec()));
                    }
                    Some(ChannelMsg::ExitStatus { exit_status }) => {
                        push(events, Event::Data(
                            format!("\r\n[远程进程退出，代码 {exit_status}]\r\n").into_bytes(),
                        ));
                    }
                    Some(ChannelMsg::Close) | None => {
                        let _ = handle
                            .lock()
                            .await
                            .disconnect(russh::Disconnect::ByApplication, "", "en")
                            .await;
                        return Ok(());
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn authenticate(
    session: &mut client::Handle<HostKeyVerifier>,
    config: &ConnectConfig,
    user: &str,
    password: &str,
    target: &str,
    gate: &PromptGate,
) -> Result<(), String> {
    match &config.auth {
        Auth::Password(_) => {
            if password.is_empty() {
                return Err(format!("{target} 未保存密码，请在会话编辑器里填写"));
            }
            let accepted = session
                .authenticate_password(user, password.to_string())
                .await
                .map_err(|err| format!("认证 {user}@{target} 失败: {err}"))?;
            if !accepted {
                return Err(format!("{user}@{target} 密码认证被拒绝"));
            }
        }
        Auth::KeyboardInteractive(_) => {
            keyboard_interactive_auth(session, user, password, target, gate).await?;
        }
        Auth::Key(pem) => {
            if pem.trim().is_empty() {
                return Err(format!(
                    "{target} 私钥为空或路径不可读，请在会话编辑器里粘贴私钥"
                ));
            }
            let key = russh::keys::decode_secret_key(pem, None)
                .map_err(|err| format!("解析私钥失败: {err}"))?;
            let key = russh_keys::key::PrivateKeyWithHashAlg::new(Arc::new(key), None)
                .map_err(|err| format!("构建私钥认证失败: {err}"))?;
            let accepted = session
                .authenticate_publickey(user, key)
                .await
                .map_err(|err| format!("认证 {user}@{target} 失败: {err}"))?;
            if !accepted {
                return Err(format!("{user}@{target} 私钥认证被拒绝"));
            }
        }
    }
    Ok(())
}

/// `keyboard-interactive` authentication, mirroring upstream
/// `keyboard_interactive_auth`: the saved password answers the first
/// password-like challenge (bastions that disable the plain `password` method,
/// #86); anything that looks like an MFA / verification code is shown to the
/// user through the `mfa-prompt-*` dialog (#86-MFA).
async fn keyboard_interactive_auth(
    session: &mut client::Handle<HostKeyVerifier>,
    user: &str,
    password: &str,
    target: &str,
    gate: &PromptGate,
) -> Result<(), String> {
    use client::KeyboardInteractiveAuthResponse as Response;

    let mut response = session
        .authenticate_keyboard_interactive_start(user, None::<String>)
        .await
        .map_err(|err| format!("认证 {user}@{target} 失败: {err}"))?;
    let mut password_used = false;
    // Bound the exchange so a misbehaving server cannot loop us forever.
    for _ in 0..16 {
        match response {
            Response::Success => return Ok(()),
            Response::Failure => {
                return Err(format!("{user}@{target} 键盘交互认证被拒绝"))
            }
            Response::InfoRequest { prompts, .. } => {
                let mut answers = Vec::with_capacity(prompts.len());
                for prompt in &prompts {
                    if !password_used && !password.is_empty() && !looks_like_mfa(&prompt.prompt) {
                        answers.push(password.to_string());
                        password_used = true;
                    } else {
                        match gate.ask_mfa(MfaPrompt {
                            host: target.to_string(),
                            prompt: prompt.prompt.clone(),
                            echo: prompt.echo,
                        }) {
                            Some(answer) => answers.push(answer),
                            None => {
                                return Err(format!("已取消 {target} 的双重验证，连接已中止。"))
                            }
                        }
                    }
                }
                response = session
                    .authenticate_keyboard_interactive_respond(answers)
                    .await
                    .map_err(|err| format!("认证 {user}@{target} 失败: {err}"))?;
            }
        }
    }
    Err(format!("{user}@{target} 键盘交互认证轮次过多，已中止"))
}

/// Heuristic copied from upstream: does this challenge ask for an MFA code
/// rather than the account password?
fn looks_like_mfa(prompt: &str) -> bool {
    let text = prompt.to_lowercase();
    [
        "code",
        "otp",
        "mfa",
        "2fa",
        "factor",
        "duo",
        "verification",
        "verify",
        "token",
        "authenticator",
        "passcode",
        "one-time",
        "one time",
        "验证码",
        "动态",
        "令牌",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}
