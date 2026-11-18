# P2-A 报告：真实上游 UI 跑在 OpenHarmony 软件渲染后端上

结论：**通过**。上游 `ui/app.slint` 的 `AppWindow`（19k 行 UI）在鸿蒙 PC 上完整渲染，
中文文案、标题栏、标签页、欢迎页、会话列表全部正常。

## 1. 改了什么

| 文件 | 作用 |
| --- | --- |
| `ohos/meatshell-slint/build.rs` | 直接编译上游 `../../ui/app.slint`（fluent 风格、嵌入字体、`EmbedFiles` 资源） |
| `ohos/meatshell-slint/src/ohos_app.rs` | 实例化 `AppWindow`，加载真实配置，构建 `sessions` / `tabs` / `panes` 模型 |
| `ohos/meatshell-slint/src/i18n.rs` | 自带 `.po` 解析 + 自定义 `tr::Translator`，实现中文且不依赖 gettext |
| `ohos/meatshell-slint/src/lib.rs` | FFI 入口改为真实 UI；`ms_spike_init(w, h, data_dir)` |
| `ohos/meatshell-core/src/config_facade.rs` | `pub` 门面再导出上游 `src/config`（上游文件零改动） |
| `ohos/meatshell-core/src/config_structs.rs` | 同上，针对 `src/config/struct/` |
| khmeatshell `napi_init.cpp` / `Index.d.ts` / `SlintSpike.ets` | `slintAttach` 增加 `dataDir`，把 `context.filesDir` 交给 Rust |

上游源码**一行未改**：引擎用 `#[path]` 引用，配置用门面模块重新导出。

## 2. 数据流

```
ArkTS  context.filesDir  ──►  slintAttach(surfaceId, w, h, dataDir)
                                   │
Rust    ms_spike_init → 设 MEATSHELL_DATA_DIR
        └─ config::configure_profile() → ConfigStore::load()
           └─ build_session_rows()（复刻上游 src/app/session_models.rs）
              └─ ui.set_sessions(...)
        └─ AppWindow::new() → 嵌入字体注册 → 泛型字体族补全 → i18n 安装
           └─ tabs=[welcome] / panes=[pane(0,0,w,h)] / active-tab=welcome
```

`ui.show()` 之后由 C++ worker 的 `draw_if_needed` 逐帧提交到 NativeWindow。

## 3. 踩过并解决的坑（P1 未遇到过）

1. **`EmbedForSoftwareRenderer` 编译期 panic**
   `embed_glyphs.rs` 预光栅化真实字体时 `PositionedGlyph` 溢出
   （`large glyph y coordinate`）。改用 `EmbedFiles`，并在 `i-slint-core`
   开启 `image-decoders` + `image-default-formats`（纯 Rust `image`，
   OHOS 可编译），运行时解码嵌入图标。

2. **编辑器补丁必须一起 vendor**
   上游 `editor_pane.slint` 用了 `syntax-source` / `syntax-spans` /
   `line-number-width`，这些属性只存在于上游 vendored 的
   `vendor/i-slint-core` / `vendor/i-slint-compiler`。本地 `[patch.crates-io]`
   必须同时指向 `vendor/{parley,fontique,i-slint-core,i-slint-compiler}`。

3. **启动即 abort：`fallbackfont` 的 `unwrap()`**
   崩溃栈：`ensure_tree_instantiated` → `TextInput` 游标矩形 →
   `i_slint_renderer_software::fonts::match_font` → `core::option::unwrap_failed`。
   根因：OHOS 无 fontconfig，`Collection::new(system_fonts: true)` 找不到任何
   字体，**泛型字体族（SansSerif/SystemUi/UiSansSerif）全空**；不带 family
   的字体查询没有候选，`query_fontique(...).unwrap()` 直接 abort。
   修复：
   - `set_platform` **之前** 设 `SLINT_DEFAULT_FONT=/system/fonts/HarmonyOS_Sans_SC.ttf`
   - 注册后对 SansSerif/SystemUi/UiSansSerif/Serif/Cursive/Fantasy
     调 `set_generic_families`
   - `AppWindow::new()` 之后（嵌入的 Meatshell Mono 已注册）再
     `append_generic_families(Monospace, "Meatshell Mono")`

4. **`ui.show()` 了但整屏只有背景色**
   渲染其实成功（436455/436455 像素不透明、`#23262D`），只是**内容区为空**：
   `panes` 模型没设置，Slint 的 pane 区域什么都不画。补上
   `tabs=[welcome]` + 单 pane（`x=0,y=0,w=窗口宽,h=窗口高-38`）后欢迎页出现。

5. **上游 config API 是 `pub(crate)`**
   `pub use` 无法跨 crate 提升可见性。做法：`ohos/meatshell-core/src/
   config_facade.rs` + `config_structs.rs` 用 `#[path]` 指向上游文件并
   全部 `pub` 再导出。结构与上游 `mod.rs` 对应，**上游零改动**。
   上游新增 `src/config/` 模块时要同步这两个门面文件。

6. **中文翻译不能用 gettext**
   OHOS sysroot 有 `libintl.h` 但**没有 `libintl.so`**，`slint` 的 `gettext`
   feature 会链接失败；`tr` crate 的默认 feature 还会拉系统 gettext（其
   build script 要求 `xz`）。
   做法：`i-slint-core` 开 `tr` feature，`tr = { default-features = false }`
   只取 `Translator` trait；`src/i18n.rs` 用 `include_str!` 内嵌
   `lang/zh/LC_MESSAGES/meatshell.po`，运行时解析一次，通过
   `with_global_context(|ctx| ctx.set_external_translator(...))` 安装。
   另设 `ui.set_lang_en(false)` 让 Rust 驱动的文案也走中文。

## 4. 真机验证（Kaihong PC，1366×768，API 23）

| 项 | 结果 |
| --- | --- |
| 真实 AppWindow 渲染 | ✅ 标题栏「meatshell」+ 窗口按钮（— □ ×） |
| 标签页 | ✅ 「新标签页」+ `+` / `⌄` |
| 欢迎页 | ✅ 「meatshell / 一个轻量、高效的 Rust + Slint SSH 客户端」 |
| 快速连接面板 | ✅ 「快速连接」「+ 新建会话」「按名称或主机搜索…」 |
| 空状态 | ✅ 「尚无会话 / 点击右上角"新建会话"添加你的第一台服务器」 |
| 中文字体 | ✅ HarmonyOS Sans SC 正常；嵌入的 Meatshell Mono 用于终端 |
| 数据目录 | ✅ `context.filesDir` → `hap s/entry/files`，`ConfigStore` 可写 |
| 事件驱动 | ✅ 空闲不重绘（frames 稳定），点击/按键才出新帧 |
| 渲染像素 | ✅ 915×477 全帧、stride 928、全部不透明 |

截图：`/tmp/p2zh2.jpeg`（真机桌面）。

## 5. 调试手段（保留）

在设备上设置环境变量 `MS_OHOS_DUMP_FRAME=1`，首帧会把像素统计写到
`<files>/slint-diag.log`，并把整帧写成 `<files>/slint-frame.ppm`。
在真机上比 JPEG 截图可靠得多（后者被压缩且带缩放）：

```bash
hdc file recv /data/app/el2/100/base/com.kaihong.khmeatshell/haps/entry/files/slint-frame.ppm /tmp/f.ppm
sips -s format png /tmp/f.ppm --out /tmp/f.png
```

## 6. 未完成（P2-B）

1. **输入**：IME 组字（需要隐藏 `TextInput` 或 `inputMethod` 引擎）、
   滚轮（`PointerScrolled`）、右键菜单、鼠标光标形态。
2. **交互接线**：基础通路已在 P2-B-1 完成（见 `P2B-INTERACTION.md`：搜索/标签页/
   分组折叠/窗口控制/指针滚轮）；剩余：新建/编辑会话对话框、真实连接流程。
3. **侧栏状态**：上游 `refresh_sidebar` 还会写 `proc-list` /
   `downloads` / `sftp` 等模型，当前只喂了 `sessions`。
4. **窗口尺寸**：上游 `preferred-width: 1440px`，建议在 ArkTS 侧给
   XComponent 一个接近全屏的初始尺寸（现在 915×477 是 P1 遗留默认值）。
5. **桌面专属功能**：`rfd` 文件对话框、托盘、串口枚举、`arboard` 剪贴板、
   原生窗口菜单都需要 ArkUI 侧替代实现。
6. **性能**：终端大段输出时的局部刷新（当前 `NewBuffer` 全帧重绘，
   1344×677 约 60fps）。
