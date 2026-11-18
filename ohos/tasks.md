# Tasks：OHOS 快捷命令 / 命令历史

依据：`ohos/spec.md`。只改 `ohos/`（上游 `src/`、`ui/` 零改动）。

## [x] Q1 新增 `quick_commands.rs` 模型层（高）

- 新建 `ohos/meatshell-slint/src/quick_commands.rs`。
- 移植 `src/app/quick_commands.rs`：`all_quick_group_names`、`quick_cmd_model`、
  `command_preview`、`edit_index_after_delete`、`edit_index_after_swap`、
  `reorder_quick_command`。
- 移植 `src/app/terminal_ui.rs:3-112`：`history_model`、`history_view_rows`、
  `history_view_model`、`history_preview_model`、`set_history_view`。
- 移植 `src/terminal/impls/input.rs:54-60`：`encode_command_bar_input`。

验证：`cargo check` 通过（未接线也应编译通过）。

## [x] Q2 回调接线（高）

- `register(ui, store, send)` 实现 spec §2.1 表格中的 14 个回调。
- 初始写入 `quick-commands`（全折叠）、空的 `command-history` /
  `history-view` / `history-preview`。
- 所有写操作经 `ConfigStore` + `save()`；`store == None` 时只写空模型。

验证：管理弹窗增删改后模型立即刷新。

## [x] Q3 `ohos_app.rs` 终端写入桥（高）

- `wire_callbacks` 末尾构造 `send: Rc<dyn Fn(Option<&str>, &[u8])>`：
  `Some(id)` 写该 tab，`None` 写所有已连接 tab。
- 调用 `quick_commands::register(ui, store.clone(), send)`。
- `lib.rs` 增加 `mod quick_commands;`。

验证：命令栏命令能出现在终端。

补充：`cargo check --release --target x86_64-unknown-linux-ohos` 通过
（首轮缺 `ComponentHandle` 已补）。

## [x] Q4 构建部署（中）

- `cargo check` → `cd ~/DevEcoStudioProjects/khmeatshell && bash deploy.sh`
  （后台，thin LTO，约 20–30 分钟）→ 安装启动。
- 结果：BUILD SUCCESSFUL、SignHap success、install/start 成功。

## [x] Q5 真机验证（高）

按 `ohos/spec.md` §5 的 Q1–Q11 执行，截图存 `/tmp/qc-*.jpeg`。
重点：chip 执行 / 仅填入、分组折叠、管理弹窗全操作、历史过滤与删除、
重启后持久化、终端/SFTP/隧道回归。

进度（明细见 `ohos/spec.md` §9、`P2B-SESSION-EDITOR.md` §22-4）：
- [x] Q1 分组头与折叠
- [x] Q2 chip 执行
- [x] Q3 仅填入
- [x] Q4 新增/编辑/保存
- [x] Q5 组内重排
- [x] Q6 新建分组 + 右键删除空分组（`quick_groups: []`）
- [x] Q7 删除条目
- [x] Q8 历史回填
- [x] Q9 历史删除
- [x] Q10 重启持久化（UI 已确认 default/g1 折叠）
- [x] Q11 回归：终端输入、SFTP、隧道面板

## [x] Q6 回写报告（中）

- `ohos/P2B-SESSION-EDITOR.md` 追加 §22：能力、改动点、降级、矩阵结果。

---

# Tasks：三栏布局对齐 0.8.2 + 真实资源数据

依据：`ohos/spec.md` §10–§13、`.trae/documents/ohos-ui-layout-align.md`。
只改 `ohos/`（上游 `src/`、`ui/` 零改动）。

## [x] L1 `dock_layout.rs`：三栏几何与 `dock-panels` 写入（高）

- 常量：`WELCOME_WIDTH=240`、`SIDEBAR_WIDTH=280`、`WELCOME_MIN/MAX=160/480`、
  `SIDEBAR_MIN/MAX=180/420`、`MIN_CENTRAL=420`、`COLLAPSED_STRIP=36`。
- `init(ui)`：`welcome-as-sidebar=true`、两栏均展开；建空模型。
- `apply_dock(ui,w,h)`：写 `dock-central-*`，按折叠态决定是否 push panel；
  折叠时中央仍让出 36px，避免 `rest` 盖住 ToolStrip。
- `drag_panel(ui,index,pos)`：按 `kind` 回写宽度后重算。

验证：`cargo check --target x86_64-unknown-linux-ohos` 通过。

## [x] L2 `resource_stats.rs`：采样解析与面板写入（高）

- `StatsSample` / `PrevState`；`parse_block` 解析 cpu/meminfo/netdev/`__DF__`。
- `df_output()` 绝对路径回退（`/bin/df` → `df`）+ `PATH` 兜底。
- 过滤 `^/module_update`，磁盘按容量降序；环形缓冲 60 点。
- `tick_local`（2s 节流）、`show_local`、`show_remote`、`active`。

验证：本机面板出现 CPU/内存/交换/网速/磁盘；磁盘不再被 8 个
`/module_update` 100% 条目淹没。

## [x] L3 `ssh_session.rs`：远端 2s 采样（高）

- `Event::Stats(StatsSample)`；`shell_loop` 启动 `spawn_stats_sampler`。
- 独立 exec channel + 3s 超时，不干扰 shell / 隧道。

验证：连接 VPS 后数据 2s 刷新，数值与上游截图一致。

## [x] L4 `ohos_app.rs`：接线与消费（高）

- 初始化 `dock_layout::init/apply_dock`、`app-version=0.8.1`。
- 回调：`dock-area-resized`、`panel-extent-drag`、`dock-layout-changed`、
  `refresh-sidebar`、`set-*-collapsed`、`persist-*`（空实现）。
- poll 循环消费 `Event::Stats`，切 tab/断开按缓存回放。

## [x] L5 `session_editor.rs`：快速连接栏缺失回调（中）

- `reorder-session` / `reorder-session-end` / `submit-group` /
  `delete-group` / `import-ssh-config`（OHOS 提示不支持）。

## [x] L6 构建部署与真机验收（高）

- `cargo check --release --target x86_64-unknown-linux-ohos` 通过 →
  `bash deploy.sh`（后台）→ BUILD SUCCESSFUL、SignHap success、install/start 成功。
- 按 `ohos/spec.md` §12 R1–R8 验收，全部通过（明细见 §14）。
- 关键：`COLLAPSED_STRIP=36` 落地后 `dock-right-gap=36`，工具栏整体左移
  40px，右侧条带图标暴露；点击 `(窗口x+W-36+18, 标题栏高+19)` 可展开回 280。
- 截图：`/tmp/s16.jpeg`（默认三栏）、`/tmp/s5.jpeg`（右折叠）、
  `/tmp/s6.jpeg`（右展开）、`/tmp/s9.jpeg`（左折叠）、`/tmp/s10.jpeg`（左展开）、
  `/tmp/s13.jpeg`（拖拽）、`/tmp/s23.jpeg`（服务器资源）。

## [x] L7 布局持久化（中）

- `ohos_app.rs`：启动时读 `ConfigStore` 的 `welcome_sidebar_width` /
  `sidebar_width` / `welcome_collapsed` / `sidebar_collapsed` 覆盖默认；
  `sidebar_width==220`（上游默认）视为未设置，保留 280。
- `persist-sidebar-width` / `persist-welcome-sidebar-width` 在分隔条释放时
  clamp 后写回并 `save()`；`set-*-collapsed` 同时持久化折叠态。
  实现改用 `panel-extent-drag` + 400ms 防抖（dock-stack 分隔条无释放事件），
  统一走 `save_layout()`。
- 停靠边保持固定，`persist-*-dock` 仍 no-op。

验收：`ohos/spec.md` §15 P1–P4，全部通过（明细见 §15 真机结果）：
`welcome_sidebar_width: 301.0` 落盘 → 重启仍 301；`sidebar_collapsed: true`
→ 重启仍为 36px 条带；展开后重启保持 280。截图 `/tmp/p2.jpeg`–`/tmp/p5.jpeg`。

## [x] L8 启动页对齐 0.8.2（中）

- `ohos_app.rs`：启动不再创建 `welcome` 标签——`tabs` / `active-tab-id` /
  pane 的 `active-id` 均为空，中央只留壁纸（`app.slint` 仅在
  `pane.active-id == "welcome"` 时渲染欢迎卡片）。
- `close_tab` 关闭最后一个标签后保持空，`remaining_or_welcome` 改为
  `visible_tabs`，不再补默认标签；`update_pane_tabs` 允许 0 标签。
- 会话入口改为左栏「快速连接」行（`SessionRow` pointer up → `connect`）；
  标签栏 `+`（`activate_welcome_tab`）仍可回看欢迎卡片。
- `ui/app.slint`（经确认允许改上游）：内容区新增「空 pane + welcome-sidebar」
  的居中 Logo（`Theme.accent` 圆角块 `>_` + `MeatShell` + 标语），并把该状态的
  内容区背景改为 `transparent` 以露出壁纸。

验收：`ohos/spec.md` §16 S1–S3 通过：启动 Logo 居中（`/tmp/h1.jpeg`）、
单击左栏会话可连接（`/tmp/w2.jpeg`）、关掉最后一个标签回到 Logo 空态
（`/tmp/h2.jpeg`）。

## [x] L9 导入 / 导出（中）

- `session_editor.rs`：四个入口全部接线（齿轮菜单）。
  - `export-sessions` → `ConfigStore::export_to` 写
    `<files>/exports/meatshell-connections-<epoch>.json`。
  - `import-sessions` → `ConfigStore::import_from` 读
    `<files>/import/meatshell-connections.json`。
  - `import-ssh-config` → 读 `<files>/import/ssh_config`（或 `import/.ssh/config`），
    内置精简解析器（Host/HostName/Port/User/IdentityFile，忽略 `Host *`），
    按“同名或同 host+user”去重后 upsert + 保存 + 刷新列表。
  - `batch-import-confirm` → `host|port|user|password|name` 逐行解析、去重、
    保存、关闭弹窗并刷新。
- 所有结果通过 `ssh-import-hint` 提示条反馈；`Secret::new` 存批量导入的密码。
- 上游 `export_to` / `import_from` 未改；ssh_config 解析器因上游为 `pub(crate)`
  在端口内自实现。

验收：`ohos/spec.md` §17 I1–I4 通过：ssh_config 导入 2 主机（`/tmp/i3.jpeg`）、
导出 5 会话 JSON、JSON 导入 1 新 + 跳过 4（`/tmp/i4.jpeg`）、批量导入新增
`root@batch-box`。测试数据已清理（`/tmp/i10.jpeg`）。

## [x] L10 设置 › 界面接线与持久化（中）

- `color_theme.rs`：移植上游纯计算部分 + `apply_color_theme(&AppWindow, &ConfigStore)`；
  去掉 `sync_detached` / `registry.broadcast`。
- `settings.rs`：
  - `apply_saved_appearance`：dark-mode（system→dark）、term font family/size/
    line-spacing/bold、cursor style/color、ui-scale、panel-font、`term-fonts`、
    配色主题、collapse-*-default、welcome/quick-commands-as-sidebar、
    ssh-keepalive、session-log、paste/download/zen/sftp-follow-cd 开关。
  - `register`：`set-language`（仅持久化）、`toggle-theme`、`set-term-font`、
    `set-term-font-size`、`set-terminal-line-spacing`、`set-term-font-bold`、
    `set-term-cursor-style`、`set-term-cursor-color`(bool)、`set-ui-scale`、
    `set-panel-font`、`set-color-theme`、`set-color-theme-custom`(bool)、
    `set-immersive-colors`、`set-collapse-sidebar-default`、
    `set-collapse-sftp-default`、`set-welcome-as-sidebar`、
    `set-quick-commands-as-sidebar`、`set-download-always-ask`、
    `set-paste-confirm-enabled`、`set-extra-paste-shortcuts-enabled`、
    `set-zen-mode`、`set-ssh-keepalive`、`set-sftp-follow-cd`、
    `set-session-log-enabled`，全部 `set_*` + `save()`。
- `ohos_app.rs`：`AppWindow::new()` 后 `apply_saved_appearance`；
  `session_editor::register` 之后 `settings::register`（store clone）。
- 限制：运行时切语言不生效（静态文案已缓存）；切主题不影响已连接终端配色。
- 构建：`cargo check --release --target x86_64-unknown-linux-ohos`，部署验收后
  回写 §18 T1–T4。
- 验收：spec §18 T1–T4 通过——钛灰/默认预设与深浅色即时切换并重启保留
  （`/tmp/st11.jpeg`、`/tmp/st14.jpeg`、`/tmp/st16.jpeg`）、字号 13→16px 与
  120% 缩放即时生效（`/tmp/st15.jpeg`）、`terminal_bold`/`ui_scale`/`font_size`/
  `color_theme` 均落盘 `sessions.json`（`/tmp/st17.jpeg`）、回归正常
  （`/tmp/st20.jpeg`）。已恢复默认主题、13px、100%。




## [x] L11 终端输出高亮（中）

- 新增 `output_highlight.rs`：`Rules::from_store`、`validate_rule`、`custom_model`、
  `highlight`，逐行移植 `log_level_marker` / `devops_marker` / `ascii_word_boundary`。
- `terminal.rs`：`Run`/`palette_rgb` 提为 `pub(crate)`，`rebuild` 增加 `rules` 参数
  并在每行结束后调用 `highlight`。
- `settings.rs`：`apply_saved_appearance` 写高亮三属性；`register` 增参
  `highlight`/`repaint`，接线 `set-output-highlight`、`add/remove/set-*-enabled` 四个回调。
- `ohos_app.rs`：构建 `highlight` 与 `repaint_all`，传入 `wire_callbacks` /
  `start_poll_timer` / `settings::register`；`Cargo.toml` 加 `regex`。
- 验证：`cargo check --release --target x86_64-unknown-linux-ohos` → 部署真机 →
  按 `spec.md` §19 O1–O5 验收并截图 → 回写结果、恢复默认状态。

验收结果（`spec.md` §19.1）：O1–O5 全部通过。主开关与预设切换即时重绘
（`/tmp/o8.jpeg`、`/tmp/o12.jpeg`、`/tmp/o19.jpeg`）；关键词规则与整行规则
分别验证（`/tmp/o31crop.png`、`/tmp/o33.jpeg`）；`sessions.json` 三字段
正确落盘并在重启后生效；测试后已清空自定义规则、恢复日志级别与开启状态
（`/tmp/o35.jpeg`）。遗留问题：设置页内容在小屏被裁剪且 ScrollView 不随
触摸滚动，规则列表开关/删除不可达，见 `spec.md` §19.2。




## [x] L12 下载落点改为系统公共下载目录（中）

- `module.json5` 声明 `ohos.permission.READ_WRITE_DOWNLOAD_DIRECTORY`，`string.json`
  补 `download_dir_reason`。
- `SlintSpike.ets`：启动后 `prepareDownloadDir()` 申请权限 → `Environment.getUserDownloadDir()`
  → 逐个候选目录 `canWriteDir` 探针 → `meatshell.slintSetDownloadDir(dir)`。
- NAPI `slintSetDownloadDir` → `ms_spike_set_download_dir` → `UiState::set_download_dir`；
  `downloads_dir()` 优先公共目录，SFTP 本地栏跟随切换，文案改「已下载到下载目录」。
- 验证：`cargo check --release --target x86_64-unknown-linux-ohos` → ArkTS 热更部署 →
  双栏 SFTP 双击远端 `.bash_history` 下载 → 校验公共目录文件 → 清理测试文件。

验收结果（`spec.md` §20.1）：P1–P3 通过。日志 `download dir ready:
/storage/Users/currentUser/Download`；下载的 `.bash_history` 落在
`/storage/media/100/local/files/Docs/Download/`（1680 字节，md5 一致），测试文件已删除。
阻塞说明（`spec.md` §20.2）：本机 ROM 请求该权限时弹出「位置信息」模板、
`authResults[0] = -1`，ACL 预授予与 `requestPermissionOnSetting` 均无效，
只能 root `atm perm -g` 预授权；`Environment.getUserDownloadDir()` 在本机抛 801，
实际落点由候选路径探测得到。

### L12.1 picker 兜底（`spec.md` §20.3）

- `ohos_app.rs`：`PENDING_SAVE_EXPORT` 队列 + `public_download_ready()`；
  下载完成且公共目录未就绪时用 `finished_local_path` 定位实际文件入队，
  `take_action` 输出 `save-download:<路径>`。
- `SlintSpike.ets`：动作分流 → `exportDownload()`，系统保存对话框选位置后
  `fs.copyFileSync` 复制到返回 URI。
- 验收：临时直调 `exportDownload` 触发，对话框弹出并预填文件名，选「下载」→「保存」
  后 `acme.sh` 落盘公共目录（1295 字节，内容一致）；取消路径无异常；测试文件已删除，
  权限与外观已复位。

### L13 系统文件选择器上传（`spec.md` §21）

- [x] `ohos_app.rs`：`PENDING_UPLOAD_PICK` / `UPLOAD_DEST_TAB` 静态；
      `on_sftp_upload_clicked` 入队；`take_action` 输出 `pick-upload:file|folder`；
      新增 `UiState::upload_picked_files` 复用 `SftpCommand::Upload`；
      `upload_pick_cancelled` 复位取消状态。
- [x] `lib.rs`：`ms_spike_upload_files`（`\n` 分隔路径）、`ms_spike_upload_cancel`。
- [x] `napi_init.cpp` + `Index.d.ts`：`CommandKind::UploadFiles` / `UploadCancel`、
      `slintUploadFiles` / `slintUploadCancel`。
- [x] `SlintSpike.ets`：`pick-upload:` 分流 → `pickUpload(folder)`；picker 选择 →
      复制到 `${filesDir}/upload-staging`（保留原文件名、目录递归）→ `slintUploadFiles`；
      取消回传 `slintUploadCancel`。
- [x] 构建部署：`cargo check --release --target x86_64-unknown-linux-ohos` →
      `bash deploy.sh`（x86_64 + aarch64）→ 签名 → `hdc install -r`。
- [x] 真机验收 U1–U7（`spec.md` §21.6）：U1/U2/U4/U5/U6/U7 通过；U3 代码就绪，
      因本机 picker 多选需 Ctrl+单击、自动化无修饰键而记为限制。
- [x] 清理：删除远端与本地测试文件、清空 `upload-staging`，回写 §21.6 结果。

### L14 设置菜单的导入 / 导出连接可用化（`spec.md` §22）

- [x] `ohos_app.rs`：新增 `IMPORT_PICK_REQUESTED` / `PENDING_TOAST` /
      `IMPORT_PICKED_HANDLER` 与 `queue_toast` / `queue_save_export` /
      `request_import_pick` / `set_import_picked_handler` / `import_picked_file`；
      `take_action` 增加 `toast:<文本>`、`pick-import` 两个动作；
      `downloads_dir()` / `public_download_ready()` 提到 `pub(crate)`。
- [x] `session_editor.rs`：导出改写到公共下载目录（未授权时沙箱 + 系统保存对话框），
      导入改为系统 picker → `import_from` → 刷新列表，结果统一走 toast。
- [x] `lib.rs` / `napi_init.cpp` / `Index.d.ts`：`ms_spike_import_picked` 全链路 +
      `slintImportPicked`。
- [x] `SlintSpike.ets`：`pick-import` → `pickImport()`（单选 `.json`，复制到
      `${filesDir}/import`）；`toast:` → `showToast()`。
- [x] 构建部署 + 真机验收 I1–I6 全部通过（结果见 `spec.md` §22.5）：
      导出落到公共下载目录并 toast；导入经系统 picker → 合并 → toast；
      取消/非法 JSON/连接回归正常；不再写 `ssh-import-hint` 后欢迎栏无残留提示。
- [x] 清理：`Download` 的导出 JSON 与 `badimport.json`、沙箱 `import/*.json`、
      `upload-staging/*`、历史遗留 `files/exports/`；应用回到欢迎页。

### L15 会话编辑对话框的「测试连接」与「选择私钥」（`spec.md` §23）

- [x] `ohos_app.rs`：抽出 `pub(crate) fn auth_for_session`（`connect_session` 改调用，
      行为不变）；新增 `PENDING_DIALOG_STATUS` + `queue_dialog_status` +
      `take_dialog_status`，`poll_sessions` 开头写回 `dialog-test-status`；
      新增 `PENDING_KEY_PICK` / `KEY_PICKED_HANDLER` / `request_key_pick` /
      `set_key_picked_handler` / `key_picked_file`，`take_action` 增加 `pick-key`。
- [x] `session_editor.rs`：`on_session_dialog_test` 用草稿构 `Session`（空密码沿用
      已存值、不启转发）→ `SshSession::spawn` 线程内每 100 ms `drain()`、12 s 超时；
      `Connected`/`Failed`/`HostKey`/`Credentials`/`Mfa`/超时分别给出中文状态；
      `test_cancel` 代次位由 test / stop / changed / closed / submit 共享。
      `on_session_dialog_pick_key` 注册回填 handler 后 `request_key_pick()`。
- [x] `lib.rs` / `napi_init.cpp` / `Index.d.ts`：`ms_spike_key_picked` +
      `slintKeyPicked` 全链路。
- [x] `SlintSpike.ets`：`pick-key` → `pickKey()`（单选、复制到
      `${filesDir}/keys`、`slintKeyPicked`）。
- [x] 构建部署：`cargo check --release --target x86_64-unknown-linux-ohos` →
      `bash deploy.sh`（已部署到 `com.kaihong.khmeatshell`）。
- [x] 真机验收 T1–T7（步骤见 `spec.md` §23.5）：T1/T2/T5 通过；
      T3（错误密码 →「连接失败: root@203.0.113.10…」）、T4（取消后重开状态为空，
      14 s 后仍为空）、T6（浏览私钥回填 `files/keys/id_ed25519`）、
      T7（连接/终端/SFTP 上传下载/导出/字体重开关）本轮全部通过。
- [x] 回写 `spec.md` §23.5 结果表与 §23.6 清理记录；已删除 `files/keys`、
      `upload-staging`、`Download` 测试文件与远端 `/root/id_ed25519`；
      `ui_scale=100`、`terminal_bold=false`，应用回到 Logo 空态。

### L16 设置页滚动不可用（`spec.md` §24）

- [x] `ohos_app.rs`：新增 `touch(x, y, phase)`，以 `InternalEvent::Touch`
      转发（id=0，phase 0/1/2/3）。
- [x] `lib.rs`：新增 `ms_spike_touch(x, y, phase)` FFI。
- [x] `napi_init.cpp` / `Index.d.ts`：`CommandKind::Touch` + `slintTouch` 全链路。
- [x] `SlintSpike.ets`：`onTouch` 改发触摸事件（含 Cancel 与
      `changedTouches` 回退），`onMouse` 保持原样。
- [x] 构建部署：`cargo check --release --target x86_64-unknown-linux-ohos` →
      `bash deploy.sh`（`/tmp/deploy-l16.log`，BUILD SUCCESSFUL + 签名成功；
      手动 `hdc -t 192.0.2.145:10178 install -r /tmp/khmeatshell-signed.hap`）。
- [x] 真机验收 S1–S7：S1/S2/S3/S4/S7 通过；S5 不适用（移植未接
      `term-select-*`，选择高亮本就不存在）；S6 无回归（列表未溢出、SFTP 行
      拖拽属上游 drag-out）。
- [x] 回写 `spec.md` §24.8 真机结果与清理记录（`output_highlight_rules=[]`、
      `ui_scale=100`、3 条会话、回到欢迎页）。

### L17 终端选择 / 复制 / 粘贴（`spec.md` §25）

- [x] `ohos_app.rs`：`TermTab` 增加 `sel_anchor` / `sel_focus` / `selection` /
      `pending_paste`；接线 `on_term_select_start/update/end/word`、
      `on_copy_terminal_text`、`on_paste_from_clipboard`、
      `on_paste_confirmed/cancelled`；`handle_pasted_text` 在 `PASTE_TARGET`
      为空时回落到 `active-tab-id`（长按浮层路径没有 target）。
- [x] `terminal.rs`：`row_chars` / `row_span` / `selection_rects` /
      `selected_text` / `visible_text` / `word_bounds` / `paste_bytes`。
- [x] `lib.rs` / `napi_init.cpp` / `Index.d.ts`：`ms_spike_clipboard_take`、
      `ms_spike_clipboard_text`、`ms_spike_free_string` 与
      `slintClipboardTake` / `slintClipboardText` 全链路（此前已接，本轮未改）。
- [x] `SlintSpike.ets`：`clipboard-set` → `setData`；`clipboard-get` →
      读剪贴板，无权限读到空时浮出系统安全控件
      `PasteButton({icon: LINES, text: PASTE})`；长按 600 ms 合成右键打开
      Slint 菜单并在其下方 8px 浮出按钮；`getPrimaryText()` 的 `undefined`
      守卫（无权限返回空 PasteData）。
- [x] 放弃 `READ_PASTEBOARD` 权限方案并回滚：`module.json5`、`string.json`、
      `signing/profile.json`（`allowed-acls`）与 `profile.p7b` 均无残留。
- [x] 构建部署：`cargo build --release --target x86_64-unknown-linux-ohos`
      （7m01s，仅编译 x86_64）→ clang++ 链接 → hvigor → sign-app →
      `hdc install -r` + `aa start`（`com.kaihong.khmeatshell`）。
- [x] 真机验收 C1–C7（步骤见 `spec.md` §25.5）：C1/C2 拖选 + `clipboard-set: 29`
      自动复制；C3 菜单 + 系统「粘贴」按钮同时出现（`/tmp/p2.jpeg`）；
      C4 点按钮授权后 `clipboard-get: 29` 并写入提示符（`/tmp/p3.jpeg`）；
      C5 多行 97 字符粘贴弹「确认多行粘贴」，取消不写入（`/tmp/p5.jpeg`、
      `/tmp/p6.jpeg`）；C6 菜单路径同链路；C7 无回归（`/tmp/p7.jpeg`）。
- [x] 回写 `spec.md` §25（现象 / 根因 / 方案 / 非目标 / 验收矩阵 / 环境限制 /
      真机结果 / 回滚）。清理：关闭 vps 标签回欢迎页，`sessions.json` 未改。

### L18 终端交互回归：双击取词 / 清空缓存 / 查找（`spec.md` §25.9）

- [x] `ohos_app.rs`：`TermTab` 增加 `last_tap` / `skip_end_copy` /
      `find_matches`；`on_term_select_start` 内本地双击检测（400 ms + 同格）
      调用 `terminal::word_bounds` 并复用复制链路，`on_term_select_end` 用
      `skip_end_copy` 跳过重复复制（OHOS 触摸下 Slint `double-clicked`
      不触发）。
- [x] `ohos_app.rs`：新增 `on_clear_terminal`（清 spans/选区/查找模型 +
      PTY 写 `Ctrl+L`）与 `on_find_query_changed`（写 `find_matches` 模型）。
- [x] `terminal.rs`：新增 `find_matches(parser, query)`（大小写不敏感、按 cell
      折算 `(row, col, len)`，CJK 用首尾列）。
- [x] 行模型 `find_matches` 由 `empty_model()` 改为 `Rc<VecModel<TermMatch>>`。
- [x] 构建部署：`cargo build --release --target x86_64-unknown-linux-ohos`
      （7m50s）→ clang++ 链接 → hvigor → sign-app → `hdc install -r` →
      `aa start`。
- [x] 真机验收：双击提示符取词 → `clipboard-set: 24 chars` + 高亮
      （`/tmp/u2c.png`）；「清空缓存」→ 屏幕清空、提示符回首行
      （`/tmp/u3.jpeg`）；「查找」→ 查找栏出现（`/tmp/v9.jpeg`），注入
      `root` 后终端全部 `root` 高亮（`/tmp/w6c.jpeg`），× 关闭后高亮清除
      （`/tmp/w7.jpeg`）；`Ctrl+C` 回归正常（`/tmp/w8.jpeg`）。
- [x] 回写 `spec.md` §25.9（含本机操作限制：查找栏物理 y≈147..168、打开后
      直接 `uitest uiInput text` 勿点击输入框、菜单四行 y≈310/340/370/400）。

### L19 命令栏复制 / 粘贴（`spec.md` §26）

- [x] `ohos_app.rs`：新增 `CLIPBOARD_CACHE` / `CLIPBOARD_SET_REQUESTED` /
      `CLIPBOARD_GET_REQUESTED` / `PASTE_FOCUS`；`remember_clipboard_text`、
      `clipboard_cache_text`（未命中时排队 `clipboard-get-input`）、
      `publish_clipboard_text`（排队 `clipboard-set`）、`commit_text_to_focus`
      （`InternalKeyEvent::CommitComposition`）；`handle_pasted_text` 在
      `PASTE_FOCUS` 且无 `PASTE_TARGET` 时插入焦点控件；`take_action`
      新增 `clipboard-set` / `clipboard-get-input`。
- [x] `lib.rs`：`impl Platform for OhosPlatform` 接线
      `clipboard_text` / `set_clipboard_text`（返回 `Option<String>`）。
- [x] `SlintSpike.ets`：`clipboard-get-input` → `pasteFromClipboard(false)`；
      未读到文本时 `showPasteChip(false)` 把粘贴按钮放底部居中，
      粘贴后仅在菜单来源时补一次点击收起菜单（`pasteChipFromMenu`）。
- [x] 构建部署：`CARGO_TARGET_DIR=/tmp/meatshell-slint-target cargo build
      --release --target x86_64-unknown-linux-ohos`（6m32s）→ clang++ 链接
      → hvigor（11s）→ sign-app → `hdc install -r` → `aa start`。
- [x] 真机验收（`spec.md` §26 表）：`Ctrl+A`+`Ctrl+C` → `clipboard-set: 10`
      + `setData ok`；`Ctrl+U` 剪切整行；同进程 `Ctrl+V` 缓存回填；
      重启后 `Ctrl+V` 走 `clipboard-get: 10` → 插入命令栏；点发送执行
      （`helloecho: command not found`）；终端长按粘贴仍写 PTY。
- [x] 回写 `spec.md` §26（现象 / 根因 / 方案 / 已知边界 / 真机结果 /
      键码与坐标提示）。

### L20 命令栏鼠标右键菜单（`spec.md` §27）

- [x] `ohos_app.rs`：`pointer()` 在右键（`kind == 0 && button == Right`）
      且 `focus_text_field_at(ui, x, y)` 命中时抛 `input-menu` 动作；
      新增 `focus_text_field_at`——用 `WindowInner::focus_item` +
      `TextInput` downcast + 逐级 `parent_item(StopAtPopups)` 累加原点得到
      窗口矩形做命中测试（终端 `ime-input` 是 0×0，永远不会命中）。
- [x] `SlintSpike.ets`：`input-menu` → `openInputMenu()`（120×152，贴边夹紧、
      放不下翻上方）；`synthCtrl` 回放 Ctrl+A/C/X/V；`inputMenuItem` 四行
      （全选 `keepOpen`，其余点击即收起）；全屏透明背景层点击只收菜单。
- [x] 触摸长按（600 ms）合成右键时同步记录 `menuAnchorX/Y`，与鼠标右键共用
      同一入口。
- [x] 构建部署：`cargo build --release --target x86_64-unknown-linux-ohos`
      → clang++ 链接 → hvigor → sign-app → `hdc install -r` → `aa start`。
- [x] 真机验收（`spec.md` §27 表）：长按命令栏弹出四项；「全选」保持菜单；
      「复制」`clipboard-set: 5 chars`；清空后「粘贴」缓存命中直接回填；
      「剪切」清空并写剪贴板；长按终端主体仍是上游菜单。
- [x] 真实鼠标右键（人工操作）：日志 `mouse 1/2 btn=1`，命令栏输入框内右键
      弹出四项菜单（`/tmp/r1.jpeg`）；点在输入框上沿之外不弹菜单。
- [x] 定位补偿：`menuAnchorX/Y` 改存 XComponent 局部 px，`openInputMenu`
      用 `getRectangleById('slintSurface').windowOffset` 换算到页面 vp（取不到
      时退回 `px2vp`）；复测菜单贴住点击点（700,468 → 704,476），
      「全选」+「复制」仍写剪贴板 5 chars（`/tmp/f1.jpeg`、`/tmp/f2.jpeg`）。
- [x] 回写 `spec.md` §27（现象 / 根因 / 命中测试 / 已知边界 / 真机结果 /
      自动化提示：`longClick`、`uinput -M -c 1` 的右键被系统桌面 shell
      消费、IME 需空格提交、菜单坐标）。
- [x] 修复长按误触按钮：`ohos_app.rs` `pointer()` 新增 `kind == 3 =>
      WindowEvent::PointerExited`；`SlintSpike.ets` 长按定时器在合成右键前
      先发 `slintPointer(x, y, 3, 0)`，抬手恢复 `slintTouch(x, y, 2)`。
      原先改用 `TouchPhase::Cancelled` 无效——`TouchState::process_ended`
      在 Idle 状态对 Ended/Cancelled 都补发 `Released{Left}`。
- [x] 真机回归：长按「发送」不再发送（`/tmp/w2.jpeg`）、普通点击仍发送
      （`/tmp/w3.jpeg`）、长按命令栏仍弹四项菜单（`/tmp/w4.jpeg`）、
      长按终端主体仍弹上游菜单（`/tmp/x3.jpeg`）。
- [x] 回写 `spec.md` §27「长按误触按钮的修复」+ 自动化提示（有 Slint
      弹层时事件会被吞、截图缩放 1.334 倍换算物理 px）。

### L21 长按粘贴按钮按需浮出 + 名称/图标统一（`spec.md` §27）

- [x] 回退 `term-menu` 方案：`ohos_app.rs` `pointer()` 恢复原版；
      `SlintSpike.ets` 长按定时器不再提前 `showPasteChip()`，
      仅当点菜单「粘贴」且 `readClipboardText()` 为空时兜底浮出。
- [x] `showPasteChip` 改用 `menuAnchorX/Y` + `windowOffset` 定位；
      `dismissSlintMenu` 改用菜单锚点补点击。
- [x] 名称统一：`AppScope` `app_name`、`entry` `module_desc` 改为 MeatShell；
      `ui/app.slint`（窗口 title、自定义标题栏、关于页、关闭确认）、
      `ui/sidebar.slint`、`ui/welcome.slint`、三个独立窗口 title、
      `ui/interface_panel.slint` 两条提示、`lang/{zh,en}` 的关于行 msgid；
      命令行名/文件名/URL 保持小写。
- [x] 图标统一：复用上游肉块几何与配色重绘 `foreground.png`（1024、
      安全区 0.88）、`background.png`（1024 全出血）、`startIcon.png`（512）。
- [x] 构建部署：cargo build x86_64（6m34s）→ clang++ 链接 → hvigor（5s）
      → sign-app → `hdc install -r` → `aa start`。
- [x] 真机验收：标题栏/欢迎页/页脚显示 `MeatShell`、图标为五花肉
      （`/tmp/r4.jpeg`）；侧栏长按仅上游会话菜单（`/tmp/r5.jpeg`）；
      终端菜单无按钮、点「粘贴」后浮出（`/tmp/r8.jpeg`、`/tmp/r9.jpeg`）；
      长按发送不误发（`/tmp/r11.jpeg`）；命令栏四项菜单无按钮
      （`/tmp/r12.jpeg`）。
- [x] 回写 `spec.md` §27（按需浮出方案、名称与图标、ui 改动需重跑 cargo）。

### L22 上架前收尾：主体/版本/授权署名/双 ABI

- [x] `AppScope/app.json5`：`vendor` `example` → `kaihong`（与 bundleName
      `com.kaihong.khmeatshell` 一致）；`versionName` `1.0.0` → `0.8.1`，
      与 `ohos_app.rs:393` 的 `set_app_version("0.8.1")` 及界面页脚一致。
- [x] 关于页补齐 Slint Royalty-free 2.0 要求的署名：`Made with Slint` +
      `Slint (Royalty-free 2.0) · Twemoji (CC BY 4.0) · Material Icons
      (Apache 2.0) · Cascadia Mono (OFL 1.1)`；卡片高度 188 → 236px，
      避免长文本换行后被裁切。
- [x] 双 ABI：x86_64 与 aarch64 各重建一次（含署名改动），
      链接到 `entry/libs/{x86_64,arm64-v8a}/libmeatshell.so`，
      并随包携带 `libc++_shared.so`、`libtime_service_ndk.so`。
- [x] hvigor `assembleHap` → `sign-app` → `hdc install -r` → `aa start`；
      设备 `bm dump` 显示 `"vendor": "kaihong"`、`"versionName": "0.8.1"`。
- [x] 真机核验：关于卡片显示 `MeatShell v0.8.1`、`Made with Slint`
      与三行许可清单，排版未溢出（`/tmp/s5.jpeg`）。
- [ ] 未纳入本轮（需另行处理）：正式发布证书替换 debug provision、
      aarch64 真机回归、应用市场软著/备案材料。

### L23 壁纸与主题：内置切换 + 自定义图片（`spec.md` §29）

- [x] 新增 `ohos/meatshell-slint/src/wallpaper.rs`：`load()` 复刻上游
      `render_builtin` / `render_tech`（幻想 3048）、`decode_miku` /
      `decode_ms` 与自定义解码；`derive_palette` / `vivid_accent` 取色；
      `apply()` 写回 `wallpaper-img` / `wp-accent` / `wp-tint` /
      `current-wallpaper` / `dark-mode`。
- [x] `settings.rs` 注册三个从未接线的回调：`on_set_wallpaper`（保存并应用）、
      `on_pick_wallpaper_file`（请求 ArkTS 选图）、
      `on_persist_wallpaper_overlay`（改遮罩并落盘）；`apply_saved_appearance`
      末尾按 `wallpaper.txt` / `wallpaper-overlay.txt` 恢复。
- [x] `Cargo.toml` 增加 `image`（png/jpeg/webp/bmp，与 image-decoders 同版本）。
- [x] 选图桥：`lib.rs` 新增 NAPI `ms_spike_wallpaper_picked(path)`，
      `napi_init.cpp` 新增 `WallpaperPicked` 命令与 `slintWallpaperPicked`，
      `index.d.ts` 补声明，`SlintSpike.ets` 的 `onAction` 处理 `pick-wallpaper`。
- [x] ArkTS `pickWallpaper()`：`DocumentViewPicker` 选图 → `wallpaperCandidates()`
      依次尝试原始 URI / `/storage/Users/...` / 媒体库真实路径 → 复制到
      `<filesDir>/wallpapers/` → 把沙盒路径交回 Rust。
- [x] 真机回归（x86_64）：内置壁纸切换 + 选中态 + 亮色主题自动切换；
      自定义 `w.jpg` 复制进沙盒（93698 B）、`wallpaper.txt` 写入沙盒路径、
      背景立即替换、`aa force-stop` 后重启仍保持。
- [x] 回写 `spec.md` §29（实现表、URI 兼容策略、真机结果）与自动化提示：
      **注入左键 `uinput -M -c` 会同时合成 `px=0,0` 的 touch 吞掉点击，
      必须改用 `uinput -T -c x y`**；系统选图器需双击文件行确认。




### L24 补齐上游未接线回调（`spec.md` §30）

- [x] 盘点：上游 `src/app/*` 257 个 `on_*` 与移植侧求差（90 个缺失）。
- [x] 标签：`cycle-tab`、`tab-duplicate`、`tab-rename-request`、`rename-tab`；
      新增 `set_tab_title`，同时更新 `tabs` 与 pane 快照。
- [x] 缩放：`zoom-term-font` 单标签 px 覆盖 / 全局字号，范围 8–32。
- [x] 鼠标：`terminal-mouse`（X10 + SGR），轮询补 `mouse_tracked` /
      `is_alt_screen`。
- [x] 全屏：`toggle-term-fullscreen`、`term-fullscreen-check`。
- [x] 资源：`select-net-iface`（`TermTab.selected_iface` +
      `show_remote(preferred)`）。
- [x] 设置：`set-cmd-bar-hidden`、`set-json-format-output`、
      `set-renderer-mode`、`set-editor-embedded`、`set-sync-upload-enabled`、
      `set-mcp-permissions`、`set-sync-input`（广播输入）。
- [x] `cargo check --release --target x86_64-unknown-linux-ohos` 通过。
- [x] 真机：标签右键重命名（对话框弹出、取消正常）、复制标签（第二标签出现）。
- [ ] 真机（受阻）：输入同步、网卡下拉、vim/htop 鼠标点击 —— 三者都需要一条
      已连通的 SSH 会话；当前设备上没有可达的 sshd（`127.0.0.1:2222`
      Connection refused，macOS 端 sshd 未开启），无法验证，待有可达主机时补测。
- [ ] 真机（受阻）：Ctrl+Tab 等键盘路径 —— `uinput -K` 注入的按键不进入
      XComponent（命令栏输入 `abc` 无回显），需真实键盘或 IME 才能验证。
- [ ] 未做项按 `spec.md` §30.3 记为降级（独立窗口 / 传输取消 / WebDAV /
      更新检查 / 快捷键录制 / scrollback）。

### L25 分屏（`spec.md` §31）

- [x] `SplitState` + `refresh_panes`：内容尺寸变化时重算两个 pane 与分隔条。
- [x] `pane-split`（向右/向下，最多两 pane）、`pane-merge`、`splitter-drag`。
- [x] `close_tab` 分屏感知：从各 pane 快照移除；pane 空则自动合并。
- [x] `update_pane_tabs` 分屏时只追加新标签，不再用全局标签重建。
- [x] `pane-focus` 同步全局 `active-tab-id`。
- [x] `cargo check` 通过。
- [x] 分隔条拖动改为平台层接管（`splitter_gesture` + `split_drag`），
      坐标按 pane 容器原点 `content-origin-x/y` 换算；触摸与鼠标左键共用。
      根因与实现见 `spec.md` §31，验证步骤见 §31.1。
- [x] 真机：向右拆分 / 向下拆分 / 拖动分隔条 / 合并 / 关闭空 pane 自动合并
      全部通过（`spec.md` §31.1 的 9 步，含触摸与鼠标两种指针）。
      拖动修复的关键点：命中判定必须用 `content-origin-x/y`（`dock-central-y`
      不含标题栏，纵向会偏 26px）。
- [ ] 未接线：标签跨 pane 拖拽、pane 内标签重排。
