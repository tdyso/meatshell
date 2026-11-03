//! meatshell-core —— meatshell 开源鸿蒙移植的引擎层。
//!
//! 通过 `#[path]` **原样引用** 上游 meatshell 0.8.1 的引擎源码
//! （ssh / sftp / tunnel / config / i18n / terminal-headless），
//! 上游文件零改动，`git fetch upstream` 后重新编译即完成同步。
//!
//! 模块对应关系（全部与上游 main.rs 中的声明一致）：
//! - `config`   会话配置 / 密码加密存储 / FinalShell 导入
//! - `i18n`     双语文案
//! - `ssh`      SSH 协议引擎（含 proxy、known_hosts、PuTTY PPK、ssh_config）
//! - `sftp`     SFTP 文件传输
//! - `tunnel`   端口转发 / 隧道
//! - `terminal` headless 变体（encoding + zmodem，即 sz/rz）
//!
//! 暂未纳入：ui/app/layout/session/rdp/mcp/cli/automation 等（桌面或服务侧，
//! 待 UI 层确定 Slint-OHOS 方案后再接）。
//!
//! FFI（由 HAP 侧 NAPI 模块 `libmeatshell.so` 导出给 ArkTS）：
//! - `ms_version()`                              版本串
//! - `ms_selftest()` / `ms_selftest_detail()`    tokio + chacha 冒烟自检
//! - `ms_tcp_probe(host, port, timeout_ms)`      异步 TCP 连通性探测

use std::ffi::{c_char, CStr};
use std::sync::OnceLock;

// ———————— 上游引擎源码（零修改引用）————————

#[path = "../../../src/config/mod.rs"]
pub mod config;
#[path = "../../../src/i18n/mod.rs"]
pub mod i18n;
#[path = "../../../src/ssh/mod.rs"]
pub mod ssh;
#[path = "../../../src/sftp/mod.rs"]
pub mod sftp;
#[path = "../../../src/tunnel/mod.rs"]
pub mod tunnel;
#[path = "../../../src/terminal/headless.rs"]
pub mod terminal;

// ———————— FFI ————————

static SELFTEST_DETAIL: OnceLock<&'static str> = OnceLock::new();
static LAST_ERR: OnceLock<String> = OnceLock::new();

fn set_detail(s: String) {
    let _ = SELFTEST_DETAIL.set(Box::leak(s.into_boxed_str()));
}

fn set_err(e: String) {
    let _ = LAST_ERR.set(e);
}

/// 返回最近一次 FFI 调用的错误详情（诊断用，进程生命周期内有效）。
#[no_mangle]
pub extern "C" fn ms_last_error() -> *const c_char {
    match LAST_ERR.get() {
        Some(s) => s.as_ptr() as *const c_char,
        None => concat!("no error\0").as_ptr() as *const c_char,
    }
}

/// 返回核心库版本号（静态字符串，调用方无需释放）。
#[no_mangle]
pub extern "C" fn ms_version() -> *const c_char {
    concat!(
        "meatshell-core ",
        env!("CARGO_PKG_VERSION"),
        " (ohos-port, upstream engine v0.8.1)\0"
    )
    .as_ptr() as *const c_char
}

/// 冒烟自检：tokio 运行时（spawn/join/sleep）+ ChaCha20-Poly1305 加解密回环。
/// 返回 0 = 通过；-1 运行时创建失败；-2 自检失败。
#[no_mangle]
pub extern "C" fn ms_selftest() -> i32 {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(_) => return -1,
    };

    let result: Result<String, &'static str> = rt.block_on(async {
        let task = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            21u32
        });
        let spawned = task.await.map_err(|_| "tokio join failed")?;

        use chacha20poly1305::{
            aead::{Aead, AeadCore, KeyInit, OsRng},
            ChaCha20Poly1305,
        };
        let key = ChaCha20Poly1305::generate_key(&mut OsRng);
        let cipher = ChaCha20Poly1305::new(&key);
        let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
        const PLAINTEXT: &[u8] = b"meatshell-ohos-selftest";
        let ciphertext = cipher
            .encrypt(&nonce, PLAINTEXT)
            .map_err(|_| "chacha encrypt failed")?;
        let plaintext = cipher
            .decrypt(&nonce, ciphertext.as_ref())
            .map_err(|_| "chacha decrypt failed")?;
        if plaintext != PLAINTEXT {
            return Err("chacha roundtrip mismatch");
        }
        Ok(format!(
            "tokio:spawn/join/sleep=ok(ch sum {spawned}); chacha20poly1305=ok"
        ))
    });

    match result {
        Ok(detail) => {
            set_detail(detail);
            0
        }
        Err(e) => {
            set_detail(format!("FAILED: {e}"));
            -2
        }
    }
}

/// 返回自检详情（进程生命周期内有效，调用方无需释放）。
#[no_mangle]
pub extern "C" fn ms_selftest_detail() -> *const c_char {
    match SELFTEST_DETAIL.get() {
        Some(s) => s.as_ptr() as *const c_char,
        None => concat!("selftest not run yet\0").as_ptr() as *const c_char,
    }
}

/// 异步 TCP 连通性探测（验证 tokio::net 在 OpenHarmony 上可用）。
/// 返回 0 = 连接成功；-1 = 连接失败或超时；-2 = 参数无效；-3 = 运行时创建失败。
///
/// # Safety
/// `host` 必须是合法的 NUL 结尾 C 字符串。
#[no_mangle]
pub unsafe extern "C" fn ms_tcp_probe(host: *const c_char, port: u16, timeout_ms: u32) -> i32 {
    if host.is_null() {
        return -2;
    }
    let host = match CStr::from_ptr(host).to_str() {
        Ok(s) if !s.is_empty() => s,
        _ => return -2,
    };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => return -3,
    };
    let target = format!("{host}:{port}");
    let target_for_err = target.clone();
    let res = rt.block_on(async move {
        tokio::time::timeout(
            std::time::Duration::from_millis(u64::from(timeout_ms).max(1)),
            tokio::net::TcpStream::connect(&target),
        )
        .await
    });
    match res {
        Ok(Ok(_)) => 0,
        Ok(Err(e)) => {
            set_err(format!("connect {}: {e}", target_for_err));
            -1
        }
        Err(_) => {
            set_err(format!("connect {}: timeout after {timeout_ms}ms", target_for_err));
            -1
        }
    }
}
