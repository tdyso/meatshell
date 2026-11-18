//! Quick commands + command history for the OHOS port.
//!
//! Ports the upstream model builders (`src/app/quick_commands.rs`), the history
//! helpers (`src/app/terminal_ui.rs`) and `encode_command_bar_input`
//! (`src/terminal/impls/input.rs:54`). The wiring mirrors
//! `src/app/key_input.rs:110-370`, with the desktop clipboard and dock panel
//! left out (no OHOS counterpart yet).

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::{AppWindow, QuickCmd};
use meatshell_core::config::{ConfigStore, QuickCommand};

pub(crate) type CollapsedGroups = Rc<RefCell<HashSet<String>>>;

/// Writes bytes to one tab's PTY, or to every live session when `None`
/// (`to-all` in the command bar).
pub(crate) type SendBytes = Rc<dyn Fn(Option<&str>, &[u8])>;

pub fn all_quick_group_names(store: &ConfigStore) -> HashSet<String> {
    let cmds = store.quick_commands();
    let mut set: HashSet<String> = HashSet::new();
    if cmds.iter().any(|c| c.group.trim().is_empty()) {
        set.insert("default".to_string());
    }
    for g in store.quick_groups() {
        set.insert(g.clone());
    }
    for c in cmds {
        let g = c.group.trim();
        if !g.is_empty() {
            set.insert(g.to_string());
        }
    }
    set
}

/// Build the quick-command model for the command bar + manage dialog (#55).
pub fn quick_cmd_model(
    store: &ConfigStore,
    collapsed_groups: &HashSet<String>,
) -> ModelRc<QuickCmd> {
    let cmds = store.quick_commands();

    let has_default = cmds.iter().any(|c| c.group.trim().is_empty());
    let mut named: Vec<String> = store
        .quick_groups()
        .iter()
        .cloned()
        .chain(
            cmds.iter()
                .map(|c| c.group.trim().to_string())
                .filter(|g| !g.is_empty()),
        )
        .collect();
    named.sort_by_key(|g| g.to_lowercase());
    named.dedup();

    let mut groups: Vec<String> = Vec::new();
    if has_default {
        groups.push("default".to_string());
    }
    groups.extend(named);

    let mut rows: Vec<QuickCmd> = Vec::new();
    for group in &groups {
        let is_collapsed = collapsed_groups.contains(group);
        let members: Vec<(usize, &QuickCommand)> = cmds
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                let g = c.group.trim();
                if group == "default" {
                    g.is_empty()
                } else {
                    g == group
                }
            })
            .collect();
        if members.is_empty() {
            rows.push(QuickCmd {
                name: "".into(),
                command: "".into(),
                group: group.clone().into(),
                group_header: group.clone().into(),
                collapsed: is_collapsed,
                orig_index: -1,
                send_enter: true,
                command_preview: "".into(),
            });
        } else {
            for (i, (orig_idx, c)) in members.iter().enumerate() {
                rows.push(QuickCmd {
                    name: c.name.clone().into(),
                    command: c.command.clone().into(),
                    group: group.clone().into(),
                    group_header: if i == 0 {
                        group.clone().into()
                    } else {
                        "".into()
                    },
                    collapsed: is_collapsed,
                    orig_index: *orig_idx as i32,
                    send_enter: c.send_enter,
                    command_preview: command_preview(&c.command).into(),
                });
            }
        }
    }
    ModelRc::from(Rc::new(VecModel::from(rows)))
}

/// One-line rendering of a quick command for lists and chips (#419).
pub fn command_preview(command: &str) -> String {
    const MAX_CHARS: usize = 240;
    let mut out = String::new();
    for (i, line) in command
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .enumerate()
    {
        if i > 0 {
            out.push_str(" \u{23CE} ");
        }
        out.push_str(&line);
        if out.chars().count() > MAX_CHARS {
            break;
        }
    }
    if out.chars().count() > MAX_CHARS {
        out = out.chars().take(MAX_CHARS).collect();
        out.push('…');
    }
    out
}

/// Where the entry being edited ends up after deleting `deleted` (#419).
pub fn edit_index_after_delete(edit_index: i32, deleted: i32) -> i32 {
    if edit_index < 0 || deleted < 0 {
        edit_index
    } else if edit_index == deleted {
        -1
    } else if edit_index > deleted {
        edit_index - 1
    } else {
        edit_index
    }
}

/// Where the entry being edited ends up after `a` and `b` swap places.
pub fn edit_index_after_swap(edit_index: i32, a: usize, b: usize) -> i32 {
    if edit_index == a as i32 {
        b as i32
    } else if edit_index == b as i32 {
        a as i32
    } else {
        edit_index
    }
}

/// Move entry `index` one step within its group.
pub fn reorder_quick_command(
    commands: &mut [QuickCommand],
    index: usize,
    move_up: bool,
) -> Option<usize> {
    let Some(current) = commands.get(index) else {
        return None;
    };
    let group = current.group.trim().to_string();
    let target = if move_up {
        (0..index)
            .rev()
            .find(|&candidate| commands[candidate].group.trim() == group)
    } else {
        (index + 1..commands.len())
            .find(|&candidate| commands[candidate].group.trim() == group)
    };
    if let Some(target) = target {
        commands.swap(index, target);
    }
    target
}

pub fn history_model(store: &ConfigStore) -> ModelRc<SharedString> {
    let rows: Vec<SharedString> = store
        .command_history()
        .iter()
        .map(|s| s.clone().into())
        .collect();
    ModelRc::from(Rc::new(VecModel::from(rows)))
}

fn history_view_rows(history: &[String], query: &str) -> Vec<SharedString> {
    let q = query.trim().to_lowercase();
    history
        .iter()
        .filter(|command| q.is_empty() || command.to_lowercase().contains(&q))
        .map(|command| command.clone().into())
        .collect()
}

fn history_view_model(store: &ConfigStore, query: &str) -> ModelRc<SharedString> {
    let rows = history_view_rows(store.command_history(), query);
    ModelRc::from(Rc::new(VecModel::from(rows)))
}

fn history_preview_model(store: &ConfigStore, query: &str) -> ModelRc<SharedString> {
    let rows: Vec<SharedString> = history_view_rows(store.command_history(), query)
        .iter()
        .map(|command| command_preview(command).into())
        .collect();
    ModelRc::from(Rc::new(VecModel::from(rows)))
}

/// Refresh the history dropdown's raw rows and their display previews together.
pub fn set_history_view(ui: &AppWindow, store: &ConfigStore, query: &str) {
    ui.set_history_view(history_view_model(store, query));
    ui.set_history_preview(history_preview_model(store, query));
}

/// Encode a command-bar submission; the optional history entry excludes the
/// trailing Return and empty commands (#307).
pub fn encode_command_bar_input(command: &str) -> (Option<String>, Vec<u8>) {
    let command = command.trim_end().to_string();
    let mut bytes = command.clone().into_bytes();
    bytes.push(b'\n');
    let history = (!command.is_empty()).then_some(command);
    (history, bytes)
}

fn refresh_quick(
    weak: &slint::Weak<AppWindow>,
    store: &Rc<RefCell<ConfigStore>>,
    collapsed: &CollapsedGroups,
) {
    if let Some(ui) = weak.upgrade() {
        ui.set_quick_commands(quick_cmd_model(&store.borrow(), &collapsed.borrow()));
    }
}

fn save_or_log(store: &mut ConfigStore) {
    if let Err(err) = store.save() {
        eprintln!("meatshell-ohos: save config: {err}");
    }
}

/// Wires the command bar, history dropdown and quick-command manage dialog.
pub fn register(ui: &AppWindow, store: Option<Rc<RefCell<ConfigStore>>>, send: SendBytes) {
    let Some(store) = store else {
        ui.set_quick_commands(ModelRc::from(Rc::new(VecModel::<QuickCmd>::default())));
        ui.set_command_history(ModelRc::from(Rc::new(VecModel::<SharedString>::default())));
        ui.set_history_view(ModelRc::from(Rc::new(VecModel::<SharedString>::default())));
        ui.set_history_preview(ModelRc::from(Rc::new(VecModel::<SharedString>::default())));
        return;
    };

    // Runtime-only collapse state, default-collapsed like upstream.
    let collapsed: CollapsedGroups =
        Rc::new(RefCell::new(all_quick_group_names(&store.borrow())));
    ui.set_quick_commands(quick_cmd_model(&store.borrow(), &collapsed.borrow()));
    ui.set_command_history(history_model(&store.borrow()));
    ui.set_history_view(history_view_model(&store.borrow(), ""));
    ui.set_history_preview(history_preview_model(&store.borrow(), ""));
    let hist_query: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));

    // --- command bar: run + history ---------------------------------------
    {
        let store = store.clone();
        let send = send.clone();
        let hist_query = hist_query.clone();
        let weak = ui.as_weak();
        ui.on_run_command(move |tab_id: SharedString, cmd: SharedString, to_all: bool| {
            let (history_line, bytes) = encode_command_bar_input(cmd.as_str());
            if to_all {
                send(None, &bytes);
            } else {
                send(Some(tab_id.as_str()), &bytes);
            }
            if let Some(line) = history_line {
                let mut s = store.borrow_mut();
                s.push_command_history(line);
                save_or_log(&mut s);
                if let Some(ui) = weak.upgrade() {
                    ui.set_command_history(history_model(&s));
                    set_history_view(&ui, &s, &hist_query.borrow());
                }
            }
        });
    }
    {
        let store = store.clone();
        let hist_query = hist_query.clone();
        let weak = ui.as_weak();
        ui.on_search_history(move |query: SharedString| {
            *hist_query.borrow_mut() = query.to_string();
            if let Some(ui) = weak.upgrade() {
                set_history_view(&ui, &store.borrow(), &query);
            }
        });
    }
    {
        let store = store.clone();
        let hist_query = hist_query.clone();
        let weak = ui.as_weak();
        ui.on_delete_history(move |i: i32| {
            {
                let mut s = store.borrow_mut();
                let idx = i as usize;
                if idx < s.command_history().len() {
                    s.remove_command_history(idx);
                    save_or_log(&mut s);
                }
            }
            if let Some(ui) = weak.upgrade() {
                let s = store.borrow();
                ui.set_command_history(history_model(&s));
                set_history_view(&ui, &s, &hist_query.borrow());
            }
        });
    }
    {
        let store = store.clone();
        let hist_query = hist_query.clone();
        let weak = ui.as_weak();
        ui.on_delete_history_cmd(move |cmd: SharedString| {
            {
                let mut s = store.borrow_mut();
                if let Some(idx) = s.command_history().iter().position(|c| c == cmd.as_str()) {
                    s.remove_command_history(idx);
                    save_or_log(&mut s);
                }
            }
            if let Some(ui) = weak.upgrade() {
                let s = store.borrow();
                ui.set_command_history(history_model(&s));
                set_history_view(&ui, &s, &hist_query.borrow());
            }
        });
    }

    // --- quick-command manage dialog (#55) --------------------------------
    {
        let store = store.clone();
        let collapsed = collapsed.clone();
        let weak = ui.as_weak();
        ui.on_add_quick_command(
            move |name: SharedString,
                  command: SharedString,
                  group: SharedString,
                  send_enter: bool| {
                let name = name.trim().to_string();
                let command = command.to_string();
                let group = group.trim().to_string();
                if name.is_empty() || command.trim().is_empty() {
                    return;
                }
                {
                    let mut s = store.borrow_mut();
                    let mut v = s.quick_commands().to_vec();
                    v.push(QuickCommand {
                        name,
                        command,
                        group,
                        send_enter,
                    });
                    s.set_quick_commands(v);
                    save_or_log(&mut s);
                }
                refresh_quick(&weak, &store, &collapsed);
            },
        );
    }
    {
        let store = store.clone();
        let collapsed = collapsed.clone();
        let weak = ui.as_weak();
        ui.on_delete_quick_command(move |index: i32| {
            {
                let mut s = store.borrow_mut();
                let mut v = s.quick_commands().to_vec();
                let i = index as usize;
                if i < v.len() {
                    v.remove(i);
                }
                s.set_quick_commands(v);
                save_or_log(&mut s);
            }
            if let Some(ui) = weak.upgrade() {
                let edit = edit_index_after_delete(ui.get_qcm_edit_index(), index);
                if edit < 0 && ui.get_qcm_edit_index() >= 0 {
                    ui.set_qcm_name("".into());
                    ui.set_qcm_command("".into());
                    ui.set_qcm_group("".into());
                    ui.set_qcm_send_enter(true);
                }
                ui.set_qcm_edit_index(edit);
            }
            refresh_quick(&weak, &store, &collapsed);
        });
    }
    {
        let store = store.clone();
        let collapsed = collapsed.clone();
        let weak = ui.as_weak();
        ui.on_toggle_quick_group(move |group: SharedString| {
            let g = group.to_string();
            {
                let mut set = collapsed.borrow_mut();
                if !set.remove(&g) {
                    set.insert(g);
                }
            }
            refresh_quick(&weak, &store, &collapsed);
        });
    }
    {
        let store = store.clone();
        let weak = ui.as_weak();
        ui.on_edit_quick_command(move |index: i32| {
            let i = index as usize;
            let cmd = store.borrow().quick_commands().get(i).cloned();
            if let (Some(c), Some(ui)) = (cmd, weak.upgrade()) {
                ui.set_qcm_name(c.name.into());
                ui.set_qcm_command(c.command.into());
                ui.set_qcm_group(c.group.into());
                ui.set_qcm_send_enter(c.send_enter);
                ui.set_qcm_edit_index(index);
                ui.set_quick_cmd_manage_open(true);
            }
        });
    }
    {
        let store = store.clone();
        let collapsed = collapsed.clone();
        let weak = ui.as_weak();
        ui.on_save_quick_command(
            move |index: i32,
                  name: SharedString,
                  command: SharedString,
                  group: SharedString,
                  send_enter: bool| {
                let name = name.trim().to_string();
                let command = command.to_string();
                let group = group.trim().to_string();
                if name.is_empty() || command.trim().is_empty() {
                    return;
                }
                {
                    let mut s = store.borrow_mut();
                    s.update_quick_command(
                        index as usize,
                        QuickCommand {
                            name,
                            command,
                            group,
                            send_enter,
                        },
                    );
                    save_or_log(&mut s);
                }
                refresh_quick(&weak, &store, &collapsed);
            },
        );
    }
    {
        let store = store.clone();
        let collapsed = collapsed.clone();
        let weak = ui.as_weak();
        ui.on_duplicate_quick_command(move |index: i32| {
            {
                let mut s = store.borrow_mut();
                let mut v = s.quick_commands().to_vec();
                if let Some(c) = v.get(index as usize).cloned() {
                    let dup = QuickCommand {
                        name: format!("{} (copy)", c.name),
                        command: c.command,
                        group: c.group,
                        send_enter: c.send_enter,
                    };
                    v.insert(index as usize + 1, dup);
                    s.set_quick_commands(v);
                    save_or_log(&mut s);
                }
            }
            if let Some(ui) = weak.upgrade() {
                let edit = ui.get_qcm_edit_index();
                if edit > index {
                    ui.set_qcm_edit_index(edit + 1);
                }
            }
            refresh_quick(&weak, &store, &collapsed);
        });
    }
    {
        let store = store.clone();
        let collapsed = collapsed.clone();
        let weak = ui.as_weak();
        ui.on_move_quick_command(move |index: i32, group: SharedString| {
            let target = group.to_string();
            let target = if target == "default" {
                String::new()
            } else {
                target
            };
            {
                let mut s = store.borrow_mut();
                let mut v = s.quick_commands().to_vec();
                if let Some(c) = v.get_mut(index as usize) {
                    c.group = target;
                }
                s.set_quick_commands(v);
                save_or_log(&mut s);
            }
            refresh_quick(&weak, &store, &collapsed);
        });
    }
    {
        let store = store.clone();
        let collapsed = collapsed.clone();
        let weak = ui.as_weak();
        ui.on_reorder_quick_command(move |index: i32, move_up: bool| {
            let swapped = {
                let mut s = store.borrow_mut();
                let mut commands = s.quick_commands().to_vec();
                let swapped = reorder_quick_command(&mut commands, index as usize, move_up);
                if swapped.is_some() {
                    s.set_quick_commands(commands);
                    save_or_log(&mut s);
                }
                swapped
            };
            if let Some(target) = swapped {
                if let Some(ui) = weak.upgrade() {
                    ui.set_qcm_edit_index(edit_index_after_swap(
                        ui.get_qcm_edit_index(),
                        index as usize,
                        target,
                    ));
                }
            }
            refresh_quick(&weak, &store, &collapsed);
        });
    }
    {
        let store = store.clone();
        let collapsed = collapsed.clone();
        let weak = ui.as_weak();
        ui.on_submit_quick_group(move |orig: SharedString, name: SharedString| {
            {
                let mut s = store.borrow_mut();
                if orig.is_empty() {
                    s.add_quick_group(name.to_string());
                } else {
                    s.rename_quick_group(&orig.to_string(), name.to_string());
                }
                save_or_log(&mut s);
            }
            refresh_quick(&weak, &store, &collapsed);
        });
    }
    {
        let store = store.clone();
        let collapsed = collapsed.clone();
        let weak = ui.as_weak();
        ui.on_delete_quick_group(move |name: SharedString| {
            {
                let mut s = store.borrow_mut();
                s.remove_quick_group(&name.to_string());
                save_or_log(&mut s);
            }
            refresh_quick(&weak, &store, &collapsed);
        });
    }
}
