use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gpui::{
    App, Bounds, Context, Entity, FocusHandle, Focusable, IntoElement, KeyDownEvent, MouseButton,
    SharedString, TitlebarOptions, Window, WindowBounds, WindowOptions, div, point, prelude::*, px,
    size,
};
use gpui_platform::application;

mod config;
mod editor;
mod highlight;
mod instance;
mod terminal;
mod theme;

use editor::CodeEditor;
use terminal::Terminal;

use config::Workspace;
use instance::{DirEntry, Instance};

struct Protocol {
    focus: FocusHandle,
    workspaces: Vec<(Workspace, Option<String>)>,
    instances: HashMap<String, Vec<Instance>>,
    selected_workspace: Option<String>,
    selected_instance: Option<String>,
    expanded: HashSet<PathBuf>,
    selected_file: Option<PathBuf>,
    file_error: Option<String>,
    editor: Option<Entity<CodeEditor>>,
    creating_instance: Option<String>,
    cloning: Option<String>,
    pending_delete: Option<(String, String)>,
    status: String,
    // Per-(workspace, instance) terminal sessions, kept alive across instance switches.
    terminals: HashMap<(String, String), Vec<gpui::Entity<Terminal>>>,
    active_terminal_idx: HashMap<(String, String), usize>,
}

impl Protocol {
    fn new(cx: &mut Context<Self>) -> Self {
        let _ = config::ensure_dirs();
        let workspaces = config::load_workspaces();
        let app_state = config::load_app_state();
        let mut this = Self {
            focus: cx.focus_handle(),
            workspaces,
            instances: HashMap::new(),
            selected_workspace: None,
            selected_instance: None,
            expanded: HashSet::new(),
            selected_file: None,
            file_error: None,
            editor: None,
            creating_instance: None,
            cloning: None,
            pending_delete: None,
            status: String::new(),
            terminals: HashMap::new(),
            active_terminal_idx: HashMap::new(),
        };
        let initial_ws = app_state
            .last_workspace
            .clone()
            .filter(|n| this.workspaces.iter().any(|(w, _)| &w.name == n))
            .or_else(|| this.workspaces.first().map(|(w, _)| w.name.clone()));
        if let Some(name) = initial_ws {
            this.select_workspace(&name, cx);
            if let Some(inst) = app_state.last_instance.clone() {
                if this.instances_of(&name).iter().any(|i| i.name == inst) {
                    this.select_instance(&inst, cx);
                    this.expanded = app_state.expanded.iter().cloned().collect();
                    if let Some(f) = app_state.selected_file.clone() {
                        if f.exists() {
                            this.open_file(f, cx);
                        }
                    }
                }
            }
        }
        this
    }

    fn instances_of(&self, workspace: &str) -> &[Instance] {
        self.instances.get(workspace).map(|v| v.as_slice()).unwrap_or(&[])
    }

    fn refresh_instances(&mut self, workspace: &str) {
        let list = instance::list_instances(workspace);
        self.instances.insert(workspace.to_string(), list);
    }

    fn select_workspace(&mut self, name: &str, _cx: &mut Context<Self>) {
        self.selected_workspace = Some(name.to_string());
        self.refresh_instances(name);
        self.selected_instance = None;
        self.selected_file = None;
        self.file_error = None;
        self.editor = None;
        self.expanded.clear();
    }

    fn select_instance(&mut self, name: &str, _cx: &mut Context<Self>) {
        self.selected_instance = Some(name.to_string());
        self.selected_file = None;
        self.file_error = None;
        self.editor = None;
        self.expanded.clear();
    }

    fn persist(&self) {
        config::save_app_state(&config::AppState {
            last_workspace: self.selected_workspace.clone(),
            last_instance: self.selected_instance.clone(),
            expanded: self.expanded.iter().cloned().collect(),
            selected_file: self.selected_file.clone(),
        });
    }

    fn active_workspace(&self) -> Option<&Workspace> {
        let name = self.selected_workspace.as_deref()?;
        self.workspaces.iter().find(|(w, _)| w.name == name).map(|(w, _)| w)
    }

    fn active_instance_path(&self) -> Option<PathBuf> {
        let ws = self.selected_workspace.as_deref()?;
        let inst = self.selected_instance.as_deref()?;
        Some(instance::instance_dir(ws, inst))
    }

    fn active_instance_key(&self) -> Option<(String, String)> {
        Some((
            self.selected_workspace.clone()?,
            self.selected_instance.clone()?,
        ))
    }

    fn spawn_terminal(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.active_instance_key() else { return };
        let Some(cwd) = self.active_instance_path() else { return };
        if !cwd.exists() {
            self.status = "instance dir missing".into();
            return;
        }
        match Terminal::create(cwd.clone(), cx) {
            Ok(entity) => {
                self.terminals.entry(key.clone()).or_default().push(entity);
                let idx = self.terminals[&key].len() - 1;
                self.active_terminal_idx.insert(key, idx);
            }
            Err(e) => {
                self.status = format!("terminal failed: {e:#}");
            }
        }
        cx.notify();
    }

    fn switch_terminal(&mut self, idx: usize) {
        if let Some(key) = self.active_instance_key() {
            self.active_terminal_idx.insert(key, idx);
        }
    }

    fn close_terminal(&mut self, idx: usize) {
        let Some(key) = self.active_instance_key() else { return };
        if let Some(list) = self.terminals.get_mut(&key) {
            if idx < list.len() {
                list.remove(idx);
                let new_active = if list.is_empty() {
                    None
                } else {
                    Some(idx.min(list.len() - 1))
                };
                match new_active {
                    Some(i) => { self.active_terminal_idx.insert(key, i); }
                    None => { self.active_terminal_idx.remove(&key); }
                }
            }
        }
    }

    fn active_terminal_entity(&self) -> Option<gpui::Entity<Terminal>> {
        let key = self.active_instance_key()?;
        let list = self.terminals.get(&key)?;
        if list.is_empty() { return None; }
        let idx = self.active_terminal_idx.get(&key).copied().unwrap_or(0).min(list.len() - 1);
        list.get(idx).cloned()
    }

    fn open_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                self.file_error = None;
                if let Some(editor) = self.editor.as_ref() {
                    let path_clone = path.clone();
                    editor.update(cx, |ed, _| ed.replace_with_file(path_clone, text));
                } else {
                    let path_clone = path.clone();
                    self.editor =
                        Some(cx.new(|cx| CodeEditor::new(path_clone, text, cx)));
                }
            }
            Err(e) => {
                self.file_error = Some(format!("{e}"));
                self.editor = None;
            }
        }
        self.selected_file = Some(path);
        self.persist();
    }

    fn toggle_dir(&mut self, path: &Path) {
        if !self.expanded.insert(path.to_path_buf()) {
            self.expanded.remove(path);
        }
        self.persist();
    }

    fn start_create_instance(&mut self) {
        if self.selected_workspace.is_some() {
            self.creating_instance = Some(String::new());
        }
    }

    fn finish_create_instance(&mut self, cx: &mut Context<Self>) {
        let Some(name_raw) = self.creating_instance.take() else { return };
        let name = name_raw.trim().to_string();
        if name.is_empty() {
            return;
        }
        let Some(ws) = self.active_workspace().cloned() else { return };
        self.cloning = Some(name.clone());
        self.status = format!("cloning {}/{}…", ws.name, name);
        cx.notify();

        let ws_owned = ws.clone();
        let name_owned = name.clone();
        let ws_name = ws.name.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { instance::create_instance(&ws_owned, &name_owned) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.cloning = None;
                match result {
                    Ok(()) => {
                        this.refresh_instances(&ws_name);
                        this.select_instance(&name, cx);
                        this.persist();
                        this.status = format!("created {}/{}", ws_name, name);
                    }
                    Err(e) => {
                        this.status = format!("clone failed: {e:#}");
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn cancel_create_instance(&mut self) {
        self.creating_instance = None;
    }

    fn confirm_delete(&mut self, cx: &mut Context<Self>) {
        let Some((ws, inst)) = self.pending_delete.take() else { return };
        match instance::delete_instance(&ws, &inst) {
            Ok(()) => {
                self.refresh_instances(&ws);
                if self.selected_instance.as_deref() == Some(inst.as_str()) {
                    self.selected_instance = None;
                    self.selected_file = None;
                    self.file_error = None;
                    self.editor = None;
                }
                self.status = format!("deleted {}/{}", ws, inst);
                self.persist();
            }
            Err(e) => {
                self.status = format!("delete failed: {e:#}");
            }
        }
        cx.notify();
    }

    fn cancel_delete(&mut self) {
        self.pending_delete = None;
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        if self.pending_delete.is_some() {
            match key {
                "enter" => self.confirm_delete(cx),
                "escape" => { self.cancel_delete(); cx.notify(); }
                _ => {}
            }
            return;
        }
        if self.creating_instance.is_some() {
            match key {
                "enter" => self.finish_create_instance(cx),
                "escape" => self.cancel_create_instance(),
                "backspace" => {
                    if let Some(name) = self.creating_instance.as_mut() {
                        name.pop();
                    }
                }
                _ => {
                    let push: Option<String> = event
                        .keystroke
                        .key_char
                        .as_deref()
                        .filter(|c| !c.is_empty() && !c.chars().any(|ch| ch.is_control()))
                        .map(|c| c.to_string());
                    if let (Some(text), Some(name)) =
                        (push, self.creating_instance.as_mut())
                    {
                        name.push_str(&text);
                    }
                }
            }
            cx.notify();
        }
    }
}

impl Focusable for Protocol {
    fn focus_handle(&self, _: &App) -> FocusHandle { self.focus.clone() }
}

impl Render for Protocol {
    fn render(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = div()
            .flex()
            .flex_row()
            .flex_1()
            .min_h_0()
            .child(self.render_left(cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .child(self.render_main(cx))
                    .child(self.render_terminal_panel(cx)),
            );

        let mut root = div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::bg())
            .text_color(theme::text())
            .font_family(".SystemUIFont")
            .text_size(px(13.))
            .child(self.render_titlebar())
            .child(body)
            .child(self.render_status());

        if let Some((ws, inst)) = self.pending_delete.clone() {
            root = root.child(render_delete_modal(&ws, &inst, cx));
        }
        if let Some(name) = self.creating_instance.clone() {
            let ws_label = self.selected_workspace.clone().unwrap_or_default();
            root = root.child(render_new_instance_modal(&ws_label, &name, cx));
        }
        if let Some(name) = self.cloning.clone() {
            let ws_label = self.selected_workspace.clone().unwrap_or_default();
            root = root.child(render_cloning_overlay(&ws_label, &name));
        }
        root
    }
}

fn section_header(label: &'static str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_1()
        .h(px(theme::SECTION_HEADER_H))
        .px(px(theme::ROW_PAD_X))
        .text_size(px(10.))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme::text_muted())
        .child(div().text_size(px(8.5)).text_color(theme::text_dim()).child("▾"))
        .child(SharedString::from(label.to_string()))
}

fn row_base() -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1p5()
        .h(px(theme::ROW_H))
        .mx(px(4.))
        .px(px(theme::ROW_PAD_X - 4.))
        .rounded(px(4.))
}

impl Protocol {
    fn render_titlebar(&self) -> impl IntoElement {
        let traffic_light_room = px(76.);
        let mut crumb = String::from("Protocol");
        if let Some(ws) = &self.selected_workspace {
            crumb = format!("Protocol  ›  {ws}");
            if let Some(inst) = &self.selected_instance {
                crumb = format!("Protocol  ›  {ws}  ›  {inst}");
            }
        }
        div()
            .flex()
            .items_center()
            .h(px(theme::TITLEBAR_H))
            .w_full()
            .bg(theme::titlebar_bg())
            .border_b_1()
            .border_color(theme::divider())
            .pl(traffic_light_room)
            .pr_3()
            .text_size(px(12.))
            .text_color(theme::text_strong())
            .child(SharedString::from(crumb))
    }

    fn render_left(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .w(px(theme::PANEL_W))
            .h_full()
            .bg(theme::panel_bg())
            .border_r_1()
            .border_color(theme::divider())
            .child(self.render_workspaces_section(cx))
            .child(self.render_instances_section(cx))
            .child(self.render_repositories_section(cx))
    }

    fn render_repositories_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = div().flex().flex_col().pb_1().child(section_header("REPOSITORIES"));
        let Some(root) = self.active_instance_path() else {
            col = col.child(
                row_base()
                    .text_color(theme::text_dim())
                    .child("select an instance"),
            );
            return col;
        };
        if !root.exists() {
            col = col.child(
                row_base()
                    .text_color(theme::text_dim())
                    .child("instance dir missing"),
            );
            return col;
        }
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        self.collect_tree(&root, 0, &mut rows, cx);
        for r in rows {
            col = col.child(r);
        }
        col
    }

    fn collect_tree(
        &self,
        dir: &Path,
        depth: u32,
        rows: &mut Vec<gpui::AnyElement>,
        cx: &mut Context<Self>,
    ) {
        let entries = instance::read_dir_sorted(dir);
        for entry in entries {
            let DirEntry { name, path, is_dir } = entry;
            let expanded = self.expanded.contains(&path);
            let selected = self.selected_file.as_deref() == Some(&path);
            let path_for_click = path.clone();
            let chevron_text = if is_dir { if expanded { "▾" } else { "▸" } } else { "" };
            let row = row_base()
                .pl(px(theme::ROW_PAD_X - 4. + depth as f32 * theme::INDENT_PX))
                .when(selected, |d| d.bg(theme::row_selected()))
                .hover(|s| s.bg(theme::row_hover()))
                .cursor_pointer()
                .child(
                    div()
                        .w(px(12.))
                        .text_size(px(9.))
                        .text_color(theme::text_dim())
                        .child(chevron_text),
                )
                .child(
                    div()
                        .text_color(if selected { theme::text_strong() } else { theme::text() })
                        .child(SharedString::from(name)),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        if is_dir {
                            this.toggle_dir(&path_for_click);
                        } else {
                            this.open_file(path_for_click.clone(), cx);
                        }
                        cx.notify();
                    }),
                )
                .into_any_element();
            rows.push(row);
            if is_dir && expanded {
                self.collect_tree(&path, depth + 1, rows, cx);
            }
        }
    }

    fn render_workspaces_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = div().flex().flex_col().pb_1().child(section_header("WORKSPACES"));
        if self.workspaces.is_empty() {
            col = col.child(
                row_base()
                    .text_color(theme::text_dim())
                    .child(SharedString::from("no workspaces — drop a .toml in")),
            );
            col = col.child(
                row_base()
                    .text_color(theme::text_dim())
                    .text_size(px(11.))
                    .child(SharedString::from(format!(
                        "{}",
                        config::workspaces_dir().display()
                    ))),
            );
            return col;
        }
        for (w, err) in &self.workspaces {
            let active = self.selected_workspace.as_deref() == Some(w.name.as_str());
            let name = w.name.clone();
            let row = row_base()
                .when(active, |d| d.bg(theme::row_selected()))
                .hover(|s| s.bg(theme::row_hover()))
                .cursor_pointer()
                .child(
                    div()
                        .w(px(12.))
                        .flex()
                        .items_center()
                        .text_size(px(8.))
                        .text_color(if active { theme::accent() } else { theme::text_dim() })
                        .child(if active { "●" } else { "○" }),
                )
                .child(
                    div()
                        .flex_1()
                        .text_color(if err.is_some() {
                            theme::danger()
                        } else if active {
                            theme::text_strong()
                        } else {
                            theme::text()
                        })
                        .child(SharedString::from(w.name.clone())),
                )
                .when_some(err.clone(), |d, msg| {
                    d.child(
                        div()
                            .text_color(theme::danger())
                            .text_size(px(10.))
                            .child(SharedString::from(
                                msg.lines().next().unwrap_or("error").to_string(),
                            )),
                    )
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.select_workspace(&name, cx);
                        this.persist();
                        cx.notify();
                    }),
                );
            col = col.child(row);
        }
        col
    }

    fn render_instances_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = div().flex().flex_col().pb_1().child(section_header("INSTANCES"));
        let Some(ws) = self.selected_workspace.clone() else {
            col = col.child(
                row_base()
                    .text_color(theme::text_dim())
                    .child("select a workspace"),
            );
            return col;
        };
        let instances = self.instances_of(&ws).to_vec();
        if instances.is_empty() && self.creating_instance.is_none() {
            col = col.child(
                row_base()
                    .text_color(theme::text_dim())
                    .child("no instances yet"),
            );
        }
        for inst in instances {
            let active = self.selected_instance.as_deref() == Some(inst.name.as_str());
            let name = inst.name.clone();
            let name_for_right = inst.name.clone();
            let ws_for_right = ws.clone();
            let row = row_base()
                .when(active, |d| d.bg(theme::row_selected()))
                .hover(|s| s.bg(theme::row_hover()))
                .cursor_pointer()
                .child(
                    div()
                        .w(px(12.))
                        .flex()
                        .items_center()
                        .text_size(px(8.))
                        .text_color(if active { theme::accent() } else { theme::text_dim() })
                        .child(if active { "●" } else { "○" }),
                )
                .child(
                    div()
                        .flex_1()
                        .text_color(if active { theme::text_strong() } else { theme::text() })
                        .child(SharedString::from(inst.name.clone())),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.select_instance(&name, cx);
                        this.persist();
                        cx.notify();
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, _, _, cx| {
                        this.pending_delete = Some((ws_for_right.clone(), name_for_right.clone()));
                        cx.notify();
                    }),
                );
            col = col.child(row);
        }

        col = col.child(
            row_base()
                .id("new-instance-button")
                .hover(|s| s.bg(theme::row_hover()))
                .cursor_pointer()
                .child(
                    div()
                        .w(px(12.))
                        .text_color(theme::text_dim())
                        .child("+"),
                )
                .child(
                    div()
                        .flex_1()
                        .text_color(theme::text_dim())
                        .child("new instance"),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.start_create_instance();
                        cx.notify();
                    }),
                ),
        );
        col
    }

    fn render_main(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let outer = div().flex().flex_col().flex_1().h_full();
        if let Some(path) = &self.selected_file {
            let dirty = self
                .editor
                .as_ref()
                .map(|e| e.read(cx).dirty)
                .unwrap_or(false);
            let mut header_text =
                short_path(path, self.active_instance_path().as_deref());
            if dirty {
                header_text.push_str(" •");
            }
            let header = div()
                .flex()
                .items_center()
                .h(px(32.))
                .px_3()
                .bg(theme::titlebar_bg())
                .border_b_1()
                .border_color(theme::divider())
                .text_color(if dirty { theme::accent() } else { theme::text_muted() })
                .text_size(px(11.5))
                .child(SharedString::from(header_text));

            if let Some(err) = &self.file_error {
                return outer.child(header).child(
                    div()
                        .p_3()
                        .text_color(theme::danger())
                        .child(SharedString::from(format!("could not read: {err}"))),
                );
            }
            if let Some(editor) = self.editor.clone() {
                return outer.child(header).child(editor);
            }
        }
        outer.child(
            div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_color(theme::text_muted())
                .child("no file selected"),
        )
    }

    fn render_terminal_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let key = self.active_instance_key();
        let terminals = key.as_ref().and_then(|k| self.terminals.get(k));
        let terminal_count = terminals.map(|v| v.len()).unwrap_or(0);
        let active_idx = key
            .as_ref()
            .and_then(|k| self.active_terminal_idx.get(k).copied())
            .unwrap_or(0);

        // Tab bar with [+] new terminal button.
        let mut tabs = div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(28.))
            .bg(theme::titlebar_bg())
            .border_t_1()
            .border_b_1()
            .border_color(theme::divider())
            .text_size(px(11.5));

        if let (Some(_), Some(list)) = (key.clone(), terminals) {
            for (i, term) in list.iter().enumerate() {
                let active = i == active_idx;
                let title = term.read(cx).title.clone();
                let label: SharedString = if title.is_empty() {
                    format!("term {}", i + 1).into()
                } else {
                    title.into()
                };
                let i_for_close = i;
                let i_for_switch = i;
                tabs = tabs.child(
                    div()
                        .id(("term-tab", i))
                        .flex()
                        .flex_row()
                        .items_center()
                        .h_full()
                        .px_3()
                        .gap_2()
                        .when(active, |d| d.bg(theme::bg()))
                        .text_color(if active { theme::text_strong() } else { theme::text_muted() })
                        .border_r_1()
                        .border_color(theme::divider())
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::row_hover()))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.switch_terminal(i_for_switch);
                                cx.notify();
                            }),
                        )
                        .child(label)
                        .child(
                            div()
                                .id(("term-close", i))
                                .text_color(theme::text_dim())
                                .hover(|s| s.text_color(theme::danger()))
                                .child("×")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, ev: &gpui::MouseDownEvent, _, cx| {
                                        let _ = ev;
                                        this.close_terminal(i_for_close);
                                        cx.notify();
                                    }),
                                ),
                        ),
                );
            }
        }

        tabs = tabs.child(
            div()
                .id("term-new")
                .flex()
                .items_center()
                .px_3()
                .h_full()
                .text_color(theme::text_muted())
                .cursor_pointer()
                .hover(|s| s.bg(theme::row_hover()))
                .child("+")
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.spawn_terminal(cx);
                    }),
                ),
        );

        let body: gpui::AnyElement = if terminal_count == 0 {
            div()
                .flex()
                .items_center()
                .justify_center()
                .flex_1()
                .min_h_0()
                .text_color(theme::text_dim())
                .text_size(px(11.5))
                .child("no terminals — click + to start one")
                .into_any_element()
        } else if let Some(active) = self.active_terminal_entity() {
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .child(active)
                .into_any_element()
        } else {
            div().flex_1().min_h_0().into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .h(px(280.))
            .flex_none()
            .child(tabs)
            .child(body)
    }

    fn render_status(&self) -> impl IntoElement {
        let mut left = String::new();
        if let Some(file) = &self.selected_file {
            if let Some(repo) = instance::find_repo_root(file) {
                if let Some(branch) = instance::current_branch(&repo) {
                    left.push_str(&format!(" {branch}"));
                }
                if let Some(dirty) = instance::is_dirty(&repo) {
                    left.push_str(if dirty { "  ·  dirty" } else { "  ·  clean" });
                }
            }
        }
        let right: SharedString = if !self.status.is_empty() {
            self.status.clone().into()
        } else {
            "ready".into()
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(theme::STATUSBAR_H))
            .px_3()
            .gap_3()
            .bg(theme::titlebar_bg())
            .border_t_1()
            .border_color(theme::divider())
            .text_size(px(10.5))
            .text_color(theme::text_muted())
            .child(div().flex_1().child(SharedString::from(left)))
            .child(div().child(right))
    }
}

fn render_cloning_overlay(workspace: &str, instance: &str) -> impl IntoElement {
    let title = SharedString::from(format!("Cloning {workspace} / {instance}"));
    let hint = SharedString::from(
        "Running git clone for each repo in the workspace. The window will update when finished.",
    );
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui::hsla(0., 0., 0., 0.55))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .w(px(420.))
                .p_5()
                .bg(theme::panel_bg())
                .border_1()
                .border_color(theme::divider())
                .rounded(px(6.))
                .child(div().text_color(theme::text_strong()).child(title))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme::text_muted())
                        .child(hint),
                ),
        )
}

fn render_new_instance_modal(
    workspace: &str,
    name: &str,
    cx: &mut Context<Protocol>,
) -> impl IntoElement {
    let title = SharedString::from(format!("New instance in workspace \"{workspace}\""));
    let hint = SharedString::from("Enter a name. This will git-clone every repo in the workspace into a new directory. Press Enter to create, Esc to cancel.");
    let display: SharedString = if name.is_empty() {
        "type a name…".into()
    } else {
        format!("{name}▏").into()
    };
    let muted = name.is_empty();
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui::hsla(0., 0., 0., 0.5))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_3()
                .w(px(480.))
                .p_5()
                .bg(theme::panel_bg())
                .border_1()
                .border_color(theme::divider())
                .rounded(px(6.))
                .child(div().text_color(theme::text_strong()).child(title))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme::text_muted())
                        .child(hint),
                )
                .child(
                    div()
                        .h(px(36.))
                        .px_3()
                        .flex()
                        .items_center()
                        .bg(theme::bg())
                        .border_1()
                        .border_color(theme::divider())
                        .rounded(px(4.))
                        .text_color(if muted { theme::text_dim() } else { theme::text_strong() })
                        .font_family("Menlo")
                        .child(display),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .justify_end()
                        .child(
                            div()
                                .id("new-cancel")
                                .px_3()
                                .py_1()
                                .rounded(px(4.))
                                .bg(theme::bg())
                                .border_1()
                                .border_color(theme::divider())
                                .cursor_pointer()
                                .hover(|s| s.bg(theme::row_hover()))
                                .child("Cancel")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.cancel_create_instance();
                                        cx.notify();
                                    }),
                                ),
                        )
                        .child(
                            div()
                                .id("new-create")
                                .px_3()
                                .py_1()
                                .rounded(px(4.))
                                .bg(theme::accent())
                                .text_color(gpui::black())
                                .cursor_pointer()
                                .child("Create")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.finish_create_instance(cx);
                                        cx.notify();
                                    }),
                                ),
                        ),
                ),
        )
}

fn render_delete_modal(workspace: &str, instance: &str, cx: &mut Context<Protocol>) -> impl IntoElement {
    let msg = SharedString::from(format!("Delete instance \"{}/{}\"?", workspace, instance));
    let hint = SharedString::from("This removes the entire directory. Press Enter to confirm, Esc to cancel.");
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui::hsla(0., 0., 0., 0.5))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_3()
                .w(px(420.))
                .p_5()
                .bg(theme::panel_bg())
                .border_1()
                .border_color(theme::divider())
                .rounded(px(6.))
                .child(div().text_color(theme::text()).child(msg))
                .child(div().text_size(px(11.)).text_color(theme::text_muted()).child(hint))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .justify_end()
                        .child(
                            div()
                                .px_3()
                                .py_1()
                                .rounded(px(4.))
                                .bg(theme::bg())
                                .border_1()
                                .border_color(theme::divider())
                                .cursor_pointer()
                                .hover(|s| s.bg(theme::row_hover()))
                                .child("Cancel")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.cancel_delete();
                                        cx.notify();
                                    }),
                                ),
                        )
                        .child(
                            div()
                                .px_3()
                                .py_1()
                                .rounded(px(4.))
                                .bg(theme::danger())
                                .text_color(gpui::white())
                                .cursor_pointer()
                                .child("Delete")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.confirm_delete(cx);
                                    }),
                                ),
                        ),
                ),
        )
}

fn short_path(path: &Path, root: Option<&Path>) -> String {
    if let Some(root) = root {
        if let Ok(rel) = path.strip_prefix(root) {
            return rel.display().to_string();
        }
    }
    path.display().to_string()
}

fn main() {
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1400.), px(900.)), cx);
        cx.open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("Protocol".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(12.), px(9.))),
                }),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(Protocol::new);
                let handle = view.read(cx).focus.clone();
                window.focus(&handle, cx);
                view
            },
        )
        .unwrap();
        cx.activate(true);
    });
}
