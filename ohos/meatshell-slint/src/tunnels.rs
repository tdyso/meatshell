//! Runtime SSH port forwarding (-L / -D / -R) for the OHOS port.
//!
//! Ported from upstream `src/tunnel/impls/forward.rs` and the runtime tunnel
//! bookkeeping in `src/ssh/impls/ssh.rs` (`RuntimeForward`,
//! `start_remote_forward`, `stop_runtime_forward`). The only differences are the
//! event sink (`EventQueue` instead of a tokio channel) and the handle type
//! (our `HostKeyVerifier` instead of upstream's `ClientHandler`).

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use meatshell_core::config::PortForward;
use russh::client::{Handle, Msg};
use russh::Channel;
use tokio::io::{copy_bidirectional, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use crate::ssh_session::{push_event, Event, EventQueue, HostKeyVerifier};

/// One row in the upstream Tunnels list (`TunnelInfo` in the UI).
#[derive(Clone)]
pub struct TunnelRow {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub bind_addr: String,
    pub bind_port: u16,
    pub host: String,
    pub host_port: u16,
    pub active: bool,
    pub status: String,
}

pub type SharedHandle = Arc<tokio::sync::Mutex<Handle<HostKeyVerifier>>>;

/// Remote listen port → local target, consulted by
/// `HostKeyVerifier::server_channel_open_forwarded_tcpip`.
pub type RemoteTargets = Arc<Mutex<HashMap<u32, (String, u16)>>>;

pub struct RuntimeForward {
    pub info: TunnelRow,
    pub task: Option<JoinHandle<()>>,
    pub forward: PortForward,
}

fn notice(events: &EventQueue, msg: String) {
    push_event(events, Event::Data(format!("\r\n[meatshell] {msg}\r\n").into_bytes()));
}

fn bind_target(bind_addr: &str, bind_port: u16) -> String {
    let addr = if bind_addr.trim().is_empty() { "127.0.0.1" } else { bind_addr.trim() };
    if addr.contains(':') && !addr.starts_with('[') {
        format!("[{addr}]:{bind_port}")
    } else {
        format!("{addr}:{bind_port}")
    }
}

fn normalized_bind_addr(f: &PortForward) -> String {
    let bind = f.bind_addr.trim();
    if bind.is_empty() {
        "127.0.0.1".to_string()
    } else {
        bind.to_string()
    }
}

pub fn tunnel_label(f: &PortForward) -> String {
    if !f.name.trim().is_empty() {
        return f.name.trim().to_string();
    }
    match f.kind.as_str() {
        "local" => format!("-L {}:{}", normalized_bind_addr(f), f.bind_port),
        "remote" => format!("-R {}:{}", normalized_bind_addr(f), f.bind_port),
        "dynamic" => format!("-D {}:{}", normalized_bind_addr(f), f.bind_port),
        _ => format!("{} {}:{}", f.kind, normalized_bind_addr(f), f.bind_port),
    }
}

pub fn tunnel_info(id: String, f: &PortForward, active: bool, status: &str) -> TunnelRow {
    TunnelRow {
        id,
        name: tunnel_label(f),
        kind: f.kind.clone(),
        bind_addr: normalized_bind_addr(f),
        bind_port: f.bind_port,
        host: f.host.trim().to_string(),
        host_port: f.host_port,
        active,
        status: status.to_string(),
    }
}

pub fn emit_tunnel_update(forwards: &HashMap<String, RuntimeForward>, events: &EventQueue) {
    let mut rows: Vec<TunnelRow> = forwards.values().map(|f| f.info.clone()).collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    push_event(events, Event::Tunnels(rows));
}

/// A saved forward that is configured but not running (`auto_start = false`).
pub fn stopped_entry(id: String, forward: PortForward) -> RuntimeForward {
    RuntimeForward {
        info: tunnel_info(id, &forward, false, "已停止"),
        task: None,
        forward,
    }
}

/// Starts a `-L` or `-D` forward and returns its bookkeeping row.
pub fn start_runtime_forward(
    handle: SharedHandle,
    id: String,
    forward: PortForward,
    events: &EventQueue,
) -> RuntimeForward {
    let info = tunnel_info(id, &forward, true, "运行中");
    let task = match forward.kind.as_str() {
        "local" => Some(spawn_local(
            handle,
            info.bind_addr.clone(),
            info.bind_port,
            info.host.clone(),
            info.host_port,
            events.clone(),
        )),
        "dynamic" => Some(spawn_dynamic(
            handle,
            info.bind_addr.clone(),
            info.bind_port,
            events.clone(),
        )),
        _ => None,
    };
    RuntimeForward { info, task, forward }
}

/// Requests a server-side listener (`-R`) and returns its bookkeeping row.
pub async fn start_remote_forward(
    handle: &SharedHandle,
    targets: &RemoteTargets,
    id: String,
    forward: PortForward,
    events: &EventQueue,
) -> RuntimeForward {
    let mut info = tunnel_info(id, &forward, false, "启动失败");
    let port = u32::from(forward.bind_port);
    let bind = info.bind_addr.clone();
    let duplicate = targets
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(&port);
    if duplicate {
        notice(
            events,
            format!("-R {bind}:{port} 远程监听端口已被另一个转发使用"),
        );
    } else {
        // Register before requesting the listener: a fast incoming connection
        // can arrive as soon as the server accepts the request.
        targets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(port, (forward.host.clone(), forward.host_port));
        match handle.lock().await.tcpip_forward(bind.clone(), port).await {
            Ok(_) => {
                info.active = true;
                info.status = "运行中".to_string();
                notice(
                    events,
                    format!("-R {bind}:{port} → {}:{}", forward.host, forward.host_port),
                );
            }
            Err(error) => {
                targets
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&port);
                notice(events, format!("-R {bind}:{port} 请求失败 / request failed: {error}"));
            }
        }
    }
    RuntimeForward { info, task: None, forward }
}

/// Stops a forward. Returns `false` when a remote cancel failed and the row
/// must stay active.
pub async fn stop_forward(
    entry: &mut RuntimeForward,
    handle: &SharedHandle,
    targets: &RemoteTargets,
    events: &EventQueue,
) -> bool {
    if !entry.info.active {
        return true;
    }
    if entry.forward.kind == "remote" {
        let bind = entry.info.bind_addr.clone();
        let port = u32::from(entry.info.bind_port);
        if let Err(error) = handle.lock().await.cancel_tcpip_forward(bind.clone(), port).await {
            notice(events, format!("-R {bind}:{port} 取消失败 / cancel failed: {error}"));
            return false;
        }
        targets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&port);
    }
    if let Some(task) = entry.task.take() {
        task.abort();
    }
    entry.info.active = false;
    entry.info.status = "已停止".to_string();
    true
}

/// Opens a `direct-tcpip` channel to `host:port`, recording the originating
/// peer (some servers log / ACL on it).
async fn open_direct(
    handle: &SharedHandle,
    host: &str,
    port: u16,
    peer: SocketAddr,
) -> Result<Channel<Msg>, russh::Error> {
    handle
        .lock()
        .await
        .channel_open_direct_tcpip(
            host.to_string(),
            u32::from(port),
            peer.ip().to_string(),
            u32::from(peer.port()),
        )
        .await
}

/// Local forward (-L): listen locally and tunnel each connection to
/// `target_host:target_port` reached from the SSH server's side.
fn spawn_local(
    handle: SharedHandle,
    bind_addr: String,
    bind_port: u16,
    target_host: String,
    target_port: u16,
    events: EventQueue,
) -> JoinHandle<()> {
    let bind = bind_target(&bind_addr, bind_port);
    tokio::spawn(async move {
        let listener = match TcpListener::bind(&bind).await {
            Ok(listener) => listener,
            Err(err) => {
                notice(&events, format!("-L {bind} 监听失败 / bind failed: {err}"));
                return;
            }
        };
        notice(&events, format!("-L {bind} → {target_host}:{target_port}"));
        loop {
            let (mut inbound, peer) = match listener.accept().await {
                Ok(accepted) => accepted,
                Err(_) => break,
            };
            let handle = handle.clone();
            let host = target_host.clone();
            let events = events.clone();
            tokio::spawn(async move {
                match open_direct(&handle, &host, target_port, peer).await {
                    Ok(channel) => {
                        let mut stream = channel.into_stream();
                        let _ = copy_bidirectional(&mut inbound, &mut stream).await;
                    }
                    Err(err) => notice(
                        &events,
                        format!("-L {host}:{target_port} 连接失败 / open failed: {err}"),
                    ),
                }
            });
        }
    })
}

/// Dynamic forward (-D): a minimal SOCKS5 proxy. Each accepted connection
/// negotiates SOCKS5 (no auth, CONNECT only), then we open a `direct-tcpip`
/// channel to the requested destination and splice.
fn spawn_dynamic(
    handle: SharedHandle,
    bind_addr: String,
    bind_port: u16,
    events: EventQueue,
) -> JoinHandle<()> {
    let bind = bind_target(&bind_addr, bind_port);
    tokio::spawn(async move {
        let listener = match TcpListener::bind(&bind).await {
            Ok(listener) => listener,
            Err(err) => {
                notice(&events, format!("-D {bind} 监听失败 / bind failed: {err}"));
                return;
            }
        };
        notice(&events, format!("-D {bind} (SOCKS5)"));
        loop {
            let (inbound, peer) = match listener.accept().await {
                Ok(accepted) => accepted,
                Err(_) => break,
            };
            let handle = handle.clone();
            tokio::spawn(async move {
                let _ = socks5_serve(&handle, inbound, peer).await;
            });
        }
    })
}

/// Handle one SOCKS5 client connection end-to-end.
async fn socks5_serve(
    handle: &SharedHandle,
    mut inbound: TcpStream,
    peer: SocketAddr,
) -> std::io::Result<()> {
    let mut head = [0u8; 2];
    inbound.read_exact(&mut head).await?;
    if head[0] != 0x05 {
        return Ok(());
    }
    let mut methods = vec![0u8; head[1] as usize];
    inbound.read_exact(&mut methods).await?;
    inbound.write_all(&[0x05, 0x00]).await?;

    let mut req = [0u8; 4];
    inbound.read_exact(&mut req).await?;
    if req[0] != 0x05 {
        return Ok(());
    }
    if req[1] != 0x01 {
        let _ = inbound.write_all(&socks_reply(0x07)).await;
        return Ok(());
    }
    let host = match req[3] {
        0x01 => {
            let mut addr = [0u8; 4];
            inbound.read_exact(&mut addr).await?;
            Ipv4Addr::from(addr).to_string()
        }
        0x04 => {
            let mut addr = [0u8; 16];
            inbound.read_exact(&mut addr).await?;
            Ipv6Addr::from(addr).to_string()
        }
        0x03 => {
            let mut len = [0u8; 1];
            inbound.read_exact(&mut len).await?;
            let mut domain = vec![0u8; len[0] as usize];
            inbound.read_exact(&mut domain).await?;
            String::from_utf8_lossy(&domain).into_owned()
        }
        _ => {
            let _ = inbound.write_all(&socks_reply(0x08)).await;
            return Ok(());
        }
    };
    let mut port = [0u8; 2];
    inbound.read_exact(&mut port).await?;
    let port = u16::from_be_bytes(port);

    match open_direct(handle, &host, port, peer).await {
        Ok(channel) => {
            inbound.write_all(&socks_reply(0x00)).await?;
            let mut stream = channel.into_stream();
            let _ = copy_bidirectional(&mut inbound, &mut stream).await;
        }
        Err(_) => {
            let _ = inbound.write_all(&socks_reply(0x05)).await;
        }
    }
    Ok(())
}

fn socks_reply(code: u8) -> [u8; 10] {
    [0x05, code, 0x00, 0x01, 0, 0, 0, 0, 0, 0]
}
