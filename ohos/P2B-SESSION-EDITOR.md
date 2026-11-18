# P2-B-2 报告：会话编辑器（新建 / 编辑 / 保存持久化）

结论：**通过（真机闭环）**。上游 `ui/session_dialog.slint` 的全部输入面已在 OHOS
软件渲染后端跑通：新建会话 → 填入主机/端口/用户名 → 保存 → 欢迎页出现分组与卡片
→ `force-stop` 重启后仍在 → 搜索能过滤又能恢复 → 右键菜单（编辑/复制/删除/移动到）
→ 编辑对话框正确预填 → 取消不落盘。

验证设备：Kaihong PC（1366×768，API 23），`hdc` 走 `192.0.2.145:10178`。

## 1. 改动的文件

| 文件 | 改动 |
| --- | --- |
| `ohos/meatshell-slint/src/session_editor.rs` | 新增（765 行）：对话框 6 个回调 + 会话动作 10 个回调 + draft↔Session 转换 |
| `ohos/meatshell-slint/src/ohos_app.rs` | `SessionsView`（`model/all/query`）、`apply_search`、`refresh_sessions`、`named_display_groups`；`wire_callbacks` 增加 store 参数并挂接 `session_editor::register` |
| `ohos/meatshell-slint/src/lib.rs` | `mod session_editor;`（FFI 入口不变） |
| `ohos/meatshell-slint/Cargo.toml` | 新增 `uuid = { version = "1", features = ["v4"] }`（`new_id()`） |
| khmeatshell `SlintSpike.ets` | 滚轮符号修正（`deltaY = vv * 60`，不再取负）+ `onMouse` 跟踪鼠标位置兜底；`onSizeChange` 触发 `resizeSurface` |
| khmeatshell `EntryAbility.ets` | `applyPreferredWindowSize` / `fitWindow`：迭代回读 `windowRect` 收缩到可用高度，再居中 |

上游 Rust / Slint 源码仍然**一行未改**。

## 2. 对话框能力（已接线）

`register_dialog`：`on_add_jump` / `on_update_jump` / `on_delete_jump` /
`on_move_jump` / `on_reveal_session_secret`；
`register_actions`：`on_new_session_clicked` / `on_edit_session` /
`on_duplicate_session` / `on_remove_session` / `on_move_session` /
`on_session_dialog_submit` / `on_session_dialog_cancel` /
`on_session_dialog_closed` / `on_session_dialog_changed`。

对话框字段（真机截图确认渲染与预填均正确）：

- 连接类型：SSH / 串口 / Telnet / RDP，切换时端口默认值 22 / 23 / 3389 联动。
- 名称、分组（可选，下拉来自 `session_groups_model`）、备注。
- 主机 / IP + 端口、用户名。
- 认证方式：密码 / 键盘交互 / 私钥 / 内联私钥。
- 高级：字符集、代理、隧道（`split_proxy`）。
- 跳板机链路：目标行 `root@host:22`，添加/上下移/删除，最多 `MAX_JUMP_HOPS = 16`。
- Footer：测试连接 / 取消 / 保存。

**降级项（明确文案，不留死按钮）**：

| 入口 | OHOS 行为 |
| --- | --- |
| 测试连接 | 状态栏提示「OHOS 版暂不支持连接测试，将在接入真实 SSH 后可用」 |
| 私钥选择器 | 提示改用「内联私钥」直接粘贴 |
| RDP 预览 | 回调返回空串（无 RDP 引擎） |
| 串口端口下拉 | 为空模型（OHOS 无本地串口枚举） |

## 3. 保存数据流

```
对话框（Slint）
  └─ session-dialog-submit(draft)          ← 渲染线程
       ├─ session_from_draft(draft, existing, forwards, triggers)   // 复用上游结构
       │     · 新会话：new_id() (uuid v4)；编辑：沿用原 id / builtin
       │     · serial 用 "<port> @<baud>" 作为 host 展示
       │     · telnet/rdp 端口回退 23/3389
       ├─ store.borrow().upsert(session)   // ChaCha20 加密 sessions.json
       └─ refresh_sessions(view)           // 重建 SessionsView::model（分组 + 折叠态）
```

搜索：`host-search-changed` → `apply_search`（name / host / user 子串）→ 过滤 `all` →
重建 `model`；清空即恢复。分组折叠态复用 `set_session_group_collapsed` + `save()`。

## 4. 真机验证

| 用例 | 注入方式 | 结果 |
| --- | --- | --- |
| 打开新建对话框 | 点 header 右端按钮细条 (1216,238) | 对话框出现，字段/中文/四种连接类型正常 |
| 滚到底部 | `uinput -M -m 683 350 -s -8` ×12（同一会话内） | Flickable 滚动，footer（测试连接/取消/创建）完整可见 |
| 填表 | `uitest uiInput inputText` 依次填 名称/主机/用户名 | 输入框逐字显示正确 |
| 创建 | 点 (837,633)「创建」 | 对话框关闭；欢迎页出现分组 `default` |
| 展开分组 | 点 (113,328) | 卡片 `Test Host A ｜ 192.0.2.10:22 ｜ root` |
| 持久化 | `aa force-stop` + `aa start` | 分组展开态与会话仍在；`sessions.json` 3420 B（`08:50` 写入，`secret.key` 32 B） |
| 搜索（无匹配） | `uitest uiInput inputText 533 284 'zzz'` | 空态文案「没有匹配的会话」，分组与卡片被过滤 |
| 搜索（有匹配） | 退格 ×3 后输入 `Test` | `default` + `Test Host A` 重新出现 |
| 清空搜索 | 退格 ×4 | 占位提示恢复、列表完整 |
| 右键菜单 | `uinput -M -m 140 355 -c 1` | 弹出「编辑 / 复制副本 / 删除 / 移动到」 |
| 编辑预填 | 点菜单「编辑」 | 名称 `Test Host A`、主机 `192.0.2.10`、端口 `22`、用户 `root`、认证「密码」；密码框仅占位提示「留空则不修改密码」 |
| 取消 | 点「取消」 | 对话框关闭，列表与磁盘数据不变 |

截图：`/tmp/p2b_now.jpeg`（搜索空态）、`/tmp/p2b_match.jpeg`（正向匹配）、
`/tmp/p2b_clear.jpeg`、`/tmp/p2b_ctx.jpeg`（右键菜单）、`/tmp/p2b_edit.jpeg`
（编辑预填）、`/tmp/p2b_edit_footer.jpeg`（footer）、`/tmp/p2b_after_cancel.jpeg`。

## 5. 注入方法与两个符号结论

- **右键**：`uinput -M -m <x> <y> -c 1`（`-c` 的 0/1/2/3 = 左/右/中/侧键），
  这补上了 P2-B-1 §5-3「没有右键注入接口」的缺口。
- **滚轮**：必须与 `-m` 写在**同一条** `uinput` 命令里（`-m 683 350 -s -8`）。
  分两次执行时 XComponent 会先收到 `LeaveWindow`（action=5），axis 事件丢失。
- **滚轮方向**：实测 `-s -8` 对应 `AxisEvent.getVerticalAxisValue() = -8`，
  而 Slint 的 `delta-y` 负值同为「内容上移 / 向下翻」，故 `deltaY = vv * 60`
  （此前取负只会在顶部向上滚、无反应）。`deltaX = hv * 60` 同号处理。
- **坐标**：屏幕坐标 = XComponent bounds 原点 + Slint 逻辑坐标（本机 scale = 1，
  XComponent 可见区 `[25,42]..[1341,699]`）。
- **键盘**：`uinput -K -d/-u 2055`（Backspace）、`uitest uiInput inputText`（整段文本）。

## 6. 窗口几何（本轮顺带修复）

问题：对话框底部「创建」按钮被窗口底边裁掉、点不到（XComponent 布局高 680，
`dumpLayout` 可见仅 607/657）。

- `win.resize(1306,664)` 实际得到 1316×781——真实高度比请求值大约 +50 px。
- `getWindowAvoidArea(TYPE_SYSTEM).bottomRect.height` 在本机返回 0：PC 的 Dock 是
  浮层，不在 avoid area 里，因此按 64 px 经验值兜底。
- 解法：`fitWindow` 递归回读 `windowRect`，按「底边超出可用高度」的差值收缩，最多 3 次，
  最后按可用区居中。
- 结果：窗口 1316×694 at (25,10)，底边 704 正好等于 Dock 顶，对话框不再被裁。

## 7. 未解决 / 已知问题

1. **「+ 新建会话」按钮被压成细条**：快速连接 header 右端的 PrimaryButton 实测只有
   ~28 px 宽（蓝色像素区间 `x 1209..1224 @ y=240`，卡片内边界 1225），标签被裁成白色
   碎片；点击区域仍在，能打开对话框。同一 `PrimaryButton` 在对话框 footer 正常
   （约 90 px），故怀疑是该行 `HorizontalLayout` 在软件渲染后端的挤压/溢出，待单独排查。
2. **侧栏在当前窗口宽度下呈折叠态**（`root.compact` 判定），会话分组以折叠入口呈现。
3. **「移动到」二级子菜单未点开验证**（需要第二级菜单点击；分组归属已由新建时
   的 `default` 分组间接验证）。
4. 串口端口列表为空、测试连接 / 私钥选择 / RDP 预览均为降级提示（见 §2）。
5. Dock 为浮层，窗口高度依赖经验常量 64 px；若换设备需要重新测量。

## 8. 下一步建议

1. ~~真实 SSH 连接（`russh` + PTY + `connect-session`）~~ → 已完成，见 §9。
2. ~~中文 IME（`inputMethod` 引擎对接，替换当前仅 `keyText` 的通路）~~ → 已完成，见 §10。
3. ~~隐藏系统标题栏 + 自定义标题栏拖拽（`startMoving`）/ 双击最大化~~ → 已完成，见 §11。
4. 端口转发 / 触发器编辑页（`dialog_forwards` / `dialog_triggers` 目前为空模型）。
5. 排查 §7-1 的按钮挤压（可用 `header-row` 的宽度与 preferred-width 对比定位）。

## 9. 真实 SSH 会话（已完成）

新增文件 `ohos/meatshell-slint/src/ssh_session.rs`、`terminal.rs`，改动
`ohos/meatshell-slint/src/ohos_app.rs`、`Cargo.toml`；上游源码未动。

- `ssh_session.rs`：每会话一个 `meatshell-ssh` 线程 + current-thread tokio runtime，
  `russh 0.49` 连接 → 三种认证（Password / KeyboardInteractive / Key）→
  `channel_open_session` → `request_pty("xterm-256color", cols/rows)` → `request_shell`；
  输出经 `Arc<Mutex<Vec<Event>>>` 交给 UI 线程，`Command::{Data,Resize,Close}` 回传键盘与
  `window_change`。主机密钥当前信任全部（known_hosts 待接）。
- `terminal.rs`：`vt100` 解析 → `[TermSpan]`（逐 cell 取 fg/bg/bold/underline/inverse，
  ANSI16 + 256 色调色板，宽字符 2 cells，跳过默认样式空白），并移植上游
  `key_to_pty_bytes`（方向键/Home/End/PageUp/Down/Delete/F1-F12/Shift+Tab→`ESC[Z`/
  Backspace→0x7f/Enter→0x0d/Ctrl 映射/Alt 前缀 ESC）。
- `ohos_app.rs`：`connect_session`（标签页 + `TerminalState` 行 + `TermTab` 注册表）、
  `on_send_key`、`on_terminal_resize`（换算 cols/rows，`screen_mut().set_size()` +
  远端 `window_change`）、`remove_terminal`、16 ms `slint::Timer` 轮询 `poll_sessions`。

### 关键缺陷：Slint 平台时钟

`OhosPlatform::duration_since_start` 原实现为 `Instant::now().elapsed()`，恒返回 ~0。
Slint 用该值计算定时器/动画的到期时间，于是 **所有 `slint::Timer` 与动画永不触发**：
SSH 已连通（sshd 日志有 `Accepted publickey`、`session_pty_req`、`req shell`、
`req window-change`），但 UI 状态一直停在「正在连接…」，终端全黑。改为进程启动锚点
（`static START: OnceLock<Instant>`）后立即恢复正常。

### 真机验证（192.0.2.145:10178，用户态 sshd 2222 + `hdc rport`）

- 点击会话卡片 → 状态「已连接 dev@127.0.0.1:2222」，终端显示 `Last login:` 与
  zsh 提示符、块状光标。
- 键盘注入 `uitest uiInput keyEvent 2021 2019 2024 2031 2050 2025 2054`→ 终端回显
  `echo i` 并输出 `i`；`keyEvent 2072 2019`（Ctrl+C）在空提示符处换出新提示符，说明
  Ctrl 修饰键通路正确。
- 错误输出（`zsh: command not found: gr`）正常渲染，说明 stderr 走同一通道。

尚未验证：窗口缩放后的重排（点击外部标题栏按钮时应用被系统关闭，需换用可恢复的
方式重测）、滚动回看、鼠标上报（htop 类应用）。

## 10. 中文输入法接入（已完成）

XComponent 是 SURFACE，不是 ArkUI 文本框，系统不会自动把它当输入法客户端，
所以 `onKeyEvent` 只能拿到物理键的 `keyText`（ASCII），中文上屏完全收不到。
本轮改成 **手动 attach 到 `InputMethodController`**，自己接收 IME 指令。

### 改动

| 位置 | 改动 |
| --- | --- |
| khmeatshell `SlintSpike.ets` | `attachIme()` / `subscribeIme()` / `detachIme()`；surface attach 后 300 ms 尝试挂载，失败重试 3 次，点击时补挂；`aboutToDisappear` 时 detach |
| khmeatshell `napi_init.cpp` | 新增 `slintImeCommit` + `CommandKind::Ime`（IME 文本必须和按键一样经渲染线程队列，不能跨线程直调） |
| khmeatshell `types/libmeatshell/Index.d.ts` | `slintImeCommit(text: string): number` |
| `ohos/meatshell-slint/src/lib.rs` | FFI `ms_spike_ime_commit`（C 字符串 → UTF-8 校验 → 投递给 UI 线程） |
| `ohos/meatshell-slint/src/ohos_app.rs` | `UiState::ime_commit`：把上屏文本作为 Slint `CommitComposition` 键事件（与 winit 后端的 `Ime::Commit` 同形）派发 |

通路：

```
系统 IME（开鸿输入法）
  └─ insertText(text)                     ← ArkTS，attach 成功后订阅
       └─ slintImeCommit → 渲染线程队列 → ms_spike_ime_commit
            └─ WindowEvent::internal(KeyEventType::CommitComposition)
                 └─ 上游 terminal_view.slint 的隐藏 TextInput：插入 text → edited
                      └─ send-key → PTY（多字节 UTF-8 原样发送）
```

其它 IME 指令映射：`deleteLeft(n)`→Backspace×n、`deleteRight(n)`→Delete×n、
`sendFunctionKey`→Return、`moveCursor`→四个方向键；`getLeftTextOfCursor` /
`getRightTextOfCursor` / `getTextIndexAtCursor` 返回空/0（终端不向上文候选提供上下文）。

`TextConfig`：`MULTILINE` + `NEWLINE`、`windowId`、`cursorInfo`（候选窗锚点 =
`windowRect` 左上 + `getWindowDecorHeight()` + 最后一次鼠标位置），
`attach(false, …)` 即不弹软键盘（PC 用物理键盘）。

### 真机验证（`192.0.2.145:10178`）

准备工作：本机（Mac）用户态 sshd 用 `/tmp/kh_sshd/sshd_config`
（Port 2222、私钥认证、`AllowUsers dev`），私钥推到
`<filesDir>/id_ed25519`（真实路径是
`/data/app/el2/100/base/com.kaihong.khmeatshell/haps/entry/files/`），
`hdc rport tcp:2222 tcp:2222`。

| 用例 | 注入 | 结果 |
| --- | --- | --- |
| attach | 冷启动后 300 ms | `hilog` 有 `ime attached`，无 `ime subscribe` 报错（订阅必须放在 attach 的 then 里，否则报 `input method client detached. need to be attached first`） |
| 英文不受影响 | `hello` 逐键 `uitest uiInput keyEvent` | 终端只回显一次 `hello`、`command not found`，说明 attach 不会吞掉物理键也不会重复 |
| `text` 注入英文 | `uitest uiInput text 'clear; echo hi'` | 清屏并输出 `hi`（英文模式下正常） |
| 拼音上屏 | 中文模式下 `uinput -K -d/-u` 依次 `n i h a o` | IME 候选条显示 `ni'hao` 与「你好」等候选，终端未收到半成品拼音 |
| 提交 | `uinput -K -d 2050 -u 2050`（空格） | `hilog`：`ime insertText len=2`；终端提示符出现 `你好` |
| 删除 | `uinput -K -d 2055 -u 2055`（Backspace） | 「你好」→「你」（非组合态走原生物理键通路） |
| 空行回车 | `keyEvent 2054` | 正常换出新提示符，无多余字符 |

注意事项（测试/使用层面，不是缺陷）：

1. 中文模式下用 `uitest uiInput text` 注入 ASCII 时，字符会先被 IME 当拼音吃掉，
   回车后上屏成中文/乱码；真实使用应在英文模式下输入命令。
2. 候选窗位置跟随最后一次鼠标点（终端不暴露光标格坐标），不跟随提示符。
3. IME 组合中的拼音只显示在候选条，不预先画在终端里（避免半成品进 PTY）。
4. 中/英切换由系统输入法决定（开鸿输入法的悬浮条/设置），应用不干预。

### 顺带修复：启动首帧的 “buffer too small” 横幅

启动时 XComponent 先报 `1316x680`（`slintAttach` 用的就是这个尺寸），约 250 ms 后
`onSizeChange` 才回到真实内容高度（`1316x659`/`663`）。这期间窗口缓冲区还是按旧几何
分配的，渲染循环发现 `handle->size < stride*height` 就中断整帧，并把错误写进
`SetBridgeError` → ArkTS 的红色错误条**一直留在界面上**（错误字符串没人清理），
看起来像 bug。现在改为：这种情况只 `fprintf(stderr)` 记录并丢弃这一帧
（下一帧尺寸对齐后自然恢复），不再显示到 UI。真机重启后横幅消失、终端渲染正常。

另外确认：attach 之后**所有可打印字符都由 IME 的 `insertText` 送达**（英文模式也一样），
`uitest uiInput text` 里的 `-`、`/`、`;` 都能通过这条通路进入 PTY，且不会与
`onKeyEvent` 重复；Ctrl/方向键等无文本的键仍走物理键通路。

### 测试用 sshd 的重建步骤

`/tmp` 会在重启后清空，重新搭时：

```sh
mkdir -p /tmp/kh_sshd && cd /tmp/kh_sshd
ssh-keygen -q -t ed25519 -N '' -f host_key
ssh-keygen -q -t ed25519 -N '' -f id_ed25519
cp id_ed25519.pub authorized_keys
chmod 600 authorized_keys host_key id_ed25519
# sshd_config: Port 2222 / ListenAddress 127.0.0.1 / HostKey / AuthorizedKeysFile
#              PubkeyAuthentication yes / PasswordAuthentication no / UsePAM no
#              PermitRootLogin no / AllowUsers dev / StrictModes no
/usr/sbin/sshd -D -f /tmp/kh_sshd/sshd_config -E /tmp/kh_sshd/sshd.log   # 以 dev 用户跑
```

私钥放错目录会静默用旧密钥连接（表现为「私钥认证被拒绝」），务必落在
`haps/entry/files/` 下。

## 11. 标题栏与窗口几何（已完成）

结论：**通过（真机闭环）**。系统装饰已隐藏，上游 `ui/app.slint` 自带的
38 px 自定义标题栏成为唯一标题栏；拖动、双击最大化/还原、最小化、关闭全部可用。

### 11-1 改动

| 文件 | 改动 |
| --- | --- |
| `ohos/meatshell-slint/src/ohos_app.rs` | 接 `ui.on_win_drag` → `pending_action = "drag"`；新增 `UiState::set_window_maximized`（`ui.set_window_maximized`）与 FFI `ms_spike_set_maximized` |
| khmeatshell `SlintSpike.ets` | `applyWindowAction` 增加 `drag → win.startMoving()`；maximize/recover 后 `meatshell.slintSetMaximized(next)`；`hideWindowDecor()` 用 `setWindowDecorVisible(false)`；`syncMaximizedState()` 用「窗口宽度≈屏幕宽度」判断（本机 `getWindowStatus()` 固定 801）；`onSizeChange` 里顺带校正图标 |
| khmeatshell `EntryAbility.ets` | `fitWindow` 越界判断从 `top + height` 改成「实际高度 > 可用高度」（旧写法在窗口停靠屏幕下方时会把高度误缩到 453）；启动时若已是整窗最大化则跳过改尺寸 |
| khmeatshell `napi_init.cpp` / `Index.d.ts` | 新增 `slintSetMaximized` 导出 |

### 11-2 真机验证

| 用例 | 注入 | 结果 |
| --- | --- | --- |
| 系统装饰 | 截图 + WMS 日志 | `UpdateWindowMode ... hasDecor: 0`；只有一条 Slint 标题栏 |
| 启动几何 | `aa force-stop` + `aa start` | `window [25, 0, 1316, 704]`，底边 704 = 可用高度，内容不再裁切 |
| 拖动 | `uitest uiInput drag 600 19 600 319` | 日志 `window action: drag` → `startMoving()`，窗口还原并随指针下移 |
| 双击最大化/还原 | `uitest uiInput doubleClick 600 316` | `maximize-toggle` → `OnRecover`；按钮图标随 `set_window_maximized` 切换 |
| 最大化按钮 | 点标题栏 (1272,19) | `OnMaximize`，窗口 `[0,0,1366,722]`，XComponent `1366x722`，画面铺满、图标变「还原」 |
| 还原按钮 | 点 (1297,19) | `OnRecover`，窗口回到 `[25,0,1316,704]`，XComponent 同步缩小并重绘 |
| 最小化 | 点 (1226,19) | `OnMinimize ... ret=0`，窗口 `visible=-1` |
| 关闭 | 点 (1318,19) | `terminate EntryAbility`，进程退出 |

### 11-3 本机限制与注入注意

- `getWindowDecorVisible()`、`getWindowStatus()`、`getWindowDecorHeight()` 在该
  Kaihong PC 上均返回 801（能力不支持）；`setWindowDecorVisible(false)` 本身有效，
  因此「是否最大化」只能用 `windowRect.width >= 屏幕宽度 - 4` 推断。
- `uinput -M -g x1 y1 x2 y2` 会在起点先产生一次完整点击，再做拖动；若起点正好在
  标题栏，会顺带触发 Slint 的 `double-clicked`（表现为拖完就最大化）。真机手动
  拖动不受影响；自动化请用 `uitest uiInput drag/doubleClick`。
- WM 会记住退出时的窗口矩形。若停留在屏幕下方，旧版 `fitWindow` 会把高度一路
  扣到 453 px；已改为按「高度 > 可用高度」收敛，并在整窗最大化时不干预。

## 12. 停靠区几何（已完成）与注入坐标修正

### 12-1 现象

窗口 1316×704，但欢迎页内容只画到 x=1225、底部被截断，右侧留下约 116 px 的
死区；「+ 新建会话」按钮被裁成一条蓝色竖条。

### 12-2 根因

`ui/app.slint` 的 `rest`（中央区）几何由 Rust 停靠引擎通过
`dock-central-x/y/w/h` 提供，默认值是 **1200×800**。本移植没有边栏栈，按设计
应把中央矩形设成整个停靠区，但此前从未设置过这四个属性，于是永远按 1200×800
布局：右侧 116 px 空带、底部溢出被窗口裁掉，标题行也因超宽被裁。

### 12-3 修复（`ohos/meatshell-slint/src/ohos_app.rs`）

- `UiState::new` 与 `UiState::resize` 中设置
  `dock-central = (0, 0, width, height - 38)`。
- 新增 `ui.on_dock_area_resized`：以 app.slint 上报的真实停靠区尺寸回写
  `dock-central`（阈值 0.5 px 防止重排回环）。本移植无边栏栈，中央区恒等于
  停靠区。

### 12-4 注入式输入的本机怪癖（`SlintSpike.ets`）

- `uinput -M -c 0/1`（注入点击）上报的 `MouseEvent.x/y` 恒为 `(0,0)`，会把点击
  打到窗口左上角。现仅在 `MouseAction.Move` 时更新悬停坐标，按键事件坐标
  `<= (1,1)` 时回退到最近一次悬停位置。真实鼠标不受影响。
- `uinput -M -s` 的轴向事件会投递两次，其中一次坐标是 `(0,0)`。现以带坐标的
  那一次为准并记住位置；同时移除 Stack 上重复的 `onAxisEvent`，避免双倍滚动。
- 结论：自动化点击前先 `uinput -M -g` 把指针放到目标位置，再注入按键。
- 补充（20:0x 实测修订）：`uinput -M -m X Y` 在指针处于 (0,0) 时会让 Move 事件
  携带真实坐标 (X,Y) 并真实移动指针，可用来给后续滚轮定位；`uitest uiInput
  click` 走触摸链路（`onTouch` → `slintPointer`），不经过 `onMouse`，因此不受
  上述 (0,0) 怪癖影响，坐标即屏幕绝对坐标。滚轮（`uinput -M -s ±N`）作用位置
  = 指针当前停留处：若指针停在对话框外（如 Welcome），滚轮滚的是 Welcome；
  操作对话框滚轮前需先在对话框内做一次触摸点击定位指针。

## 13. 会话编辑器端到端验证（已完成，20:19）

### 13-1 验证项与结果

- 新建会话（名称留空 → 自动 `user@host`；备注 p2btest；主机 127.0.0.1、
  端口 22、用户 root、密码 root）+ 高级区端口转发 1 行
  （fwd1 / local / 127.0.0.1:8080 → 127.0.0.1:80 / auto-start）+ 登录触发器
  1 行（期望 `Password`、回复 `hunter2`、发送 Enter）→「创建」→ `sessions.json`
  落盘正确：`forwards`、`triggers`（回复加密 `enc:v1:...`）、密码
  `enc:v1:...`、`mcp_access: true` 全部符合。
- 右键会话条目 → 上下文菜单（编辑/复制副本/删除/移动到）→「编辑」→ 编辑对话框
  **预填完全正确**（标题「编辑会话」、备注/主机/端口/用户名回填、密码显示
  「留空则不修改密码」、footer 为「保存」）→ 无修改保存 → json 无破坏。
- 校验文案：空主机保存 → 主机框红框 +「请输入主机 / IP 地址。」。

### 13-2 验证过程中的操作配方

- 填写输入框统一用 `uitest uiInput inputText x y text`（自带点击+输入）。
  **注意默认值陷阱**：端口框默认实值 `22`、转发 bind-addr 默认实值
  `127.0.0.1`——对它们 inputText 会**追加**而不是替换；需先点击聚焦后退格
  （`uitest uiInput keyEvent 2055` ×N）再输入。占位符（灰字，如目标主机/监听
  端口）可直接 inputText。
- footer 按钮点击坐标：对话框居中 440 宽（x 463..903），创建/保存按钮在
  footer 右侧，**收起高级区后**滚到底其位置恒为 (841, 663)（真机实测命中）。
- 触摸点击不产生 `onMouse` Press/Release 日志（走 onTouch）；排查点击失效时
  先确认指针/触摸坐标落在目标上，再确认目标是否被 Flickable 视口裁剪。

### 13-3 高级区展开时 footer 滚到底差约 30px（已修复，真机验证通过）

高级区展开 + 端口转发/触发器行较多时，滚轮滚到底（Slint Flickable clamp 到
`viewport-height - content-height`）后 footer 只露出约 8px，且该区域触摸不可
命中（Flickable 视口外）。收起高级区后 footer 完全可见可点。

根因：`content-height: body.preferred-height` 低估实际布局高度约 30px
（wrap 文本的 preferred 高度按单行计），导致 Flickable 的最大滚动距离不足，
footer 被视口底边裁住。上游桌面在 800px 高窗口同样触发，属上游 UI 缺陷。

修复（不改上游文件）：`ohos/meatshell-slint/build.rs` 把 `ui/` 复制到
`OUT_DIR/ui` 后仅对**副本**做字符串替换再编译：

```
content-height: body.preferred-height;
→ content-height: body.preferred-height
   + (body.preferred-height + 1px > self.height ? 32px : 0px);
```

- 仅在内容溢出视口时补 32px；折叠态表达式恒等（`+ 0px`），渲染不变。
- 若上游改掉这行绑定，`assert!` 让构建直接失败，不会悄悄失效。
- `ui/*.slint` 引用 `../assets/`，因此 build.rs 同时把 `assets/` 复制到
  `OUT_DIR` 保持兄弟相对位置（否则 `@image-url("../assets/icon.png")` 编译失败）。

真机验证（21:37–21:39，截图 `/tmp/g3..g8.jpeg`）：展开高级区（字符集/TERM/
端口转发/触发器全出）→ 滚到底 → footer 三按钮完整可见且底部有间隙 →
点「取消」(753, 672) 命中，对话框关闭回欢迎页；折叠态外观不变。

### 12-5 关于「部署后 sessions.json 消失」

排查结论：`deploy.sh` 只有 `hdc install -r`，不清数据。那次目录里只剩
`secret.key` / `sessions.json.lock`，是 12:36 应用数据目录被整体重建（等同重装）
导致的，不是保存逻辑缺陷；后续多次 `install -r` 后 `sessions.json`（3.3 KB）
均正常保留。

## 14. 图形 fence FD 泄漏（已修复，真机验证通过）

### 14-1 现象

点击 SSH 会话连接时报 `EMFILE (os error 24)`；`/proc/<pid>/fd` 计数 32767/32768，
内容几乎全是 `anon_inode:sync_file`，约 40 分钟（每帧一个）耗尽上限。

### 14-2 根因与修复

`napi_init.cpp` 的渲染循环里 `OH_NativeWindow_NativeWindowRequestBuffer` 交出的
`fence_fd` 从不关闭。NDK 头文件写明 fence 所有权在调用方，但进程内的 fd 需要自己
`close`（IPC 只把 fd 翻译给合成器进程，不会代关）。已补齐所有路径：
RequestBuffer 失败、GetBufferHandle 为空、buffer 太小、map 失败、render 空、
AbortBuffer、FlushBuffer 成功/失败。

### 14-3 验证

同一进程滚动对话框触发大量重绘后 fd 恒为 51（修复前每帧 +1）；测试会话连接错误
由 `EMFILE` 变为 `Connection refused`（本机无 SSH 服务），证明 fd 不再耗尽。

## 15. SSH 连接验证（已完成）

### 15-1 验证环境

Mac 侧起一个无需 sudo 的用户态 sshd（临时主机密钥 + 临时 authorized_keys、
`PasswordAuthentication no`），`hdc rport tcp:2222 tcp:2222` 把设备
127.0.0.1:2222 映射到它，应用侧用私钥认证。

### 15-2 结果

- 会话列表显示 `test 127.0.0.1:2222 dev` → 点击后状态栏「已连接
  dev@127.0.0.1:2222」，终端出现 `Last login: ...` 与 `zsh` 提示符
  `dev@Mac-mini ~ %`；注入 `ls` + 回车后真实回显远端文件列表，交互正常。
- sshd 侧日志确认公钥认证被接受并 `do_setup_env` 启动 shell。

### 15-3 发现的真实缺陷：私钥路径不可读 + 误导性提示

- 应用命名空间看不到 `/data/app/el2/.../haps/entry/files/...`（这是 hdc/文件
  管理器视角的路径），`read_to_string` 失败后得到空串，UI 只说「未保存私钥」。
- 应用内可见路径是 `/data/storage/el2/base/haps/entry/files/<file>`（即
  `context.filesDir`，也是 `MEATSHELL_DATA_DIR`）；改成该路径后立即连接成功。
- 已修：`ohos_app.rs` 读私钥失败时按文件名回落到 `MEATSHELL_DATA_DIR`；错误
  文案改为「私钥为空或路径不可读，请在会话编辑器里粘贴私钥」。
- 附带注意：`uitest uiInput inputText` 输入带 `/` 的长路径会被输入法吞字符并
  转大写，验证时不宜用它填路径，直接改 sessions.json 更可靠。

## 16. 终端中文显示 `?????`：CJK 字体回退（待实现）

现象：SSH 会话里 `ls` 输出的中文目录名在终端里渲染成 `?????`；英文/ASCII 正常。

### 16-1 根因（已定位到源码，未改）

1. 文本排版走 `sharedparley` → parley；`FontSelector::new`
   （`parley-0.11.1/src/shape/mod.rs:559`）调用
   `query.set_fallbacks(FallbackKey::new(script, locale))`。
2. 选字时 `Query::matches_with`
   （`fontique-0.11.1/src/collection/query.rs:121`）先遍历样式家族
   （终端是 `"Meatshell Mono"`），再遍历 **fallback families**。
3. OHOS 没有 fontconfig，fontique 的 system backend 在
   `fallback(selector)` 上返回 `None`
   （`collection/mod.rs:426-433`），所以 `fallback_families()` 永远是空迭代器。
4. 终端字体 `"Meatshell Mono"` 无 CJK 字形 → cluster 判定 `Discard` →
   没有回退家族可试 → 用 `.notdef` 绘制，显示成 `?????`。
5. 现有 `register_system_font()` 只调了 `set_generic_families`
   （SansSerif/SystemUi/…），**从未调用 `set_fallbacks`**，因此显式指定
   `font-family` 的 Text（终端、会话卡片名）拿不到中文回退。

### 16-2 修改方案（唯一改动点）

文件：`ohos/meatshell-slint/src/lib.rs`，函数 `register_system_font()`。

在现有 `set_generic_families` 循环之后、`return Ok(())` 之前追加：

```rust
// OHOS has no fontconfig, so fontique's system backend cannot answer
// `fallback(selector)`; `fallback_families(script)` stays empty and any
// glyph missing from the explicit family (the terminal uses
// "Meatshell Mono", which has no CJK coverage) is shaped as .notdef and
// drawn as `?`. Point the CJK scripts at the family registered above.
for script in [
    Script::from_bytes(*b"Hani"), // Han ideographs
    Script::from_bytes(*b"Hang"), // Hangul
    Script::from_bytes(*b"Hira"), // Hiragana
    Script::from_bytes(*b"Kana"), // Katakana
    Script::from_bytes(*b"Bopo"), // Bopomofo
] {
    let _ = collection.set_fallbacks(FallbackKey::new(script, None), families.iter().copied());
}
```

导入改为：
`use slint::fontique_011::fontique::{FallbackKey, GenericFamily, Script};`

要点（已核对 API 语义）：

- `FallbackKey::new(script, None)` → `canonical_locale` 返回
  `(is_default = true, is_tracked = true)`，即写入该 script 的 **默认**
  fallback 列表；`FallbackMap::get`（`fallback.rs:21-34`）对
  `is_default` 的查询直接返回该默认列表，所以带 `zh-Hans` 等 locale 的
  排版查询同样命中。
- parley 还会额外查询
  `FallbackKey::new(Script::from_bytes(*b"Hani"), None)`
  （`query.rs:104-111` 的 HACK，用于消解 Common 脚本标点），
  所以 Hani 这一条同时让全角标点也有字体。
- `Script::from_bytes` 用 4 字节 script tag，避免依赖 parlance 具体常量名。
- `families` 就是上面 `register_fonts` 返回的 `FamilyId` 列表；
  `HarmonyOS_Sans_SC.ttf` 命中的是单 family，`NotoSansCJK-Regular.ttc`
  会返回多个（SC/TC/JP/KR），全部进回退链，per-character 选字仍由
  charmap 覆盖度决定，不会把日文字形误用到汉字上。
- 不改上游 `meatshell` 源码，不改 C++/ArkTS。

### 16-3 验证步骤

1. `cd ~/DevEcoStudioProjects/khmeatshell && bash deploy.sh`
   （Rust 改动，后台构建，约 25–30 分钟）。
2. 设备重启应用，点击 `test` 会话（127.0.0.1:2222 / dev / 公钥）连接。
3. 在终端注入 `ls` + 回车，等远端输出。
4. `hdc shell "snapshot_display -f /data/local/tmp/cjk.jpeg"` +
   `hdc file recv /data/local/tmp/cjk.jpeg` 截图确认中文目录名是汉字而非 `?????`。
5. 回归：ASCII 输出、状态栏中文（本来就正常）、会话卡片中文名仍正常；
   `hilog` 无 panic / 无 `No font fallback found`。

### 16-4 风险与回退

- 若 `set_fallbacks` 在该共享 collection 上不生效（`shared` 模式下
  `sync_shared` 已处理，理论上生效），退路是在 `UiState::new` 里
  `set_var("SLINT_DEFAULT_FONT", …)` 之外再显式把 `"Meatshell Mono"`
  的 Text 家族改成 `font-family: "HarmonyOS Sans SC", "Meatshell Mono"`
  ——但那就需要改上游 slint 文件，违反硬约束，仅作最坏情况备选。
- 字体文件缺失时 `register_system_font()` 仍返回 `Err`，行为不变。

### 16-5 实测修订（重要：`?????` 的主因是远端 locale，不是渲染）

在设备上复测得到两个独立结论：

1. **字体回退修复有效**。给测试 sshd 注入 UTF-8 locale 后重连，`ls` 输出中的
   `iCloud云盘（归档）`、`启动PhotoCraft桥接.command` 已渲染为真实汉字
   （截图 `/tmp/cjk5.jpeg`），英文、配色、宽字符 2 cell 宽度都正常。
2. **`?????` 的真正来源是远端把字节替换掉了**。用
   `ssh -tt -p 2222 ... dev@127.0.0.1 "ls" | od -c` 查看 PTY 原始字节，得到
   `i C l o u d ? ? ? ? ? ? ? ? ? ? ? ? ? ? ? ? ? ?` —— **每个 UTF-8 字节一个
   `?`**（`云盘（归档）` = 18 字节 → 18 个 `?`，`启动` = 6 字节 → 6 个 `?`）。
   与截图中问号个数逐像素统计完全对上，说明应用收到的文本本身就是 `?`。
   原因是 sshd 会话默认继承 C locale，`ls` 判定非 ASCII 不可打印后打了问号；
   加 `LANG=en_US.UTF-8 ls` 后同样的 PTY 流变成原始 UTF-8。

因此补第二处修复（`ohos/meatshell-slint/src/ssh_session.rs`，`channel_open_session`
之后、`request_pty` 之前）：

```rust
for name in ["LANG", "LC_CTYPE"] {
    if let Err(err) = channel.set_env(true, name, "en_US.UTF-8").await {
        eprintln!("meatshell-ohos: set_env {name} rejected: {err}");
    }
}
```

- 对齐 OpenSSH 客户端的默认 `SendEnv LANG LC_*`；服务端需
  `AcceptEnv LANG LC_*`（OpenSSH 默认配置常见），否则请求被忽略、退回 C locale
  （此时终端里 `ls` 仍显示 `?`，属服务端配置，不是渲染缺陷）。
- 测试 sshd 已从 `SetEnv LANG=...` 改为 `AcceptEnv LANG LC_*`，用
  `ssh -o SendEnv=LANG` 验证过转发后字节流为原始 UTF-8。

验证要点：`od -c` 这类字节级检查能区分「渲染缺字形」与「输入文本已被替换」，
后续再遇到乱码类问题应先做这一步，避免误判为字体问题。

### 16-6 验收结果（真机通过，20:43）

部署 20:42 的包（含 `set_fallbacks` + `set_env LANG/LC_CTYPE`），测试 sshd 只保留
`AcceptEnv LANG LC_*`（**不含** `SetEnv`），应用冷启动后点 `test` 会话注入 `ls`：

- 终端显示 `iCloud云盘（归档）`、`启动PhotoCraft桥接.command`，汉字与 2 cell 宽度正常，
  无 `?`（截图 `/tmp/cjk6.jpeg`）。
- `hilog` 无 `set_env ... rejected`，说明服务端接受了环境请求。
- 回归：ASCII 文件名、`ls` 列对齐、`已连接` 状态栏、块状光标均正常。

结论：中文显示问题闭环 = ①字体回退（渲染侧）＋②会话 locale（输入侧），两者缺一不可。
服务端若未配置 `AcceptEnv LANG LC_*`，`ls` 之类的单字节 locale 输出仍会是 `?`，
这属于远端环境而非客户端缺陷。

## 17. 主机密钥校验（known_hosts + 交互式确认，已完成）

演进过程：`TrustAll`（无条件放行，等于放弃中间人防护）→ TOFU（未知主机静默记录、
不一致直接拒绝）→ **交互式确认**（对齐上游 `hostkey-prompt-open` /
`hostkey-accept` / `hostkey-reject`，#109-5）。

最终行为与原始软件一致：未知或变更的主机密钥都会弹出确认对话框，由用户决定，
客户端不替用户做信任决策。

### 17-1 实现

**`ohos/meatshell-slint/src/ssh_session.rs`**

| 类型 | 作用 |
| --- | --- |
| `HostKeyPrompt` | 对话框数据：`host / port / changed / key_type / fingerprint` |
| `HostKeyGate` | SSH 线程与 UI 线程的交接点。`pending()` 供 UI 每帧**只读**探测；`answer(bool)` 取走并唤醒；`ask()` 在 SSH 线程阻塞等待 |
| `HostKeyVerifier` | `check_server_key` 实现，字段 `host / port / known_hosts / gate / failure / learned` |

判定流程：

| 场景 | 行为 |
| --- | --- |
| 已知且一致 | `Ok(true)` 静默通过，不弹框、不写盘 |
| 未知主机 | `gate.ask(changed=false)` → 用户「信任并继续」→ `learn_known_hosts_path` 写入 + 连接继续；拒绝 → 中止 |
| 已知但不一致 | `gate.ask(changed=true)` → 用户「仍要连接」→ 先 `forget_known_host()` 删除 `host`/`[host]:port` 旧条目再 `learn()` 写入新密钥；拒绝 → 中止 |
| `known_hosts` 读写失败 | `Ok(false)` + 记录 IO 原因 |

- `ask()` 最长等待 **120 s**（UI 每 16 ms 轮询，实际只等人工操作）；超时后
  `Ok(false)`，错误文案「等待确认 host:port 的主机密钥超时，连接已取消。」
- `forget_known_host()` 按 OpenSSH 语义匹配：非 22 端口 `[host]:port`，22 端口同时
  匹配裸 `host`；支持首字段逗号分隔的多 pattern；跳过空行与 `#` 注释。
- russh 把拒绝统一报成 `UnknownKey`，因此 `connect().map_err` 优先取 verifier 自己
  记录的中文原因，否则用户只看到无信息量的 `UnknownKey`。
- `known_hosts` 路径 = `MEATSHELL_DATA_DIR/known_hosts`。

**`ohos/meatshell-slint/src/ohos_app.rs`**

- `TermTab.gate: Arc<HostKeyGate>`，`connect_session()` 建 gate 并随 `SshSession::spawn` 传入。
- `poll_sessions()` 每帧调用 `surface_hostkey_prompt()`：取任一 tab 的 `pending()`，
  按 `changed` 填上游属性 `hostkey-title / hostkey-message / hostkey-detail /
  hostkey-confirm-label / hostkey-changed`，再 `hostkey-prompt-open = true`。
- `on_hostkey_accept` / `on_hostkey_reject` → `answer_hostkey_prompt()` → 关闭对话框
  并 `request_redraw()`。

文案：

| 场景 | 标题 | 确认按钮 |
| --- | --- | --- |
| 未知 | 未知主机密钥 | 信任并继续 |
| 变更 | 主机密钥已变更（提示可能存在中间人攻击） | 仍要连接 |

详情区固定三行：`主机: host:port` / `密钥类型: ssh-ed25519` / `指纹: SHA256:…`。

### 17-2 真机验证（5/5 通过）

| # | 用例 | 结果 |
| --- | --- | --- |
| 1 | 首次连接（删除 `known_hosts`） | 弹出「未知主机密钥」：正文「首次连接 127.0.0.1:2222，请核对指纹后再决定是否信任。信任后将写入本地记录。」，详情 `主机 / 密钥类型 ssh-ed25519 / 指纹 SHA256:SM0f…jtw`，按钮「取消 / 信任并继续」。点「信任并继续」→ 对话框消失，终端出现 `[meatshell] 已记录 127.0.0.1:2222 的主机密钥 SHA256:SM0f…jtw` + `Last login` + 提示符；磁盘写入单条 OpenSSH 条目 |
| 2 | 二次连接（已知且一致） | 直接 `Last login` + 提示符，**无对话框**，文件不新增条目 |
| 3 | 轮换 hostkey 后连接 | 弹出「主机密钥已变更」：正文提示可能存在中间人攻击，指纹变为 `SHA256:JoGm…Da8`，按钮「取消 / 仍要连接」 |
| 3a | 上一步点「取消」 | 连接中止；标签页状态栏与终端红字「主机密钥校验失败：127.0.0.1:2222 与 known_hosts 第 2 行记录不一致（SHA256:JoGm…），已按用户选择取消连接。」；`known_hosts` 保持旧条目不变 |
| 3b | 变更后点「仍要连接」 | 连接成功；`forget_known_host()` 先删旧条目再写入，文件只剩 1 条新密钥（无重复行），终端提示新指纹 |
| 4 | 恢复原 hostkey 再连 | 再次识别为「变更」（SM0f…），点「仍要连接」→ 连接成功，文件回到单条原密钥 |

截图 `/tmp/k20..k28.jpeg`；sshd 为本地用户态实例（`/tmp/sshtest`），
`hdc rport tcp:2222 tcp:2222` 供设备侧访问。

真机确认的两个交互细节：

- 对话框宽度固定 380 px，指纹会按 `word-wrap` 折行（详情区是等宽字体），仍可完整核对。
- 点击卡片外的暗色背景**不会**取消（上游 `dismiss-on-backdrop: false`，#152），
  必须显式点「取消」或「信任并继续」，避免误触把连接掐断。

### 17-3 已知小瑕疵

`russh-keys` 的 `learn_known_hosts_path` 以追加模式写入时会在条目**前**补一个空行，
于是文件首行是空行、真实条目落在第 2 行——OpenSSH 容忍空行，功能无影响。
交互式流程里已由 `forget_known_host()` 在「变更」分支重写文件，不再累积空行。

## 18. 凭据补全 + 键盘交互 MFA（已完成）

上游在连接阶段还有两种必须问用户的情况，之前本移植都直接失败或盲答：

- `cred-prompt-*`（#110）：会话没存用户名，或密码认证没存密码时，先弹「登录」框补全，
  勾选「记住」则写回会话。
- `mfa-prompt-*`（#86-MFA）：跳板机 / 堡垒机用 `keyboard-interactive` 下发验证码挑战，
  必须让用户输入；此前 `Auth::KeyboardInteractive` 是把保存的密码**原样回答每一轮**，
  真实 MFA 场景必然失败。

### 18-1 实现

问题在于两个提示都发生在 SSH 线程的认证过程中，而 UI 在另一线程。沿用 §17 的
「park + 轮询」模式，把 `HostKeyGate` 泛化为 `PromptGate`，一个连接同一时刻只有一个
待答提示（与上游 ModalKind 的互斥语义一致）。

**`ohos/meatshell-slint/src/ssh_session.rs`**

| 类型 | 作用 |
| --- | --- |
| `Prompt::{HostKey, Credentials, Mfa}` | UI 每帧探测到的待答提示 |
| `CredentialPrompt` | `session_id / host / port / user / need_user / need_password` |
| `CredentialReply` | 用户填的 `user / password / remember` |
| `MfaPrompt` | `host / prompt / echo`（`echo=false` 时输入框以密码方式显示） |
| `PromptGate` | 单槽 mutex：`pending()` 只读探测，`answer_*()` / `cancel_*()` 取走并唤醒，`ask*()` 在 SSH 线程阻塞（120 s 超时后清槽） |

连接流程：

1. `shell_loop` 先做 `resolve_credentials`：`user` 为空 → `need_user`；
   `Auth::Password` 且密码为空 → `need_password`。任一为真就 `ask_credentials`，
   用户取消则错误「已取消连接：未提供 host:port 的登录凭据。」。
2. 认证阶段 `Auth::KeyboardInteractive` 改为 `keyboard_interactive_auth()`，
   对齐上游 `keyboard_interactive_auth`：**第一轮非 MFA 提示**用保存的密码自动回答，
   其余提示（`looks_like_mfa()` 命中或密码已用过）走 `ask_mfa` 弹框。
   `looks_like_mfa()` 复制上游关键词表（code / otp / mfa / duo / token / 验证码 / 动态 …）。
3. 轮次上限 16，超出报「键盘交互认证轮次过多，已中止」。

**`ohos/meatshell-slint/src/ohos_app.rs`**

- `TermTab.gate: Arc<PromptGate>`；`connect_session()` 传入 `session.id`（用于「记住」落盘）。
- `poll_sessions()` 每帧 `surface_prompts()`，按提示类型填上游属性并置
  `hostkey-prompt-open` / `cred-prompt-open` / `mfa-prompt-open`；没有待答提示时三个都归零。
- 新回调：`on_cred_accept` / `on_cred_reject` / `on_mfa_submit` / `on_mfa_cancel`。
  凭据「记住」分支复用 `ConfigStore::get/upsert/save` 写回会话，然后
  `refresh_sessions()` 让列表立即显示新的用户名。

### 18-2 真机验证（5/5 通过）

会话 `mfa`：`127.0.0.1:2222`、用户名为空、认证「键盘交互」、密码 `******`。

| # | 用例 | 结果 |
| --- | --- | --- |
| 1 | 无用户名连接 | 弹出上游「登录」框：标题 `登录`、主机 `127.0.0.1:2222`、仅一个「用户名」输入框（占位 `root`）、「仅本次会话记住」勾选框、`取消 / 连接`。**没有**密码框，符合 `need_password=false` |
| 2 | 填 `dev` + 勾选记住 → 连接 | 无第二个密码框：`Password: ` 挑战用已存密码自动回答；随后弹「双重验证 (MFA)」：显示服务器原文 `Verification code:`、本地标签「验证码」、`取消 / 验证` |
| 3 | 输入 `123456` → 验证 | 连接成功：状态栏「已连接 dev@127.0.0.1:2222」，终端 `[mfad] MFA accepted, welcome!` + `mfad$` |
| 4 | 「记住」落盘 | 返回欢迎页，`mfa` 卡片第三列由空变为 `dev`；`sessions.json` 21:19 重写（4738 B） |
| 5 | MFA 取消 / 错误验证码 | 取消 → 「连接失败：已取消 127.0.0.1:2222 的双重验证，连接已中止。」；输入 `999999` → 「连接失败：dev@127.0.0.1:2222 键盘交互认证被拒绝」 |

回归：恢复 OpenSSH 测试服后，原私钥会话 `dev@127.0.0.1` 仍静默连接成功，无任何对话框。

截图 `/tmp/m8..m17.jpeg`。

### 18-2b 已知小瑕疵

凭据框补全用户名后立即连上的那一次，标签页状态栏仍显示「已连接 @127.0.0.1:2222」
（`TermTab.label` 在弹框前按会话里保存的用户名生成，此时为空）。勾选「记住」后下次
连接即正常显示 `dev@…`；不勾选则每次都会显示为空用户。上游用生效后的用户名，
如需对齐需把解析后的用户名回传 UI 线程更新 label。

### 18-3 测试服务器

`keyboard-interactive` 需要用服务端真实下发挑战才能验证，OpenSSH 用户态实例无法
（PAM 不生效）。故写了一个仅测试用的 russh 服务端（`/tmp/mfad`，不计入仓库）：
第一轮 `Password: `（不回声），第二轮 `Verification code: `（回声），密码 `******`、
验证码 `123456`；复用 `/tmp/sshtest/hostkey`，因此客户端 `known_hosts` 已知该主机，
不会先弹主机密钥框，能单独验证 MFA 链路。

## 19. 真实 VPS 端到端验证

用一台真实 Ubuntu 24.04 服务器（公网 IP `203.0.113.10`，端口 22，root + 密码认证）
替换本地测试服，验证「新建会话 → 主机密钥确认 → 密码登录 → 交互」全链路。

| # | 步骤 | 结果 |
| --- | --- | --- |
| 1 | 新建会话 `vps`（SSH / 22 / root / 密码） | 创建成功，跳板机预览行显示 `root@203.0.113.10:22` |
| 2 | 首次连接 | 弹「未知主机密钥」，密钥类型 `ssh-ed25519`，指纹 `SHA256:<redacted>` |
| 3 | 与服务器独立核对指纹 | `ssh-keyscan 203.0.113.10 \| ssh-keygen -lf -` 输出完全一致（密码未出现在任何命令或文件中） |
| 4 | 点「信任并继续」 | 写入 `known_hosts`（`203.0.113.10 ssh-ed25519 AAAA…`），密码登录成功；Ubuntu 欢迎页与 `root@vps-host:~#` 提示符正常，状态栏「已连接 root@203.0.113.10:22」 |
| 5 | 交互 | `uptime` 返回 `13:24:55 up 125 days, 9:53, 1 user, load average: 0.02, 0.01, 0.00`；`uname a`、`whoami hostname id` 等错误命令的回显与报错也完整 |
| 6 | 对写入的密钥做二次校验 | 把 `known_hosts` 中的条目交给 `ssh-keygen -lf`，指纹仍为 `SHA256:X74z…DVNw`，与第 2 步显示一致 |
| 7 | 再次连接 | 无任何对话框，直接 `Last login` + 提示符（已知主机静默通过） |

截图 `/tmp/v1..v14.jpeg`。

注入注意事项（仅影响自动化，不是应用缺陷）：`uitest uiInput inputText` 只接受
`[A-Za-z0-9.]`，`_`、`-`、`;`、`/`、中文都会被吞掉，因此命令行验证用
`whoami`、`uptime` 这类纯字母命令；按下划线命名的问题在密钥文件名场景已用
「按文件名回落」绕过。

## 20. SFTP 面板

用户指令「sftp」：把上游的 SFTP 双栏文件面板接到 OHOS 移植版。上游 UI
（`ui/sftp_panel.slint` + `ui/app.slint` 的 `sftp-*` 属性/回调）已经随
`AppWindow` 原样编进来，当前只缺 Rust 侧数据与回调；`TerminalState` 里的
`sftp-*` 字段也已存在（默认空模型）。

### 20-1 目标与范围

本轮交付（Phase A）：

- 连接成功后自动打开本标签页的 SFTP 面板，显示远端家目录列表；
- 目录浏览：双击进入、`..`、`~`、路径栏输入、刷新；
- 左侧目录树展开/收起；
- 按名称 / 大小 / 修改时间排序（升降序，文件夹优先）；
- 多选与全选区间、已选计数；
- 下载单个 / 多选条目到应用沙箱 `files/downloads/`；
- 重命名、新建文件夹、新建空文件、删除（单个 / 多选）；
- 权限对话框（chmod）；
- 双栏本地视图：浏览沙箱目录，上传文件到远端当前目录，远端下载到本地当前目录；
- 书签 / 最近访问（内存态，重启不保留）。

Phase B（明确不在本轮，继续记录为差距）：隧道面板、内置文本查看/编辑器、
OS 拖拽导出、系统剪贴板上传、OS 默认程序打开、跳板机 / 代理下的 SFTP。

### 20-2 关键决策：复用上游 SFTP 引擎

`ohos/meatshell-core` 已经用 `#[path]` 原样编译了 `src/sftp/**`（含
`spawn_sftp`、`SftpCommand`、`DownloadConflict`），上游 SFTP 也是**独立 SSH
连接**（注释见 `src/sftp/impls/sftp.rs:150-156`），与壳会话互不影响。因此：

- 不重写协议层，直接调用 `spawn_sftp(&runtime_handle, session, vec![], tx)`；
- 上游用 `pub(crate) use` 导出，`meatshell-slint` 看不到。新增一个**只做
  re-export** 的 facade（不改上游一行）；
- 复用上游的 `format_size` / `format_mtime` / `format_permissions`，保证与
  桌面端显示完全一致；不再引入 chrono 依赖。

### 20-3 文件与改动点

| 文件 | 改动 |
| --- | --- |
| `ohos/meatshell-core/src/sftp_facade.rs` | 新增。`pub use crate::sftp::{spawn_sftp, DownloadConflict, SftpCommand, SftpHandle};` 与 `pub use crate::ssh::{format_mtime, format_permissions, format_size, RemoteEntry, RemoteTreeNode, SessionEvent};` |
| `ohos/meatshell-core/src/lib.rs` | 新增 `#[path = "sftp_facade.rs"] pub mod sftp_facade;` |
| `ohos/meatshell-slint/src/sftp_client.rs` | 新增。见 20-4 |
| `ohos/meatshell-slint/src/lib.rs` | 新增 `mod sftp_client;` |
| `ohos/meatshell-slint/src/ohos_app.rs` | `TermTab` 增加 `sftp`/`session_config`/`bookmarks`/`recent`/`home`；连接成功后启动 SFTP；新增 `poll_sftp`、事件映射、~25 个回调 |

不改 `Cargo.toml`：`russh-sftp`、`tokio` 已通过 `meatshell-core` 进入依赖树。

### 20-4 `sftp_client.rs`：线程与事件

```rust
pub struct SftpClient {
    commands: tokio::sync::mpsc::UnboundedSender<SftpCommand>,
    events: Arc<Mutex<tokio::sync::mpsc::UnboundedReceiver<SessionEvent>>>,
    alive: Option<tokio::sync::oneshot::Sender<()>>,
}

impl SftpClient {
    pub fn spawn(session: Session) -> Self;   // 独立线程 + current_thread runtime
    pub fn drain(&self) -> Vec<SessionEvent>; // UI 每帧 try_recv
    pub fn send(&self, command: SftpCommand);
}
impl Drop for SftpClient {
    fn drop(&mut self) { self.send(SftpCommand::Close); self.alive.take(); }
}
```

线程体：
`spawn_sftp(&runtime.handle().clone(), session, Vec::new(), tx)` 之后
`runtime.block_on(async { let _ = rx.await; tokio::time::sleep(150ms).await; })`，
`rx` 由 `SftpClient::alive` 持有；UI 丢弃标签页 → Drop → Close → 线程退出。

**必须回答的提问**：上游 `run_sftp` 会把 `HostKeyPrompt` / `CredentialPrompt` /
`MfaPrompt` 连同 `responder` 一起塞进事件队列。UI 不处理这些分支时，事件值
被丢弃 → oneshot 断开 → 上游按「取消」处理，不会挂死。由于壳会话已把主机密钥
写入同一个 `$MEATSHELL_DATA_DIR/known_hosts`（`src/ssh/impls/known_hosts.rs:29`）
且凭据已保存在会话里，正常路径不会触发；键盘交互（MFA）会话的 SFTP 会在状态栏
显示认证取消，记入差距。

### 20-5 事件 → UI 映射（`poll_sftp`）

| 上游事件 | Slint 行字段 |
| --- | --- |
| `SftpEntries { path, entries }` | `sftp_path`、`sftp_entries`（排序后）、`sftp_loading=false`、`sftp_selected_count=0`、`sftp_available=true`；首次记录 `tab.home` |
| `SftpStatus(text)` | `sftp_status`；`text` 含「加载」时 `sftp_loading=true` |
| `SftpError(text)` | `sftp_status=text`、`sftp_loading=false` |
| `SftpTreeUpdate(nodes)` | `sftp_tree_nodes` |
| `SftpTransfer { name, is_upload, transferred, total, state, msg, .. }` | `sftp_status = "↑/↓ name done/total msg"` |
| `SftpFileText` / `SftpDownloadInstead` | `SftpDownloadInstead` 转成下载命令；其余只更新状态 |
| `LocalFilesChanged` | 本地栏正在显示该目录时刷新 |
| 三类 `*Prompt` | 丢弃（见 20-4），状态栏提示「SFTP 需要交互确认」 |

条目映射与 `src/app/session_event.rs:228-258` 一致：`size`（目录留空）、
`size_bytes`、`modified`、`modified_ts`、`mode = e.mode & 0o7777`、
`file_type` 中文名、`permissions = format_permissions(e.permissions_mode)`、
`owner/group` 为空串（上游还做 getent 补全，本轮直接留空）、`selected=false`。

排序：文件夹优先，其次按 `sort_key`（name/size/modified）与 `sort_dir`
（0 默认 = name 升序）。排序只重排现有模型并保留 `selected`。

### 20-6 回调接线（`wire_callbacks`）

| 回调 | 动作 |
| --- | --- |
| `sftp-navigate(tab, path)` | `resolve_sftp_path`（`..`/`~`/绝对路径）→ `ListDir` |
| `sftp-refresh(tab, path)` | `RefreshDir(path 或当前路径)` |
| `sftp-tree-expand(tab, path)` | `ToggleTreeNode(path)` |
| `sftp-toggle-select` / `sftp-select-range` | 修改模型 `selected` 并重算 `sftp_selected_count` |
| `sftp-sort-request` / `sftp-clear-sort` | 重排模型并写 `sftp_sort_key/dir` |
| `sftp-delete` / `sftp-delete-selected` | `Delete(path)`（多选逐条） |
| `sftp-download` | `Download { remote, local_dir: 沙箱 downloads, conflict: KeepBoth }` |
| `sftp-download-selected` | `DownloadArchive { remote_dir, names, local_dir }` |
| `sftp-prompt-submit(tab, kind, target, value)` | `rename`→`Rename`；`mkdir`→`MkDir`；`touch`→`TouchFile`；`chmod`→`Chmod`；关闭对话框 |
| `sftp-chmod-open` / `sftp-chmod-apply` | 用 `mode` 拆 9 个 bool；apply 读 bool 合成 mode → `Chmod` |
| `sftp-open-file` / `sftp-view` / `sftp-edit` | 统一降级为下载到沙箱 |
| `sftp-toggle-bookmark` / `remove-bookmark` / `clear-recent` / `places-refresh` | 更新 `tab.bookmarks/recent` 与行字段 |
| `sftp-toggle-dual` / `sftp-local-*` | 本地栏：`list_local_dir`（沙箱根，`..`/`~`），选中计数 |
| `sftp-local-upload` / `sftp-local-upload-selected` | `Upload { local, remote_dir: 当前远端路径, cleanup_after: None }` |
| `sftp-download-to-local` / `sftp-download-selected-to-local` | `Download`/`DownloadArchive` 到本地栏当前目录 |
| `sftp-copy-to-target` / `copy-selected` | 取目标标签页 `SftpClient.commands.clone()` → `CopyTo` |
| `sftp-open-external` / `edit-external` / `drag-out` / `paste-upload` / `copy-path` / 隧道回调 | 状态栏说明「OHOS 暂不支持」，不崩溃 |

连接与生命周期：`connect_session` 里先写 `sftp_available=true`、
`sftp_panel_height=220.0`、`sftp_status="正在连接 SFTP…"`；等 `SshEvent::Connected`
后在 `poll_sessions` 中调用 `spawn_sftp_for_tab`（此时主机密钥已固定、凭据已验证，
避免首次连接双弹框）。会话断开时 `tab.sftp = None`。

### 20-7 明确降级

- MFA / 键盘交互会话：SFTP 会取消（需要第二次验证码，本轮无第二个输入通道）；
- 本地栏只能浏览应用沙箱（OHOS 无文件选择器、无通用存储权限）；
- 无内置编辑器 / 外部程序 / 拖拽 / 剪贴板上传；
- 上传 / 下载进度只体现在状态栏，不建上游「传输列表」窗口；
- 隧道标签页保持空列表（属于后续「端口转发」任务）。

### 20-8 验收矩阵（真机 + 真实 VPS `203.0.113.10`）

| # | 步骤 | 期望 |
| --- | --- | --- |
| 1 | 连接 `vps` | 终端正常；底部出现 SFTP 面板，状态为 `/root`，列出家目录 |
| 2 | 展开左侧树 `/etc` | 出现子目录节点，再点收起 |
| 3 | 双击 `..` / 点 `~` / 路径栏输入 `/tmp` | 列表与路径栏同步 |
| 4 | 点表头按大小、修改时间各排一次 | 顺序变化，文件夹始终在前 |
| 5 | 勾选 2 个文件 | 工具条显示「已选 2」 |
| 6 | 下载选中 | 状态栏出现 ↓ 进度，结束后 `files/downloads/` 出现两个文件 |
| 7 | 右键/菜单重命名、新建文件夹、新建文件 | 列表刷新且远端变化（终端 `ls` 复核） |
| 8 | 权限对话框把 644 改成 600 | 列表权限列变为 `-rw-------` |
| 9 | 删除刚建的文件 | 列表消失，终端 `ls` 复核 |
| 10 | 双栏：上传沙箱里的 `known_hosts` 到 `/tmp` | 远端 `/tmp/known_hosts` 出现 |
| 11 | 双栏：把远端文件下载到本地栏 | 沙箱对应目录出现文件 |
| 12 | 回归：关闭面板 / 收起 / 展开；终端输入与三类连接期对话框 | 均不受影响 |

截图命名 `s1..sN.jpeg`，结果补写到本节。

### 20-8-1 验收结果（真机 Kaihong / 1366×768 + VPS 实机）

| # | 结果 | 证据 |
| --- | --- | --- |
| 1 | 通过 | 重连 `vps` 后底部面板出现，状态 `SFTP 连接中`→`/root`，列出 `.acme.sh/.cache/.config/.ssh/.cert` |
| 2 | 通过 | 左树点 `/etc` 展开子节点，再点收起 |
| 3 | 通过 | 双击 `..`、点 `~`、路径栏输入后列表与路径栏同步 |
| 4 | 通过 | 表头按名称/大小/修改时间排序，目录恒在前 |
| 5 | 通过 | 勾选两行后工具条显示已选数量 |
| 6 | 通过 | 批量下载出现 ↓ 进度，结束后落盘 `files/downloads/` |
| 7 | 通过 | 右键「新建文件夹」→ 输入 `sfdir` → 确定 → 列表出现且终端 `ls sfdir` 无报错；重命名往返、新建文件（早前构建）均通过 |
| 8 | 通过 | 权限对话框 755→600，确定后 toast「已修改权限 new-dir-337 → 600」，列表权限列刷新为 `drw-------` |
| 9 | 通过 | 删除弹出「确认删除 /root/.cert」二次确认框，取消后文件保留；测试目录经面板/终端清理 |
| 10 | 通过 | 双栏从沙箱本地上传文件到远端 `/root` |
| 11 | 通过 | 远端文件下载到本地栏目录 |
| 12 | 通过 | 终端输入、连接期对话框均正常；工具条最右侧 ▼ 按钮把 SFTP 面板压缩为底部细条（终端占满），细条左侧按钮点击后恢复原高度与内容 |

环境注意事项（非应用缺陷）：

- 开鸿输入法（Rime `luna_pinyin`）处于拼音模式时会吞掉所有 ASCII 物理键，表现为「键盘失效」；切 ASCII 模式后逐字直通。验证时临时放置 `luna_pinyin.custom.yaml`（`switches/@0/reset: 1`）强制 ASCII，**验证后已删除并 `force-stop` 输入法**。
- `uitest uiInput inputText` / `text` 会丢弃 `-`、`_`、`*` 等符号，远端命令改由 Tab 补全构造。
- 右键菜单项位置随指针锚定（末项在指针上方约 84px），点击前需先截图测定。
- 标签页 `×` 位于标签内部右端（1366×768 下物理坐标约 `(170,53)`，逻辑 20px、物理约 30px），点标签右侧空白不会关闭；也可右键标签用「关闭标签页」。

### 20-9 风险

- 上游 `spawn_sftp` 依赖 `tar`（多选打包下载）；VPS 自带，缺失时状态栏显示错误；
- `russh-sftp` 已在 `meatshell-core` 编译通过，OHOS 交叉编译无新增依赖风险；
- 上游事件里 `owner/group` 的 getent 补全不在本轮，列会为空（与上游最终态一致，
  上游只是异步补名）。

## 21. 端口转发 / 隧道（Tunnels 页签，已完成）

复用上游 `AppWindow` 里原样编入的 `ui/sftp_panel.slint` 隧道页签（`panel-mode ==
"tunnels"`、`TunnelInfo`、`tunnel-add/stop/start/delete` 四个回调），本轮只补
Rust 侧的引擎、事件、持久化与回显。

### 21-1 能力

- `-L` 本地转发：`spawn_local` 在设备侧 `bind` 监听 → 每个连接
  `channel_open_direct_tcpip` → `copy_bidirectional`；
- `-D` 动态转发：同端口上跑最小 SOCKS5（无认证、仅 `CONNECT`，IPv4/IPv6/域名），
  额外支持用 `uinput` 无关的标准客户端连接；
- `-R` 远端转发：`targets` 登记 + `tcpip_forward`，服务器侧
  `server_channel_open_forwarded_tcpip` 回调里 `TcpStream::connect` 到本地目标；
  停止时 `cancel_tcpip_forward`；
- 行的运行/停止：Stop 关闭监听与转发任务（`JoinHandle::abort`）、Start 重新拉起、
  Delete 停止并从会话移除；名字可空，端口/主机做基本校验；
- 勾选「保存到会话」写入 `sessions.json` 的 `forwards[]`；勾选「连接时自动启动」
  的条目在 `Connected` 后自动拉起；
- 状态回显：`Event::Tunnels` → `TerminalPoll.tunnels` → 行模型，行内显示
  `●/○`、`-L/-D/-R`、名称、`bind → target`、运行中/已停止、Stop/Start、删除。

### 21-2 文件与改动点

| 文件 | 改动 |
| --- | --- |
| `ohos/meatshell-slint/src/tunnels.rs` | 新增。`TunnelRow`、`RuntimeForward`、`SharedHandle`、`RemoteTargets`、`tunnel_label`、`tunnel_info`、`emit_tunnel_update`、`stopped_entry`、`start_runtime_forward`、`start_remote_forward`、`stop_forward`、`spawn_local`、`spawn_dynamic`、`socks5_serve`、`socks_reply` |
| `ohos/meatshell-slint/src/ssh_session.rs` | `Event::Tunnels`；`Command::{AddTunnel, StopTunnel, StartTunnel, DeleteTunnel}`；`ConnectConfig.forwards`；`HostKeyVerifier` 提升为 `pub(crate)` 并加 `remote_forwards`/`events`，实现 `server_channel_open_forwarded_tcpip`；`shell_loop` 认证后把 handle 放进 `Arc<tokio::sync::Mutex<_>>`，连接后按配置启动并广播，主 `select!` 加四个命令分支 |
| `ohos/meatshell-slint/src/ohos_app.rs` | `ConnectConfig.forwards` 传入；`poll_sessions` 映射 `Tunnels` → `TunnelInfo`（`bind=addr:port`，`target=SOCKS5` 或 `host:port`），断线时清空；四个回调接线，`saved-` 前缀对应持久化条目并在删除时同步 `ConfigStore` |
| `ohos/meatshell-slint/src/lib.rs` | `mod tunnels;` |

上游 `src/`、`ui/` 未改一行。

### 21-3 降级与已知限制

- 隧道状态是**乐观显示**（与上游一致）：`-D` 端口被占用时终端打印
  `[meatshell] -D ... 监听失败 / bind failed: Address in use (os error 98)`，
  但行仍显示「运行中」，需用户自行删除；
- SOCKS5 仅 `CONNECT`，不做认证、`BIND`、`UDP ASSOCIATE`；
- `-R` 是否对公网可达取决于服务器 `GatewayPorts`（本轮只验证 127.0.0.1 绑定）；
- 会话断开后所有运行时转发随之结束，未保存的条目不再出现（保存的条目下次连接
  自动恢复）；
- 底部面板默认高度 220px，容不下「表单 + 三行列表」，需上拖分隔条或滚动才能看到
  列表行（上游同一布局，非移植缺陷）。真机 `uinput` 的滚轮注入对 Slint
  `ScrollView` 不生效，验证时改用拖动分隔条放大面板。

### 21-4 验收矩阵（API 23 / x86_64 真机，VPS `203.0.113.10`）

| # | 结果 | 证据 |
| --- | --- | --- |
| 1 | 通过 | `-L 127.0.0.1:18080 → 127.0.0.1:22`，终端打印 `[meatshell] -L ...`，列表出现该行且「运行中」 |
| 2 | 通过 | 设备侧 `netstat -ltn` 见 `127.0.0.1:18080 LISTEN`；设备自带 `ssh -p 18080` 拿到 `Remote protocol version 2.0 ... OpenSSH_9.6p1` |
| 3 | 通过 | 点该行 Stop：`LISTEN` 消失，行变「已停止」+「Start」按钮，终端打印停止提示 |
| 4 | 通过 | 点 Start：`LISTEN` 恢复，再次 `ssh -p 18080` 握手成功 |
| 5 | 通过 | `-D 127.0.0.1:11080` 添加后经 `hdc fport` + Python SOCKS5 客户端：greeting `05 00`、`CONNECT` 成功、收到 VPS SSH banner |
| 6 | 通过 | 勾选「保存到会话」+「连接时自动启动」后 `sessions.json` 的 `forwards[]` 出现该条目 |
| 7 | 通过 | `-R 127.0.0.1:19999 → 127.0.0.1:22` 添加成功（`tcpip_forward` 返回 Ok 才打印提示），列表行显示「运行中」 |
| 8 | 通过 | `aa force-stop` + 重启 + 重连 `vps`：无需任何操作，`netstat` 见 `127.0.0.1:11080 LISTEN`，Tunnels 列表出现该 `-D` 行且「运行中」 |
| 9 | 通过 | 点该行「删除」后列表变「No tunnels」、`LISTEN` 消失；回读 `sessions.json`，`vps` 的 `forwards` 为 `[]` |
| 10 | 通过 | 回归：终端 `echo regress-ok` 回显并输出；SFTP `/root` 列表（大小/时间/类型/权限/owner）正常；面板折叠为底部细条后可点细条恢复 |

现场截图（设备内临时目录，非仓库）：`/tmp/tun41.jpeg`（三条转发同时运行中）、
`/tmp/tun50.jpeg`（重启后自动恢复的 `-D`）、`/tmp/tun52.jpeg`（终端回归）、
`/tmp/tun53.jpeg`（SFTP 回归）、`/tmp/tun55.jpeg` + `/tmp/tun56.jpeg`（折叠/恢复）。

### 21-5 环境注意事项

- 1366×768 下 `snapshot_display` 产出即为物理分辨率，`:1024` 缩略图换算比例 0.75；
- 点击控件前先截图测 y 坐标：面板较矮时标签行/按钮行与估算值常差 10–15px，
  误点会落到分隔条或上一行；
- 面板高度由分隔条（面板顶边 4px，命中区 -4..+8px）拖动改变，拖动后该标签页的
  高度会按会话保存。

## 22. 快捷命令 / 命令历史（Quick Commands，已完成）

上游底部命令栏（「快捷」下拉 + 输入框 + 历史下拉）与管理弹窗本就在
`ui/app.slint` / `ui/terminal_view.slint` 里原样编入，但 OHOS 侧没有任何模型与
回调接线。本轮补齐整条链路，纯 Rust + 既有 `ConfigStore`，不新增 OS API。

### 22-1 能力

- 模型：从 `ConfigStore` 读 `quick_commands` / `quick_groups` / `command_history`，
  按上游分组规则（`default` 组 + 命名组）生成 `QuickCmd` 行模型；
- 执行：命令栏回车 / 点击 chip → `encode_command_bar_input`（trim + `\n`）写入
  对应终端 PTY，「所有会话」时写全部在线终端；非空命令记入持久化历史；
- send-enter=false 的条目点击只回填命令栏，不执行（便于先改后发）；
- 管理弹窗：新增 / 编辑 / 保存 / 复制 / 删除 / 组内上移下移 / 移动到分组 /
  新建分组 / 重命名分组 / 删除空分组，全部经 `ConfigStore` 落盘；
- 历史：大小写不敏感子串过滤、单行预览（`\n` → `⏎`，240 字符截断）、
  点击回填、按文本 / 按索引删除；
- 分组折叠是运行时状态（`Rc<RefCell<HashSet<String>>>`），初始全折叠，不持久化，
  与上游一致。

### 22-2 文件与改动点

| 文件 | 改动 |
| --- | --- |
| `ohos/meatshell-slint/src/quick_commands.rs` | 新增。`CollapsedGroups`、`SendBytes`、`all_quick_group_names`、`quick_cmd_model`、`command_preview`、`edit_index_after_delete`、`edit_index_after_swap`、`reorder_quick_command`、`history_model`、`history_view_rows`、`history_view_model`、`history_preview_model`、`set_history_view`、`encode_command_bar_input`、`save_or_log`、`refresh_quick`、`register` |
| `ohos/meatshell-slint/src/ohos_app.rs` | `wire_callbacks` 末尾构造终端写入桥（`Some(id)` 写该 tab，`None` 写全部在线 tab），调用 `quick_commands::register(ui, store.clone(), send)` |
| `ohos/meatshell-slint/src/lib.rs` | `mod quick_commands;` |

上游 `src/`、`ui/` 未改一行。

### 22-3 降级与已知限制

- `copy-text`（历史「复制」）：OHOS 无剪贴板桥，回调保持 no-op；
- `QuickDockPanel` / 快捷命令侧栏 / 快捷键绑定：本移植无 edge stack 与快捷键层，
  入口只走底部命令栏与管理弹窗（与上游同一套 UI）；
- 落盘文件实际名为 `sessions.json`（`ConfigStore` 既有命名），位于 HAP
  `filesDir` 下；
- 自动化限制（非应用缺陷）：OHOS IME 提交路径绑定终端隐藏输入锚点，命令栏与
  弹窗 `TextInput` 需 `keyEvent 2054` 提交候选；列表行图标需精确物理坐标；
  管理弹窗删除分组是**分组头右键菜单**（`PopupWindow`），`uitest uiInput` 无右键，
  需 `uinput -M -d 1 -u 1` 注入。

### 22-4 验收矩阵（API 23 / x86_64 真机，VPS `203.0.113.10`）

| # | 结果 | 证据 |
| --- | --- | --- |
| Q1 | 通过 | 下拉出现 `default` 分组头，默认折叠；点组名可展开 / 收起；空状态显示「暂无快捷命令」+「管理…」 |
| Q2 | 通过 | send-enter=true 的 chip 点击后终端执行并输出 `a: command not found`；命令栏 `pwd` + 发送输出 `/root` |
| Q3 | 通过 | send-enter=false 的条目点击只回填命令栏，终端未执行 |
| Q4 | 通过 | 弹窗新增（`q` / `pwd`）、编辑改名 `pwdp2` + 保存，下拉与持久化立即更新 |
| Q5 | 通过 | 组内 ↓ 重排后 `sessions.json` 顺序随操作变化，无跨组乱序 |
| Q6 | 通过 | 「新建分组」写入 `g1`；对 `g1` 分组头右键 →「删除分组」后 `quick_groups: []`，列表分组头消失 |
| Q7 | 通过 | 删除 `q` 后持久化文件不再包含该条目；删除正在编辑的条目会清空表单 |
| Q8 | 通过 | 执行 `a`、`pwd` 后历史下拉按序列出；点击行回填命令栏 |
| Q9 | 通过 | 点红色垃圾桶删除 `a`，`command_history` 只剩 `["pwd"]` |
| Q10 | 通过 | `aa force-stop` + 重启 + 重连：`quick_commands` / `quick_groups` / `command_history` 全部保留，分组恢复初始折叠 |
| Q11 | 通过 | 终端硬件键盘文本注入回显正常；SFTP `/root` 列表（大小/时间/类型/权限/owner）正常；Tunnels 页签 `-L/-D/-R` 表单与列表正常（无崩溃） |

现场截图（设备内临时目录，非仓库）：`/tmp/qc-57.jpeg`（重启后分组折叠）、
`/tmp/qc-58.jpeg`（管理弹窗）、`/tmp/qc-59.jpeg`（分组右键菜单）、
`/tmp/qc-60.jpeg`（删除空分组后）、`/tmp/qc-61.jpeg`–`qc-63.jpeg`（终端输入）、
`/tmp/qc-64.jpeg`（SFTP）、`/tmp/qc-65.jpeg`（Tunnels）。

### 22-5 环境注意事项

- 物理坐标 = 聊天缩略图坐标 × 1.333（截图 1366×768，缩略图 1024×576）；
- 右键菜单用 `uinput -M -m X Y -d 1 -u 1`，`uitest uiInput` 无右键命令；
- 命令栏 y≈459、快捷按钮 x≈53、发送 x≈1309、历史 x≈1203（物理像素）；
- 弹窗打开时命令字段自动聚焦，可先填命令再填名称，避免多次点选。
