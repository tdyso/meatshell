# P1 验证报告：Slint 软件渲染跑在 OpenHarmony PC 上

结论：**Go**。Slint 1.18.1 的 software renderer 可以通过 `XComponent + OHNativeWindow`
把画面直接送进鸿蒙 PC 的显示合成链路，中文、键盘、触摸、窗口缩放全部在真机上验证通过，
并且可以做到事件驱动（空闲时不重绘）。

## 1. 验证环境

| 项 | 值 |
| --- | --- |
| 设备 | Kaihong PC（x86_64），1366×768 |
| 系统 | OpenHarmony-6.1.0.35（API 23），KaihongOS 6.1.1.10 |
| 交叉编译 target | `x86_64-unknown-linux-ohos` / `aarch64-unknown-linux-ohos`（musl） |
| Slint | 1.18.1，`renderer-software` + 自定义 `Platform` / `WindowAdapter` |
| 像素格式 | `NATIVEBUFFER_PIXEL_FMT_RGBA_8888` ← `PremultipliedRgbaColor` |

## 2. 渲染链路

```
ArkTS  XComponent(SURFACE)
   │  getXComponentSurfaceId() + getXComponentSurfaceRect()
   ▼
NAPI   slintAttach(surfaceId, w, h)                （napi_init.cpp）
   │  OH_NativeWindow_CreateNativeWindowFromSurfaceId
   │  NativeWindowHandleOpt(SET_FORMAT/SET_BUFFER_GEOMETRY)
   ▼
渲染 worker 线程（16ms 节拍）
   │  NativeWindowRequestBuffer
   │  GetBufferHandleFromNative → mmap(handle->fd, handle->size)
   │  ms_spike_render_to(ptr, w, h, stride_pixels)  （Rust）
   │      → MinimalSoftwareWindow::draw_if_needed → 全帧软件光栅
   │  msync + munmap
   │  NativeWindowFlushBuffer（未绘制则 AbortBuffer）
   ▼
系统合成器
```

关键点：

- `RepaintBufferType::NewBuffer`：NativeWindow 是轮转的多缓冲区，不能用 `ReusedBuffer`
  （只重绘脏区会把新 buffer 的其余部分留成黑色，实测表现为「按键后整屏变黑」）。
  选 `NewBuffer` 后每帧都是全量重绘，画面稳定。
- `stride`：`handle->stride` 是**字节**，渲染 stride 用 `stride_bytes / 4`。
  实测 1344 宽的窗口 stride = 1376 像素（32 像素对齐），必须按 stride 而不是 width 写。
- 事件驱动：`render_to()` 里只调 `update_timers_and_animations()` + `draw_if_needed`，
  **不**无条件 `request_redraw()`。Slint 不需要重绘时 C++ 侧 `AbortBuffer`，
  合成器继续显示上一帧。实测空闲 20 秒 frames 恒为 1；一次点击 + 一次按键后 frames = 5。
- 输入端：`WindowEvent::KeyPressed/Released`、`PointerPressed/Released/Moved` 直接
  `dispatch_event` 给 Slint，坐标用 `vp2px()` 转像素、`LogicalPosition` 传入。

## 3. 真机实测结果

| 验证项 | 结果 |
| --- | --- |
| XComponent 上显示 Slint UI | ✅ 全屏、无花屏、最大化后布局正确重排 |
| 中文字体 | ✅ 注册 `/system/fonts/HarmonyOS_Sans_SC.ttf`，族名 `HarmonyOS Sans SC` |
| 键盘输入 | ✅ `按键计数` 随按键递增，`最后按键` 显示实际字符（`d`、`(space)`） |
| 触摸 / 点击 | ✅ `点击计数` 递增，`指针坐标` 与注入坐标一致（600, 263） |
| 窗口 resize | ✅ 915×477 → 1344×677，stride 928 → 1376，Slint 自动重排 |
| 连续重绘吞吐 | 1344×677 全帧软件光栅可跑到 ~60fps（历史上限测试 ~3100 帧/43s） |
| 空闲 CPU | ✅ 事件驱动后空闲 0 帧，之前无条件 request_redraw 会持续空转 |

## 4. 踩过并解决的关键问题

1. **fontconfig 不存在于 OHOS**：`parley`/`fontique` 默认 feature `system` 会拉入
   `yeslogic-fontconfig-sys`。做法：把两个 crate vendored 到
   `ohos/meatshell-slint/vendor/{parley,fontique}`，`default = ["std"]`，
   并在 `Cargo.toml` 里 `[patch.crates-io]` 指向本地。字体改为运行时
   `fontique_011::shared_collection().register_fonts(...)` 手动注册。
2. **`i-slint-core` 的 `font_context` 不存在**：`slint` 用 `default-features = false`
   时不会启用 `shared-parley`，需要显式依赖
   `i-slint-core = { features = ["std", "shared-parley", "shared-swash"] }`。
3. **两个 Rust staticlib 重复 std 符号**：引擎和 UI 各出一个 `staticlib` 链接进同一个
   `.so` 时 `ld.lld: duplicate symbol: <alloc::string::String as Clone>::clone`。
   做法：`meatshell-core` 增加 `rlib`，`meatshell-slint` 依赖它，只产出一个
   `libmeatshell_slint.a`。注意 LTO 会丢掉未被引用的 `#[no_mangle]` 引擎入口，
   所以 `lib.rs` 里保留了 `ms_engine_link_keepalive()` 显式引用全部引擎导出符号。
4. **`libc++_shared.so` 找不到**：`MUSL-LDSO: Error loading shared library
   libc++_shared.so`。app 沙箱命名空间不暴露 `/system/lib64/libc++_shared.so`，
   必须把 NDK 的 `libc++_shared.so` 一起打进 HAP（`entry/libs/<abi>/`）。
5. **`keyText` 是符号名**：ArkUI 给的 `KeyEvent.keyText` 是 `KEYCODE_A` 这类名字，
   不是字符。按 `keyCode` 自己映射（A–Z = 2017–2042，0–9 = 2000–2009，
   Enter/Backspace/Tab/Esc/Delete/Space 等）。
6. **NAPI 类型声明**：`entry/src/main/cpp/types/libmeatshell/` 需要
   `Index.d.ts` + `oh-package.json5` + `package.json` 三件套，否则 ArkTS 把
   `import meatshell from 'libmeatshell.so'` 当 `any`，`const rc = ...` 会触发
   `arkts-no-any-unknown`。

## 5. 复现

```bash
cd ~/DevEcoStudioProjects/khmeatshell
bash deploy.sh      # cargo build(两个 ABI) → clang++ 链接 → hvigor 打包 → 签名 → 安装 → 启动
```

产物：`entry/libs/{x86_64,arm64-v8a}/{libmeatshell.so, libc++_shared.so}`，
应用启动页在 `EntryAbility.ets` 里指向 `pages/SlintSpike`。

## 6. 剩余工作（P2 及之后）

1. **中文 IME**：`XComponent.onKeyEvent` 拿不到输入法组字结果，需要叠一个隐藏
   `TextInput` 或用 `inputMethod` 引擎把 composing text 送进 Slint。
2. **窗口/对话框/文件选择**：`rfd`、原生窗口标题栏、右键菜单都要换成 ArkUI 侧实现。
3. **桌面专属依赖**：`arboard`（wayland）、`tray-icon`（ksni）、`serialport`（libudev）
   在 OHOS 不可用；且 OHOS target 的 `target_os` 仍是 `linux`，
   现有 `cfg(target_os = "linux")` 分支会误命中，必须补 `cfg(target_env = "ohos")` 判定。
4. **上游 UI 规模**：19k 行 Slint 需要在 `MinimalSoftwareWindow` 平台上逐页跑通
   （字体度量、emoji、图标、动画）。
5. **性能**：终端大段输出时的局部刷新策略（`ReusedBuffer` + 单缓冲，或脏区→FlushBuffer 区域）。
6. **鼠标光标形态**、滚动、双击、右键、滚轮（`WindowEvent::PointerScrolled`）目前未接。
