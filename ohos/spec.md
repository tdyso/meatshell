# Spec：OHOS 快捷命令 / 命令历史（对齐上游 Quick Commands）

状态：已完成（代码实施、部署、Q1–Q11 真机验收全部通过）
范围：只改 `ohos/`，上游 `src/`、`ui/` 一行不改。
对应上游：`src/app/quick_commands.rs`、`src/app/key_input.rs:110-370`、
`src/app/terminal_ui.rs:3-112`、`src/terminal/impls/input.rs:54-60`、
`src/config/impls/config.rs:1103-1310`、`ui/terminal_view.slint:353-395`、
`ui/app.slint:631-677 / 2264-2309 / 3718-4190`（均已原样编译）。

## 1. 背景与目标

上游底部命令栏（quick chip 下拉 + 输入框 + 历史下拉）与管理弹窗已存在于
编译产物中，但 OHOS 端没有任何数据模型和回调接线：

- `quick-commands` 恒为空 → 「快捷」下拉显示「No quick commands yet」。
- `command-history` / `history-view` / `history-preview` 恒为空 → 历史下拉恒空，
  ↑/↓ 无法召回。
- 管理弹窗的新增/编辑/删除/复制/分组/排序全部无效。

本增量把整条链路接通（纯 Rust + 已有 ConfigStore，不新增 OS API）：

1. 数据：从 `ConfigStore` 读 `quick_commands` / `quick_groups` /
   `command_history`，按上游分组规则生成 `QuickCmd` 行模型。
2. 执行：命令栏回车或点击 chip → 文本 + `\n` 写入对应终端 PTY（`to-all`
   时写全部在线终端），并记入持久化历史。
3. 管理：新增/编辑/复制/删除/组内上移下移/移动到分组/新建/重命名/删除空分组，
   全部经 `ConfigStore` 持久化到 `config.json`。
4. 分组折叠：运行时状态（不持久化），初始全部折叠，与上游一致。
5. 历史：过滤（大小写不敏感子串）、按文本删除、按索引删除、单行预览。

非目标（明确不做）：
- 复制到系统剪贴板（`copy-text`）：OHOS 剪贴板需新增 NAPI，本轮保持 no-op。
- 右侧 QuickDockPanel（`gp.kind == "quick"`）：本移植尚无 edge stack，
  快捷入口只走终端底部命令栏与弹窗（与上游同一套 UI）。
- 快捷命令作为侧栏（`quick-commands-as-sidebar`）、快捷键绑定
  （`Ctrl+Shift+Q` 等）不在本轮范围。

## 2. 文件与改动

### 2.1 新增 `ohos/meatshell-slint/src/quick_commands.rs`

逐行移植上游逻辑，仅把 `pub(super)` 改为 `pub(crate)`，窗口类型换成本 crate
生成的 `AppWindow`：

```text
pub(crate) type CollapsedGroups = Rc<RefCell<HashSet<String>>>;

pub fn all_quick_group_names(store: &ConfigStore) -> HashSet<String>
pub fn quick_cmd_model(store: &ConfigStore, collapsed: &HashSet<String>) -> ModelRc<QuickCmd>
pub fn command_preview(command: &str) -> String
pub fn edit_index_after_delete(edit: i32, deleted: i32) -> i32
pub fn edit_index_after_swap(edit: i32, a: usize, b: usize) -> i32
pub fn reorder_quick_command(cmds: &mut [QuickCommand], index: usize, up: bool) -> Option<usize>

pub fn history_model(store: &ConfigStore) -> ModelRc<SharedString>
pub fn history_view_rows(history: &[String], query: &str) -> Vec<SharedString>
pub fn history_view_model(store: &ConfigStore, query: &str) -> ModelRc<SharedString>
pub fn history_preview_model(store: &ConfigStore, query: &str) -> ModelRc<SharedString>
pub fn set_history_view(ui: &AppWindow, store: &ConfigStore, query: &str)
pub fn encode_command_bar_input(command: &str) -> (Option<String>, Vec<u8>)

pub fn register(
    ui: &AppWindow,
    store: Option<Rc<RefCell<ConfigStore>>>,
    send: Rc<dyn Fn(Option<&str>, &[u8])>,
)
```

`register` 行为（与上游 `key_input.rs` 一一对应）：

| 回调 | 语义 |
| --- | --- |
| 初始化 | `set_quick_commands(quick_cmd_model(..))`；`collapsed` 初值 = `all_quick_group_names`（全部折叠）；历史三个模型置空 |
| `run_command(tab, cmd, to_all)` | `encode_command_bar_input` → `send(Some(tab) / None, bytes)`；非空历史 `push_command_history` + `save()`；刷新 `command-history` 与当前过滤视图 |
| `search_history(q)` | 记录 `hist_query`，重设 `history-view` + `history-preview` |
| `delete_history(i)` | 按存储索引删除 + `save()`，刷新全部历史模型 |
| `delete_history_cmd(cmd)` | 按文本删除 + `save()`，刷新全部历史模型并保持过滤 |
| `add_quick_command` | 校验 name/command 非空；push + `set_quick_commands` + `save()`；刷新模型 |
| `delete_quick_command(i)` | 删除 + `edit_index_after_delete` 调整 `qcm-edit-index`，必要时清空表单；刷新 |
| `edit_quick_command(i)` | 载入 `qcm-name/command/group/send-enter`，`qcm-edit-index=i`，打开 `quick-cmd-manage-open` |
| `save_quick_command(i,..)` | `update_quick_command` + `save()`；刷新 |
| `duplicate_quick_command(i)` | `"{name} (copy)"` 插入 i+1，`qcm-edit-index` 顺移；刷新 |
| `move_quick_command(i, group)` | `"default"` → 空串；写 group + `save()`；刷新 |
| `reorder_quick_command(i, up)` | `reorder_quick_command` + `save()`，`qcm-edit-index` 跟随交换；刷新 |
| `toggle_quick_group(g)` | 翻转运行时 `collapsed`；刷新 |
| `submit_quick_group(orig, name)` | `orig` 空 → `add_quick_group`，否则 `rename_quick_group`；`save()`；刷新 |
| `delete_quick_group(name)` | `remove_quick_group` + `save()`；刷新 |

`store == None` 时只写空模型，所有回调直接返回。
`copy_text` 不接线（Slint 未设置的回调为 no-op），在 §4 记为降级。

### 2.2 `ohos/meatshell-slint/src/ohos_app.rs`

在 `wire_callbacks` 末尾（隧道回调之后）新增：

```rust
let send: Rc<dyn Fn(Option<&str>, &[u8])> = {
    let terms = terms.clone();
    Rc::new(move |tab_id, bytes| {
        let map = terms.borrow();
        match tab_id {
            Some(id) => { /* 仅该 tab 的 session.write(bytes) */ }
            None => { /* 所有已连接 tab 的 session.write(bytes) */ }
        }
    })
};
crate::quick_commands::register(ui, store.clone(), send);
```

- 复用 `TermRegistry` 与 `SshSession::write`，不新增线程/通道。
- `store.clone()` 在 `wire_callbacks` 入参上直接可用；`session_editor::register`
  仍拿到原 `store`，顺序不变。
- 不修改 `TermTab` 字段，不触碰 SSH 事件循环。

### 2.3 `ohos/meatshell-slint/src/lib.rs`

新增 `mod quick_commands;`（与 `tunnels` 同级）。

## 3. 数据与持久化

- 全部读写经由 `meatshell_core::config::ConfigStore`（上游源码原样编译）：
  `quick_commands()` / `set_quick_commands()` / `update_quick_command()` /
  `quick_groups()` / `add_quick_group()` / `rename_quick_group()` /
  `remove_quick_group()` / `command_history()` / `push_command_history()` /
  `remove_command_history()`。
- 落盘位置与既有会话一致：`MEATSHELL_DATA_DIR`（HAP `filesDir`）下的
  `sessions.json`（ConfigStore 的实际文件名，实测路径
  `/data/app/el2/100/base/com.tdyso.khshell/haps/entry/files/sessions.json`）。
- 分组折叠状态只存内存（`Rc<RefCell<HashSet<String>>>`），重启回到全折叠，
  与上游注释一致。

## 4. 已知降级（写入报告）

1. `copy-text`（历史「复制」）：无 OHOS 剪贴板桥，no-op。
2. QuickDockPanel（可停靠快捷面板）：无 edge stack，不出现；命令栏下拉 +
   管理弹窗可用。
3. 快捷命令侧栏模式、快捷键绑定：未接线，保持默认关闭。

## 5. 验收矩阵（真机 1366×768）

| # | 步骤 | 通过标准 |
| --- | --- | --- |
| Q1 | 连接 VPS，展开底部「快捷」下拉 | 分组头出现且默认折叠；点组名可展开/收起 |
| Q2 | 新增 send-enter=true 的 `echo hi-ohos` | 点击 chip 后终端输出 `hi-ohos` |
| Q3 | 新增 send-enter=false 的 `ls -l` | 点击只填入输入框，不执行 |
| Q4 | 管理弹窗：编辑名称/命令并保存 | 下拉与终端行为立即更新 |
| Q5 | 复制条目、组内上移/下移、移动到分组 | 顺序/分组正确且不跨组乱序 |
| Q6 | 新建分组、重命名、删除空分组 | 下拉分组头随之变化 |
| Q7 | 删除条目、删除正在编辑的条目 | 列表消失，编辑表单重置 |
| Q8 | 命令栏输入命令回车 → 打开历史下拉 | 新命令在末尾；搜索框过滤生效；点击可再执行 |
| Q9 | 历史删除单条（×） | 该条消失且不影响其他条目 |
| Q10 | 强制停止应用并重启，重连 | 快捷命令、分组、历史全部保留 |
| Q11 | 回归：键盘输入、SFTP、隧道 | 均正常 |

## 6. 构建与验证命令

```bash
cd ~/DevEcoStudioProjects/khmeatshell && bash deploy.sh
```

（后台，thin LTO 约 20–30 分钟；`cargo check` 在部署前先过。）

真机：`hdc shell uitest uiInput click X Y` / `keyEvent 2054`（Enter），
截图存 `/tmp/qc-*.jpeg`；持久化核对 `config.json` 中
`quick_commands` / `quick_groups` / `command_history`。

## 7. 交付物

1. 上述三个文件的代码改动。
2. `ohos/P2B-SESSION-EDITOR.md` 追加 §22（能力 / 改动点 / 降级 / 矩阵结果）。
3. `ohos/tasks.md` 勾选 Q1–Q6。

## 8. 需要批准的点

- 是否接受「本轮不接剪贴板、不接 QuickDockPanel、不接快捷键」的降级边界。
- 是否同意按 §5 的 Q1–Q11 逐项真机验收后再回写报告。

## 9. 真机验证结果（全部通过）

| # | 结果 | 证据 |
| --- | --- | --- |
| Q1 | 通过 | 下拉出现 `default` 分组头，可折叠/展开；空状态显示「暂无快捷命令」+「管理…」 |
| Q2 | 通过 | send-enter=true 的 chip 点击后终端输出 `a: command not found`；`pwd` 发送后输出 `/root` |
| Q3 | 通过 | send-enter=false 的条目点击仅回填命令栏，终端未执行 |
| Q4 | 通过 | 弹窗新增 / 编辑改名 / 保存，立即刷新并落盘 |
| Q5 | 通过（组内上下移） | 持久化顺序随操作变化，无跨组乱序 |
| Q6 | 通过 | 新建分组 `g1` 落盘；分组头右键 →「删除分组」后 `quick_groups: []`，列表消失 |
| Q7 | 通过 | 删除条目后 `sessions.json` 不再包含该条目 |
| Q8 | 通过 | 历史下拉列出 `a`、`pwd`；点击回填命令栏 |
| Q9 | 通过 | 垃圾桶删除后 `command_history` 仅剩 `pwd` |
| Q10 | 通过 | force-stop 重启后 `quick_commands` / `quick_groups` / `command_history` 全部保留，分组恢复初始折叠 |
| Q11 | 通过 | 终端硬件键盘文本注入回显正常；SFTP `/root` 列表正常；Tunnels 页签 `-L/-D/-R` 表单与列表正常 |

自动化限制（记录，非缺陷）：OHOS IME 提交绑定终端隐藏锚点，命令栏/弹窗
TextInput 需 `keyEvent 2054` 提交候选；列表行图标需精确物理坐标
（截图 1366×768，聊天显示 1024×576，物理 = 显示 × 1.333）。

---

# 增量：三栏布局对齐上游 0.8.2 + 真实资源数据

状态：代码完成、已部署；剩余「右侧折叠条带展开」一项真机复验。
范围：只改 `ohos/meatshell-slint/src/`；上游 `src/`、`ui/` 零改动；
版本号仍显示 `0.8.1`（vendored 源码未升级）。

## 10. 布局契约（`dock_layout.rs`）

| 项 | 值 |
| --- | --- |
| 左侧欢迎栏 | kind `welcome`，edge `left`，x=0，w=`welcome-sidebar-width`（默认 240，clamp 160–480，且 ≤ 0.3W） |
| 右侧资源栏 | kind `sidebar`，edge `right`，x=W-w，w=`sidebar-width`（默认 280，clamp 180–420，且 ≤ 0.35W） |
| 中央区 | `dock-central-x=left`、`dock-central-y=0`、`dock-central-w=W-left-right`、`dock-central-h=H` |
| 折叠 | 该面板不入 `dock-panels`，但对应边仍保留 `COLLAPSED_STRIP = 36px`（否则 `rest` 会盖住 ToolStrip） |
| 拖拽 | `panel-extent-drag(gi,pos)`：左栏 pos 即新宽度；右栏 pos = `gp.x+gp.w-pos`；写回 `welcome-sidebar-width` / `sidebar-width` 后重算 |
| 重算入口 | `dock-area-resized`、`dock-layout-changed`、`refresh-sidebar`、`set-sidebar-collapsed`、`set-welcome-collapsed`、窗口 `resize` 全部走 `apply_dock(last_size())` |
| 持久化 | `persist-sidebar-*` / `persist-welcome-*` 空实现（仅内存，避免污染上游配置） |

折叠条带几何（`ui/app.slint:1690-1717`，只读引用）：`x = dock-area.width - 36`，
`y = 0`，宽 36、高 `dock-area.height`；图标为 30×30，位于条带顶部
`y = 4..34`，`x` 居中（`+3..+33`）。因此**只有条带顶部 30×30 区域可点**。

浮动工具栏遮挡：`toolbar-x-off = dock-right-gap + 4`（`ui/app.slint:560`），
`dock-right-gap = W - dock-central-x - dock-central-w`。右栏折叠时
`dock-right-gap` 必须为 36，工具栏整体左移 40px，条带图标才不被
`root.width` 锚定的工具栏（`ui/app.slint:2902`）压住。若 `dock-central-w`
未按折叠值重算（`dock-right-gap=0`），点击条带位置会命中工具栏的「设置」
按钮并弹出菜单——这正是早期复验失败的根因。

## 11. 资源数据契约（`resource_stats.rs`）

`StatsSample { cpu_percent, mem_used_kib, mem_total_kib, swap_used_kib,
swap_total_kib, net: Vec<(iface,rx,tx)>, disks: Vec<(path,used,total)> }`。

- 远端：`ssh_session` 每 2s 在独立 exec channel 采集
  `/proc/stat`、`/proc/meminfo`、`/proc/net/dev`、`df -kP`；
  CPU/网速按两次采样时间差换算；单次 3s 超时，失败静默跳过，不影响 shell。
- 本机：同一 `parse_block` 解析本地快照；`df` 依次尝试
  `/bin/df`、`/system/bin/df`、`/usr/bin/df`、`df`（app 进程 PATH 为空）。
- 过滤 `^/module_update` 挂载，磁盘按总容量降序。
- 展示：活动 tab 已连接且有缓存 → 「服务器资源」+ host；否则「本机资源」。
  切换 tab / 断开后按缓存回放，避免闪空。
- `system-info-available` / `proc-available` 保持 false（OHOS 未实现对应窗口）。

## 12. 验收矩阵（真机 1316×704 客户区，窗口原点 25,0）

| # | 步骤 | 通过标准 |
| --- | --- | --- |
| R1 | 启动 | 左 240 / 右 280 / 中央居中；底部 `meatshell v0.8.1` |
| R2 | 本机资源 | CPU、内存、交换、网速曲线、磁盘列表均有真实数值，2s 刷新 |
| R3 | 连接 VPS | 标题变「服务器资源」+ host；数值与服务器 `top/df` 一致 |
| R4 | 切回欢迎页 | 面板回落「本机资源 / 未连接」 |
| R5 | 折叠左栏 | 中央左移，点击左侧条带顶部图标恢复 240 |
| R6 | 折叠右栏 | 中央右扩；`dock-right-gap=36`；点击右侧条带顶部图标恢复 280 |
| R7 | 拖拽分隔 | 左栏 160–480、右栏 180–420 内跟手，松手不回弹 |
| R8 | 回归 | 终端输入/输出、SFTP、隧道、快捷命令面板不受影响 |

## 13. 构建部署说明（已执行）

- 重跑构建部署把 `COLLAPSED_STRIP` 修复落到设备，再执行 R1–R8。
- 折叠条带图标位置保持上游 `y = 4`，Rust 侧只保证 `dock-right-gap == 36`。

## 14. 真机验证结果（全部通过）

构建：`cargo check --release --target x86_64-unknown-linux-ohos` 通过；
`deploy.sh` BUILD SUCCESSFUL → SignHap success → install/start 成功。

| # | 结果 | 证据（截图） |
| --- | --- | --- |
| R1 | 通过 | `/tmp/s16.jpeg`：左 240（快速连接）、中央欢迎页、右 280（运行状态），底部 `meatshell v0.8.1` |
| R2 | 通过 | `/tmp/s16.jpeg`、`/tmp/s31.jpeg`：本机 CPU/内存 2.3G/15.4G、交换 0/3.1G、上下行曲线、磁盘（`/data/storage/el1/bundle` 81.5GB/90.3GB 起，已过滤 `module_update`） |
| R3 | 通过 | `/tmp/s23.jpeg`：连上 vps 后右栏标题变「服务器资源」+ `203.0.113.10`，CPU/内存 495/961M、交换 37/1023M、eth0 网速、远端挂载 |
| R4 | 通过 | `/tmp/s31.jpeg`：断开/切回欢迎页后回到「本机资源 / 未连接」，数值切换为本地 |
| R5 | 通过 | `/tmp/s9.jpeg` 折叠为 36px 条带、中央左扩；点击 `(43,55)` 后 `/tmp/s10.jpeg` 恢复 240 |
| R6 | 通过 | `/tmp/s5.jpeg` 折叠为 36px 条带、中央右扩、工具栏左移 40px；点击 `(1321,55)` 后 `/tmp/s6.jpeg` 恢复 280 |
| R7 | 通过 | `/tmp/s13.jpeg`：左分隔条 4px 手柄可拖动改宽，随拖随变、松手保持；重启恢复默认 240/280 |
| R8 | 通过 | `/tmp/s23.jpeg`、`/tmp/s32.jpeg`：SSH 登录 banner/提示符渲染、SFTP `/root` 列表与目录树正常、标签切换正常 |

补充说明：
- 折叠按钮是面板标题栏右侧 18×18 折叠箭头（左栏约 `(213,72)`、右栏约
  `(1311,66)`）；展开入口是条带顶部 30×30 图标，不在条带中部。
- 命令栏 IME 提交顺序问题属既有自动化限制（见前文），非本增量回归。
- 布局宽度与折叠态已改为持久化，见 §15。

## 15. 布局持久化（增量 2）

沿用上游 `ConfigStore` 既有字段，不新增配置键：

| 属性 | 配置字段 | 何时写 |
| --- | --- | --- |
| `welcome-sidebar-width` | `welcome_sidebar_width` | 拖动停止 400ms 后（防抖） |
| `sidebar-width` | `sidebar_width` | 拖动停止 400ms 后（防抖） |
| `welcome-collapsed` | `welcome_collapsed` | 折叠/展开（`set-welcome-collapsed`） |
| `sidebar-collapsed` | `sidebar_collapsed` | 折叠/展开（`set-sidebar-collapsed`） |

说明：0.8.2 的 dock-stack 分隔条只在移动时回调 `panel-extent-drag`，没有
「拖动结束」事件；`persist-*-width` 属于本移植未渲染的旧手柄。因此在
`panel-extent-drag` 里用 400ms 单次定时器（`save_layout`）合并写入，避免每次
指针移动都落盘。

规则：

- 启动时 `dock_layout::init` 先置默认 240/280 + 展开；随后若有
  `ConfigStore`，用持久值覆盖（宽度再 clamp 到 160–480 / 180–420）。
- `sidebar_width` 上游默认值 220 视为「未设置」，保留 OHOS 对齐后的
  280；其余非 220 的值原样恢复。
- 停靠边固定 `welcome=left` / `sidebar=right`，`persist-*-sidebar-dock`
  仍为 no-op（与 0.8.2 三栏一致）。
- `save()` 失败忽略（与原会话保存一致），不阻塞 UI。

验收（P1–P4）：

| # | 步骤 | 通过标准 |
| --- | --- | --- |
| P1 | 拖动左分隔条到约 300 后松开 | 配置写入 `welcome_sidebar_width≈300`，界面立即保持 |
| P2 | 折叠右栏 | 配置 `sidebar_collapsed=true` |
| P3 | force-stop 后重启 | 左栏≈300、右栏仍为 36px 条带 |
| P4 | 展开右栏并重启 | 右栏 280（或拖动后的值）保持展开 |

真机结果（全部通过，窗口原点 25,0 / 1316×704）：

| # | 结果 | 证据 |
| --- | --- | --- |
| P1 | 通过 | `drag 263 300 330 300` → 防抖后 `welcome_sidebar_width: 301.0`（`/tmp/p2.jpeg` → `/tmp/p3.jpeg`），界面立即变宽 |
| P2 | 通过 | 点击右栏折叠箭头 `(1311,66)` → `sidebar_collapsed: true`，右栏收为 36px 条带（`/tmp/p3.jpeg`） |
| P3 | 通过 | `aa force-stop` + `aa start` 后 `/tmp/p4.jpeg`：左栏仍为 301、右栏仍为条带 |
| P4 | 通过 | 点击 `(1321,55)` 展开（`sidebar_collapsed: false`），重启后 `/tmp/p5.jpeg`：左 301 / 右 280 展开，运行状态有实时数据 |

构建：`cargo check --release --target x86_64-unknown-linux-ohos` 通过（仅上游
warning）；`deploy.sh` BUILD SUCCESSFUL → install → start 成功。

## 16. 启动页对齐 0.8.2（增量 3）

0.8.2 启动时不再有中央「新标签页 / 欢迎卡片」，快速连接只在左栏；中央在打开
会话前为壁纸上的产品标识。`ohos_app.rs` 负责初始模型，`ui/app.slint` 负责在
「空 pane + welcome-sidebar」时绘制 Logo（经用户确认后允许改上游 ui）。

| 项 | 0.8.1 旧行为 | 0.8.2 对齐后 |
| --- | --- | --- |
| 启动标签 | 一个 `welcome` 标签 | 无标签（`tabs` 空、`active-tab-id` 空） |
| 中央区域 | `Welcome` 卡片（标题 + 快速连接列表） | 居中 Logo：`Theme.accent` 圆角块内 `>_` + `MeatShell` + 标语 |
| 中央背景 | `Theme.bg-root` 不透明 | 空 pane 且 `welcome-as-sidebar` 时 `transparent`，露出壁纸 |
| 关闭最后一个标签 | 自动补一个 `welcome` 标签 | 保持空（回到 Logo） |
| 进入会话 | 双击中央欢迎卡片 | 单击左栏「快速连接」会话行（`SessionRow` 在 pointer up 触发 `connect`） |

上游 ui 改动（`ui/app.slint`，内容区 Rectangle，约 2138–2205 行）：仅在
`pane.tabs.length == 0 && root.welcome-as-sidebar` 时新增居中 `VerticalLayout`
（46×46 accent 圆角块 + `>_`、34px/800 `MeatShell`、`Theme.fs-md` 标语，文案复用
`@tr("A lightweight and efficient Rust + Slint SSH client")`）；背景条件改为
透明。桌面端仅在同样空 pane 场景可见，不影响有标签时的渲染。

`activate_welcome_tab`（标签栏 `+`）仍保留欢迎页，作为唯一可回看快速连接卡片
的入口；`remaining_or_welcome` 改为 `visible_tabs`，不再补默认标签。

真机结果（通过）：

| # | 结果 | 证据 |
| --- | --- | --- |
| S1 | 启动即 Logo 居中 | `/tmp/h1.jpeg`：左栏快速连接、中央 `>_ MeatShell` + 标语、右栏运行状态实时数据 |
| S2 | 单击左栏会话可连接 | 点 `(81,247)` 打开 `dev@127.0.0.1` 标签，终端与 SFTP 面板正常（`/tmp/w2.jpeg`，127.0.0.1:2222 未监听属预期 refused） |
| S3 | 关闭最后一个标签 | 点标签 `×` 后回到 Logo 空态，不再补欢迎卡片（`/tmp/h2.jpeg`） |

## 17. 导入 / 导出（增量 4）

OHOS 无系统文件选择框，四个入口（齿轮菜单：导出连接 / 导入连接 / 批量导入(文本) /
导入 ~/.ssh/config）改为固定沙箱目录，可用 `hdc file send/recv` 读写：

| 入口 | 行为 | 路径 |
| --- | --- | --- |
| 导出连接 | `ConfigStore::export_to` 写 JSON，提示 `已导出 N 个连接：exports/<文件名>` | `<files>/exports/meatshell-connections-<epoch>.json` |
| 导入连接 | `ConfigStore::import_from` 合并，提示 `已导入 N 个（跳过 M）` | `<files>/import/meatshell-connections.json` |
| 从 ssh_config 导入 | 自带精简解析器（Host/HostName/Port/User/IdentityFile，忽略 `Host *`），按“同名或同 host+user”去重 | `<files>/import/ssh_config`，其次 `import/.ssh/config` |
| 批量导入(文本) | `host\|port\|user\|password\|name` 每行一个，`|` 后可省略后续字段，跳过表头 | 弹窗内输入 |

说明：上游 `ssh_config` 解析器在引擎内是 `pub(crate)`，无法跨 crate 调用，故在
`session_editor.rs` 内置精简解析器；`export_to` / `import_from` 仍复用上游实现，
导出的 JSON 带 `meatshell_export: 1` 版本号。写盘失败只在提示条显示，不弹窗。

真机结果（通过）：

| # | 结果 | 证据 |
| --- | --- | --- |
| I1 | ssh_config 导入 | 推送 2 个 Host（`ovh-vps` / `db-box`）后点菜单项，提示“已从 ssh_config 导入 2 个主机”，左栏出现两项（`/tmp/i3.jpeg`） |
| I2 | 导出连接 | 生成 `exports/meatshell-connections-1794710019.json`（6466 B，5 个会话，`meatshell_export: 1`） |
| I3 | 导入连接 | 改 1 条为新主机后导入，提示“已导入 1 个会话（跳过 4）”，列表新增 `imported-test`（`/tmp/i4.jpeg`） |
| I4 | 批量导入 | 弹窗输入后点“导入”，保存并刷新，新增 `root@batch-box`；IME 经 `uitest inputText` 会丢/错序 `|`，属既有自动化限制，手打正常 |

测试数据已清理（会话回到 `dev@127.0.0.1` / `mfa` / `vps`，宽度回默认 240/280，
删除 `import/`、`exports/` 下的测试文件），见 `/tmp/i10.jpeg`。

## 18. 设置 › 界面接线与持久化（增量 5）

上游 `ui/app.slint` 的 InterfacePanel 已完整渲染，但端口此前只实现了会话编辑等
回调，改字号 / 主题 / 缩放 / 各开关均不生效、重启丢失。本增量把设置页接上
`ConfigStore`，与上游 `src/app/settings_callbacks.rs` + `src/app/color_theme.rs`
对齐：

| 文件 | 内容 |
| --- | --- |
| `src/color_theme.rs`（新） | 移植上游纯计算部分：`Rgb` / `Palette` / 13 个预设 / `derive` / `custom_palettes` / `items`，以及 `apply_color_theme`（写 `theme-palette-dark/light`、`color-theme`、`color-theme-custom`、`color-themes`、`immersive-colors`）。去掉上游的 `sync_detached`（端口无独立窗口）与 `registry.broadcast`（端口无多窗口注册表）。 |
| `src/settings.rs`（新） | `apply_saved_appearance`：启动时把深色模式、终端字体/字号/行距/粗体/光标形状与颜色、UI 缩放、面板字号、字体列表、配色主题、折叠与开关类设置写进窗口。`register`：逐个接线 InterfacePanel 的 21 个回调，全部 `set_*` + `save()`。 |
| `src/ohos_app.rs` | `AppWindow::new()` 后调用 `apply_saved_appearance`；`session_editor::register` 之后调用 `settings::register`（store 改为 clone 传入）。 |
| `src/lib.rs` | `mod color_theme; mod settings;` |

关键取舍：

- 主题偏好 `system` 在 OHOS 上一律按深色（上游用 `dark_light` crate 探测系统，
  端口无对应依赖）。
- `set-language` 只写 `ConfigStore.language()`；运行时静态文案由
  `src/i18n.rs` 编译进中文 `.po`，Slint 绑定已缓存，切语言需重启且 Rust 侧
  `lang-en` 与静态文案不同步，故本轮不做运行时切换（限制记录在此）。
- 切换深/浅色不重建已连接会话的终端 ANSI 配色（`TermTab.is_dark` 在连接时
  固定）；新开标签按当前模式着色。
- `ui-scale` / `panel-font` 以百分比存储，窗口按 `percent / 100` 的比例属性
  下发；写入后回读 store 的 clamp 值，避免 UI 与磁盘不一致。

验收（真机）：

| # | 结果 | 证据 |
| --- | --- | --- |
| T1 | 通过：切「钛灰」预设后整窗配色与强调色立即变化，下拉回显正确；强停重启后仍为钛灰。工具栏太阳/月亮按钮切浅色/深色即时生效，`theme_pref` 持久化。自定义色与预设共用同一 `apply_color_theme` 路径，仅代码级验证（真机文本注入不可靠）。 | `/tmp/st11.jpeg`、`/tmp/st14.jpeg`、`/tmp/st16.jpeg` |
| T2 | 通过：字号 13→16px 立即生效（终端错误文本明显变大），界面缩放 120% 立即生效，重启后二者保留（缩放 120、字号 16）；行距/粗体/光标形状取默认值显示正常。恢复 100%/13px 同样即时。 | `/tmp/st15.jpeg`、`/tmp/st14.jpeg`、`/tmp/st18.jpeg` |
| T3 | 通过：`terminal_bold` 开关写入 `sessions.json`（`true`），再次关闭写回 `false`；`ui_scale: 120→100`、`font_size: 16→13`、`color_theme: titanium-gray→默认` 均正确落盘并在强停重启后恢复。 | `/tmp/st17.jpeg`、`sessions.json` |
| T4 | 通过：终端连接失败提示、SFTP 面板、资源面板、左栏快速连接均正常渲染，无崩溃；测试后已恢复默认主题/字号/缩放。 | `/tmp/st19.jpeg`、`/tmp/st20.jpeg` |

补充观察：深浅色切换时，光标颜色输入框的默认值随模式在 `#D4D4D4` / `#2D2D2F`
之间切换（`ui/app.slint` 侧行为），不需要 Rust 干预。



## 19. 终端输出高亮（增量 6）

设置 › 输出高亮此前只渲染控件：开关、内置规则集（日志级别 / DevOps）与自定义
规则都能点，但终端不会补色，也不落盘。本增量把上游
`src/terminal/impls/output_highlight.rs` + `presentation.rs` 的高亮算法移植到
OHOS 渲染路径，并接线 4 个设置回调。

| 文件 | 内容 |
| --- | --- |
| `src/output_highlight.rs`（新） | `Preset{Off,Log,DevOps}`、`CompiledRule{matcher,whole_line,ansi_index}`、`Rules{preset,custom}`；`Rules::from_store(&ConfigStore)` 读 `output_highlight_enabled/preset/rules`；`validate_rule`（空串、>512 字符、非法正则 → 中文错误，上限 128 条，移植自 `src/app/terminal_ui.rs`）；`custom_model(&ConfigStore) -> ModelRc<OutputRuleItem>`（颜色只保留 yellow/green/cyan/magenta/gray，其余归 red）；`highlight(Vec<Run>, &Rules, default_fg) -> Vec<Run>`（先全行/逐条自定义规则，再内置标记，列宽按 `cell_width` 计算）。标记匹配 `log_level_marker` / `devops_marker` / `ascii_word_boundary` 逐行移植；颜色表 `yellow→11 green→10 cyan→14 magenta→13 gray→8 其余→9`。 |
| `src/terminal.rs` | `Run` 与字段改 `pub(crate)`、`palette_rgb` 改 `pub(crate)`；`rebuild` 增加 `rules: &Rules` 参数，逐行产出 run 后交给 `output_highlight::highlight`（仅 `fg==默认前景 && bg=None && !bold && !underline` 的 run 参与，与上游 `custom_rule_eligible` 一致）。 |
| `src/settings.rs` | `apply_saved_appearance` 额外写 `output-highlight-enabled`、`output-highlight-preset`、`output-highlight-rules`；`register` 增加 `highlight: Rc<RefCell<Rules>>` 与 `repaint: Rc<dyn Fn()>`，接线 `set-output-highlight(bool,string)`、`add-output-highlight-rule(...)->bool`、`remove-output-highlight-rule(int)`、`set-output-highlight-rule-enabled(int,bool)`，每次改完重新 `Rules::from_store` 并 `repaint()`。 |
| `src/ohos_app.rs` | `UiState::new` 用配置构建 `highlight`，并构造 `repaint_all`（遍历 `terms` 逐个 `rebuild` + `request_redraw`）；传给 `wire_callbacks`（`terminal-resize` 重建）、`start_poll_timer`（输出到达重建）与 `settings::register`。 |
| `src/lib.rs` / `Cargo.toml` | `mod output_highlight;`；新增 `regex = "1"`（与 core 同版本，纯 Rust，OHOS 可编译）。 |

关键取舍：

- 只作用于可见屏幕的普通文本 run，ANSI 自带样式的不覆盖（与上游一致）。
- 「格式化并着色 JSON 行」仍不接线：需要改写输出字节流，超出本轮范围，
  留待后续增量。
- 自定义规则参与匹配前统一 `regex::escape`（非正则模式），大小写默认不敏感。
- 高亮是渲染期行为，切换开关/规则后对所有已连接标签立即重建，不重连。

验收（真机）：

| # | 结果 | 证据 |
| --- | --- | --- |
| O1 | 关闭「高亮纯文本终端输出」后，`echo ERROR WARN INFO` 输出为纯前景色；开启后首个级别词变粗且着色（ERROR 红、WARN 黄、INFO 青）。 | 截图 |
| O2 | 内置规则集切到 DevOps 后，`echo SUCCESS FAILED HEALTHY` 分别着色（绿/红/绿），切回日志级别则不再着色。 | 截图 |
| O3 | 自定义规则（关键词 `hello`、红色、整行）新增后输出包含 `hello` 的行整行变红；关闭该规则开关后恢复；删除规则后列表为空。 | 截图 |
| O4 | `output_highlight: false`（或 preset 变更）与规则数组正确写入 `sessions.json`，强停重启后开关、预设、规则列表均保留。 | `sessions.json` + 重启截图 |
| O5 | 回归：终端输入、字号/主题、SFTP 面板、资源面板、会话列表均正常；测试后恢复默认（高亮开、预设日志级别、清空自定义规则）。 | 截图 |

### 19.1 真机结果（全部通过）

| # | 结果 | 证据 |
| --- | --- | --- |
| O1 | 通过：开启时 `echo ERROR WARN INFO` 输出的首个级别词着色（ERROR 红、WARN 黄、INFO 青，粗体）；关闭主开关并关闭对话框后，同一行立即回退为纯前景色，无需重连。 | `/tmp/o8.jpeg`（开启）、`/tmp/o12.jpeg`（关闭） |
| O2 | 通过：内置规则集切「DevOps」后 `echo SUCCESS FAILED HEALTHY` 分别为绿/红/绿；切回「日志级别」后同一行立即恢复无色，`ERROR WARN INFO` 仍着色。 | `/tmp/o19.jpeg`、`/tmp/o21.jpeg`、`/tmp/o23crop2.png` |
| O3 | 通过：输入框回车新增关键词规则 `hello`（红色、大小写不敏感）后，`echo hello world` 中 `hello` 变红而 `world` 不变；把该规则改为整行后在重启后整行（含回显命令行）变红；`enabled:false` 后不再着色。 | `/tmp/o31crop.png`、`/tmp/o33.jpeg`、`/tmp/o34.jpeg` |
| O4 | 通过：`sessions.json` 中 `output_highlight_disabled`、`output_highlight_preset`、`output_highlight_rules[]` 与 UI 操作一致；force-stop 重启后开关、预设与规则仍然生效（重启后直接打开会话即按整行规则着色）。 | `/tmp/o32.jpeg`、`/tmp/o33.jpeg`、`sessions.json` |
| O5 | 通过：终端输入/输出、SFTP 双栏、资源面板 2s 刷新、左栏会话列表均正常；测试后已恢复默认（`output_highlight_disabled:false`、预设 `log`、自定义规则清空）。 | `/tmp/o35.jpeg` |

### 19.2 发现的问题（记录，非本增量引入）

- 设置页在 704px 高的窗口里内容被底部裁剪，且面板 ScrollView 对触摸拖拽
  （`drag` / `fling` / `dircFling`）无响应，导致「输出高亮」页下部的
  关键词/颜色下拉、整行/正则/大小写开关、「添加」「删除」按钮不可达。
  本轮用输入框回车（上游 `accepted` 回调）完成新增，规则的 `enabled` /
  `whole_line` 通过改 `sessions.json` 后重启验证渲染与加载。
  该裁剪来自上游面板布局在小屏下的固定高度，需要后续单独增量处理
  （例如对话框高度自适应或让 ScrollView 可滚动）。
  **→ 已定位根因并在 §24 修复（触摸事件被当成鼠标事件转发，导致 Flickable 拖拽被禁用）。**
- `uitest uiInput text` 在多次注入偶发 caps 锁定（注入文本被全大写），
  用 `keyEvent 2047`（Shift）恢复；属自动化工具限制，与端口无关。

## 20. 下载落点改为系统公共下载目录（增量 7）

此前 SFTP 下载统一写到 HAP 沙箱 `files/downloads`，用户在「文件管理」里看不到。
本增量按《开源鸿蒙文件下载》的做法，申请 `ohos.permission.READ_WRITE_DOWNLOAD_DIRECTORY`
后把公共 Download 目录交给 Rust，下载静默写入，不弹文件选择器。

| 文件 | 内容 |
| --- | --- |
| `entry/src/main/module.json5` | `requestPermissions` 增加 `ohos.permission.READ_WRITE_DOWNLOAD_DIRECTORY`（`reason: $string:download_dir_reason`，`usedScene.when: inuse`）。 |
| `entry/src/main/resources/base/element/string.json` | 新增 `download_dir_reason`：「把 SFTP 下载的文件保存到系统公共下载目录」。 |
| `entry/src/main/ets/pages/SlintSpike.ets` | `attachSurface` 成功后 `prepareDownloadDir()`：`abilityAccessCtrl.requestPermissionsFromUser` 申请权限 → 成功取 `Environment.getUserDownloadDir()`；随后把候选目录（API 返回值、`/storage/Users/currentUser/Download`、`/storage/media/100/local/files/Docs/Download`）逐个用 `canWriteDir`（建探针文件再删）验证可写，命中即 `meatshell.slintSetDownloadDir(dir)`。全部不可写则保持沙箱兜底。 |
| `entry/src/main/cpp/napi_init.cpp` / `Index.d.ts` | 新增 `slintSetDownloadDir(dir): number`，走命令队列 `CommandKind::DownloadDir`，worker 调 `ms_spike_set_download_dir`。 |
| `src/lib.rs` | 新增 FFI `ms_spike_set_download_dir`：UI 已建好时调 `UiState::set_download_dir`，否则先写静态目录。 |
| `src/ohos_app.rs` | 新增 `PUBLIC_DOWNLOAD_DIR` 静态 + `set_public_download_dir`；`downloads_dir()` 优先返回公共目录（并 `create_dir_all`），未设置时退回沙箱；`UiState::set_download_dir` 把仍停留在旧目录的 SFTP 标签切到新目录并重建本地栏；状态文案「已下载到沙箱」改「已下载到下载目录」。 |

关键取舍：

- 公共目录是「可写目录路径」而非 URI：Rust 侧仍是普通 `File::create`，无需把
  文件流搬回 ArkTS；权限由公共目录授权保证，进程内全链路可用。
- 每次启动都重新解析落点：权限被撤销时自动退回沙箱，不会出现下载失败。
- 目录切换只影响本地栏仍指向旧目录的标签，用户手动改过目录的标签不被动。

### 20.1 真机结果

| # | 结果 | 证据 |
| --- | --- | --- |
| P1 | 通过：权限到位后启动日志为 `download dir ready: /storage/Users/currentUser/Download`，探针文件建删成功。 | hilog |
| P2 | 通过：双栏 SFTP 双击远端 `.bash_history`（1.6 KB），状态栏显示「下载完成，.bash_history」，文件落到公共目录 `/storage/media/100/local/files/Docs/Download/.bash_history`（1680 字节，md5 `c3220fc3ec533fd86005ba775caf8a96`），同时可见于 `/storage/cloud/100/files/Docs/Download`。 | 截图 + `ls/md5sum` |
| P3 | 通过：测试文件已删除，应用与设备状态复位。 | — |

### 20.2 设备侧缺陷（非应用问题）

- **权限弹窗模板错乱**：请求 `READ_WRITE_DOWNLOAD_DIRECTORY` 时，本机 PermissionManager
  弹出的是「位置信息」授权框（按钮「取消 / 仅使用期间允许」），点允许后实际尝试授予
  `LOCATION` / `LOCATION_IN_BACKGROUND` / `APPROXIMATELY_LOCATION`（应用未声明，
  全部失败），对被请求权限返回 `authResults[0] = -1`。反汇编 `permission_manager`
  可见 `DownloadDirectoryStrategy` / `PermissionGroup.DOWNLOAD_DIRECTORY` 均存在，
  但设备 `permission_definitions.json` 中该权限缺少 `permissionGroup` 字段，运行时
  落到 Location 策略。`requestPermissionOnSetting` 在本机抛 `Common inner error`。
- **ACL 预授予无效**：在 `signing/profile.json` 的 `allowed-acls` 加入该权限并重新签名
  安装（含全新安装）后 `grantStatus` 仍为 `-1`（ACL 只放宽等级，不预授权 user_grant）。
- **`Environment.getUserDownloadDir()` 不可用**：本机调用直接抛 801
  （`The device doesn't support this api`），实际落点由候选路径探测得到。
- 结论：本机只能通过 root 预授权（`atm perm -g -i <tokenId> -p ohos.permission.READ_WRITE_DOWNLOAD_DIRECTORY`）
  让功能生效；代码路径本身与正常 ROM 一致。覆盖此类 ROM 的做法见 §20.3。
- 附带修复：重装后应用 uid 变化（20020067 → 20020074），回填的
  `files/` 属主不符导致会话列表为空的假象，`chown` 后恢复。

### 20.3 picker 兜底

拿不到公共目录权限时不能让下载卡在沙箱里，因此补一条系统保存对话框的兜底：

| 文件 | 内容 |
| --- | --- |
| `src/ohos_app.rs` | 新增 `PENDING_SAVE_EXPORT` 队列与 `public_download_ready()`；`SftpTransfer` 完成且为下载、公共目录未就绪时，用 `finished_local_path` 定位实际落盘文件（`KeepBoth` 会把重名改成 `stem (n).ext`，取同前缀里最新的一个）并入队；`take_action` 先出窗口动作、再出 `save-download:<本地路径>`。 |
| `SlintSpike.ets` | `applyWindowAction` 识别 `save-download:` → `exportDownload(src)`：`picker.DocumentViewPicker.save()` 预填原文件名，用户可选位置，再用 `fs.copyFileSync(src, destFd)` 把沙箱文件写进返回的 URI。取消则只打日志，沙箱副本保留。 |

行为：有权限时完全静默写公共目录；无权限时每次下载弹一次系统保存框（本机实测
对话框默认停在「此电脑」，用户点左侧「下载」+「保存」即可）。

真机验证（临时在 attach 后手动调 `exportDownload`，测完已移除）：

- 对话框正常弹出，文件名预填 `acme.sh`；
- 选「下载」→「保存」后落盘 `/storage/media/100/local/files/Docs/Download/acme.sh`
  （1295 字节，内容一致），日志 `download exported: acme.sh -> file://docs/storage/Users/currentUser/Download/acme.sh`；
- 取消路径日志 `save dialog cancelled`，无异常。
- 自动化注意：系统保存对话框不接收 `uinput -M` 注入的指针事件，需用
  `uitest uiInput click <x> <y>`（坐标 = 截图坐标 × 1.33）。

---

# 增量：系统文件选择器上传（Upload picker）

状态：已实施并真机验证（2026-11-15，1366×768 / bundle com.tdyso.khshell）
范围：只改 `ohos/meatshell-slint/src/` 与 ArkTS 宿主 `khmeatshell/entry/src/main/`；
上游 `src/`、`ui/` 零改动。

## 21. 背景

`sftp_panel.slint` 的远端上传菜单（「上传文件 / 上传文件夹」）已经接到
`upload-clicked(current-path, folder)` → `sftp-upload-clicked(tab-id, remote-dir, folder)`
（`ui/app.slint:1337/2375`），但 `ohos_app.rs:1918-1925` 是桩，只提示
「OHOS 暂无文件选择器」；`on_sftp_paste_upload` 同样只提示。

引擎侧上传能力已完备：`SftpCommand::Upload { local, remote_dir }`
（`ohos/meatshell-core/src/sftp_facade.rs:221-224,288-292`）在
`src/sftp/impls/sftp.rs:1007-1032` 判定 `local` 为目录时走
`upload_dir`（:2958，递归 mkdir + pipelined 上传，保留根目录名），
文件走 `upload_pipelined`（:3002）。缺少的只是「选文件」这一层。

## 21.1 设计

数据流：远端面板点上传 → Rust 记录目标 tab → `take_action` 输出
`pick-upload:file|folder` → ArkTS 弹系统 picker → 选中项复制到沙箱暂存 →
`slintUploadFiles` → Rust 复用 `SftpCommand::Upload` 传到该 tab 当前目录。

| 文件 | 改动 |
| --- | --- |
| `ohos/meatshell-slint/src/ohos_app.rs` | 新增静态 `PENDING_UPLOAD_PICK: Mutex<Option<(String /*tab*/, bool /*folder*/)>>` 与 `UPLOAD_DEST_TAB: Mutex<Option<String>>`；把 `on_sftp_upload_clicked` 桩替换为「未连接则提示，否则入队 + 状态栏『正在打开文件选择器…』」；`take_action` 在窗口动作、`save-download:` 之后取走上传请求，把 tab 存入 `UPLOAD_DEST_TAB` 并返回 `pick-upload:file` / `pick-upload:folder`（**取走**，否则 100 ms 轮询会重复弹窗）；新增 `UiState::upload_picked_files(&self, paths: &[PathBuf])`：读 `UPLOAD_DEST_TAB` + 该 tab `sftp_path`，逐个发 `SftpCommand::Upload`，状态栏 `↑ 上传 N 项到 <dir>…`，最后重绘；`upload_pick_cancelled` 清 `UPLOAD_DEST_TAB` 并复位状态栏。 |
| `ohos/meatshell-slint/src/lib.rs` | 新增 `#[no_mangle] ms_spike_upload_files(paths: *const c_char) -> i32`：按 `\n` 切分（避免为一条消息新增 serde 依赖），空串返回 0；渲染线程内转调 `upload_picked_files`。新增 `ms_spike_upload_cancel() -> i32` 转调 `upload_pick_cancelled`。 |
| `khmeatshell/entry/src/main/cpp/napi_init.cpp` | 声明 `ms_spike_upload_files` / `ms_spike_upload_cancel`；`CommandKind::UploadFiles`、`UploadCancel` 与对应分发分支；`JSlintUploadFiles`（字符串入队）、`JSlintUploadCancel`；`Init` 注册两个方法。 |
| `khmeatshell/entry/src/main/cpp/types/libmeatshell/Index.d.ts` | `export const slintUploadFiles: (pathsJoinedByNewline: string) => number;`、`export const slintUploadCancel: () => number;` |
| `khmeatshell/entry/src/main/ets/pages/SlintSpike.ets` | `applyWindowAction` 识别 `pick-upload:` → `pickUpload(action.endsWith('folder'))`；新增 `pickUpload(folder)` 与 `uriName(uri)`、`copyFileUri(src, dest)`、`copyDirUri(src, dest)`、`removeTree(root)`。 |

### picker 调用

```ts
const p = new picker.DocumentViewPicker(getContext(this) as common.UIAbilityContext);
const opts = new picker.DocumentSelectOptions();
if (folder) {
  opts.selectMode = picker.DocumentSelectMode.FOLDER; // 一次只能选一个目录
  opts.maxSelectNumber = 1;
} else {
  opts.maxSelectNumber = 20;                          // 多选
}
const uris: string[] = await p.select(opts);
```

`DocumentSelectOptions.maxSelectNumber`（picker.d.ts:471）与
`selectMode`（:480）、`select(option?): Promise<Array<string>>`（:679）已核对。

### 沙箱暂存

picker 返回 `file://docs/...` URI，SFTP 工作线程只能读真实路径，且公共目录
之外的位置不在应用沙箱内，因此先复制到 `${getContext(this).filesDir}/upload-staging/`：

- 暂存名 = URI 末段经 `decodeURIComponent` 还原的原文件名 —— Rust 用本地文件名
  作为远端文件名，改名会改变落点。
- 文件：`fs.openSync(uri, READ_ONLY)` → `fs.openSync(dest, CREATE|WRITE_ONLY|TRUNC)`
  → `fs.copyFileSync(srcFd, destFd)`，两个 fd 都 `closeSync`。
- 目录（实测踩坑后定稿）：`fs.copy` 对 docs URI 报 `13900020`；
  `fs.listFile(docs URI)` 报 `13900002`；`fs.uri` 子项不在授权范围。可行组合是
  **去掉 `file://docs` 前缀得到应用可见路径**（`/storage/Users/currentUser/...`，
  与 `Environment.getUserDownloadDir()` 同一命名空间）列举子文件，再逐个读到暂存；
  读内容优先用该路径，失败时退回原 docs URI。`fs.accessSync` 返回 boolean 而不抛
  异常，父目录必须据此判断后 `mkdirSync`。Rust 侧对目录走 `upload_dir`，远端保留
  根目录名；空子目录不会被复制。
- 每次弹出的选择器**之前**清空暂存目录；上传结束**不立刻删**（异步传输仍在读），
  由下一次选择覆盖。取消选择时保留旧暂存内容不动。

取消路径：`select` 返回空数组 → `meatshell.slintUploadCancel()`（Rust 清
`UPLOAD_DEST_TAB` 与状态栏）→ 只 `console.warn('upload picker cancelled')`，
不调用 `slintUploadFiles`，不产生上传任务。

## 21.2 非目标

- 剪贴板上传（`on_sftp_paste_upload` 保持提示）。
- 拖拽上传、上传进度面板/冲突策略增强、双栏本地栏增强。
- 断点续传、暂停/取消（沿用引擎既有 `CancelTransfer`，本轮不新增 UI）。

## 21.3 验收矩阵（真机 1366×768）

| # | 步骤 | 通过标准 |
| --- | --- | --- |
| U1 | 连接 VPS，SFTP 面板右键/点击「上传文件」 | 系统 picker 弹出；状态栏提示「正在打开文件选择器…」 |
| U2 | 从公共「下载」目录选 1 个文件 | 远端当前目录出现同名文件，大小与本地一致（`ls -l` / `md5sum`） |
| U3 | 多选 3 个文件 | 3 个文件都上传成功，状态栏 `↑ 上传 3 项到 <dir>…` 后变为完成态 |
| U4 | 「上传文件夹」→ 选一个含子目录的文件夹 | picker 进入目录模式；远端出现同名目录，递归内容一致 |
| U5 | 打开 picker 后取消 | 无上传、无残留状态栏消息；暂存目录保持原样 |
| U6 | 上传与远端已有文件同名的文件 | 行为与双栏上传一致（记录覆盖结果） |
| U7 | 回归 | 双栏上传、下载、终端输入、快捷命令均正常；`upload-staging` 下次选择前被清空 |

## 21.4 构建与验证

```bash
cd ~/DevEcoStudioProjects/khmeatshell && bash deploy.sh   # x86_64 + aarch64
```

真机：`hdc shell uitest uiInput click <x> <y>` 操作应用与系统 picker 对话框，
坐标与 1366×768 截图 **1:1**（应用窗口 rect 从 x=25 起，点击由系统直接投递到窗口，
无需换算）；`uinput -M` 注入对系统对话框无效（见 §20.3）。`uitest uiInput help`
仅支持 click/doubleClick/longClick/swipe/drag/fling/keyEvent/text，**无法注入
Ctrl+单击**，故 picker 的多选只能人工操作。

## 21.5 需要批准

1. 范围仅「系统文件选择器上传」，不做剪贴板/拖拽/进度增强。
2. 暂存策略：复制到 `${filesDir}/upload-staging`，保留原文件名，下一次选择前清空。
3. `slintUploadFiles` 采用 `\n` 分隔路径而非 JSON。

## 21.6 真机结果

| # | 结果 | 证据 / 说明 |
| --- | --- | --- |
| U1 | 通过 | 远端面板「上传」→「上传文件 / 上传文件夹」后系统 picker 弹出，面板状态「正在打开文件选择器…」 |
| U2 | 通过 | 选 `Download/0up.txt`：日志 `upload staged 1`，远端 `/root/0up.txt` 的 `md5sum 81d77eff09b9ef81f5a49d41207bad92` 与本地一致，列表出现 `updir`/文件行 |
| U3 | 部分（自动化限制） | 代码按 `maxSelectNumber = 20`、`uris` 循环、`\n` 拼接入 Rust 并逐项 `SftpCommand::Upload`；但本机 picker 的多选需 **Ctrl+单击**，`uitest uiInput` 无修饰键、`keyEvent 2072 2017`（Ctrl+A）与拖拽框选均无效，无法脚本化验证。多文件链路由 U4（目录含 2 文件）间接覆盖 |
| U4 | 通过 | 选 `Download/updir`（含 `t.txt`、`sub/n1.txt`）：暂存 `updir/{t.txt,sub/n1.txt}` 完整；远端 `find updir` 输出 `updir`、`updir/t.txt`、`updir/sub`、`updir/sub/n1.txt`；面板状态「上传完成：updir」 |
| U5 | 通过 | 打开选择器后点「取消」：日志 `upload picker cancelled`，无上传；`slintUploadCancel` 把状态栏清空，不残留「正在打开文件选择器…」 |
| U6 | 通过 | 同名覆盖：本地 `ow.txt` 由 `v1` 改为 `v2-overwrite` 后重传，远端 md5 由 `f490e3a9…` 变为 `607d0158…`，与双栏上传同走 `SftpCommand::Upload` |
| U7 | 通过 | 终端输入（`find updir`、`md5sum ow.txt`）、快捷命令菜单、双栏/状态刷新正常；每次弹出 picker 前 `upload-staging` 被清空（`total 0` 后重新写入） |

实施偏差与坑：

- 目录复制不能一次 `fs.copy(docsUri, dest)`（`13900020` Invalid argument），改为
  「去 `file://docs` 前缀列举 + 逐文件复制」，见 §21.1 目录一节。
- `fs.accessSync` 返回 boolean，不抛异常；据此判断父目录是否存在再 `mkdirSync`，
  否则子目录文件写入报 ENOENT。
- 取消需要专门回传 `slintUploadCancel`，否则状态栏停在「正在打开文件选择器…」。
- 自动化：`uitest uiInput` 坐标为屏幕 1:1；`uinput -K -t` / `uitest uiInput text`
  会丢弃 `-`、`/`、`*`，验证远端时改用无特殊字符命令（`md5sum xxx`、`cd`/`rmdir`
  分步删除），必要时用 `keyEvent 2049`（Tab）补全带 `-` 的文件名。
- 测试数据已清理：远端 `/root` 的 `0up.txt`、`up-a.txt`、`ow.txt`、`updir/`；
  本地 `Download` 下的 `0up.txt`、`up-a.txt`、`up-b.txt`、`up-c.txt`、`multi/`、
  `updir/`、`ow.txt`；沙箱 `upload-staging/*` 已清空。

---

# 增量：设置菜单的导入 / 导出连接

状态：已实施（2026-11-15）
范围：只改 `ohos/meatshell-slint/src/` 与 ArkTS 宿主 `khmeatshell/entry/src/main/`；
上游 `src/`、`ui/` 零改动（`ui/app.slint:3438-3463` 点完即 `settings-open = false`
的行为保持不变）。

## 22. 背景与根因

`ui/app.slint` 的设置菜单有两项（`ui/app.slint:3432-3474`）：

```slint
clicked => { root.settings-open = false; root.export-sessions(); }   // 导出连接
clicked => { root.settings-open = false; root.import-sessions(); }   // 导入连接
```

移植版此前已接上回调，但用户点击后看不到任何效果：

1. **导出**：写到沙箱 `<filesDir>/exports/meatshell-connections-<stamp>.json`
   （设备上已积了 3 个文件），文件管理器看不到；
2. **导入**：只认预先放进沙箱 `<filesDir>/import/meatshell-connections.json` 的
   文件，该目录为空且用户无法写入；
3. 结果只通过 `ssh-import-hint` 显示，而菜单在调用前就已关闭 → 提示无处渲染。

## 22.1 设计

| 文件 | 改动 |
| --- | --- |
| `ohos_app.rs` | 新增 `IMPORT_PICK_REQUESTED`（`AtomicBool`）、`PENDING_TOAST`（`Mutex<Vec<String>>`）、`IMPORT_PICKED_HANDLER`（`thread_local` 的 `Box<dyn Fn(&Path)>`）与 `queue_toast` / `queue_save_export` / `request_import_pick` / `set_import_picked_handler` / `import_picked_file`；`take_action` 在窗口动作之后依次返回 `toast:<文本>`、`save-download:<路径>`、`pick-import`；`downloads_dir()`、`public_download_ready()` 提升为 `pub(crate)`。 |
| `session_editor.rs` | `on_export_sessions`：写 `downloads_dir()`（公共下载目录，未授权时落沙箱 `downloads/` 并入队 `save-download` 走系统保存对话框），文件名 `meatshell-connections-<unix 秒>.json`，结果 `queue_toast` + `set_ssh_import_hint`。`on_import_sessions`：注册 `set_import_picked_handler` 后 `request_import_pick()`；回调里 `import_from(path)`（上游 `src/config/impls/import.rs:278`），`added > 0` 才 `save()`，刷新会话列表并 toast「已导入 N 个会话（跳过 M）」。删除已无用的 `export_dir()`；`import_dir()` 保留给 ssh_config 导入。 |
| `lib.rs` | 新增 `ms_spike_import_picked(path: *const c_char) -> i32`。 |
| `napi_init.cpp` | `CommandKind::ImportPicked` + 分发分支、`JSlintImportPicked`、注册 `slintImportPicked`、声明 `ms_spike_import_picked`。 |
| `Index.d.ts` | `slintImportPicked: (path: string) => number`。 |
| `SlintSpike.ets` | `applyWindowAction` 识别 `pick-import` → `pickImport()`、`toast:` → `showToast()`；`pickImport()` 用系统文件选择器（`maxSelectNumber = 1`、`fileSuffixFilters = ['连接文件|.json']`）选 1 个 JSON，复制到 `${filesDir}/import/<原名>` 后调 `slintImportPicked`；`showToast()` 走 `win.getUIContext().getPromptAction().showToast({ message, duration: 2500 })`。 |

导出文件格式由上游 `export_json()`（`src/config/impls/config.rs:2006`）生成：
`{"meatshell_export":1,"sessions":[...]}`，密码/内联私钥/触发器回复用内置导出密钥
重新加密（跨设备可读），`last_used` 不导出。导入端同文件 `import_from` 解析，
按 name 或 `host+user` 去重（重复计入 skipped）。

## 22.2 非目标

- 不新增「导出到指定目录」的二级菜单（授权可用时直接写公共下载目录，不可用时
  回退系统保存对话框）。
- 不新增导入预览/勾选界面（沿用上游 `import_from` 的整包合并语义）。

## 22.3 验收矩阵（真机 1366×768）

| # | 步骤 | 通过标准 |
| --- | --- | --- |
| I1 | 设置 → 导出连接 | 无崩溃；toast「已导出 N 个连接到 meatshell-connections-<stamp>.json」；文件出现在公共下载目录，大小与内容为合法导出 JSON |
| I2 | 设置 → 导入连接 | 系统文件选择器弹出（单选、过滤 `.json`） |
| I3 | 选中 I1 导出的文件 | toast「已导入 0 个会话（跳过 N）」；会话列表数量不变、无重复 |
| I4 | 选择器取消 | 无 toast、无异常日志、列表不变 |
| I5 | 选中非法 JSON | toast 以「导入失败：」开头，列表不变 |
| I6 | 回归 | 双击会话连接、SFTP 面板、终端输入、上传/下载均正常 |

## 22.4 构建与验证

```bash
cd ~/DevEcoStudioProjects/khmeatshell && bash deploy.sh   # x86_64 + aarch64
```

自动化提示（沿用 §21.4）：`uitest uiInput click` 坐标与 1366×768 截图 1:1；picker
里进入「下载」目录后再点文件名；`hdc shell ls -l /storage/media/100/local/files/Docs/Download/`
校验导出结果。日志：`hdc shell hilog | grep meatshell`，关注 `import picked:`。

## 22.5 真机结果

| # | 结果 | 证据 / 说明 |
| --- | --- | --- |
| I1 | 通过 | 设置 → 导出连接：toast「已导出 3 个连接到 meatshell-connections-1794732342.json」（重建后 `…1794733627.json` 同样通过）；文件出现在 `/storage/media/100/local/files/Docs/Download/`，3949 字节，与沙箱旧导出同大小 |
| I2 | 通过 | 设置 → 导入连接弹出系统文件选择器（单选）；`fileSuffixFilters = ['JSON|.json']` 在本机 ROM 上未生效（「文件类型」仍显示「所有文件(*)」，`.txt` 仍可见），不影响功能 |
| I3 | 通过 | 选中导出的 JSON：toast「已导入 0 个会话（跳过 3）」，会话列表仍为 dev / mfa / vps 3 项，无重复；选中的文件被复制到 `<filesDir>/import/` |
| I4 | 通过 | 选择器点「取消」：回到应用、无 toast、列表不变，沙箱 `import/` 无新文件 |
| I5 | 通过 | `Download/badimport.json`（内容 `not a json file`）：toast「导入失败：invalid import JSON at line 1, column 2」，列表不变 |
| I6 | 通过 | 双击 `vps` 连接成功（`root@vps-host:~#`），SFTP 面板列目录正常；「上传文件」仍弹出选择器（状态栏「正在打开文件选择器…」） |

实施偏差与坑：

- `ssh-import-hint` 只在设置菜单内与欢迎/会话编辑面板显示，而菜单点击前就已
  `settings-open = false`；首版把结果同时写进 hint，结果欢迎栏永久残留截断的
  「导入失败: invalid im…」。最终**只发 toast**，不再写 hint。
- toast 走 `win.getUIContext().getPromptAction().showToast({ message, duration: 2500 })`
  （顶层 `promptAction` 已废弃）；`toast:` / `pick-import` 必须在 `applyWindowAction`
  里先于窗口动作分发并 `return`，否则会被当成未知窗口动作。
- 导出的 JSON 由上游 `export_json()` 生成，密码用**内置导出密钥**重新加密，
  因此导入端能把密码还原；`last_used` 不导出。
- 导入**合并**语义：同一 name 或 `host+user` 已存在则跳过（计入「跳过 N」），
  `added > 0` 才 `save()`，因此重复导入不会改动 `sessions.json`。
- 清理：`Download` 下的 `meatshell-connections-*.json`、`badimport.json`、沙箱
  `import/*.json`、`upload-staging/*`、历史遗留的 `files/exports/` 均已删除；
  应用回到欢迎页（无残留标签）。

---

# 增量：会话编辑对话框的「测试连接」与「选择私钥」

状态：已实施、已部署，真机验收 T1–T7 全部通过（2026-11-16，见 §23.5）
范围：只改 `ohos/meatshell-slint/src/` 与 ArkTS 宿主
`khmeatshell/entry/src/main/`；上游 `src/`、`ui/` 零改动。

## 23. 背景与根因

对话框底部「测试连接」按钮（`ui/session_dialog.slint:1949-1963`）点击后先写
`test-status = "Testing..."` 再发 `test-connection(draft)`，但移植版的处理是写死一句
提示：

```rust
ui.on_session_dialog_test(move |_draft: SessionDraft| {
    w.set_dialog_test_status("OHOS 版暂不支持连接测试，将在接入真实 SSH 后可用".into());
});
```

而这句提示本身已经过期：`ssh_session.rs` 早已提供完整的连接、主机指纹、密码 /
密钥 / MFA 与失败上报（`SshSession::spawn` + `Event::{Connected,Failed}` +
`PromptGate`），`ohos_app.rs:2461-2493` 也已有把 `Session` 解析成 `Auth` 的逻辑，
只是内联在 `connect_session` 里没有复用。同理，「密钥」页的「浏览…」按钮
（`ui/session_dialog.slint:1309-1326`）只回一句「请切换到内联私钥并粘贴」，而系统
文件选择器在 §21 / §22 已经接好，可以直接复刻。

## 23.1 设计（文件级改动）

| 文件 | 改动 |
| --- | --- |
| `ohos_app.rs` | ① 把 `connect_session` 内的鉴权解析（`AuthMethod::Password/KeyboardInteractive/Key` + 内联密钥 + 路径读文件失败时按文件名回落 `MEATSHELL_DATA_DIR` + `default_identity_key()`）原样抽成 `pub(crate) fn auth_for_session(session: &Session) -> Auth`，`connect_session` 改为调用它，行为不变。② 新增 `PENDING_DIALOG_STATUS: Mutex<Option<String>>`、`pub(crate) fn queue_dialog_status(&str)`、`fn take_dialog_status() -> Option<String>`；`poll_sessions` 在遍历标签前先排空：有值则 `ui.set_dialog_test_status(message.into())` 并 `request_redraw()`（结果由 SSH 线程以 100 ms 节奏产生，16 ms 轮询写入足够）。③ 新增 `PENDING_KEY_PICK: AtomicBool` + `pub(crate) fn request_key_pick()`，`take_action` 在 `pick-import` 之后返回 `pick-key`；新增 `KEY_PICKED_HANDLER`（thread_local `Box<dyn Fn(&Path)>`）+ `set_key_picked_handler` + `pub fn key_picked_file(path) -> Result<(), String>`。 |
| `session_editor.rs` | `register_dialog` 内维护 `test_cancel: Rc<RefCell<Option<Arc<AtomicBool>>>>`，供 test / stop / changed / closed / submit 共享。`on_session_dialog_test(draft)`：先取消上一次测试；`session_from_draft(&draft, store.get(&draft.id), vec![], vec![])`（空密码 / 空内联密钥沿用已存值；不启端口转发，避免测试产生监听副作用）；`kind != Ssh` → 状态「仅 SSH 会话支持连接测试」；host 为空 → 「请输入主机地址」；否则 `auth_for_session`，立即写「正在测试连接…」，`std::thread::spawn` 持有 `SshSession`，每 100 ms `drain()`，12 s 截止：`Connected` → 「连接正常」（与上游 `t("连接正常","Connection OK")` 一致，界面因此显示成功色）、`Failed(e)` → 「连接失败: {e}」、`gate.pending()` 为 `HostKey` → 「目标主机指纹未信任，请先打开该会话完成确认」并结束（绝不静默 pin）、`Credentials` → 按 `need_user` 给「请填写用户名和密码」/「请填写密码」、`Mfa` → 「该主机需要交互式验证码，请打开会话登录后再测试」、超时 → 「连接超时: user@host:port」；结果经 `queue_dialog_status`，若期间已取消则丢弃。`on_session_dialog_stop_test`：置取消位、清空状态。`on_session_dialog_changed` / `closed` / `submit`：同样取消在跑的测试（submit 前 UI 已先发 `stop-test`）。`on_session_dialog_pick_key`：注册 `set_key_picked_handler`（回填 `set_dialog_key_path(path)`、清测试状态）后 `request_key_pick()`。 |
| `lib.rs` | 新增 `ms_spike_key_picked(path: *const c_char) -> i32`。 |
| `napi_init.cpp` | `CommandKind::KeyPicked` + 分发分支、`JSlintKeyPicked`、注册 `slintKeyPicked`、声明 `ms_spike_key_picked`。 |
| `Index.d.ts` | `slintKeyPicked: (path: string) => number`。 |
| `SlintSpike.ets` | `applyWindowAction` 识别 `pick-key` → `pickKey()`：`DocumentViewPicker` 单选（`fileSuffixFilters = ['私钥|']`，本机 ROM 可能不生效，不影响功能），复制到 `${filesDir}/keys/<原名>`（`mkdirSync(..., true)` + 复用 `copyFileUri`），调 `meatshell.slintKeyPicked(dest)`；取消时静默返回。 |

## 23.2 非目标

- 不对 Telnet / RDP / Serial 做测试（移植版只有 SSH 引擎），直接返回
  「仅 SSH 会话支持连接测试」。
- 测试忽略草稿里的代理与跳板机（`ConnectConfig` 无这两个字段），直连目标主机；
  也不启动端口转发。
- 不静默信任未知主机指纹：`HostKey` 提示后结束，指纹信任仍走「打开会话」时的
  既有确认流程。
- 不做连接耗时、服务端 banner 等额外信息展示。

## 23.3 验收矩阵（真机 1366×768）

| # | 步骤 | 通过标准 |
| --- | --- | --- |
| T1 | 编辑已保存的 `vps` 会话（密码留空沿用已存），点「测试连接」 | 状态先「正在测试连接…」后变「连接正常」；不新增标签页、不启动 SFTP |
| T2 | 把端口改成 `2222` 再测试 | 12 s 内出现「连接失败: …」或「连接超时: …」，无崩溃 |
| T3 | 在公开密码框里填错误密码再测试 | 出现「连接失败: …」（认证失败） |
| T4 | 点「测试连接」后立刻改主机 / 点「取消」再打开同一会话 | 旧测试被取消，状态被清空且不再被迟到的结果覆盖 |
| T5 | 新建会话，只填 host，不填用户名/密码，点测试 | 出现「请填写用户名和密码」（或主机指纹提示），不会卡住 |
| T6 | 密钥认证页点「浏览…」选择 `id_ed25519` | 系统选择器弹出；选中后路径框回填 `${filesDir}/keys/id_ed25519`；保存后该会话可正常登录 |
| T7 | 回归 | 双击连接、SFTP 上传/下载、导入/导出连接、设置外观均正常 |

## 23.4 构建与验证

```bash
cd ~/DevEcoStudioProjects/meatshell/ohos/meatshell-slint
CARGO_TARGET_DIR=/tmp/meatshell-slint-target cargo check --release \
  --target x86_64-unknown-linux-ohos
cd ~/DevEcoStudioProjects/khmeatshell && bash deploy.sh   # x86_64 + aarch64
```

真机：`hdc shell hilog | grep meatshell` 关注 `key picked:`；测试连接不落任何持久
状态（`known_hosts` 仅在既有会话流程中写）。验收后清理 `files/keys/` 里的测试
密钥并恢复应用默认状态（欢迎页）。

## 23.5 真机验收记录（2026-11-16，1366×768，Kaihong x86_64）

测试环境（每次验收前重置，否则坐标与滚轮都不可用）：

1. `hdc tconn 192.0.2.145:10178`；`snapshot_display` 截图为 1366×768。
2. `ui_scale` 必须为 100：`/data/app/el2/100/base/com.tdyso.khshell/haps/
   entry/files/sessions.json` 内 `ui_scale` 置 100，`hdc file send` 覆盖后重启应用。
3. 取得 (0,0) 客户区偏移：先关闭所有对话框（ModalLayer 会吞掉标题栏点击），
   再点应用自绘最大化按钮（屏幕 `1262,19`），`hidumper -s WindowManagerService
   -a '-a'` 确认窗口矩形为 `0,0 1366x722`。
4. 坐标标定：以 `JSAPP: touch Down px=…` / `JSAPP: mouse … at X,Y` 日志为准；
   **内容坐标 = 屏幕坐标 − 客户区偏移**。

| # | 结果 | 证据（截图/日志） |
| --- | --- | --- |
| T1 | **通过** | 编辑 `vps` 会话（密码留空）→ 状态「正在测试连接…」→ 绿色「连接正常」；标签页数量不变，未出现 SFTP 面板 |
| T2 | **通过** | 端口改 `2222` → 12 s 内「连接失败: 连接 203.0.113.10…」；应用无崩溃 |
| T5 | **通过** | 空主机 →「请输入主机/IP 地址。」；有主机无凭据 →「请填写用户名和密码」 |
| T3 | **通过** | 新建会话填 `203.0.113.10` / `root` / `wrongpass` → 先「正在测试连接…」后「连接失败: root@203.0.113.10…」；仅原 1 个标签，无新标签、无 SFTP（`/tmp/t3a.jpeg`、`/tmp/t3b.jpeg`） |
| T4 | **通过** | 点「测试连接」后 1 s 内点「取消」→ 重开新建会话：状态区为空；再等 14 s 仍为空（迟到结果被取消位丢弃），标签数不变（`/tmp/t4b.jpeg`、`/tmp/t4c.jpeg`） |
| T6 | **通过** | 「密钥」页「浏览…」→ 系统 `DocumentViewPicker` → 下载目录选 `id_ed25519`：路径回填 `/data/storage/el2/base/haps/entry/files/keys/id_ed25519`，沙箱副本 78 B，hilog 有 `key picked:`（`/tmp/t6b.jpeg`–`/tmp/t6d.jpeg`） |
| T7 | **通过** | 连接 `vps` 得到提示符；终端 `ls` 有输出；SFTP 双击下载 `.bash_history`（状态「下载完成」）与菜单上传 `id_ed25519`（状态「上传完成」）；齿轮菜单导出 toast「已导出 3 个连接…」；设置字体页「终端字体加粗」开关即时生效且 `terminal_bold` 落盘，随后已复原（`/tmp/t7a.jpeg`–`/tmp/t7q.jpeg`） |

### 23.5.1 自动化输入定式（本轮最终方案）

文本框一律「点击字段 → `uinput -K -t <text>` → `uinput -K -l 2050 3000`（空格提交）」：

- 只发 `-K -t` 时文本停在 IME 浮层（左上角候选条）不提交；补一次空格键即提交，
  验证过 `root`、`wrongpass` 均正确进入目标字段。
- 删除用 `uinput -K -l 2055 3000`，每按一次删 1 字符。
- 终端命令里 `-`、`*` 会被 `-K -t` 吞掉，改用 Tab 补全（`uinput -K -l 2049 3000`）
  或避开特殊字符。
- 弹窗菜单项坐标必须从截图精确量取：`uitest uiInput click` 坐标与 1366×768 截图
  1:1，但聊天里显示的是缩放图（×0.75），按缩放图估算会整体偏小（本轮上传菜单
  因此连点 3 次未命中）。

### T3 错误密码

1. 打开「新建会话」对话框，滚到底部（偏移为 0 时滚轮可用）。
2. 主机：点 `690,223` → `uinput -K -l 2055 3000` ×14 清空 → `uinput -K -t 203.0.113.10`。
3. 用户名：点 `683,291` → 逐键 `uinput -K -l <code> 3000`，`r/o/o/t` = 2034/2031/2031/2036。
4. 密码：点 `683,419` → 逐键 `w/r/o/n/g/p/a/s/s` =
   2039/2034/2031/2030/2023/2032/2017/2035/2035。
5. 滚到底部，点「测试连接」`519,661` → 先截「正在测试连接…」，12 s 内截「连接失败: …」。
   **通过标准**：出现认证失败类文案，无崩溃、无新标签页。

### T4 取消迟到的结果

1. 点「测试连接」后立刻点「取消」`753,661`（或关闭对话框 `887,43`）。
2. 重新打开同一会话：状态区为空。
3. 等待 >12 s 再次打开：状态仍为空（迟到的 `queue_dialog_status` 被取消位丢弃），
   标签页数量不变。

### T6 选择私钥文件

1. 会话对话框「认证」切到「密钥」，点「浏览…」。
2. 系统 `DocumentViewPicker` 弹出，选中 `id_ed25519`。
3. 路径框回填 `${filesDir}/keys/id_ed25519`；`hilog | grep "key picked:"` 有对应记录。
4. **通过标准**：回填路径可保存，保存后该会话可用密钥登录（至少能发起连接）。

### T7 回归

双击连接 `vps` → 终端执行 `ls` 有输出；SFTP 上传/下载各一次；导入/导出连接；
设置里拖动外观滑块生效。四项均正常即通过。

## 23.6 收尾清理（已执行）

1. 「新建会话」对话框以 ✕ 关闭，草稿未保存（导出 toast 仍为 3 个连接）。
2. 关闭 `vps`、`mfa` 标签，应用回到 Logo 空态（`/tmp/t9e.jpeg`）。
3. `files/keys/`、`files/upload-staging/`、历史遗留的 `files/id_ed25519`
   `files/ided25519` 已删除；`import/` 为空。
4. `Download` 下的 `id_ed25519`、`meatshell-connections-1794827683.json`、
   `.bash_history` 已删除；远端 `/root/id_ed25519` 已用 `rm` 清除。
5. `sessions.json` 仍为 3 个会话（`dev@127.0.0.1` / `mfa` / `vps`），
   `ui_scale: 100`；`terminal_bold` 已复原为 `false`。
6. 后台 hilog 抓取已停止，`/tmp/cur.jpeg`、`/tmp/hilog3.txt` 等临时文件已删除。

## 23.7 环境限制（已确认，勿重复尝试）

- 客户端偏移：装饰可见 +37 px；非最大化 +25 px；最大化且 `setWindowDecorVisible(false)`
  生效时为 0。滚轮 `uinput -M -s` 与按键 `-M -c` 的坐标恒为 (0,0)，偏移非 0 时
  事件不会送达 → 必须先最大化。
- IME：`uinput -K -t <text>` / `uitest uiInput text` 只在第一个输入框可靠，后续字段
  文本会滞留在左上浮层；**定式见 §23.5.1（`-K -t` 后补一次空格 `-K -l 2050` 提交）**。
- `uitest uiInput keyEvent Back` 会触发系统返回键把应用最小化，禁用。
- 长按删除 `uinput -K -l 2055 3000` 每次只删 1 字符（约 3 s/字符）。
- `setresolution_screen` 只写配置，对运行中的显示不生效。
- 应用仅在启动时隐藏窗口装饰；显示重连后装饰会重新出现，需重启应用恢复。

## 24. 设置页滚动不可用（修复，增量 9）

### 24.1 现象与实测（2026-11-16，1366×768，192.0.2.145:10178）

- 设置浮窗按 `ui/app.slint` 计算为 720×480（`ifd-h = 480px * panel-font`），
  「输出高亮」页内容高于 480，底部（颜色下拉、整行/正则/大小写开关、
  「添加 / 删除」）被卡片底边裁剪。
- 同一份指针事件流：
  - `uitest uiInput drag 900 520 900 200 600` → 内容不滚动；
  - `uitest uiInput drag 700 155 700 265 800`（拖标题栏）→ 卡片下移 93px。
  说明 Down/Move/Up 均到达 Slint，只有 ScrollView 不跟随拖拽。
- `uinput -M -s ±8` 本轮未在 hilog 产生 `axis event`（注入滚轮坐标恒为 (0,0)，
  非最大化时不送达，见 §23.7），不能作为滚动验收手段。
- 设置内容右侧未见可拖动的滚动条滑块（std ScrollView 滚动条 2px，hover 才 6px）。

### 24.2 根因（代码级，已确认）

`ui/interface_panel.slint:777` 使用 std `ScrollView`；其内部 `Flickable` 在
`vendor/i-slint-core/items/flickable.rs:253` 只在
`event.is_from_touch() || self.mouse_drag_pan_enabled()` 时允许拖拽平移，
而 `mouse-drag-pan-enabled` 默认 false（Slint 1.18 桌面行为）。
ArkTS 宿主 `SlintSpike.ets:846-862` 把触摸 Down/Move/Up 全部转成
`slintPointer`（鼠标事件，`touch_finger_id = 0`），Slint 因此判定为非触摸，
拖拽平移被关闭 → 设置页、以及所有 std ScrollView 都无法用手指拖动。
滚轮路径（`PointerScrolled`）不受此判断影响。

### 24.3 方案（只改移植侧，不动上游 `ui/` 与 `vendor/`）

把触摸按真实触摸事件转发给 Slint，鼠标继续走原 `slintPointer`：

1. `ohos/meatshell-slint/src/ohos_app.rs`：新增
   `pub fn touch(x: f32, y: f32, phase: i32) -> Result<(), String>`，
   `WindowEvent::internal(InternalEvent::Touch { id: 0, position, phase })`；
   phase 映射 `0=Started / 1=Moved / 2=Ended / 3=Cancelled`，
   位置与窗口尺寸校验复用现有 `pointer()` 路径。
2. `ohos/meatshell-slint/src/lib.rs`：新增
   `#[no_mangle] pub extern "C" fn ms_spike_touch(x: f32, y: f32, phase: i32) -> i32`
   （未初始化时返回与 `ms_spike_pointer` 一致的错误码并写 last_error）。
3. `~/DevEcoStudioProjects/khmeatshell/entry/src/main/cpp/napi_init.cpp`：
   新增 `CommandKind::Touch`、`JSlintTouch`、`{"slintTouch", ...}` 注册与
   `ms_spike_touch` extern 声明；在监听线程/命令分发处调用。
4. `~/DevEcoStudioProjects/khmeatshell/entry/src/main/cpp/types/libmeatshell/Index.d.ts`：
   `export const slintTouch: (x: number, y: number, phase: number) => number;`
5. `~/DevEcoStudioProjects/khmeatshell/entry/src/main/ets/pages/SlintSpike.ets`：
   `onTouch` 改为 `Down→slintTouch(x,y,0)`、`Move→1`、`Up→2`、`Cancel→3`；
   取点优先 `event.touches[0]`，为空时回退 `event.changedTouches[0]`（否则
   Move 会被直接丢弃）；`onMouse` 分支完全不变。

### 24.4 非目标

- 不改 `ui/*.slint`、不修改 `vendor/i-slint-core`、不打开
  `mouse-drag-pan-enabled`。
- 不改滚轮桥、坐标回退与设置面板尺寸/布局。
- 不实现多指缩放/旋转手势，只转发主手指（id=0）。

### 24.5 验收矩阵（真机 1366×768）

| ID | 步骤 | 期望 |
| --- | --- | --- |
| S1 | 设置 → 输出高亮，向下拖拽内容区 | 内容上滚，底部颜色下拉、整行/正则/大小写、添加/删除全部可见 |
| S2 | 连续拖到底再回拖到顶 | 双向滚动，无跳动/卡死 |
| S3 | 在滚动后点击开关/下拉/添加/删除 | 单击与下拉均正常（点击路径未破坏） |
| S4 | 拖设置标题栏 | 卡片仍可移动（回归） |
| S5 | 终端里用手指拖拽 | 文本选择高亮正常（回归：触摸→指针合成） |
| S6 | 左栏会话列表、SFTP 列表滚动 | 正常（回归） |
| S7 | 鼠标：单击、双击标题栏最大化、滚轮 | 与原行为一致 |

### 24.6 构建与验证

- `cargo check --release --target x86_64-unknown-linux-ohos`（`ohos/meatshell-slint`）
- `bash deploy.sh` → `com.tdyso.khshell`
- 真机按 S1–S7 执行；截图与 hilog（`touch Down/Move/Up`、Slint 错误）留档；
  IME 输入仍按 §23.5.1 定式。

### 24.7 回滚

- 把 `SlintSpike.ets` 的 `onTouch` 恢复为调用 `slintPointer` 即回到旧行为；
  Rust/NAPI 新增符号不影响旧路径。

### 24.8 真机结果（2026-11-16，1366×768，192.0.2.145:10178）

| ID | 结果 | 证据 |
| --- | --- | --- |
| S1 | 通过 | 设置 → 输出高亮，内容区向下拖拽后上滚，底部「关键词 / 颜色下拉 / 区分大小写 / 高亮整行 / 添加规则 / 暂无自定义规则」全部露出（`/tmp/u6.jpeg`） |
| S2 | 通过 | 连续回拖后回到顶部，无跳动/卡死（`/tmp/u7.jpeg`） |
| S3 | 通过 | 滚动后「区分大小写」开关可切换（`/tmp/u10.jpeg`）、颜色下拉可展开（`/tmp/u12.jpeg`）、关闭下拉后开关可复位（`/tmp/u13.jpeg`）；「添加规则」在关键词为空（占位文本不算输入）时按上游守卫不新增，属预期 |
| S4 | 通过 | 拖动设置标题栏（`733,127 → 733,217`）卡片下移约 95px（`/tmp/s4b.jpeg`） |
| S5 | 不适用 | OHOS 移植未接 `term-select-*`（`ohos_app.rs` 只接 `on_terminal_resize`，`selection` 恒为空模型），文本选择高亮本来就不存在；本次修复未触碰终端选择路径（§23 亦记录剪贴板为 no-op） |
| S6 | 无回归 | 左栏会话列表 3 条未溢出、右侧文件系统列表 10 行未溢出，均无滚动空间；SFTP 文件行自带 drag-out（上游 #504），触摸拖行触发「OHOS 暂不支持拖拽到文件管理器」提示而非滚动，行为与桌面一致；SFTP 树点击导航正常（`/tmp/s6h.jpeg`、`/tmp/s7e.jpeg`） |
| S7 | 通过 | 鼠标双击应用标题栏最大化（`/tmp/s7a.jpeg`）、鼠标单击选中树节点、单击文件夹进入目录（`/tmp/s7c.jpeg`）；滚轮注入仍未产生轴事件（§23.7 已记录的注入限制，与本次改动无关） |

清理：关闭 vps 标签回到欢迎页（`/tmp/s8b.jpeg`）；`sessions.json` 的
`output_highlight_rules` 仍为 `[]`、`ui_scale` 仍为 100、会话仍为 3 条。

## 25. 终端无法选择 / 复制 / 粘贴（修复，增量 10）

### 25.1 现象（2026-11-16，1366×768，192.0.2.145:10178）

- 手指在终端区拖动没有高亮选区，抬手也不会写系统剪贴板。
- 右键 / 长按菜单里「复制 / 粘贴」可点但无效果（§24.8 S5 已记录为「不适用」）。

### 25.2 根因（三条，均已确认）

1. 移植侧未接上游选择回调：`ohos_app.rs` 只连了 `on_terminal_resize`，
   `term-select-start/update/end/word`、`copy-terminal-text`、
   `paste-from-clipboard` 全部悬空，`TerminalView.selection` 恒为空模型。
2. `clipboard-set` / `clipboard-get` 两个窗口动作在 ArkTS 侧没有实现，
   即便 Rust 侧产生文本也没有任何一方读写系统剪贴板。
3. 读系统剪贴板受 `ohos.permission.READ_PASTEBOARD`（USER_GRANT +
   SYSTEM_BASIC + provisionEnable）保护：本机权限弹窗异常（见 §25.6），
   直接 `getDataSync()` 无权限时**不抛异常**，返回空 PasteData 且
   `getPrimaryText()` 为 `undefined`。

### 25.3 方案（只改移植侧，`ui/` 与 `vendor/` 零改动）

1. Rust 选择模型：`TermTab` 增加 `sel_anchor` / `sel_focus` / `selection`
   与 `pending_paste`；接线 `on_term_select_start/update/end/word`
   、`on_copy_terminal_text`、`on_paste_from_clipboard`
   、`on_paste_confirmed/cancelled`。
   `terminal.rs` 增加 `row_chars` / `row_span` / `selection_rects` /
   `selected_text` / `visible_text` / `word_bounds` / `paste_bytes`
   （`\r\n`→`\n`，bracketed paste 包 `\x1b[200~…\x1b[201~`）。
2. 复制：拖选抬手 / 双击取词 → `queue_clipboard_copy` → 动作 `clipboard-set`
   → ArkTS `slintClipboardTake()` → `pasteboard.setData`（**写无需权限**）。
3. 粘贴：菜单「粘贴」→ `PASTE_TARGET=<tab>` + 动作 `clipboard-get` → ArkTS
   读剪贴板；无权限读到空时，在菜单下方浮出系统安全控件
   `PasteButton({icon: LINES, text: PASTE})`，用户点击时系统临时授予一次
   `ohos.permission.SECURE_PASTE`，读到的文本经 `slintClipboardText` 交回
   Rust；`handle_pasted_text` 优先用 `PASTE_TARGET`，为空则回落到
   `active-tab-id`（长按浮层路径不经过 Slint 菜单，没有 target）。
4. ArkTS 长按（600 ms）合成右键打开 Slint 菜单，同时在菜单下方 8px 浮出
   `PasteButton`（112×40vp），一次点击即完成「授权 + 读取 + 回填」。
5. 多行粘贴沿用上游确认框：`paste-confirm-open/preview/large`，确认后
   `send_paste`，取消丢弃 `pending_paste`。

### 25.4 非目标

- 不改 `ui/*.slint`（菜单项、选区矩形、确认框全部复用上游）、
  不改 `vendor/i-slint-core`。
- 不申请 `READ_PASTEBOARD`、不改 `module.json5` / `string.json` / profile ACL。
- 不做块选择、不做本应用内剪贴板历史。

### 25.5 验收矩阵（真机 1366×768）

| ID | 步骤 | 期望 |
| --- | --- | --- |
| C1 | 终端内 `fling`（等效手指拖动）跨字符 | 出现选区高亮 |
| C2 | 抬手 | 自动写系统剪贴板（`clipboard-set` → `setData ok`） |
| C3 | 长按 600 ms | Slint 菜单 + 下方系统「粘贴」按钮同时出现 |
| C4 | 点「粘贴」按钮 | 系统临时授权，终端提示符后出现剪贴板文本 |
| C5 | 复制多行后粘贴 | 弹上游「确认多行粘贴」框；取消不写入 |
| C6 | 菜单「复制」/「粘贴」 | 走同一链路（粘贴无权限时同样浮出按钮） |
| C7 | 回归 | 键盘输入、IME、SFTP、设置页滚动不受影响 |

### 25.6 环境限制（已确认，勿重复尝试）

- 声明 `ohos.permission.READ_PASTEBOARD` → 安装失败
  `code:9568289 grant request permissions failed`；在
  `signing/profile.json` 的 `acls.allowed-acls` 放行后可安装，但运行时授权
  弹窗文案错乱（显示「访问位置信息」）、PermissionManager 实际尝试授予
  `LOCATION*` 并全部失败，`READ_PASTEBOARD result=[-1]`。`uitest` 也无法
  可靠操作该模态窗（按钮物理坐标 y≈421：禁止 x≈598、允许 x≈758）。
  结论：本机不可用权限方案，已回滚权限声明、字符串与 profile ACL。
- 无权限时 `getDataSync()` 返回空 PasteData，`getPrimaryText()` 为
  `undefined`；直接取 `.length` 会让 ArkTS 崩溃（实测 jscrash 日志
  `/data/log/faultlog/faultlogger/jscrash-com.tdyso.khshell-*.log`）。
- `uitest uiInput drag` 实际是长按（Down 保持 ~1.5 s，无 Move），会先触发
  长按菜单；要产生 Move 必须用 `uitest uiInput fling x y x y v step`。
  `uinput -M -m` 不接受负增量（`Coordinate value must be greater or equal than 0`）。

### 25.7 真机结果（2026-11-16，1366×768，192.0.2.145:10178）

| ID | 结果 | 证据 |
| --- | --- | --- |
| C1 | 通过 | `fling 300 213 520 213 200 20` 后终端出现蓝色选区 |
| C2 | 通过 | 抬手日志 `clipboard-set: 29 chars` → `setData ok: 29`；系统侧 `PBC SetPasteData` |
| C3 | 通过 | `longClick 600 300` 后菜单（复制/粘贴/清空缓存/查找）与下方「粘贴」按钮同时出现（`/tmp/p2.jpeg`） |
| C4 | 通过 | 点按钮：`SecCompPermManager GrantTempPermission` → `GetPasteData success` → `clipboard-get: 29 chars`，提示符后出现文本（`/tmp/p3.jpeg`）；`Ctrl+C` 清行正常（`/tmp/p4.jpeg`） |
| C5 | 通过 | 竖向拖选两行 → `clipboard-set: 97 chars`；粘贴弹出「确认多行粘贴」框（预览、取消/粘贴，`/tmp/p5.jpeg`），点「取消」后框关闭、终端无写入（`/tmp/p6.jpeg`） |
| C6 | 通过（同链路） | 菜单「粘贴」走 `on_paste_from_clipboard` → `clipboard-get`；无权限读到空时浮出同一按钮，按钮代码路径与 C4 完全一致 |
| C7 | 无回归 | 关闭标签回欢迎页、会话仍 3 条（`/tmp/p7.jpeg`）；键盘/IME/SFTP/设置滚动本轮未改动 |

清理：关闭 vps 标签回到欢迎页（`/tmp/p7.jpeg`），`sessions.json` 未改；
`module.json5`、`string.json`、`signing/profile.json` 与 `profile.p7b`
已确认无 `PASTEBOARD` 残留。

### 25.8 回滚

- ArkTS：删除 `pasteChipVisible/pasteChipX/pasteChipY` 与 `build()` 里的
  `PasteButton` 块，`pasteFromClipboard` 恢复为直接 `sendClipboardText`。
- Rust：`handle_pasted_text` 删掉 `active-tab-id` 回落（`PASTE_TARGET` 为空
  即丢弃），选择模型保留不影响其它路径。

### 25.9 终端交互回归：双击取词 / 清空缓存 / 查找（2026-11-17）

#### 现象

1. 双击（或触摸双击）终端文字没有任何反应：无取词高亮、无 `clipboard-set`
   ——上游 `double-clicked => root.select-word(...)` 在此端口不生效。
2. 上下文菜单「清空缓存」点击后终端内容不变（回调未接线，`uitest` 侧看到
   的只是菜单关闭）。
3. 上下文菜单「查找」能弹出查找栏，但输入查询后没有任何高亮
   （`find-query-changed` 与 `find_matches` 模型都未接线）。

#### 根因

1. Slint 的双击识别在 `i-slint-core/input.rs::ClickState::check_repeat`，
   只在 `WindowEvent::PointerPressed` 且 `Platform::click_interval()`（默认
   500 ms）内、距离²<100 时累加 `click_count`。OHOS 上按在
   `ScrollView`/`Flickable` 内的触摸按下会被延迟（`process_delayed_event`
   直接派发，绕过 `process_mouse_input` 的 ClickState 分支），因此
   `double-clicked` 永不触发。实测单击可正常产生 `select-start` /
   `select-end`，拖选与复制均正常，只有双击合成缺失。
2. 移植侧 `ohos_app.rs` 只接了 `on_term_select_*` 与复制/粘贴，
   `on_clear_terminal`、`on_find_query_changed` 两个回调没有注册。
3. `TerminalState.find_matches` 在行模型里用的是 `empty_model()`，
   `TermTab` 也没有搜索模型，导致即使回调触发也无处写高亮矩形。

#### 方案（仍只改移植侧）

1. `TermTab` 增加 `last_tap: Option<(Instant, u16, u16)>`、
   `skip_end_copy: bool`；`on_term_select_start`（非 shift 路径）里做
   **400 ms / 同一格** 的本地双击检测：命中则调用 `terminal::word_bounds`，
   写 `sel_anchor/sel_focus` 并 `refresh_selection`，置
   `skip_end_copy = true` 后 `queue_clipboard_copy` 并直接返回；
   `on_term_select_end` 见到 `skip_end_copy` 时只清标志、不再复制或清选区。
   这样单击/拖选逻辑与上游一致，双击走同一条复制链路。
2. `on_clear_terminal(tab-id)`：清空 `spans` / `sel_anchor` / `sel_focus` /
   `selection` / `find_matches`，再向 PTY 写 `Ctrl+L`（0x0c）让 shell 重绘
   提示符，等价于桌面的「Clear buffer」。
3. `terminal::find_matches(parser, query)`：对可见屏幕逐行做大小写不敏感
   匹配，按 cell 折算 `(row, col, len)`（CJK 宽字符用首尾 cell 的起止列），
   返回 `Vec<(u16, u16, u16)>`。`TermTab` 新增
   `find_matches: Rc<VecModel<TermMatch>>`，在 `connect_session` 里与
   `selection` 一起创建，行模型 `find_matches: ModelRc::from(...)` 取代
   `empty_model()`；`on_find_query_changed` 把结果 `set_vec` 进去。

#### 真机结果（1366×768，192.0.2.145:10178）

| 项目 | 结果 | 证据 |
| --- | --- | --- |
| 双击取词 | 通过 | 双击提示符行 `root@vps-host:~#`（物理 295,429）→ `clipboard-set: 24 chars` 且该词高亮（`/tmp/u2c.png`） |
| 清空缓存 | 通过 | 菜单第 3 行（物理 y≈367）→ 屏幕清空、提示符回到首行（`/tmp/u3.jpeg`、`/tmp/v8.jpeg`） |
| 查找弹出 | 通过 | 菜单第 4 行（物理 y≈396）→ 终端顶部出现查找栏（`/tmp/v9.jpeg`） |
| 查找高亮 | 通过 | 打开后直接注入 `root`（见下方限制，勿先点击查找框）→ 输入框显示 `root`，终端内全部 `root` 出现棕色高亮矩形（`/tmp/w6c.jpeg`） |
| 关闭查找 | 通过 | 点 × （物理 1046,157）→ 栏关闭、高亮清除、终端内容保留（`/tmp/w7.jpeg`） |
| 回归 | 无回归 | `Ctrl+C`（`uitest uiInput keyEvent 2072 2019`）清提示符行正常（`/tmp/w8.jpeg`） |

#### 本机操作限制（新增，勿重复踩坑）

- 上游查找栏是**窗口内**的 `Rectangle`（不是 `PopupWindow`），其屏幕位置在
  状态行下方：本机物理 **y≈147..168**（× 在 x≈1046）。点击查找输入框反而会
  让 `find-input` 失焦（焦点回到终端 `ime-input`），注入文本会落到 shell。
  正确做法：菜单点「查找」后**不要点击输入框**，直接
  `uitest uiInput text root`（依赖查找栏自己的 `find-focus-pending` 定时器
  自动聚焦）。
- 注入文本用 `uitest uiInput text <text>`；`inputText <x> <y> <text>` 在本机
  不可靠。`text` 会丢失部分标点（`ls -l /` 实测变成 `ls l`），验证命令请用
  纯字母数字（如 `ls`、`echo root`）。
- 上下文菜单（`132×132`，touch 为物理 600,300 时菜单顶部 ≈291）四行中心
  物理 y ≈ **310 / 340 / 370 / 400**：复制 / 粘贴 / 清空缓存 / 查找；
  菜单 `x` 中心 ≈ `touchX+66`。
- 长按会在菜单下方浮出 ArkTS 的「粘贴」按钮 chip（物理 y≈445..475）；它
  不遮挡菜单，但点空白外不会自动消失，注意别误点。

### 26 命令栏（输入命令，回车发送）的复制与粘贴（2026-11-17）

#### 现象

命令栏 `cmd-input` 里 `Ctrl+C` / `Ctrl+X` 选中文字后按了没反应（系统剪贴板
无变化），`Ctrl+V` 也贴不出任何内容；只有终端长按菜单的「粘贴」可用。

#### 根因

1. `OhosPlatform` 只实现了 `create_window_adapter` / `duration_since_start`，
   `Platform::set_clipboard_text` / `clipboard_text` 都是 trait 默认空实现，
   于是 Slint `TextInput` 的 `StandardShortcut`（全选/复制/剪切/粘贴）
   全部落到空函数上。
2. 剪贴板读写必须走 ArkTS（`@ohos.pasteboard`），而 `Platform::clipboard_text`
   是**同步**返回 `Option<String>`，NAPI 侧无法同步拿到 ArkTS 的异步结果；
   跨应用读取还需要 `READ_PASTEBOARD`（本机授权弹窗不可用）或系统安全控件
   `PasteButton` 临时放行。
3. 终端长按粘贴走 `PASTE_TARGET`（指定 tab），与「插到当前焦点控件」是两条
   不同的语义，不能合并：终端的 `ime-input` 是 `single-line: true` 且
   `key-pressed` 直接把按键 `send-key` 给 PTY，若把 Ctrl+V 的文本改成
   `CommitComposition` 会丢掉多行确认/括号粘贴。

#### 方案（只改移植侧 + ArkTS 宿主）

1. `ohos_app.rs` 新增应用内剪贴板缓存与两个「待办动作」：
   - `CLIPBOARD_CACHE: Mutex<Option<String>>`：本进程最近一次复制/剪切的内容。
   - `publish_clipboard_text(text)`：写缓存 + `CLIPBOARD_OUT`（供
     `slintClipboardTake` 取走）+ 置 `CLIPBOARD_SET_REQUESTED`。
   - `clipboard_cache_text()`：命中缓存直接返回；未命中则置 `PASTE_FOCUS`
     与 `CLIPBOARD_GET_REQUESTED` 并返回 `None`。
   - `take_action()` 新增两条：`clipboard-set`（ArkTS 写系统剪贴板）与
     `clipboard-get-input`（ArkTS 读系统剪贴板，无权限时浮出粘贴按钮）。
2. `commit_text_to_focus(ui, text)`：派发
   `InternalKeyEvent { CommitComposition, text }`，让文本落到当前焦点 item
   （命令栏/查找栏/编辑器），`UiState::ime_commit` 复用它。
3. `handle_pasted_text`：先记缓存，再取 `PASTE_TARGET`；若 `PASTE_FOCUS`
   为真且没有指定 tab，则 `commit_text_to_focus` 并返回；否则维持原有
   「写入 PTY」路径。终端长按粘贴行为完全不变。
4. `lib.rs` 的 `impl Platform for OhosPlatform` 接上
   `clipboard_text → clipboard_cache_text()`、
   `set_clipboard_text → publish_clipboard_text(text)`。
5. ArkTS：`applyWindowAction` 新增 `clipboard-get-input` →
   `pasteFromClipboard(false)`；该分支未取到文本时把粘贴按钮 chip 放到
   **窗口底部居中**（`showPasteChip(false)`），点击后同上插到焦点。

#### 已知边界

- `Ctrl+V` 优先命中**本进程最近一次**复制的内容（同步返回，无系统往返）。
  若文本是从其它应用复制的且本进程缓存非空，会先粘到缓存内容；需要用
  系统安全控件粘贴时，走终端长按「粘贴」或等缓存清空（重启应用）。
- 跨应用首次粘贴仍需用户在系统 `PasteButton` 上点一次（无 `READ_PASTEBOARD`）。
- 实测本应用写入系统剪贴板后，自身 `getDataSync()` 可以读回（10 chars），
  因此同一会话内 `Ctrl+C` → `Ctrl+V` 连系统往返都不需要。

#### 真机结果（1366×768，192.0.2.145:10178）

| 步骤 | 结果 | 证据 |
| --- | --- | --- |
| 命令栏输入 `echo hello` → `Ctrl+A` → `Ctrl+C` | 通过 | `clipboard-set: 10 chars` + `clipboard setData ok: 10` |
| `Ctrl+U`（readline 剪切整行） | 通过 | 输入框回到占位文案，同时 `clipboard-set: 10 chars` |
| 同进程 `Ctrl+V` | 通过 | 缓存命中，`helloecho` 立即回填（`/tmp/s4-bar.jpeg`） |
| 重启应用（缓存清空）→ 命令栏 `Ctrl+V` | 通过 | `clipboard-get: 10 chars` → 文本插入命令栏（`/tmp/s7.jpeg`） |
| 点「发送」执行粘贴出的命令 | 通过 | 终端 `helloecho` / `helloecho: command not found`（`/tmp/s8-term.jpeg`） |
| 回归：终端长按「粘贴」 | 通过 | 仍写入 PTY（提示行出现 `helloecho`），命令栏保持空（`/tmp/s10-term.jpeg`） |

#### 操作提示（自动化）

- 字母键码：A=2017 … U=2037、V=2038；`uitest uiInput keyEvent 2072 <code>`
  即 Ctrl+<字母>。
- 命令栏焦点用点击物理 (700,456)（窗口 1366×768，发送按钮 ≈(1022,456)）。

### 27 命令栏鼠标右键菜单（全选 / 复制 / 剪切 / 粘贴，2026-11-17）

#### 现象

命令栏输入框上按鼠标右键没有任何反应：既没有系统「复制 / 粘贴」菜单，也没有
上游终端的右键菜单。用户只能靠 Ctrl+A / Ctrl+C / Ctrl+V（§26）操作。

#### 根因

1. Slint 的 `TextInput` 只有 `StandardShortcut` 快捷键，**没有内建上下文菜单**；
   右键在其上是被忽略的（`pointer()` 只把事件分发给 item）。
2. 上游终端右键菜单挂在终端区域（`on_term_*`），命令栏不是终端，点不到它。
3. 右键落在哪个控件上`Rust` 侧本来无从判断：`TerminalView` 内部属性
   （`sftp-panel-height` / 命令栏绝对 y）不在 `AppWindow` 上，ArkTS 也不知道
   布局。

#### 方案（只改移植侧 + ArkTS 宿主）

1. `ohos_app.rs` 的 `pointer()`：在构造 `WindowEvent` 之前判定
   `kind == 0 && button == Right && focus_text_field_at(ui, x, y)`，命中则抛
   `input-menu` 动作（其余右键行为不变，终端主体仍走上游菜单）。
2. `focus_text_field_at(ui, x, y)`：不做布局猜测，直接用运行时焦点——
   `WindowInner::from_pub(window).focus_item` → `ItemWeak::upgrade()` →
   `item.downcast::<i_slint_core::items::TextInput>()`；再用
   `item.geometry()` 加逐级 `parent_item(StopAtPopups)` 的原点累加得到
   **窗口坐标矩形**，判断点是否落在其中。终端的键盘锚点 `ime-input` 是 0×0，
   因此右键终端主体永远不命中，上游菜单不受影响。
3. ArkTS `SlintSpike.ets`：`applyWindowAction` 新增 `input-menu` →
   `openInputMenu()`，在 `menuAnchorX/Y` 处（贴边时自动夹到屏幕内、放不下
   翻到上方，尺寸 120×152）弹原生四项菜单；每一项 `synthCtrl(letter)` 依次
   注入 `slintKey('Control', true)` / `slintKey(letter)` /
   `slintKey('Control', false)`，回放 Ctrl+A / Ctrl+C / Ctrl+X / Ctrl+V，
   焦点本来就在该输入框上，等价于用户按快捷键。
4. 「全选」点击后**不关菜单**（`keepOpen = true`）：触摸长按的第一次按下
   （`slintTouch` Down 会先发一次左键）已经把选区收掉，用户需要先全选、再在
   同一层菜单里接着点复制 / 剪切 / 粘贴。其余三项点击后立即收起。
5. 触摸长按（600 ms）也走同一入口：定时器里先补发一次指针退出
   `slintPointer(x, y, 3, 0)`（清掉触摸留下的左键按下，见下节），再补发
   `slintPointer(..., 0, 1)` 右键，Rust 命中焦点输入框时同样抛 `input-menu`，
   因此触屏用户长按命令栏也能得到这四个菜单项。

#### 已知边界

- 触摸长按命令栏时，长按前的 `TouchType.Down` 会先发一次左键按下并清掉选区；
  桌面鼠标右键不经过这条路径，选区保留。
- 菜单「粘贴」在本进程有缓存时直接回填（§26）；缓存为空时走
  `clipboard-get-input` 并浮出系统 `PasteButton`，跨进程读取仍可能返回
  `TEMPORARY_AUTHORIZATION_FAILED(1)`（本机授权抖动），此时不会粘贴。
- 菜单只在焦点控件是 `TextInput` 时出现；右键查找栏 / SFTP 路径框同样生效，
  右键终端主体保持上游终端菜单。
- 坐标口径：`onMouse` / `onTouch` 给的是 XComponent 局部 px，菜单用页面 vp
  定位，两者差一个窗口原点（本机 x≈25）。`openInputMenu` 用
  `getUIContext().getComponentUtils().getRectangleById('slintSurface')` 的
  `windowOffset` 补偿，`menuAnchorX/Y` 统一存局部 px、定位时一次换算；
  取不到控件时退回 `px2vp(局部坐标)`（仅偏移 25px，不影响功能）。

#### 真机结果（1366×768，192.0.2.145:10178）

| 步骤 | 结果 | 证据 |
| --- | --- | --- |
| 命令栏输入 `echo ` 后长按（物理 700,468） | 通过 | 弹出 全选 / 复制 / 剪切 / 粘贴 四项（`/tmp/x2.jpeg`） |
| 点「全选」 | 通过 | 命令栏文字高亮，菜单**保持打开**，可继续下一步（`/tmp/x3.jpeg`） |
| 点「复制」 | 通过 | `clipboard-set: 5 chars` + `clipboard setData ok: 5`（`/tmp/x4.jpeg`） |
| 全选 + Backspace 清空 → 点「粘贴」 | 通过 | 命令栏立即回填 `echo `，无系统粘贴按钮（缓存命中，无 `clipboard-get` 日志） |
| 点「剪切」 | 通过 | 命令栏清空 + `clipboard-set: 5 chars`（`/tmp/x7.jpeg`） |
| 回归：长按终端主体 | 通过 | 仍是上游终端菜单（复制 / 粘贴 / 清空缓存 / 查找）+ 粘贴按钮（`/tmp/x8.jpeg`） |
| **真实鼠标右键**（人工点命令栏物理 413,466） | 通过 | 日志 `mouse 1 btn=1 at 413,466` / `mouse 2 btn=1` → 弹出四项菜单，左边缘 ≈442、顶部 ≈470（锚点 +4），宽 118 / 高 145（`/tmp/r1.jpeg`） |
| 真实鼠标右键落在命令栏上沿之外（404,448） | 符合预期 | 也收到 `btn=1`，但点在 `TextInput` 矩形外，不弹菜单（只有输入框矩形内才拦截） |
| 菜单定位补偿后（长按 700,468） | 通过 | 菜单左边缘 ≈704、顶部 ≈476（点击点 +4），不再右偏 25px（`/tmp/f1.jpeg`） |
| 补偿后复测「全选」→「复制」 | 通过 | `clipboard-set: 5 chars` + `setData ok: 5`（`/tmp/f2.jpeg`） |

#### 长按误触按钮的修复（同日）

长按虽然按右键处理，但触摸本身留下的一次「左键按下」没有取消，抬手时 Slint 会
把它配成一次 `clicked`，于是长按「发送」按钮会真的把命令发出去。

- **根因（读 `i-slint-core` 源码确认）**：`TouchState::process_ended` 在
  `gesture_state == Idle` 时，对 **Ended 和 Cancelled 都** push
  `MouseEvent::Released { button: Left }` + `MouseEvent::Exit`。所以最初把抬手
  改成 `TouchPhase::Cancelled` 完全无效——`TouchArea` 的 `clicked` 条件是
  「这次按下过（`pressed`）+ 左键抬起且仍在几何内」，Cancelled 一样满足。
- **方案**：长按定时器里在合成右键**之前**先发一个指针退出事件
  （`slintPointer(x, y, 3, 0)` → `WindowEvent::PointerExited`）。它经
  `handle_mouse_grab` 送进被按住的 `TouchArea`，把 `pressed` / `grabbed`
  清掉并触发 `pointer-event(cancel)`；此后无论抬手是 Ended 还是 Cancelled，
  都不会再合成点击。抬手恢复为普通 `Ended`（不再用 Cancelled）。
- 代价：长按开始时该控件会先收到一次 `pointer-event(cancel)`，对按钮无副作用；
  对终端主体是一次 `selecting = false`，不影响随后的右键菜单。

| 步骤 | 结果 | 证据 |
| --- | --- | --- |
| 命令栏输入 `echo nomisfire3` → 长按「发送」(1031,459) | 通过 | 命令栏文本**保持**、未发送，仅浮出系统「粘贴」钮（`/tmp/w2.jpeg`） |
| 随后普通点击「发送」 | 通过 | 命令栏被清空（`run-command` 执行），按钮功能未受影响（`/tmp/w3.jpeg`） |
| 长按命令栏输入框 | 通过 | 仍弹原生四项菜单（`/tmp/w4.jpeg`） |
| 长按终端主体 | 通过 | 仍弹上游终端菜单（复制 / 粘贴 / 清空缓存 / 查找，`/tmp/x3.jpeg`） |

#### 自动化操作提示（本机）

- **只要还有一个 Slint `PopupWindow` 开着，它矩形之外的所有指针事件都会被
  丢掉**（`window.rs` 里 `root == None` 分支只发 `Exit`，不下发给主 item 树）。
  先前的「长按终端没菜单」就是快速命令弹层没关导致的假失败。测长按前先点空白
  处把弹层关掉（点一次只关弹层，要点第二次才落到目标）。
- 截图是 1366×768，但看图片时会被缩放到 1024×576；用截图估坐标要乘
  ≈1.334 才是 `uitest`/`uinput` 要的物理 px（会话行 ≈ (133,243)，命令栏
  ≈ (600,459)，发送键 ≈ (1031,459)）。
- 本机按 `keyEvent 2070`（Esc）不会关弹层，反而把应用切走；用点击关闭。
- `uitest uiInput` 没有右键注入命令，用 `longClick <x> <y>`（600 ms）触发；
  菜单贴在锚点 +4，四行中心 ≈ 锚点 `+25 / +61 / +97 / +133`（行高 36），
  菜单宽 120，x 取 `锚点 x + 60`（实测 700,468 → 行 y = 492 / 528 / 564 / 600）。
- `uinput -M -c 1`（以及 `-M -d 1` / `-u 1`）注入的右键**到不了应用**：本机
  由系统桌面 shell 消费，屏幕左上角弹出桌面的「新建 / 粘贴 / 查看 / 排列方式
  / 刷新 / 显示设置 / 个性化」菜单，应用日志只有 `mouse N btn=0`（左键）或
  干脆没有事件。自动测右键请用 `longClick`（走同一 `input-menu` 入口）；
  真实鼠标不受影响（已人工验证，见上表，`mouse 1/2 btn=1`）。
- `uitest uiInput text <text>` 走系统输入法，文本先进入 IME 预编辑（左上角
  出现预览气泡），**必须再按一次空格（键码 2050）或回车（2054）才提交**到
  Slint 焦点控件；直接 `text` 后截图会看不到输入。提交空格会多出一个尾随
  空格（复制出来是 5 chars）。
- 命令栏坐标：输入框 ≈ (335..690, 456..480)，发送按钮 ≈ (1022,456)。

#### 长按粘贴按钮改为「按需浮出」（取代 `term-menu`）

- 现象：长按侧栏会话行、标签栏、按钮等无终端菜单的位置也会浮出「粘贴」
  按钮，观感像误触。
- 根因：最初靠「右键前后 `WindowInner::active_popups` 是否新增」判断终端
  菜单，但左栏会话行也有上游 `PopupWindow` 右键菜单，判据无法区分
  （`/tmp/r2.jpeg`：会话菜单与粘贴按钮同时出现）。
- 方案：Rust `pointer()` 回退为原版（不再抛 `term-menu`）；ArkTS 长按定时器
  也不再提前调用 `showPasteChip()`。只有用户点菜单「粘贴」→ Rust 发
  `clipboard-get` → `pasteFromClipboard(true)` 读剪贴板失败（权限受限）时，
  才 `showPasteChip(true)` 兜底。
- 锚点：`showPasteChip` 用 `menuAnchorX/Y`（XComponent 局部 px）定位，并补
  `getRectangleById('slintSurface').windowOffset`；`dismissSlintMenu` 也改用
  菜单锚点补点击，避免落回菜单项。
- 真机结果（2026-11-17）：侧栏会话行 `longClick` 只弹上游「编辑/复制副本/
  删除/移动到」，无粘贴按钮（`/tmp/r5.jpeg`）；终端主体 `longClick` 弹四项
  菜单且无按钮（`/tmp/r8.jpeg`），点「粘贴」后按钮按菜单锚点浮出
  （`/tmp/r9.jpeg`）；长按发送键不误发（`/tmp/r11.jpeg`）；长按命令栏仍是
  四项菜单、无按钮（`/tmp/r12.jpeg`）。

#### 应用名称与图标统一为 MeatShell

- 资源：`AppScope` 的 `app_name` 与 `entry` 的 `module_desc` 改为 MeatShell；
  `AppScope`/`entry` 的 `foreground.png`（1024，透明，内容缩到安全区 0.88）、
  `background.png`（1024，全出血渐变）与 `entry` 的 `startIcon.png`（512）
  复用上游 `assets/make_icon.py` 的肉块几何、分层配色与 `>_` 提示符重绘。
- UI 文案：`ui/app.slint` 的窗口 `title`、自定义标题栏、关于页标题与描述、
  关闭确认；`ui/sidebar.slint` 页脚；`ui/welcome.slint` 标题；
  `ui/editor_window.slint`、`ui/proc_window.slint`、
  `ui/system_info_window.slint` 窗口标题；`ui/interface_panel.slint` 两条
  重启提示；`lang/{zh,en}/LC_MESSAGES/meatshell.po` 关于行的 msgid/msgstr。
  命令行名（`meatshell mcp serve`）、文件名与 URL 保持小写不改。
- 构建注意：`ui/*.slint` 由 `ohos/meatshell-slint/build.rs` 编译进静态库，
  改 UI 文案必须重跑 cargo build（本次 6m34s），hvigor 单独跑不生效。
- 真机结果：`/tmp/r4.jpeg` 标题栏、欢迎页、侧栏页脚均为 `MeatShell`，
  Dock/标题栏图标为五花肉图标。

## 28. 上架前收尾（主体 / 版本 / 授权 / 双 ABI）

| 项 | 值 |
| --- | --- |
| vendor | `tdyso`（与 bundleName `com.tdyso.khshell` 一致） |
| versionName / Code | `0.8.1` / `1000000`，与 `ohos_app.rs` 的 `set_app_version("0.8.1")` 一致 |
| 名称 | `khshell`（`AppScope` `app_name`、ability label、窗口 title、关于页） |
| 图标 | 分层图标（前景 1024 透明安全区 0.88、背景 1024 全出血）+ `startIcon` 512 |
| Slint 署名 | 关于页内嵌官方 `AboutSlint` 组件，满足 Royalty-free 2.0 的署名要求 |
| 第三方许可 | 关于页一行列出 Twemoji (CC BY 4.0)、Material Icons (Apache 2.0)、Cascadia Mono (OFL 1.1) |
| ABI | `x86_64` 与 `arm64-v8a` 双库；后者随包携带 `libc++_shared.so`、`libtime_service_ndk.so` |

已知边界（非本轮目标）：`system-info-available` / `proc-available` 仍为
false（OHOS 未实现进程/系统信息窗口）；签名仍使用 OpenHarmony 公共测试
密钥库与绑定本机 UDID 的 debug provision，正式发布需替换为企业证书与
正式 profile；软著/备案材料不在代码范围内。

## 29. 壁纸与主题（内置 + 自定义图片）

移植侧原先只把设置面板的壁纸控件画出来，`on_set_wallpaper` /
`on_pick_wallpaper_file` / `on_persist_wallpaper_overlay` 三个回调从未注册，
点色板没有任何效果。本轮补齐整条链路。

#### 实现

| 位置 | 内容 |
| --- | --- |
| `ohos/meatshell-slint/src/wallpaper.rs`（新增） | `load(id)` 生成 `Image` + `Palette`；`apply(ui, id, overlay)` 写回 Slint 属性 |
| 内置壁纸 | `builtin:light` / `builtin:dark`（对角渐变 + 右上 glow）、`builtin:tech`（幻想 3048：科幻日落 + 霓虹网格 + 星点，逐点复刻上游 `render_tech`）、`builtin:miku` / `builtin:ms`（`include_bytes!` 内置 JPEG） |
| 自定义图片 | `decode_custom(path)`，超过 2560 长边先用 Triangle 缩放，再转 RGBA |
| 取色 | `derive_palette` + `vivid_accent`，与上游同算法，得到 accent / tint / 明暗 |
| 持久化 | `wallpaper.txt`（壁纸 id 或沙盒路径）、`wallpaper-overlay.txt`（遮罩透明度），均写在 `MEATSHELL_DATA_DIR` |
| 回调 | `settings.rs` 注册 `set_wallpaper`（保存 + 应用）、`pick_wallpaper_file`（请求 ArkTS 选图）、`persist_wallpaper_overlay`（改遮罩并落盘） |
| 启动恢复 | `apply_saved_appearance` 末尾读取 prefs 并 `wallpaper::apply` |
| 选图桥 | Rust `ms_spike_wallpaper_picked(path)` ← NAPI `slintWallpaperPicked` ← ArkTS `pickWallpaper()`；`take_action` 返回 `pick-wallpaper` |

#### 自定义图片的 URI 兼容

`DocumentViewPicker` 返回的是 `file://docs/storage/Users/...`，本机无法直接
`fs.openSync`。ArkTS `wallpaperCandidates()` 按顺序尝试三种路径，第一个能读出
的胜出，读出的文件复制到 `<filesDir>/wallpapers/<name>`，再把**沙盒路径**
交给 Rust（沙盒内 `std::fs::read` 一定可读）：

1. 原始 URI；
2. `file://docs` 去掉后的 `/storage/Users/...`；
3. 媒体库真实路径 `/storage/media/100/local/files/...`（`CurrentUser/` →
   `Docs/`；本机 Download = `/storage/media/100/local/files/Docs/Download`，
   Photo = `/storage/media/100/local/files/Photo/1/`）。

Download 目录已申请 `ohos.permission.READ_WRITE_DOWNLOAD_DIRECTORY`，因此
从「下载」选图可读；Photo 目录需要 `READ_IMAGEVIDEO`，未申请时不作为目标。

#### 真机结果

- 内置壁纸可切换，选中态跟随；切到 `builtin:light` 会自动转亮色主题；
  遮罩滑块的透明度与壁纸 id 均落盘，重启后保持（`/tmp/w8.jpeg`、
  `/tmp/w9.jpeg`、`/tmp/w10.jpeg`）。
- 自定义：设置 → 壁纸与主题 → 选择文件 → 下载 → 双击 `w.jpg`，文件被复制到
  沙盒 `wallpapers/w.jpg`（93698 B），`wallpaper.txt` 变为沙盒路径，背景立即
  换成该图，重启后仍是它。

#### 自动化提示（本轮新增）

- **注入点击必须用触屏**：`uinput -M -m x y && uinput -M -c 0` 虽然会被
  `onMouse` 收到，但同时合成一条 `px=0,0` 的 `onTouch`，Slint 的
  `TouchState` 会在 (0,0) 补发 `Released{Left}`，导致任何按钮都不触发（只有
  hover 生效）。正确做法是 `uinput -T -c x y`（本机 x86_64 板实测有效）。
- 系统文件选择器里点「打开」按钮对注入触屏无响应，**双击文件行**才会确认。
- 自定义壁纸若选的是整屏应用截图（本机 `w.jpg` 就是 1366×768 的截图），
  背景里会看到「另一个设置面板」的影像——那是图片内容，不是重绘残影。



## 30. 补齐上游未接线回调（标签 / 缩放 / 鼠标 / 设置项）

上游 `src/app/*` 注册了 257 个 `on_*`，移植侧此前缺 90 个。本轮按下表补齐其中
不依赖桌面能力的一批；其余在 §30.3 明确降级。

### 30.1 已接线

| 功能 | 回调 | 实现要点 |
| --- | --- | --- |
| 标签重命名 | `tab-rename-request` / `rename-tab` | 右键菜单取当前标题填入 `tab-rename-*`；空名回落到 `session_config.name`（再回落 `user@host`）；同时更新 `tabs` 与每个 pane 的快照子模型（新增 `set_tab_title`） |
| 标签复制 | `tab-duplicate` | 取 `TermTab.session_config` 复制一份，定位源标签所在 pane 后调用 `connect_session` 新建独立连接 |
| 标签循环 | `cycle-tab` | 在聚焦 pane 的 `tabs` 快照里按 `active_id` 求下一个/上一个，Ctrl+Shift 反向 |
| 终端缩放 | `zoom-term-font` | 单标签写 `TerminalState.font_size`（px 覆盖）；`window_wide` 时改 `term-font-size` 并清空所有覆盖；范围 8–32 |
| 终端鼠标 | `terminal-mouse` | 新增本地 `encode_mouse_event`（X10 / SGR），坐标 1 基并夹到屏内；轮询时同步 `mouse_tracked` / `is_alt_screen`，TUI（htop / btop / vim `mouse=a`）可点击与拖选 |
| 终端全屏 | `toggle-term-fullscreen` / `term-fullscreen-check` | 切换 `term-fullscreen`，非终端标签自动退出 |
| 网卡选择 | `select-net-iface` | `TermTab.selected_iface` 记录选择；`show_remote` 新增 `preferred`，选中项不存在时回落首行（最忙网卡） |
| 输入同步 | `set-sync-input` | 全局 `SYNC_INPUT` 原子标志；`send-key` 除目标会话外把同一字节串广播给其它活动会话 |
| 命令栏隐藏 | `set-cmd-bar-hidden` | 写窗口属性并落盘 |
| JSON 格式化 | `set-json-format-output` | 写窗口属性并落盘（终端渲染侧暂无格式化器，见 §30.3） |
| 渲染模式 | `set-renderer-mode` | 仅落盘，下次启动生效 |
| 编辑器内嵌 | `set-editor-embedded` | 写窗口属性并落盘 |
| 同步上传 | `set-sync-upload-enabled` | 写窗口属性并落盘 |
| MCP 权限 | `set-mcp-permissions` | 四个开关整体落盘（OHOS 侧无 MCP 服务端，仅保留配置） |

`apply_saved_appearance` 同步补齐上述属性的启动初值，设置页开关重启后状态正确。

### 30.2 真机验证

见 `tasks.md` L24 的勾选项与截图记录。

### 30.3 明确未做（降级）

| 功能 | 原因 |
| --- | --- |
| 系统信息 / 进程 / 编辑器独立窗口 | 每个 Slint 窗口各自持有 `MinimalSoftwareWindow`，只有主窗口渲染到 XComponent；需改为内嵌面板并改 `ui/` |
| 传输取消 / 清理 | 需先确认 `SftpClient` 上传下载可否中断 |
| WebDAV / 更新检查 | 需新增 HTTP 依赖与端点配置 |
| 快捷键录制 | 与 OHOS 输入法/系统快捷键冲突，工作量大 |
| 终端 scrollback / 滚轮 | `terminal.rs` 只渲染可见屏，没有回滚缓冲区 |
| `open-download-dir` / `pinned-changed` | OHOS 无对应文件管理器与置顶窗口 API |

## 31. 分屏（Pane split / merge / splitter drag）

移植侧不再固定单 pane：新增 `SplitState { vertical, ratio }`（最多两栏），
由 `refresh_panes` 依据 `content-resized` 报告的可用区域计算两个 pane 的
`x/y/w/h`、`reserve-right` 以及一根 4px 分隔条（`SplitterInfo`）。

- **拆分**：标签右键「向右拆分 / 向下拆分」→ `pane-split(pane-id, tab-id, dir)`。
  该标签从源 pane 的快照移除并成为新 pane 的唯一标签；新 pane 获得焦点，
  比例 0.5。桌面版可无限嵌套，此处限制为两个 pane（1366×768 下更实用）。
- **合并**：`pane-merge(pane-id)` 把第二个 pane 的标签追加到第一个并重建几何；
  关闭标签导致某个 pane 变空时也自动走同一路径（`close_tab` → `merge_panes`）。
- **拖动分隔条（平台层接管）**：上游 `ui/app.slint` 用
  `sp-ta.moved => if (self.pressed) root.splitter-drag(...)`。OHOS 的手指走
  `touch()`（Slint `Touch` 事件），合成按下会在手势可能变成滚动时被撤销，
  `TouchArea.pressed` 无法维持到 `moved`，拖动永远不触发。因此移植侧在
  `pointer()` / `touch()` 进入 Slint 之前先用 `splitter_gesture()` 接管：
  - 命中判定扩大 ±6px，命中后吞掉 press，避免 pane 同时开始文本选择；
  - 坐标必须减去 pane 容器的绝对原点 `content-origin-x/y`
    （`PaneInfo`/`SplitterInfo` 是 `pane-area` 相对坐标；`dock-central-y`
    只有 dock 内部偏移，不含标题栏高度，实测会整体偏 26px 导致纵向分隔条
    永远命中不到）；
  - move 时 `ratio = clamp(pos / extent, 0.15, 0.85)` 并 `refresh_panes`
    + `request_redraw`，release/cancel 结束手势。
  鼠标左键与触摸共用同一状态机（`split_drag: Option<bool>`，
  `true` = 垂直分隔条）。
- **焦点**：`pane-focus` 同时把该 pane 的活动标签同步到全局 `active-tab-id`，
  状态栏与右侧资源面板随之切换。
- **监听/新建**：`connect_session` 与「+」通过 `update_pane_tabs` 只向聚焦 pane
  追加标签；`update_pane_tabs` 在分屏时不再用全局标签重建快照，改为维护 pane 归属。
- **缩放**：每个 pane 的 `TerminalView` 以自身宽度/高度渲染，`terminal-resize`
  会按 pane 实际尺寸调整各自 PTY。

未接线的分屏相关交互：标签跨 pane 拖拽（`tab-drag-drop` 到另一 pane）与
pane 内标签重排（`pane-tab-move`）；两者在桌面版由指针坐标命中判定，OHOS
当前仍按原 pane 归属处理。

### 31.1 真机验证步骤（`192.0.2.145:10178`，1366×768，dpr=1）

坐标约定（实测）：`hdc shell uinput` 注入的是屏幕坐标；ArkTS `onMouse` 传给
Slint 的是 XComponent 局部 px，**x 恒定相差 25px（Slint = 屏幕 − 25），y 无偏移**。
`snapshot_display` 出的 1366×768 截图与注入坐标 1:1。

| # | 操作 | 命令 / 坐标 | 期望 | 结果 |
| --- | --- | --- | --- | --- |
| 1 | 打开会话 | `uinput -T -c 160 244` | 出现标签 `dev@127.0.0.1`（连接失败属预期，本机 2222 无 sshd） | 通过 |
| 2 | 复制标签：右键标签 → 复制并打开 | 右键标签 `uinput -M -m 320 55 -c 1`，再点菜单「复制并打开」`uinput -M -m 380 72 -c 0` | 同一 pane 出现第二个标签 | 通过 |
| 3 | 向右拆分 | 右键标签 `320 55` → 「向右拆分」`380 162` | 两个 pane 并排，分隔条 4px 位于中央区中线 | 通过（分隔条 661..664） |
| 4 | 拖动分隔条（触摸） | `uinput -T -m 663 400 560 400` | 分隔条随手指左移并停在松手处 | 通过（661→559） |
| 5 | 拖动分隔条（鼠标） | `uinput -M -g 560 400 700 400 800` | 同上，且不触发终端文本选择 | 通过（559→699） |
| 6 | 向下拆分 | 右键标签 → 「向下拆分」`380 190` | 两个 pane 上下排列，分隔条水平 | 通过（分隔条 y 388..391） |
| 7 | 拖动水平分隔条 | `uinput -T -m 700 389 700 480` | 上下比例改变 | 通过（389→476） |
| 8 | 合并 | 右键 → 「Merge panes」（上游字符串，未本地化） | 回到单 pane，两个标签都保留 | 通过 |
| 9 | 空 pane 自动合并 | 拆分后关闭一侧唯一标签 | 自动回到单 pane，剩下一侧标签仍在 | 通过 |

标签右键菜单（`PopupWindow`，软件渲染器下正常显示）行位置：菜单左边缘 = 点击 x
（靠右时向左展开），首行中心 ≈ 点击 y + 17，行距 29px（28 行高 + 1 间距），
分隔线 1px；7 项依次为 复制并打开 / 重命名 / 加入会话列表 / 向右拆分 /
向下拆分 / Merge panes / 关闭标签页。`向右拆分`、`向下拆分` 在标签数 < 2 时
为 disabled（灰字，点击无反应）；`加入会话列表` 仅本地会话可用；`Merge panes`
仅在已分屏时可用。

标签 pill 命中区随窗口位置变化（窗口可被拖动），按截图实测：pill 高约 29px、
位于标题栏下方第一行；右侧 `x` 为关闭按钮，pill 右侧空白不响应右键。
