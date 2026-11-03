//! meatshell-core —— meatshell 开源鸿蒙移植骨架。
//!
//! 本 crate 只承担「核心引擎 + FFI 冒烟自检」：把上游 meatshell 的核心依赖
//! （tokio 异步运行时、russh SSH 协议栈、rustcrypto 加密）在
//! `x86_64/aarch64-unknown-linux-ohos` 目标上真实编译并链接一次，
//! 由 HAP 侧的 NAPI 模块加载后调用自检函数验证整个链路。
//!
//! 后续移植步骤（按 fork 的 ohos-port 分支推进）：
//! 1. 把上游 src/ssh.rs、src/sftp.rs、src/telnet.rs 逐个搬进来并去掉桌面耦合；
//! 2. UI 走 Slint 自定义 OpenHarmony platform backend（XComponent 渲染）；
//! 3. rfd/arboard/dark-light 等平台能力改为 NAPI 桥接到 ArkTS 侧。

use std::ffi::c_char;
use std::sync::OnceLock;

static SELFTEST_DETAIL: OnceLock<&'static str> = OnceLock::new();

/// 返回核心库版本号（静态字符串，调用方无需释放）。
#[no_mangle]
pub extern "C" fn ms_version() -> *const c_char {
    concat!(
        "meatshell-core ",
        env!("CARGO_PKG_VERSION"),
        " (ohos-port skeleton, upstream v0.8.1)\0"
    )
    .as_ptr() as *const c_char
}

/// 冒烟自检：验证 tokio 多线程运行时在 OpenHarmony 上的线程/定时器调度、
/// tokio 任务 spawn/join，以及 ChaCha20-Poly1305 加解密回环。
///
/// 返回 0 表示全部通过；非 0 为错误码（-1 运行时创建失败，-2 自检失败）。
#[no_mangle]
pub extern "C" fn ms_selftest() -> i32 {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(_) => return -1,
    };

    let result: Result<String, &'static str> = rt.block_on(async {
        // 1) tokio：并发任务 spawn + join + 定时器
        let task = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            21u32
        });
        let spawned = task.await.map_err(|_| "tokio join failed")?;

        // 2) 加密：ChaCha20-Poly1305 加解密回环（与上游会话密码加密同款算法）
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

        Ok(format!("tokio:spawn/join/sleep=ok(ch sum {spawned}); chacha20poly1305=ok"))
    });

    match result {
        Ok(detail) => {
            let _ = SELFTEST_DETAIL.set(Box::leak(detail.into_boxed_str()));
            0
        }
        Err(e) => {
            let _ = SELFTEST_DETAIL.set(Box::leak(format!("FAILED: {e}").into_boxed_str()));
            -2
        }
    }
}

/// 返回自检详情（静态存储，进程生命周期内有效，调用方无需释放）。
/// 未执行自检时返回提示文本。
#[no_mangle]
pub extern "C" fn ms_selftest_detail() -> *const c_char {
    match SELFTEST_DETAIL.get() {
        Some(s) => s.as_ptr() as *const c_char,
        None => concat!("selftest not run yet\0").as_ptr() as *const c_char,
    }
}
