//! Facade over the upstream `src/sftp` engine and the SFTP-related `src/ssh`
//! helpers.
//!
//! Upstream keeps those types `pub(crate)` because its desktop binary lives in
//! the same crate. This crate is a library consumed by the OHOS UI layer, and a
//! `pub(crate)` item cannot simply be re-exported as `pub`. Instead of touching
//! upstream sources, this module defines small public mirrors plus the two
//! conversion tasks (commands in, events out) that bridge to the real engine.
//!
//! Every `#[path]` target stays upstream source, byte-for-byte unchanged.

use std::path::PathBuf;

use tokio::sync::mpsc::{self, UnboundedSender};

// ---------------------------------------------------------------------------
// Public mirrors of upstream data types
// ---------------------------------------------------------------------------

/// One remote directory entry (`crate::ssh::RemoteEntry`).
#[derive(Debug, Clone)]
pub struct RemoteEntry {
    pub name: String,
    pub full_path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: u32,
    pub mode: u32,
    pub permissions_mode: u32,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub owner: Option<String>,
    pub group: Option<String>,
    pub file_type: String,
}

impl From<crate::ssh::RemoteEntry> for RemoteEntry {
    fn from(e: crate::ssh::RemoteEntry) -> Self {
        Self {
            name: e.name,
            full_path: e.full_path,
            is_dir: e.is_dir,
            size: e.size,
            modified: e.modified,
            mode: e.mode,
            permissions_mode: e.permissions_mode,
            uid: e.uid,
            gid: e.gid,
            owner: e.owner,
            group: e.group,
            file_type: e.file_type,
        }
    }
}

/// One node of the remote directory tree (`crate::ssh::RemoteTreeNode`).
#[derive(Debug, Clone)]
pub struct RemoteTreeNode {
    pub path: String,
    pub name: String,
    pub depth: u32,
    pub expanded: bool,
    pub has_children: bool,
}

impl From<crate::ssh::RemoteTreeNode> for RemoteTreeNode {
    fn from(n: crate::ssh::RemoteTreeNode) -> Self {
        Self {
            path: n.path,
            name: n.name,
            depth: n.depth,
            expanded: n.expanded,
            has_children: n.has_children,
        }
    }
}

/// SFTP events forwarded to the UI. Mirrors the SFTP subset of the upstream
/// `SessionEvent` (the shell-only variants are dropped by the converter).
#[derive(Debug, Clone)]
pub enum SftpEvent {
    SftpEntries {
        path: String,
        entries: Vec<RemoteEntry>,
    },
    SftpStatus(String),
    SftpError(String),
    SftpTreeUpdate(Vec<RemoteTreeNode>),
    SftpTransfer {
        id: String,
        name: String,
        is_upload: bool,
        transferred: u64,
        total: u64,
        state: u8,
        msg: String,
    },
    SftpFileText {
        path: String,
        name: String,
        content: String,
        edit: bool,
        error: String,
    },
    SftpDownloadInstead {
        path: String,
    },
    LocalFilesChanged {
        path: String,
    },
    /// The worker's own SSH connection hit an unknown host key. The responder
    /// is dropped by the converter, so upstream treats it as "not trusted" and
    /// the panel reports the failure instead of hanging.
    HostKeyPrompt {
        host: String,
    },
    CredentialPrompt {
        host: String,
    },
    MfaPrompt {
        host: String,
    },
}

impl SftpEvent {
    fn from_upstream(event: crate::ssh::SessionEvent) -> Option<Self> {
        use crate::ssh::SessionEvent as Up;
        Some(match event {
            Up::SftpEntries { path, entries } => Self::SftpEntries {
                path,
                entries: entries.into_iter().map(RemoteEntry::from).collect(),
            },
            Up::SftpStatus(text) => Self::SftpStatus(text),
            Up::SftpError(text) => Self::SftpError(text),
            Up::SftpTreeUpdate(nodes) => {
                Self::SftpTreeUpdate(nodes.into_iter().map(RemoteTreeNode::from).collect())
            }
            Up::SftpTransfer {
                id,
                name,
                is_upload,
                transferred,
                total,
                state,
                msg,
            } => Self::SftpTransfer {
                id,
                name,
                is_upload,
                transferred,
                total,
                state,
                msg,
            },
            Up::SftpFileText {
                path,
                name,
                content,
                edit,
                error,
            } => Self::SftpFileText {
                path,
                name,
                content,
                edit,
                error,
            },
            Up::SftpDownloadInstead { path } => Self::SftpDownloadInstead { path },
            Up::LocalFilesChanged { path } => Self::LocalFilesChanged { path },
            Up::HostKeyPrompt { host, .. } => Self::HostKeyPrompt { host },
            Up::CredentialPrompt { host, .. } => Self::CredentialPrompt { host },
            Up::MfaPrompt { host, .. } => Self::MfaPrompt { host },
            _ => return None,
        })
    }
}

/// Which file to keep when a download would overwrite one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadConflict {
    Replace,
    KeepBoth,
}

impl From<DownloadConflict> for crate::sftp::DownloadConflict {
    fn from(c: DownloadConflict) -> Self {
        match c {
            DownloadConflict::Replace => Self::Replace,
            DownloadConflict::KeepBoth => Self::KeepBoth,
        }
    }
}

/// Opaque handle to another tab's SFTP worker (cross-tab copy).
#[derive(Clone, Debug)]
pub struct SftpTarget(UnboundedSender<crate::sftp::SftpCommand>);

/// Commands sent to an SFTP worker. Mirrors the upstream `SftpCommand`
/// variants this port uses.
#[derive(Debug)]
pub enum SftpCommand {
    ListDir(String),
    RefreshDir(String),
    ToggleTreeNode(String),
    Download {
        remote: String,
        local_dir: String,
        conflict: DownloadConflict,
    },
    DownloadInto {
        remotes: Vec<String>,
        local_dir: String,
        conflict: DownloadConflict,
    },
    DownloadArchive {
        remote_dir: String,
        names: Vec<String>,
        local_dir: String,
    },
    CancelTransfer(String),
    Upload {
        local: PathBuf,
        remote_dir: String,
    },
    CopyTo {
        remotes: Vec<String>,
        target: SftpTarget,
        target_dir: String,
    },
    Delete(String),
    Rename {
        from: String,
        to: String,
    },
    Chmod {
        path: String,
        mode: u32,
    },
    MkDir(String),
    TouchFile(String),
    ReadText {
        remote: String,
        edit: bool,
    },
    OpenOrDownload(String),
    WriteText {
        remote: String,
        content: String,
    },
    Close,
}

impl SftpCommand {
    fn into_upstream(self) -> crate::sftp::SftpCommand {
        use crate::sftp::SftpCommand as Up;
        match self {
            Self::ListDir(path) => Up::ListDir(path),
            Self::RefreshDir(path) => Up::RefreshDir(path),
            Self::ToggleTreeNode(path) => Up::ToggleTreeNode(path),
            Self::Download {
                remote,
                local_dir,
                conflict,
            } => Up::Download {
                remote,
                local_dir,
                conflict: conflict.into(),
            },
            Self::DownloadInto {
                remotes,
                local_dir,
                conflict,
            } => Up::DownloadInto {
                remotes,
                local_dir,
                conflict: conflict.into(),
            },
            Self::DownloadArchive {
                remote_dir,
                names,
                local_dir,
            } => Up::DownloadArchive {
                remote_dir,
                names,
                local_dir,
            },
            Self::CancelTransfer(id) => Up::CancelTransfer(id),
            Self::Upload { local, remote_dir } => Up::Upload {
                local,
                remote_dir,
                cleanup_after: None,
            },
            Self::CopyTo {
                remotes,
                target,
                target_dir,
            } => Up::CopyTo {
                remotes,
                target: target.0,
                target_dir,
            },
            Self::Delete(path) => Up::Delete(path),
            Self::Rename { from, to } => Up::Rename { from, to },
            Self::Chmod { path, mode } => Up::Chmod { path, mode },
            Self::MkDir(path) => Up::MkDir(path),
            Self::TouchFile(path) => Up::TouchFile(path),
            Self::ReadText { remote, edit } => Up::ReadText { remote, edit },
            Self::OpenOrDownload(remote) => Up::OpenOrDownload { remote },
            Self::WriteText { remote, content } => Up::WriteText { remote, content },
            Self::Close => Up::Close,
        }
    }
}

// ---------------------------------------------------------------------------
// Spawning
// ---------------------------------------------------------------------------

/// Handle to a running SFTP worker.
pub struct SftpHandle {
    /// Command channel consumed by the worker's forwarding task.
    pub commands: UnboundedSender<SftpCommand>,
    /// Opaque sender for cross-tab copy targets.
    pub target: SftpTarget,
    #[allow(dead_code)] // detached task; kept so the handle owns it
    join: tokio::task::JoinHandle<()>,
}

/// Spawns the upstream SFTP worker on `runtime` and bridges it to the public
/// command/event types above.
pub fn spawn_sftp(
    runtime: &tokio::runtime::Handle,
    session: crate::config::Session,
    jump: Vec<crate::config::Session>,
    events: UnboundedSender<SftpEvent>,
) -> SftpHandle {
    let (up_events_tx, mut up_events_rx) = mpsc::unbounded_channel();
    let inner = crate::sftp::spawn_sftp(runtime, session, jump, up_events_tx);

    // Commands: public enum -> upstream enum.
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<SftpCommand>();
    let up_cmds = inner.commands.clone();
    let join = runtime.spawn(async move {
        while let Some(command) = cmd_rx.recv().await {
            if up_cmds.send(command.into_upstream()).is_err() {
                break;
            }
        }
    });

    // Events: upstream SessionEvent -> public SftpEvent.
    runtime.spawn(async move {
        while let Some(event) = up_events_rx.recv().await {
            if let Some(event) = SftpEvent::from_upstream(event) {
                if events.send(event).is_err() {
                    break;
                }
            }
        }
    });

    SftpHandle {
        commands: cmd_tx,
        target: SftpTarget(inner.commands),
        join,
    }
}

// ---------------------------------------------------------------------------
// Host-key store (shared by the shell connection and the SFTP worker)
// ---------------------------------------------------------------------------

/// Result of checking a server key against upstream's `known_hosts`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostKeyState {
    Match,
    Changed,
    Unknown,
}

/// Looks a presented key up in the upstream `host:port`-keyed store. The shell
/// and SFTP connections must share this store: the SFTP worker opens its own
/// SSH connection and verifies the key again.
pub fn host_key_state(host: &str, port: u16, key: &ssh_key::PublicKey) -> HostKeyState {
    match crate::ssh::known_hosts::verify(host, port, key) {
        crate::ssh::HostKeyStatus::Match => HostKeyState::Match,
        crate::ssh::HostKeyStatus::Changed => HostKeyState::Changed,
        crate::ssh::HostKeyStatus::Unknown => HostKeyState::Unknown,
    }
}

/// Pins `key` for `host:port` in the upstream store (replacing a stale entry).
pub fn remember_host_key(host: &str, port: u16, key: &ssh_key::PublicKey) -> Result<(), String> {
    crate::ssh::known_hosts::remember(host, port, key).map_err(|e| format!("{e:#}"))
}

// ---------------------------------------------------------------------------
// Formatting helpers (upstream `src/ssh` implementations)
// ---------------------------------------------------------------------------

pub fn format_size(size: u64) -> String {
    crate::ssh::format_size(size)
}

pub fn format_mtime(ts: u32) -> String {
    crate::ssh::format_mtime(ts)
}

pub fn format_permissions(mode: u32) -> String {
    crate::ssh::format_permissions(mode)
}
