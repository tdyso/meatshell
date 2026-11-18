//! SFTP worker bridge.
//!
//! The upstream SFTP engine (`spawn_sftp`) owns its own SSH connection and runs
//! on a tokio runtime. It is driven by `SftpCommand`s and reports progress as
//! `SessionEvent`s. Here each terminal tab gets one worker thread with a
//! current-thread runtime; the Slint UI thread polls [`SftpClient::drain`] every
//! frame, mirroring what `poll_sessions` already does for the shell.

use std::sync::{mpsc as std_mpsc, Arc, Mutex};
use std::thread;

use meatshell_core::config::Session;
use meatshell_core::sftp_facade::{spawn_sftp, SftpCommand, SftpEvent, SftpTarget};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

/// Handle to a running SFTP worker for one terminal tab.
pub struct SftpClient {
    commands: UnboundedSender<SftpCommand>,
    /// Opaque sender used as the destination of a cross-tab copy.
    target: SftpTarget,
    events: Arc<Mutex<UnboundedReceiver<SftpEvent>>>,
    /// Dropping this sender wakes the worker thread so it can shut down.
    alive: Option<oneshot::Sender<()>>,
}

impl SftpClient {
    /// Starts a worker for `session` and returns once its runtime is ready.
    pub fn spawn(session: Session) -> Result<Self, String> {
        let (ready_tx, ready_rx) = std_mpsc::channel();
        let (alive_tx, alive_rx) = oneshot::channel::<()>();

        thread::Builder::new()
            .name("meatshell-sftp".to_string())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        let _ = ready_tx.send(Err(format!("创建 SFTP 运行时失败: {err}")));
                        return;
                    }
                };
                let (event_tx, event_rx) = mpsc::unbounded_channel();
                let handle = spawn_sftp(&runtime.handle().clone(), session, Vec::new(), event_tx);
                let _ = ready_tx.send(Ok((handle.commands.clone(), handle.target.clone(), event_rx)));
                runtime.block_on(async move {
                    let _ = alive_rx.await;
                    // Give the worker a moment to process `Close` before the
                    // runtime (and with it the SSH connection) is dropped.
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                });
            })
            .map_err(|err| format!("启动 SFTP 线程失败: {err}"))?;

        let (commands, target, events) = ready_rx
            .recv()
            .map_err(|_| "SFTP 线程提前退出".to_string())??;
        Ok(Self {
            commands,
            target,
            events: Arc::new(Mutex::new(events)),
            alive: Some(alive_tx),
        })
    }

    /// Non-blocking drain of everything the worker has reported since last frame.
    pub fn drain(&self) -> Vec<SftpEvent> {
        let mut out = Vec::new();
        if let Ok(mut events) = self.events.lock() {
            while let Ok(event) = events.try_recv() {
                out.push(event);
            }
        }
        out
    }

    pub fn send(&self, command: SftpCommand) {
        let _ = self.commands.send(command);
    }

    /// Copy target for cross-tab copies (`SftpCommand::CopyTo`).
    pub fn target(&self) -> SftpTarget {
        self.target.clone()
    }
}

impl Drop for SftpClient {
    fn drop(&mut self) {
        let _ = self.commands.send(SftpCommand::Close);
        self.alive.take();
    }
}
