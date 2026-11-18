# P2-B-1 报告：交互接线（标签页 / 分组 / 搜索 / 输入通路 / 窗口控制）

结论：**通过（真机验证）**。真实上游 `AppWindow` 的业务回调已经从"全部空实现"变成
可用的最小闭环：搜索过滤、标签页选择/关闭/新建、分组折叠（含持久化）、
`content-resized` 精确布局、键盘字符输入、左右中键与滚轮通路、
以及自定义标题栏的最小化/最大化/关闭。

验证设备：Kaihong PC（1366×768，API 23），`hdc` 走 `192.0.2.145:10178`。

## 1. 改动的文件

| 文件 | 改动 |
| --- | --- |
| `ohos/meatshell-slint/src/ohos_app.rs` | 重写：模型持有 + 11 个回调接线 + 指针/滚轮 + 符号键名映射 |
| `ohos/meatshell-slint/src/lib.rs` | `ms_spike_pointer` 增加 button；新增 `ms_spike_scroll`、`ms_spike_take_action` |
| khmeatshell `napi_init.cpp` | `Scroll` 命令、Pointer 带 button、渲染线程收集窗口动作、`slintScroll`/`slintTakeAction` |
| khmeatshell `Index.d.ts` | 对应声明（`slintPointer(x,y,kind,button?)`、`slintScroll`、`slintTakeAction`） |
| khmeatshell `SlintSpike.ets` | 符号键名映射 + 修饰键跟踪、`onMouse`、`onAxisEvent`、100ms 轮询窗口动作 |
| khmeatshell `deploy.sh` + `signing/` | 签名材料从 `/tmp` 迁到工程内（`/tmp` 被系统清理会导致签名失败） |

上游 Rust / Slint 源码仍然**一行未改**。

## 2. 已接线的回调

| 回调 | 行为 |
| --- | --- |
| `host-search-changed` | 按 name / host / user 过滤 `sessions` 模型 |
| `toggle-group` | 就地翻转折叠态 + `ConfigStore::set_session_group_collapsed` + `save()` |
| `tab-selected` / `pane-tab-selected` | 同步 `active-tab-id` 与所属 pane 的 `active-id` |
| `tab-closed` / `pane-tab-closed` | 关掉标签页；最后一个标签关闭时补一个新的欢迎页 |
| `new-tab-clicked` / `pane-new-tab` | 复用/新建 `welcome` 标签并激活（见 §5-4） |
| `pane-focus` | 更新 `focused`，决定新标签页落在哪个 pane |
| `content-resized` | 用真实区域尺寸重写 pane 0 的 `x/y/w/h`（`reserve-right` 保留右上浮动工具条宽度） |
| `win-minimize` / `win-maximize-toggle` / `win-close` | 写入 `PendingAction`，由 ArkTS 轮询后调用窗口 API |

输入通路（Rust）：

- `ms_spike_key(text, pressed)`：ArkTS 只发送**符号名**（`"Return"`、`"Control"`、`"F5"`…），
  Rust 侧 `slint_key_text()` 映射到 `slint::platform::Key`；普通字符原样作为 `text`。
- `ms_spike_pointer(x, y, kind, button)`：`kind` 0/1/2 = 按下/抬起/移动，`button` 0/1/2 = 左/右/中。
- `ms_spike_scroll(x, y, dx, dy)`：转成 `WindowEvent::PointerScrolled`。

## 3. 窗口控制的数据流（刻意单向）

```
Slint 自定义标题栏按钮
   └─ callback（渲染线程）
        └─ UiState.pending_action: Rc<RefCell<Option<String>>>
             └─ C++ worker 每帧 ms_spike_take_action() 取走 → Bridge.pending_action
                  └─ ArkTS 每 100ms slintTakeAction() → window.minimize()/maximize()/recover()
                       └─ close → UIAbilityContext.terminateSelf()
```

Rust 侧永远不碰 OHOS 窗口，ArkTS 侧永远不碰 Slint；`take_action` 只在渲染线程调用
（`UI` 是 thread-local，不能从 ArkTS 线程直接问）。

## 4. 真机验证

| 用例 | 操作 | 结果 |
| --- | --- | --- |
| 字符输入 | 点搜索框 → `uitest uiInput text "abcdef"` | 输入框显示 `abcdef`，光标可见 |
| 退格 | `uitest uiInput keyEvent 2055` ×5 | 文本被正确清除 |
| 搜索过滤 | 输入非空关键字 | 空态文案切到「没有匹配的会话」 |
| 最大化 | 点 Slint 标题栏 `□` | hilog：`MaximizeFloating status: 1`、viewport 变 `(1366,704)` |
| 还原 | 再点一次 | hilog：`OnRecover end`、`rect 225 95 915 514`；界面按新尺寸重排，搜索文本保留 |
| 最小化 | 点 Slint 标题栏 `−` | hilog：`OnMinimize ... ret=0`；`aa start` 恢复后画面完整重绘（无黑屏/残影） |
| 关闭 | 点 Slint 标题栏 `×` | 进程从 `pidof` 中消失（`terminateSelf`），再 `aa start` 可重新拉起 |
| 新建标签页菜单 | 点标签条 `+` | 弹出「AI 智能体 / 自定义…」菜单，未破坏现有标签页 |

真机截图按阶段保存在 `/tmp/s0.jpeg … /tmp/s9.jpeg`（s7/s9 为窗口还原后的重排效果）。

## 5. 未解决 / 已知问题

1. **双层标题栏**：系统标题栏 + Slint 自定义标题栏同时存在。
   隐藏系统装饰（`setWindowDecorVisible(false)`）需要同时实现标题栏拖拽
   （`window.startMoving()`）和双击最大化，否则窗口会无法移动，故本轮未做。
2. **滚轮方向未实测**：`onAxisEvent` 里按 `delta = -axis * 60` 换算，符号取决于
   设备上报的正负；插上鼠标滚一格即可确认（改一个常量即可翻转）。
3. **右键已通路但未实测**：`uitest uiInput` 只能注入触摸/按键，没有右键注入接口。
4. **新建流程缺位**：`+` 打开的是上游启动菜单（本地终端/AI 智能体/自定义），
   OHOS 上这些都还没有实现，所以 `new-tab-clicked` 实际只在菜单项被点时才走；
   目前点击后复用欢迎页，避免出现「id 不是 welcome 的空 pane」。
5. **会话数据无法预置**：`ConfigStore` 在磁盘上是 ChaCha20 加密的 `sessions.json`，
   没法在主机侧手写测试数据 → 分组折叠 / 会话选择 / 真实列表刷新
   要等会话编辑器（下一步）能创建会话后再验证。
6. **初始窗口尺寸**仍是 P1 遗留的 915×477（还原后为 915×514 含系统标题栏），
   上游 `preferred-width: 1440px`。
7. **`sessions-revision`** 只在分组折叠时 +1；上游侧栏刷新时还会写
   `proc-list` / `downloads` / `sftp` 等模型，当前仍未喂。

## 6. 签名材料（顺带修复）

`/tmp/ohsign-*.pem|p7b` 在系统重启后被清理，导致 `sign-app` 报
`Invalid file: /tmp/ohsign-chain.pem`。已在工程内生成固定材料：

```
signing/profile.json   # UnsgnedDebugProfileTemplate.json 改成 com.kaihong.khmeatshell + 设备 UDID
signing/profile.p7b    # hap-sign-tool sign-profile（alias: openharmony application profile debug）
signing/chain.pem      # 模板里的开发证书 + OpenHarmony.p12 里的 cacert/rootcacert
```

`deploy.sh` 现在直接引用这三个文件，重启后不会再失效。

## 7. 下一步建议

1. 会话编辑器（新建/编辑对话框）+ 会话持久化 → 才能验证分组折叠、列表点击、真实刷新。
2. 真实 SSH 连接流程：复用 P1 验证过的 russh/tokio，把 `connect-session` 接到
   终端标签页（含 PTY）。这是 P2 的关键里程碑。
3. 标题栏拖拽 + 隐藏系统装饰（一次做，避免只有一半体验）。
4. 中文 IME 组字（隐藏 `TextInput` 承接 `inputMethod` 的 `commitText`）。
5. 滚轮/右键/光标形态实测与微调。
