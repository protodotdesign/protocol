use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gpui::{
    App, Bounds, Context, CursorStyle, Entity, FocusHandle, Focusable, IntoElement, KeyDownEvent,
    MouseButton, SharedString, TitlebarOptions, Window, WindowBounds, WindowOptions, div, point,
    prelude::*, px, size,
};
use gpui_platform::application;

mod config;
mod editor;
mod highlight;
mod instance;
mod modals;
mod terminal;
mod theme;
mod ui;

use editor::{CodeEditor, CodeEditorEvent};
use terminal::Terminal;

use config::Workspace;
use instance::{DirEntry, Instance};
use modals::{render_cloning_overlay, render_delete_modal, render_new_instance_modal};
use ui::{card_frame, focus_overlay, placeholder, row_base, section_header, sidebar_toggle_button};

pub struct Protocol {
    focus: FocusHandle,
    workspaces: Vec<(Workspace, PathBuf, Option<String>)>,
    instances: HashMap<String, Vec<Instance>>,
    selected_workspace: Option<String>,
    selected_instance: Option<String>,
    expanded: HashSet<PathBuf>,
    selected_file: Option<PathBuf>,
    file_error: Option<String>,
    editors: Vec<OpenEditor>,
    modal: Modal,
    status: String,
    // Per-(workspace, instance) terminal sessions, kept alive across instance switches.
    terminals: HashMap<(String, String), Vec<gpui::Entity<Terminal>>>,
    active_terminal_idx: HashMap<(String, String), usize>,
    // Per-instance layout state. Falls back to defaults when no entry exists.
    layouts: HashMap<(String, String), Layout>,
    drag: Option<DragKind>,
    dir_cache: std::cell::RefCell<HashMap<PathBuf, Vec<instance::DirEntry>>>,
    collapsed_workspaces: HashSet<String>,
}

/// One-of modal/overlay state. Only one can be active at a time, so a single
/// enum is the right shape. Add a variant when you add a new modal — the
/// renderer + key handler match exhaustively.
pub(crate) enum Modal {
    None,
    NewInstance(String),
    Cloning(String),
    Delete { ws: String, instance: String },
}

impl Modal {
    pub(crate) fn is_new_instance(&self) -> bool { matches!(self, Modal::NewInstance(_)) }
}

struct OpenEditor {
    path: PathBuf,
    entity: Entity<CodeEditor>,
}

#[derive(Clone, Copy, Debug)]
struct Layout {
    left_panel_w: f32,
    right_panel_w: f32,
    terminal_panel_size: f32,
    terminal_panel_pos: PanelPos,
    left_collapsed: bool,
    right_collapsed: bool,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            left_panel_w: theme::PANEL_W,
            right_panel_w: theme::PANEL_W,
            terminal_panel_size: 280.,
            terminal_panel_pos: PanelPos::Bottom,
            left_collapsed: false,
            right_collapsed: false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PanelPos {
    Bottom,
    Right,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DragKind {
    LeftPanel,
    RightPanel,
    TerminalPanel,
    /// User is dragging the terminal panel's reposition handle. Tracks the
    /// current pointer position so we can decide which dock to snap to on drop.
    TerminalPanelMove,
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
            editors: Vec::new(),
            modal: Modal::None,
            status: String::new(),
            terminals: HashMap::new(),
            active_terminal_idx: HashMap::new(),
            layouts: HashMap::new(),
            drag: None,
            dir_cache: std::cell::RefCell::new(HashMap::new()),
            collapsed_workspaces: HashSet::new(),
        };
        for layout in &app_state.layouts {
            let pos = match layout.terminal_panel_pos.as_str() {
                "right" => PanelPos::Right,
                _ => PanelPos::Bottom,
            };
            this.layouts.insert(
                (layout.workspace.clone(), layout.instance.clone()),
                Layout {
                    left_panel_w: layout.left_panel_w,
                    right_panel_w: layout.right_panel_w,
                    terminal_panel_size: layout.terminal_panel_size,
                    terminal_panel_pos: pos,
                    left_collapsed: layout.left_collapsed,
                    right_collapsed: layout.right_collapsed,
                },
            );
        }
        let initial_ws = app_state
            .last_workspace
            .clone()
            .filter(|n| this.workspaces.iter().any(|(w, _, _)| &w.name == n))
            .or_else(|| this.workspaces.first().map(|(w, _, _)| w.name.clone()));
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
        self.expanded.clear();
        self.dir_cache.borrow_mut().clear();
    }

    fn select_instance(&mut self, name: &str, _cx: &mut Context<Self>) {
        self.selected_instance = Some(name.to_string());
        // Only clear selected_file if it's not inside the new instance.
        let inst_dir = self
            .selected_workspace
            .as_deref()
            .map(|ws| instance::instance_dir(ws, name));
        if let (Some(inst_dir), Some(file)) = (inst_dir, &self.selected_file) {
            if !file.starts_with(&inst_dir) {
                self.selected_file = None;
            }
        }
        self.file_error = None;
        self.expanded.clear();
        self.dir_cache.borrow_mut().clear();
    }

    fn persist(&self) {
        let layouts: Vec<config::InstanceLayout> = self
            .layouts
            .iter()
            .map(|((ws, inst), l)| config::InstanceLayout {
                workspace: ws.clone(),
                instance: inst.clone(),
                left_panel_w: l.left_panel_w,
                right_panel_w: l.right_panel_w,
                terminal_panel_size: l.terminal_panel_size,
                terminal_panel_pos: match l.terminal_panel_pos {
                    PanelPos::Bottom => "bottom".into(),
                    PanelPos::Right => "right".into(),
                },
                left_collapsed: l.left_collapsed,
                right_collapsed: l.right_collapsed,
            })
            .collect();
        config::save_app_state(&config::AppState {
            last_workspace: self.selected_workspace.clone(),
            last_instance: self.selected_instance.clone(),
            expanded: self.expanded.iter().cloned().collect(),
            selected_file: self.selected_file.clone(),
            layouts,
        });
    }

    fn current_layout(&self) -> Layout {
        self.active_instance_key()
            .and_then(|k| self.layouts.get(&k).copied())
            .unwrap_or_default()
    }

    fn mutate_layout(&mut self, f: impl FnOnce(&mut Layout)) {
        let Some(key) = self.active_instance_key() else { return };
        let entry = self.layouts.entry(key).or_default();
        f(entry);
        self.persist();
    }

    fn active_workspace(&self) -> Option<&Workspace> {
        let name = self.selected_workspace.as_deref()?;
        self.workspaces.iter().find(|(w, _, _)| w.name == name).map(|(w, _, _)| w)
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

    fn on_global_mouse_move(
        &mut self,
        ev: &gpui::MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.drag else { return };
        let win_size = window.viewport_size();
        let win_w = win_size.width.as_f32();
        let win_h = win_size.height.as_f32();
        let x = ev.position.x.as_f32();
        let y = ev.position.y.as_f32();
        let current = self.current_layout();
        match drag {
            DragKind::LeftPanel => {
                let w = x.clamp(140., (win_w - 240.).max(140.));
                self.mutate_layout(|l| l.left_panel_w = w);
            }
            DragKind::RightPanel => {
                let w = (win_w - x).clamp(140., (win_w - current.left_panel_w - 200.).max(140.));
                self.mutate_layout(|l| l.right_panel_w = w);
            }
            DragKind::TerminalPanel => match current.terminal_panel_pos {
                PanelPos::Bottom => {
                    let h = (win_h - y).clamp(80., (win_h - 120.).max(80.));
                    self.mutate_layout(|l| l.terminal_panel_size = h);
                }
                PanelPos::Right => {
                    let new_w = (win_w - x).clamp(160., (win_w - current.left_panel_w - 200.).max(160.));
                    self.mutate_layout(|l| l.terminal_panel_size = new_w);
                }
            },
            DragKind::TerminalPanelMove => {
                let dist_bottom = win_h - y;
                let dist_right = win_w - x;
                let pos = if dist_bottom < dist_right { PanelPos::Bottom } else { PanelPos::Right };
                self.mutate_layout(|l| l.terminal_panel_pos = pos);
            }
        }
        cx.notify();
    }

    fn on_global_mouse_up(
        &mut self,
        _ev: &gpui::MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.drag.take().is_some() {
            cx.notify();
        }
    }

    fn open_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        // Reuse if already open (preserves cursor / scroll / undo history / unsaved edits).
        if self.editors.iter().any(|e| e.path == path) {
            self.file_error = None;
            self.selected_file = Some(path);
            self.persist();
            return;
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                self.file_error = None;
                let path_for_editor = path.clone();
                let entity = cx.new(|cx| CodeEditor::new(path_for_editor, text, cx));
                // If this is a workspace TOML, refresh the workspace list on save
                // so the sidebar reflects renames/edits.
                if path.starts_with(config::workspaces_dir()) {
                    cx.subscribe(&entity, |this, _editor, event, cx| {
                        let CodeEditorEvent::Saved = event;
                        this.workspaces = config::load_workspaces();
                        cx.notify();
                    })
                    .detach();
                }
                self.editors.push(OpenEditor {
                    path: path.clone(),
                    entity,
                });
                self.selected_file = Some(path);
            }
            Err(e) => {
                self.file_error = Some(format!("{e}"));
                self.selected_file = Some(path);
            }
        }
        self.persist();
    }

    fn close_editor(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(idx) = self.editors.iter().position(|e| e.path == path) else { return };
        self.editors.remove(idx);
        if self.selected_file.as_deref() == Some(path) {
            // Pick a neighbor in the same instance, preferring the next tab, then the
            // previous, then None.
            let inst_dir = self.active_instance_path();
            let in_instance = |p: &PathBuf| {
                inst_dir
                    .as_deref()
                    .map(|i| p.starts_with(i))
                    .unwrap_or(true)
            };
            let next = self
                .editors
                .iter()
                .skip(idx)
                .find(|e| in_instance(&e.path))
                .or_else(|| self.editors.iter().take(idx).rev().find(|e| in_instance(&e.path)));
            self.selected_file = next.map(|e| e.path.clone());
            self.file_error = None;
        }
        self.persist();
        cx.notify();
    }

    fn active_editor_entity(&self) -> Option<Entity<CodeEditor>> {
        let path = self.selected_file.as_ref()?;
        self.editors
            .iter()
            .find(|e| &e.path == path)
            .map(|e| e.entity.clone())
    }

    fn toggle_dir(&mut self, path: &Path) {
        if !self.expanded.insert(path.to_path_buf()) {
            self.expanded.remove(path);
        }
        self.persist();
    }

    /// Create a blank workspace TOML, refresh the in-memory workspace list,
    /// and open the new file in the editor so the user can fill it in.
    pub(crate) fn create_workspace(&mut self, cx: &mut Context<Self>) {
        match config::create_blank_workspace() {
            Ok(path) => {
                self.workspaces = config::load_workspaces();
                self.open_file(path, cx);
                self.status = "created blank workspace — edit and save".into();
            }
            Err(e) => {
                self.status = format!("create workspace failed: {e:#}");
            }
        }
    }

    fn start_create_instance(&mut self) {
        if self.selected_workspace.is_some() {
            self.modal = Modal::NewInstance(String::new());
        }
    }

    pub(crate) fn finish_create_instance(&mut self, cx: &mut Context<Self>) {
        let name_raw = match std::mem::replace(&mut self.modal, Modal::None) {
            Modal::NewInstance(s) => s,
            other => { self.modal = other; return; }
        };
        let name = name_raw.trim().to_string();
        if name.is_empty() {
            return;
        }
        let Some(ws) = self.active_workspace().cloned() else { return };
        self.modal = Modal::Cloning(name.clone());
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
                this.modal = Modal::None;
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

    pub(crate) fn cancel_create_instance(&mut self) {
        if matches!(self.modal, Modal::NewInstance(_)) {
            self.modal = Modal::None;
        }
    }

    pub(crate) fn confirm_delete(&mut self, cx: &mut Context<Self>) {
        let (ws, inst) = match std::mem::replace(&mut self.modal, Modal::None) {
            Modal::Delete { ws, instance } => (ws, instance),
            other => { self.modal = other; return; }
        };
        match instance::delete_instance(&ws, &inst) {
            Ok(()) => {
                self.refresh_instances(&ws);
                if self.selected_instance.as_deref() == Some(inst.as_str()) {
                    let inst_dir = instance::instance_dir(&ws, &inst);
                    self.editors.retain(|e| !e.path.starts_with(&inst_dir));
                    self.selected_instance = None;
                    self.selected_file = None;
                    self.file_error = None;
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

    pub(crate) fn cancel_delete(&mut self) {
        if matches!(self.modal, Modal::Delete { .. }) {
            self.modal = Modal::None;
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        match &mut self.modal {
            Modal::Delete { .. } => {
                match key {
                    "enter" => self.confirm_delete(cx),
                    "escape" => { self.cancel_delete(); cx.notify(); }
                    _ => {}
                }
            }
            Modal::NewInstance(name) => {
                match key {
                    "enter" => self.finish_create_instance(cx),
                    "escape" => { self.cancel_create_instance(); cx.notify(); }
                    "backspace" => { name.pop(); cx.notify(); }
                    _ => {
                        let push = event
                            .keystroke
                            .key_char
                            .as_deref()
                            .filter(|c| !c.is_empty() && !c.chars().any(|ch| ch.is_control()));
                        if let Some(text) = push {
                            name.push_str(text);
                            cx.notify();
                        }
                    }
                }
            }
            Modal::Cloning(_) | Modal::None => {}
        }
    }
}

impl Focusable for Protocol {
    fn focus_handle(&self, _: &App) -> FocusHandle { self.focus.clone() }
}

impl Render for Protocol {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = std::time::Instant::now();
        let body = self.render_body(window, cx);
        if std::env::var("PROTOCOL_TIMING").is_ok() {
            let ms = t.elapsed().as_secs_f64() * 1000.;
            if ms > 0.5 {
                eprintln!("protocol render_body: {:.2}ms", ms);
            }
        }

        let mut root = div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_move(cx.listener(Self::on_global_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_global_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_global_mouse_up))
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::bg())
            .text_color(theme::text())
            .font_family(".SystemUIFont")
            .text_size(px(13.))
            .child(self.render_titlebar(cx))
            .child(body)
            .child(self.render_status());

        match &self.modal {
            Modal::None => {}
            Modal::Delete { ws, instance } => {
                root = root.child(render_delete_modal(ws, instance, cx));
            }
            Modal::NewInstance(name) => {
                let ws_label = self.selected_workspace.clone().unwrap_or_default();
                root = root.child(render_new_instance_modal(&ws_label, name, cx));
            }
            Modal::Cloning(name) => {
                let ws_label = self.selected_workspace.clone().unwrap_or_default();
                root = root.child(render_cloning_overlay(&ws_label, name));
            }
        }
        root
    }
}

impl Protocol {
    fn render_titlebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let traffic_light_room = px(76.);
        let layout = self.current_layout();
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
            .pl(traffic_light_room)
            .pr_2()
            .text_size(px(12.))
            .text_color(theme::text_strong())
            .child(sidebar_toggle_button(
                "tb-left",
                if layout.left_collapsed { "▶" } else { "◀" },
                cx.listener(|this, _, _, cx| {
                    this.mutate_layout(|l| l.left_collapsed = !l.left_collapsed);
                    cx.notify();
                }),
            ))
            .child(div().w_2())
            .child(div().flex_1().child(SharedString::from(crumb)))
            .child(sidebar_toggle_button(
                "tb-right",
                if layout.right_collapsed { "◀" } else { "▶" },
                cx.listener(|this, _, _, cx| {
                    this.mutate_layout(|l| l.right_collapsed = !l.right_collapsed);
                    cx.notify();
                }),
            ))
    }

    fn render_body(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let layout = self.current_layout();
        // Editor + terminal live INSIDE the same center card regardless of dock
        // direction so we get one rounded-card frame with an internal split seam,
        // not two separate cards.
        let r = px(6.);
        let center_inner = match layout.terminal_panel_pos {
            PanelPos::Bottom => div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .child(
                    // Editor occupies the top half — round only the top corners
                    // so its square bottom edge can't bleed past the card's clip.
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .rounded_tl(r)
                        .rounded_tr(r)
                        .overflow_hidden()
                        .child(self.render_main(window, cx)),
                )
                .child(resize_handle(DragKind::TerminalPanel, false, self.drag, cx))
                .child(self.render_terminal_panel(window, cx)),
            PanelPos::Right => div()
                .flex()
                .flex_row()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .child(
                    // Editor on the left — only top-left and bottom-left round.
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .rounded_tl(r)
                        .rounded_bl(r)
                        .overflow_hidden()
                        .child(self.render_main(window, cx)),
                )
                .child(resize_handle(DragKind::TerminalPanel, true, self.drag, cx))
                .child(self.render_terminal_panel(window, cx)),
        };
        let center = card_frame()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(center_inner);

        let mut row = div()
            .flex()
            .flex_row()
            .flex_1()
            .min_h_0()
            // Behind the rounded center card, paint the same surface as the
            // sidebars. Otherwise the editor/terminal bg matches the body bg
            // and the rounded corners read as invisible.
            .bg(theme::panel_bg());
        if !layout.left_collapsed {
            row = row
                .child(self.render_left(cx))
                .child(resize_handle(DragKind::LeftPanel, true, self.drag, cx));
        }
        row = row.child(center);
        if !layout.right_collapsed {
            row = row
                .child(resize_handle(DragKind::RightPanel, true, self.drag, cx))
                .child(self.render_right(cx));
        }
        row
    }

    fn render_right(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let layout = self.current_layout();
        let mut col = div()
            .flex()
            .flex_col()
            .w(px(layout.right_panel_w))
            .flex_none()
            .overflow_hidden()
            .bg(theme::panel_bg());
        // Header.
        col = col.child(
            div()
                .flex()
                .items_center()
                .h(px(theme::SECTION_HEADER_H + 4.))
                .px(px(theme::ROW_PAD_X))
                .text_size(px(11.5))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text_muted())
                .child("FILES"),
        );
        if let Some(root) = self.active_instance_path() {
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
        } else {
            col = col.child(
                row_base()
                    .text_color(theme::text_dim())
                    .child("no instance selected"),
            );
        }
        col
    }

    fn render_left(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let layout = self.current_layout();
        div()
            .flex()
            .flex_col()
            .w(px(layout.left_panel_w))
            .flex_none()
            .overflow_hidden()
            .bg(theme::panel_bg())
            .child(self.render_workspace_tree(cx))
    }

    fn render_workspace_tree(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = div().flex().flex_col().pb_2();
        // Top header row: "WORKSPACES" label + "+" to create a blank workspace.
        // Always present, even when the list is empty, so first-time users have
        // an obvious entry point.
        col = col.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .h(px(theme::SECTION_HEADER_H))
                .px(px(theme::ROW_PAD_X))
                .text_size(px(10.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text_muted())
                .child(SharedString::from("WORKSPACES"))
                .child(div().flex_1())
                .child(
                    div()
                        .id("ws-new")
                        .w(px(20.))
                        .h(px(20.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .text_size(px(13.))
                        .text_color(theme::text_dim())
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::row_hover()).text_color(theme::text()))
                        .child("+")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.create_workspace(cx);
                                cx.notify();
                            }),
                        ),
                ),
        );
        if self.workspaces.is_empty() {
            col = col.child(
                row_base()
                    .text_color(theme::text_dim())
                    .child(SharedString::from(format!(
                        "drop a .toml in {} or click +",
                        config::workspaces_dir().display()
                    ))),
            );
            return col;
        }
        for (w, ws_path, _err) in &self.workspaces {
            let ws_name = w.name.clone();
            let collapsed = self.collapsed_workspaces.contains(&ws_name);
            let header_chevron = if collapsed { "▸" } else { "▾" };
            let ws_name_for_toggle = ws_name.clone();
            let ws_name_for_new = ws_name.clone();
            let ws_path_for_edit = ws_path.clone();
            // Whole-row hover + click toggles collapse; gear/+ buttons sit on
            // the right and stop propagation so they don't also toggle.
            col = col.child(
                div()
                    .id(SharedString::from(format!("ws-header:{}", ws_name)))
                    .flex()
                    .items_center()
                    .gap_1()
                    .h(px(theme::SECTION_HEADER_H + 4.))
                    .px(px(theme::ROW_PAD_X))
                    .mx(px(4.))
                    .px_1()
                    .rounded(px(4.))
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::text_strong())
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::row_hover()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            if this.collapsed_workspaces.contains(&ws_name_for_toggle) {
                                this.collapsed_workspaces.remove(&ws_name_for_toggle);
                            } else {
                                this.collapsed_workspaces
                                    .insert(ws_name_for_toggle.clone());
                            }
                            cx.notify();
                        }),
                    )
                    .child(SharedString::from(format!("[ {ws_name} ]")))
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(theme::text_dim())
                            .child(header_chevron),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .id(SharedString::from(format!("ws-edit:{}", ws_name)))
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(22.))
                            .h(px(22.))
                            .rounded(px(4.))
                            .text_size(px(12.))
                            .text_color(theme::text_muted())
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::row_hover()).text_color(theme::text()))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.open_file(ws_path_for_edit.clone(), cx);
                                    cx.notify();
                                }),
                            )
                            .child("⚙"),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("ws-new:{}", ws_name)))
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(22.))
                            .h(px(22.))
                            .rounded(px(4.))
                            .text_size(px(15.))
                            .text_color(theme::text_muted())
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::row_hover()).text_color(theme::text()))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    if this.selected_workspace.as_deref()
                                        != Some(ws_name_for_new.as_str())
                                    {
                                        this.select_workspace(&ws_name_for_new, cx);
                                    }
                                    this.start_create_instance();
                                    cx.notify();
                                }),
                            )
                            .child("+"),
                    ),
            );
            if collapsed {
                continue;
            }

            // Instances of this workspace.
            let inst_root = config::instances_dir().join(&ws_name);
            let entries = {
                let mut cache = self.dir_cache.borrow_mut();
                cache
                    .entry(inst_root.clone())
                    .or_insert_with(|| instance::read_dir_sorted(&inst_root))
                    .clone()
            };
            let instance_entries: Vec<_> = entries.into_iter().filter(|e| e.is_dir).collect();
            if instance_entries.is_empty() && !self.modal.is_new_instance() {
                col = col.child(
                    row_base()
                        .pl(px(theme::ROW_PAD_X + theme::INDENT_PX - 4.))
                        .text_color(theme::text_dim())
                        .child("no instances"),
                );
            }
            for inst in &instance_entries {
                let inst_name = inst.name.clone();
                let inst_path = inst.path.clone();
                let active = self.selected_workspace.as_deref() == Some(ws_name.as_str())
                    && self.selected_instance.as_deref() == Some(inst_name.as_str());
                let expanded = self.expanded.contains(&inst_path);
                let chevron = if expanded { "▾" } else { "▸" };
                let ws_for_select = ws_name.clone();
                let ws_for_right = ws_name.clone();
                let inst_for_right = inst_name.clone();
                let inst_for_click = inst_name.clone();
                let inst_path_for_click = inst_path.clone();
                let _ = (expanded, chevron, inst_path_for_click); // file tree lives in the right sidebar now
                col = col.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .h(px(48.))
                        .mx(px(4.))
                        .my(px(2.))
                        .px(px(theme::INDENT_PX))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(if active {
                            gpui::hsla(220. / 360., 0.6, 0.6, 0.35)
                        } else {
                            gpui::hsla(0., 0., 0., 0.)
                        })
                        .text_size(px(14.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .when(active, |d| d.bg(theme::row_selected()))
                        .hover(|s| s.bg(theme::row_hover()))
                        .cursor_pointer()
                        .child(
                            div()
                                .flex_1()
                                .text_color(if active { theme::text_strong() } else { theme::text() })
                                .child(SharedString::from(inst_name.clone())),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                if this.selected_workspace.as_deref() != Some(ws_for_select.as_str()) {
                                    this.select_workspace(&ws_for_select, cx);
                                }
                                this.select_instance(&inst_for_click, cx);
                                this.persist();
                                cx.notify();
                            }),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, _, _, cx| {
                                this.modal = Modal::Delete { ws: ws_for_right.clone(), instance: inst_for_right.clone() };
                                cx.notify();
                            }),
                        ),
                );
            }

        }
        col
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
        let entries = {
            let mut cache = self.dir_cache.borrow_mut();
            cache
                .entry(dir.to_path_buf())
                .or_insert_with(|| instance::read_dir_sorted(dir))
                .clone()
        };
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
        for (w, _ws_path, err) in &self.workspaces {
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
        if instances.is_empty() && !self.modal.is_new_instance() {
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
                        this.modal = Modal::Delete { ws: ws_for_right.clone(), instance: name_for_right.clone() };
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

    fn render_main(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let editor_focused = self
            .active_editor_entity()
            .map(|e| e.read(cx).focus.is_focused(window))
            .unwrap_or(false);
        let outer = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .relative();
        let inst_dir = self.active_instance_path();
        let tabs: Vec<(PathBuf, bool, bool)> = self
            .editors
            .iter()
            .filter(|e| {
                // Include tabs that belong to the active instance, plus any
                // "global" tab living under ~/.protocol/workspaces (workspace
                // TOML editing). Workspace TOMLs aren't tied to an instance
                // and should show regardless.
                let in_instance = inst_dir
                    .as_deref()
                    .map_or(true, |i| e.path.starts_with(i));
                let is_workspace_toml = e.path.starts_with(config::workspaces_dir());
                in_instance || is_workspace_toml
            })
            .map(|e| {
                (
                    e.path.clone(),
                    e.entity.read(cx).dirty,
                    self.selected_file.as_deref() == Some(e.path.as_path()),
                )
            })
            .collect();

        let tab_bar = self.render_tab_bar(&tabs, inst_dir.as_deref(), cx);
        let body: gpui::AnyElement = if let Some(file) = &self.selected_file {
            if let Some(err) = &self.file_error {
                div()
                    .p_3()
                    .text_color(theme::danger())
                    .child(SharedString::from(format!("could not read: {err}")))
                    .into_any_element()
            } else if let Some(editor) = self.active_editor_entity() {
                let _ = file;
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(editor)
                    .into_any_element()
            } else {
                placeholder("no file selected").into_any_element()
            }
        } else {
            placeholder("no file selected").into_any_element()
        };

        let mut wrapper = outer.child(tab_bar).child(body);
        if editor_focused {
            wrapper = wrapper.child(focus_overlay());
        }
        wrapper
    }

    fn render_tab_bar(
        &self,
        tabs: &[(PathBuf, bool, bool)],
        inst_dir: Option<&Path>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let bar = div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(28.))
            .bg(theme::titlebar_bg())
            .text_size(px(11.5));
        if tabs.is_empty() {
            return bar.child(
                div()
                    .px_3()
                    .text_color(theme::text_dim())
                    .child("no files open"),
            );
        }
        let mut bar = bar;
        for (path, dirty, active) in tabs {
            let label_text = path
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| path.display().to_string());
            let mut label_str = label_text;
            if *dirty {
                label_str.push_str(" •");
            }
            let path_for_switch = path.clone();
            let path_for_close = path.clone();
            let tab_id: SharedString = format!("editor-tab:{}", path.display()).into();
            let close_id: SharedString = format!("editor-close:{}", path.display()).into();
            bar = bar.child(
                div()
                    .id(tab_id)
                    .flex()
                    .flex_row()
                    .items_center()
                    .h_full()
                    .px_3()
                    .gap_2()
                    .when(*active, |d| d.bg(theme::bg()))
                    .text_color(if *active {
                        theme::text_strong()
                    } else if *dirty {
                        theme::accent()
                    } else {
                        theme::text_muted()
                    })
                    .border_r_1()
                    .border_color(theme::divider())
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::row_hover()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.selected_file = Some(path_for_switch.clone());
                            this.file_error = None;
                            this.persist();
                            cx.notify();
                        }),
                    )
                    .child(SharedString::from(label_str))
                    .child(
                        div()
                            .id(close_id)
                            .text_color(theme::text_dim())
                            .hover(|s| s.text_color(theme::danger()))
                            .child("×")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.close_editor(&path_for_close, cx);
                                }),
                            ),
                    ),
            );
        }
        bar
    }

    fn render_terminal_panel(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let term_focused = self
            .active_terminal_entity()
            .map(|t| t.read(cx).focus.is_focused(window))
            .unwrap_or(false);
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
        // Spacer.
        tabs = tabs.child(div().flex_1());
        // Dock-position handle: drag to swap between bottom and right docks; click toggles.
        let dock_label = match self.current_layout().terminal_panel_pos {
            PanelPos::Bottom => "↧ dock bottom",
            PanelPos::Right => "↦ dock right",
        };
        tabs = tabs.child(
            div()
                .id("term-dock")
                .flex()
                .items_center()
                .px_3()
                .h_full()
                .text_size(px(11.))
                .text_color(theme::text_muted())
                .cursor(CursorStyle::OpenHand)
                .hover(|s| s.bg(theme::row_hover()).text_color(theme::text()))
                .child(dock_label)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _ev, _w, cx| {
                        this.drag = Some(DragKind::TerminalPanelMove);
                        cx.notify();
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
                // Match an open terminal's bg so the empty panel reads as a
                // terminal area, not as continuation of the editor surface.
                .bg(gpui::rgb(0x12141a))
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
            div().flex_1().min_h_0().bg(gpui::rgb(0x12141a)).into_any_element()
        };

        let layout = self.current_layout();
        let outer = div()
            .flex()
            .flex_col()
            .flex_none()
            .relative()
            .overflow_hidden();
        let outer = match layout.terminal_panel_pos {
            PanelPos::Bottom => outer.h(px(layout.terminal_panel_size)).w_full(),
            PanelPos::Right => outer.w(px(layout.terminal_panel_size)),
        };
        let mut wrapper = outer.child(tabs).child(body);
        if term_focused {
            wrapper = wrapper.child(focus_overlay());
        }
        wrapper
    }

    fn render_status(&self) -> impl IntoElement {
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
            .bg(theme::titlebar_bg())
            .text_size(px(10.5))
            .text_color(theme::text_muted())
            .child(div().flex_1())
            .child(div().child(right))
    }
}

fn resize_handle(
    kind: DragKind,
    vertical: bool,
    current_drag: Option<DragKind>,
    cx: &mut Context<Protocol>,
) -> impl IntoElement {
    let active = current_drag == Some(kind);
    let cursor = if vertical { CursorStyle::ResizeLeftRight } else { CursorStyle::ResizeUpDown };
    let id_str: SharedString = format!("resize:{:?}:{}", kind, vertical).into();

    // Zero-width layout slot. The actual interactive area is an absolutely
    // positioned 7px-wide hit zone owning the cursor + click handler so users
    // can grab it without precision aiming. Surfaces still abut seamlessly.
    let outer = div().relative().flex_none();
    let outer = if vertical {
        outer.w(px(0.)).h_full()
    } else {
        outer.h(px(0.)).w_full()
    };
    let hit = div()
        .id(id_str)
        .absolute()
        .cursor(cursor)
        .hover(|s| s.bg(gpui::hsla(220. / 360., 0.6, 0.6, 0.18)))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _ev, _w, cx| {
                this.drag = Some(kind);
                cx.notify();
            }),
        );
    let hit = if vertical {
        hit.top_0().bottom_0().left(px(-3.)).w(px(7.))
    } else {
        hit.left_0().right_0().top(px(-3.)).h(px(7.))
    };
    let mut wrapper = outer.child(hit);
    if active {
        let stroke = div().absolute();
        let stroke = if vertical {
            stroke.top_0().bottom_0().left(px(0.)).w(px(1.)).bg(theme::accent())
        } else {
            stroke.left_0().right_0().top(px(0.)).h(px(1.)).bg(theme::accent())
        };
        wrapper = wrapper.child(stroke);
    }
    wrapper
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
