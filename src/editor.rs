use std::ops::Range;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, Element, ElementInputHandler,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, InspectorElementId,
    IntoElement, KeyDownEvent, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, Point, SharedString, ShapedLine, Style, TextRun, UTF16Selection, Window, div, fill,
    font, hsla, point, prelude::*, px, relative,
};

use crate::highlight;
use crate::theme;

const LINE_H: f32 = 18.;
const CHAR_W: f32 = 7.51; // Menlo @ 12.5px, average glyph advance.
const FONT_FAMILY: &str = "Menlo";
const FONT_SIZE: f32 = 12.5;
const TOP_PAD: f32 = 8.;
const LEFT_PAD: f32 = 12.; // matches body's pl(LEFT_PAD).

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditKind {
    Insert,
    Delete,
    Other,
}

#[derive(Clone)]
struct Snapshot {
    text: String,
    cursor: usize,
    anchor: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
pub enum CodeEditorEvent {
    Saved,
}

pub struct CodeEditor {
    pub focus: FocusHandle,
    pub path: PathBuf,
    pub text: String,
    pub cursor: usize,
    pub anchor: Option<usize>,
    pub marked: Option<Range<usize>>,
    pub goal_col: Option<usize>,
    pub scroll_y: f32,
    pub scroll_x: f32,
    pub viewport_h: f32,
    pub viewport_w: f32,
    pub body_bounds: Option<Bounds<Pixels>>,
    pub dirty: bool,
    pub status: String,
    pub blink_visible: bool,
    is_dragging: bool,
    history: Vec<Snapshot>,
    history_idx: usize,
    last_edit_kind: Option<EditKind>,
    last_edit_at: Instant,
    text_version: u64,
    cached_highlight_version: u64,
    cached_highlight: Vec<TextRun>,
    cached_line_runs_version: u64,
    cached_line_runs: Vec<LineRuns>,
}

impl CodeEditor {
    pub fn new(path: PathBuf, text: String, cx: &mut Context<Self>) -> Self {
        let initial = Snapshot { text: text.clone(), cursor: 0, anchor: None };
        let mut this = Self {
            focus: cx.focus_handle(),
            path,
            text,
            cursor: 0,
            anchor: None,
            marked: None,
            goal_col: None,
            scroll_y: 0.,
            scroll_x: 0.,
            viewport_h: 600.,
            viewport_w: 800.,
            body_bounds: None,
            dirty: false,
            status: String::new(),
            blink_visible: true,
            is_dragging: false,
            history: vec![initial],
            history_idx: 0,
            last_edit_kind: None,
            last_edit_at: Instant::now(),
            text_version: 1,
            cached_highlight_version: 0,
            cached_highlight: Vec::new(),
            cached_line_runs_version: 0,
            cached_line_runs: Vec::new(),
        };
        this.start_blink(cx);
        this
    }

    pub fn replace_with_file(&mut self, path: PathBuf, text: String) {
        self.path = path;
        self.text = text.clone();
        self.cursor = 0;
        self.anchor = None;
        self.marked = None;
        self.goal_col = None;
        self.scroll_y = 0.;
        self.scroll_x = 0.;
        self.dirty = false;
        self.status.clear();
        self.history = vec![Snapshot { text, cursor: 0, anchor: None }];
        self.history_idx = 0;
        self.last_edit_kind = None;
        self.text_version = self.text_version.wrapping_add(1);
    }

    fn highlight_runs(&mut self) -> &[TextRun] {
        if self.cached_highlight_version != self.text_version {
            let ext = self.ext().map(|s| s.to_string());
            self.cached_highlight = highlight::runs_for(&self.text, ext.as_deref(), FONT_FAMILY);
            self.cached_highlight_version = self.text_version;
        }
        &self.cached_highlight
    }

    fn line_runs(&mut self) -> Vec<LineRuns> {
        if self.cached_line_runs_version != self.text_version {
            // Borrow checker dance: highlight_runs returns &[TextRun]; we then need text too.
            if self.cached_highlight_version != self.text_version {
                let ext = self.ext().map(|s| s.to_string());
                self.cached_highlight = highlight::runs_for(&self.text, ext.as_deref(), FONT_FAMILY);
                self.cached_highlight_version = self.text_version;
            }
            self.cached_line_runs = split_runs_per_line(&self.text, &self.cached_highlight);
            self.cached_line_runs_version = self.text_version;
        }
        self.cached_line_runs.clone()
    }

    fn bump_text_version(&mut self) {
        self.text_version = self.text_version.wrapping_add(1);
    }

    fn longest_line_chars(&self) -> usize {
        self.text.lines().map(|l| l.chars().count()).max().unwrap_or(0)
    }

    fn start_blink(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(530))
                    .await;
                if this
                    .update(cx, |this, cx| {
                        this.blink_visible = !this.blink_visible;
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    fn reset_blink(&mut self) {
        self.blink_visible = true;
    }

    pub fn ext(&self) -> Option<&str> {
        self.path.extension().and_then(|s| s.to_str())
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        match std::fs::write(&self.path, &self.text) {
            Ok(()) => {
                self.dirty = false;
                self.status = "saved".into();
                cx.emit(CodeEditorEvent::Saved);
            }
            Err(e) => {
                self.status = format!("save failed: {e}");
            }
        }
    }

    // ---- selection helpers ----

    fn selection_range(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?;
        let (a, b) = if anchor < self.cursor {
            (anchor, self.cursor)
        } else {
            (self.cursor, anchor)
        };
        if a == b { None } else { Some(a..b) }
    }

    fn selected_text(&self) -> Option<String> {
        self.selection_range().map(|r| self.text[r].to_string())
    }

    fn extend_to(&mut self, new_cursor: usize) {
        if self.anchor.is_none() {
            self.anchor = Some(self.cursor);
        }
        self.cursor = new_cursor;
        self.reset_blink();
    }

    fn collapse_to(&mut self, new_cursor: usize) {
        self.anchor = None;
        self.cursor = new_cursor;
        self.reset_blink();
    }

    fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.text.len();
        self.goal_col = None;
    }

    // ---- byte-offset arithmetic ----

    fn line_count(&self) -> usize { self.text.matches('\n').count() + 1 }

    fn move_to_left(&mut self, extend: bool) {
        if !extend {
            if let Some(r) = self.selection_range() {
                self.collapse_to(r.start);
                self.goal_col = None;
                return;
            }
        }
        let new = prev_grapheme(&self.text, self.cursor);
        if extend { self.extend_to(new) } else { self.collapse_to(new) }
        self.goal_col = None;
    }

    fn move_to_right(&mut self, extend: bool) {
        if !extend {
            if let Some(r) = self.selection_range() {
                self.collapse_to(r.end);
                self.goal_col = None;
                return;
            }
        }
        let new = next_grapheme(&self.text, self.cursor);
        if extend { self.extend_to(new) } else { self.collapse_to(new) }
        self.goal_col = None;
    }

    fn move_vertical(&mut self, dy: isize, extend: bool) {
        let (line, col) = byte_to_line_col(&self.text, self.cursor);
        let goal = self.goal_col.unwrap_or(col);
        let total = self.line_count();
        let new_line = (line as isize + dy).clamp(0, total as isize - 1) as usize;
        let new = if dy < 0 && new_line == 0 && line == 0 {
            0
        } else if dy > 0 && new_line == total - 1 && line == total - 1 {
            self.text.len()
        } else {
            line_col_to_byte(&self.text, new_line, goal)
        };
        if extend { self.extend_to(new) } else { self.collapse_to(new) }
        self.goal_col = Some(goal);
    }

    fn move_home(&mut self, extend: bool) {
        let (line, _) = byte_to_line_col(&self.text, self.cursor);
        let new = line_col_to_byte(&self.text, line, 0);
        if extend { self.extend_to(new) } else { self.collapse_to(new) }
        self.goal_col = None;
    }

    fn move_end(&mut self, extend: bool) {
        let (line, _) = byte_to_line_col(&self.text, self.cursor);
        let new = line_col_to_byte(&self.text, line, usize::MAX);
        if extend { self.extend_to(new) } else { self.collapse_to(new) }
        self.goal_col = None;
    }

    // ---- mutations ----

    fn record(&mut self, kind: EditKind) {
        self.history.truncate(self.history_idx + 1);
        let snap = Snapshot { text: self.text.clone(), cursor: self.cursor, anchor: self.anchor };
        let now = Instant::now();
        let coalesce = self.last_edit_kind == Some(kind)
            && now.duration_since(self.last_edit_at) < Duration::from_millis(500);
        if coalesce {
            *self.history.last_mut().unwrap() = snap;
        } else {
            self.history.push(snap);
            self.history_idx = self.history.len() - 1;
        }
        self.last_edit_kind = Some(kind);
        self.last_edit_at = now;
        const CAP: usize = 500;
        if self.history.len() > CAP {
            let drop = self.history.len() - CAP;
            self.history.drain(0..drop);
            self.history_idx = self.history_idx.saturating_sub(drop);
        }
    }

    fn replace_or_insert(&mut self, s: &str, kind: EditKind) {
        self.marked = None;
        if let Some(range) = self.selection_range() {
            self.text.replace_range(range.clone(), s);
            self.cursor = range.start + s.len();
            self.anchor = None;
        } else {
            self.text.insert_str(self.cursor, s);
            self.cursor += s.len();
        }
        self.dirty = true;
        self.goal_col = None;
        self.status.clear();
        self.reset_blink();
        self.bump_text_version();
        self.record(kind);
    }

    fn insert(&mut self, s: &str) { self.replace_or_insert(s, EditKind::Insert) }

    fn delete_back(&mut self) {
        self.marked = None;
        if let Some(range) = self.selection_range() {
            self.text.replace_range(range.clone(), "");
            self.cursor = range.start;
            self.anchor = None;
        } else if self.cursor > 0 {
            let prev = prev_grapheme(&self.text, self.cursor);
            self.text.replace_range(prev..self.cursor, "");
            self.cursor = prev;
        } else {
            return;
        }
        self.dirty = true;
        self.goal_col = None;
        self.status.clear();
        self.reset_blink();
        self.bump_text_version();
        self.record(EditKind::Delete);
    }

    fn delete_forward(&mut self) {
        self.marked = None;
        if let Some(range) = self.selection_range() {
            self.text.replace_range(range.clone(), "");
            self.cursor = range.start;
            self.anchor = None;
        } else if self.cursor < self.text.len() {
            let next = next_grapheme(&self.text, self.cursor);
            self.text.replace_range(self.cursor..next, "");
        } else {
            return;
        }
        self.dirty = true;
        self.goal_col = None;
        self.status.clear();
        self.reset_blink();
        self.bump_text_version();
        self.record(EditKind::Delete);
    }

    fn newline_with_indent(&mut self) {
        let (line, _) = byte_to_line_col(&self.text, self.cursor);
        let line_start = line_col_to_byte(&self.text, line, 0);
        let leading: String = self.text[line_start..self.cursor]
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let prev_ch = self.text[..self.cursor].chars().last();
        let extra = match prev_ch {
            Some('{') | Some('(') | Some('[') | Some(':') => "    ",
            _ => "",
        };
        let to_insert = format!("\n{leading}{extra}");
        self.replace_or_insert(&to_insert, EditKind::Insert);
    }

    fn indent_or_tab(&mut self) {
        if let Some(range) = self.selection_range() {
            // Indent every line touched by the selection.
            let start_line = byte_to_line_col(&self.text, range.start).0;
            let end_line = byte_to_line_col(&self.text, range.end.saturating_sub(1)).0;
            let mut delta = 0usize;
            let mut new_start = range.start;
            let mut new_end = range.end;
            for line in start_line..=end_line {
                let line_start = line_col_to_byte(&self.text, line, 0) + delta;
                self.text.insert_str(line_start, "    ");
                delta += 4;
                if line_start <= new_start { new_start += 4; }
                new_end += 4;
            }
            self.anchor = Some(new_start);
            self.cursor = new_end;
            self.dirty = true;
            self.status.clear();
            self.bump_text_version();
            self.record(EditKind::Other);
        } else {
            self.replace_or_insert("    ", EditKind::Insert);
        }
    }

    fn dedent(&mut self) {
        let (sel_start, sel_end) = self.selection_range()
            .map(|r| (r.start, r.end))
            .unwrap_or((self.cursor, self.cursor));
        let start_line = byte_to_line_col(&self.text, sel_start).0;
        let end_line = byte_to_line_col(&self.text, sel_end.saturating_sub(1).max(sel_start)).0;
        let mut total_removed = 0usize;
        let mut first_line_removed = 0usize;
        for line in start_line..=end_line {
            let line_start = line_col_to_byte(&self.text, line, 0).saturating_sub(total_removed);
            let line_start = line_start - if line == start_line { 0 } else { 0 };
            let line_start = line_start; // already adjusted via total_removed
            let mut count = 0;
            for ch in self.text[line_start..].chars().take(4) {
                if ch == ' ' { count += 1; } else { break; }
            }
            if count == 0 {
                continue;
            }
            self.text.replace_range(line_start..line_start + count, "");
            total_removed += count;
            if line == start_line {
                first_line_removed = count;
            }
        }
        if total_removed > 0 {
            let new_start = sel_start.saturating_sub(first_line_removed);
            let new_end = sel_end.saturating_sub(total_removed);
            if self.anchor.is_some() {
                self.anchor = Some(new_start);
                self.cursor = new_end;
            } else {
                self.cursor = new_end;
            }
            self.dirty = true;
            self.status.clear();
            self.bump_text_version();
            self.record(EditKind::Other);
        }
    }

    fn undo(&mut self) {
        if self.history_idx == 0 { return; }
        self.history_idx -= 1;
        self.apply_history();
    }

    fn redo(&mut self) {
        if self.history_idx + 1 >= self.history.len() { return; }
        self.history_idx += 1;
        self.apply_history();
    }

    fn apply_history(&mut self) {
        let snap = self.history[self.history_idx].clone();
        self.text = snap.text;
        self.cursor = snap.cursor;
        self.anchor = snap.anchor;
        self.marked = None;
        self.dirty = true;
        self.goal_col = None;
        self.last_edit_kind = None;
        self.reset_blink();
        self.bump_text_version();
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.status = "copied".into();
        }
    }

    fn cut(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.replace_or_insert("", EditKind::Delete);
        }
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        if let Some(item) = cx.read_from_clipboard() {
            if let Some(text) = item.text() {
                self.replace_or_insert(&text, EditKind::Insert);
            }
        }
    }

    fn ensure_cursor_visible(&mut self) {
        let (line, col) = byte_to_line_col(&self.text, self.cursor);
        let cursor_y = line as f32 * LINE_H;
        let v_top = self.scroll_y;
        let v_bottom = self.scroll_y + self.viewport_h;
        if cursor_y < v_top {
            self.scroll_y = cursor_y;
        } else if cursor_y + LINE_H > v_bottom {
            self.scroll_y = (cursor_y + LINE_H - self.viewport_h).max(0.);
        }
        let cursor_x = col as f32 * CHAR_W;
        let pad = CHAR_W * 4.;
        let usable_w = (self.viewport_w - LEFT_PAD - 24.).max(0.);
        let v_left = self.scroll_x;
        let v_right = self.scroll_x + usable_w;
        if cursor_x < v_left + pad {
            self.scroll_x = (cursor_x - pad).max(0.);
        } else if cursor_x + CHAR_W > v_right - pad {
            self.scroll_x = (cursor_x + CHAR_W + pad - usable_w).max(0.);
        }
    }

    pub fn handle_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ks = &event.keystroke;
        let key = ks.key.as_str();
        let mods = &ks.modifiers;
        let cmd_or_ctrl = mods.platform || mods.control;
        let shift = mods.shift;

        if cmd_or_ctrl {
            match key {
                "s" => { self.save(cx); cx.notify(); return; }
                "z" if shift => { self.redo(); self.ensure_cursor_visible(); cx.notify(); return; }
                "z" => { self.undo(); self.ensure_cursor_visible(); cx.notify(); return; }
                "y" => { self.redo(); self.ensure_cursor_visible(); cx.notify(); return; }
                "a" => { self.select_all(); cx.notify(); return; }
                "c" => { self.copy(cx); cx.notify(); return; }
                "x" => { self.cut(cx); cx.notify(); return; }
                "v" => { self.paste(cx); self.ensure_cursor_visible(); cx.notify(); return; }
                _ => {}
            }
        }
        match key {
            "left" => self.move_to_left(shift),
            "right" => self.move_to_right(shift),
            "up" => self.move_vertical(-1, shift),
            "down" => self.move_vertical(1, shift),
            "home" => self.move_home(shift),
            "end" => self.move_end(shift),
            "pageup" => self.move_vertical(-((self.viewport_h / LINE_H) as isize).max(1), shift),
            "pagedown" => self.move_vertical((self.viewport_h / LINE_H) as isize, shift),
            "backspace" => self.delete_back(),
            "delete" => self.delete_forward(),
            "enter" => self.newline_with_indent(),
            "tab" if shift => self.dedent(),
            "tab" => self.indent_or_tab(),
            // Plain text input is handled by EntityInputHandler::replace_text_in_range
            // via window.handle_input. Don't also insert here or every keystroke
            // doubles.
            _ => {}
        }
        self.ensure_cursor_visible();
        cx.notify();
    }

    fn click_to_byte(&self, point: Point<Pixels>, body_origin: Point<Pixels>) -> usize {
        // body_origin is the overlay bounds.origin (= body div's content origin).
        // Convert window coords to text coords by undoing TOP_PAD/LEFT_PAD plus scroll.
        let local_x = (point.x - body_origin.x).as_f32() - LEFT_PAD + self.scroll_x;
        let local_y = (point.y - body_origin.y).as_f32() - TOP_PAD + self.scroll_y;
        let line = (local_y.max(0.) / LINE_H) as usize;
        let col = (local_x.max(0.) / CHAR_W).round() as usize;
        line_col_to_byte(&self.text, line, col)
    }

    fn max_scroll_x(&self) -> f32 {
        let longest = self.longest_line_chars() as f32 * CHAR_W;
        let usable = (self.viewport_w - LEFT_PAD - 24.).max(0.);
        (longest + CHAR_W * 4. - usable).max(0.)
    }

    fn max_scroll_y(&self) -> f32 {
        (self.line_count() as f32 * LINE_H + TOP_PAD * 2. - self.viewport_h).max(0.)
    }

    pub fn on_body_mouse_down(
        &mut self,
        ev: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(bounds) = self.body_bounds else { return };
        let new_cursor = self.click_to_byte(ev.position, bounds.origin);
        if ev.modifiers.shift {
            self.extend_to(new_cursor);
        } else {
            self.collapse_to(new_cursor);
        }
        self.is_dragging = true;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub fn on_body_mouse_move(
        &mut self,
        ev: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_dragging { return; }
        let Some(bounds) = self.body_bounds else { return };
        let new_cursor = self.click_to_byte(ev.position, bounds.origin);
        self.extend_to(new_cursor);
        cx.notify();
    }

    pub fn on_body_mouse_up(
        &mut self,
        _ev: &MouseUpEvent,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.is_dragging = false;
    }
}

// ---- IME / input handler ----

impl EntityInputHandler for CodeEditor {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.text[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let range = self.selection_range().unwrap_or(self.cursor..self.cursor);
        let reversed = self
            .anchor
            .map(|a| a > self.cursor)
            .unwrap_or(false);
        Some(UTF16Selection {
            range: self.range_to_utf16(&range),
            reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked.as_ref().map(|r| self.range_to_utf16(r))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .map(|r| self.range_from_utf16(&r))
            .or_else(|| self.marked.clone())
            .or_else(|| self.selection_range())
            .unwrap_or(self.cursor..self.cursor);
        self.text.replace_range(range.clone(), new_text);
        self.cursor = range.start + new_text.len();
        self.anchor = None;
        self.marked = None;
        self.dirty = true;
        self.goal_col = None;
        self.reset_blink();
        self.bump_text_version();
        self.record(EditKind::Insert);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .map(|r| self.range_from_utf16(&r))
            .or_else(|| self.marked.clone())
            .or_else(|| self.selection_range())
            .unwrap_or(self.cursor..self.cursor);
        self.text.replace_range(range.clone(), new_text);
        if !new_text.is_empty() {
            self.marked = Some(range.start..range.start + new_text.len());
        } else {
            self.marked = None;
        }
        self.cursor = match new_selected_range_utf16
            .map(|r| self.range_from_utf16(&r))
        {
            Some(r) => range.start + r.end,
            None => range.start + new_text.len(),
        };
        self.anchor = None;
        self.goal_col = None;
        self.dirty = true;
        self.reset_blink();
        self.bump_text_version();
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        let (line, col) = byte_to_line_col(&self.text, range.start);
        let x = bounds.origin.x + px(LEFT_PAD - self.scroll_x + col as f32 * CHAR_W);
        let y = bounds.origin.y + px(TOP_PAD - self.scroll_y + line as f32 * LINE_H);
        Some(Bounds {
            origin: point(x, y),
            size: gpui::size(px(CHAR_W), px(LINE_H)),
        })
    }

    fn character_index_for_point(
        &mut self,
        p: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.body_bounds?;
        let local_x = (p.x - bounds.origin.x).as_f32() - LEFT_PAD + self.scroll_x;
        let local_y = (p.y - bounds.origin.y).as_f32() - TOP_PAD + self.scroll_y;
        if local_x < 0. || local_y < 0. { return None; }
        let line = (local_y / LINE_H) as usize;
        let col = (local_x / CHAR_W).round() as usize;
        let byte = line_col_to_byte(&self.text, line, col);
        Some(self.offset_to_utf16(byte))
    }
}

impl CodeEditor {
    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.text.chars() {
            if utf16_count >= offset { break; }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }
    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.text.chars() {
            if utf8_count >= offset { break; }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }
    fn range_to_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(r.start)..self.offset_to_utf16(r.end)
    }
    fn range_from_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(r.start)..self.offset_from_utf16(r.end)
    }
}

impl Focusable for CodeEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle { self.focus.clone() }
}

impl EventEmitter<CodeEditorEvent> for CodeEditor {}

// ---- byte / line-col helpers ----

fn byte_to_line_col(text: &str, byte: usize) -> (usize, usize) {
    let byte = byte.min(text.len());
    let prefix = &text[..byte];
    let line = prefix.matches('\n').count();
    let line_start = prefix.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let col = text[line_start..byte].chars().count();
    (line, col)
}

fn line_col_to_byte(text: &str, target_line: usize, col: usize) -> usize {
    let mut current_line = 0;
    let mut line_start = 0;
    for (i, ch) in text.char_indices() {
        if current_line == target_line { break; }
        if ch == '\n' {
            current_line += 1;
            line_start = i + ch.len_utf8();
        }
    }
    if current_line < target_line { return text.len(); }
    let mut byte = line_start;
    let mut count = 0;
    for (i, ch) in text[line_start..].char_indices() {
        if ch == '\n' { return line_start + i; }
        if count >= col { return line_start + i; }
        count += 1;
        byte = line_start + i + ch.len_utf8();
    }
    byte
}

fn prev_grapheme(text: &str, byte: usize) -> usize {
    if byte == 0 { return 0; }
    text[..byte].char_indices().last().map(|(i, _)| i).unwrap_or(0)
}

fn next_grapheme(text: &str, byte: usize) -> usize {
    if byte >= text.len() { return text.len(); }
    text[byte..]
        .char_indices()
        .nth(1)
        .map(|(i, _)| byte + i)
        .unwrap_or(text.len())
}

// ---- per-line precomputed runs (cached, invalidated by text_version) ----

#[derive(Clone)]
struct LineRuns {
    text: SharedString,
    runs: Vec<TextRun>,
    /// Byte offset of this line's start in the full text.
    start: usize,
    /// Byte length of the line excluding the trailing newline.
    len: usize,
}

fn split_runs_per_line(text: &str, full_runs: &[TextRun]) -> Vec<LineRuns> {
    let mut out = Vec::new();
    let mut run_idx = 0;
    let mut run_consumed = 0usize;
    let mut byte = 0usize;
    for line in text.split('\n') {
        let line_len = line.len();
        let mut line_runs: Vec<TextRun> = Vec::new();
        let mut taken = 0usize;
        while taken < line_len && run_idx < full_runs.len() {
            let run = &full_runs[run_idx];
            let avail = run.len - run_consumed;
            let need = line_len - taken;
            let take = avail.min(need);
            if take > 0 {
                line_runs.push(TextRun {
                    len: take,
                    font: run.font.clone(),
                    color: run.color,
                    background_color: run.background_color,
                    underline: run.underline,
                    strikethrough: run.strikethrough,
                });
            }
            taken += take;
            run_consumed += take;
            if run_consumed == run.len {
                run_idx += 1;
                run_consumed = 0;
            }
        }
        out.push(LineRuns {
            text: SharedString::from(line.to_string()),
            runs: line_runs,
            start: byte,
            len: line_len,
        });
        // Advance past newline (one byte) in both text and runs.
        let consumed_with_newline = if byte + line_len < text.len() {
            let mut nl = 1usize;
            while nl > 0 && run_idx < full_runs.len() {
                let run = &full_runs[run_idx];
                let avail = run.len - run_consumed;
                let take = avail.min(nl);
                run_consumed += take;
                nl -= take;
                if run_consumed == run.len {
                    run_idx += 1;
                    run_consumed = 0;
                }
            }
            line_len + 1
        } else {
            line_len
        };
        byte += consumed_with_newline;
    }
    out
}

// ---- the editor element: paints visible lines + selection + cursor, registers IME ----

pub struct EditorElement {
    pub entity: gpui::Entity<CodeEditor>,
}

pub struct PrepaintState {
    bounds: Bounds<Pixels>,
    body_bounds: Bounds<Pixels>,
    gutter_w: f32,
    visible_start: usize,
    body_lines: Vec<ShapedLine>,
    gutter_lines: Vec<ShapedLine>,
    text_snapshot: String,
    cursor: usize,
    anchor: Option<usize>,
    marked: Option<Range<usize>>,
    scroll_x: f32,
    scroll_y: f32,
    focused: bool,
    blink_visible: bool,
    prepaint_ms: f64,
}

impl IntoElement for EditorElement {
    type Element = Self;
    fn into_element(self) -> Self::Element { self }
}

impl Element for EditorElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<gpui::ElementId> { None }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> { None }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _ins: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _ins: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> PrepaintState {
        let prepaint_start = std::time::Instant::now();
        let font_size = px(FONT_SIZE);
        let (
            line_runs_all,
            text_snapshot,
            cursor,
            anchor,
            marked,
            scroll_x,
            scroll_y,
            focused,
            blink_visible,
            line_count,
            gutter_w,
        ) = self.entity.update(cx, |this, _| {
            let line_count = this.line_count();
            let gutter_w = (line_count.to_string().len() as f32 * 8.0).max(36.);
            this.body_bounds = Some(Bounds {
                origin: point(bounds.origin.x + px(gutter_w), bounds.origin.y),
                size: gpui::size(
                    (bounds.size.width - px(gutter_w)).max(px(0.)),
                    bounds.size.height,
                ),
            });
            this.viewport_h = bounds.size.height.as_f32();
            this.viewport_w = (bounds.size.width.as_f32() - gutter_w).max(0.);
            (
                this.line_runs(),
                this.text.clone(),
                this.cursor,
                this.anchor,
                this.marked.clone(),
                this.scroll_x,
                this.scroll_y,
                this.focus.is_focused(window),
                this.blink_visible,
                line_count,
                gutter_w,
            )
        });

        // Determine visible line range.
        let viewport_h = bounds.size.height.as_f32();
        let first = ((scroll_y - TOP_PAD) / LINE_H).floor().max(0.) as usize;
        let last_inclusive = ((scroll_y + viewport_h) / LINE_H).ceil() as usize;
        let visible_end = last_inclusive.min(line_count);
        let visible_start = first.min(visible_end);

        // Shape only visible body lines.
        let text_system = window.text_system();
        let mut body_lines: Vec<ShapedLine> = Vec::with_capacity(visible_end - visible_start);
        for i in visible_start..visible_end {
            if let Some(line) = line_runs_all.get(i) {
                let shaped = text_system.shape_line(
                    line.text.clone(),
                    font_size,
                    &line.runs,
                    None,
                );
                body_lines.push(shaped);
            }
        }

        // Shape gutter line numbers for visible range.
        let gutter_font = font(FONT_FAMILY);
        let gutter_color = theme::text_dim().into();
        let mut gutter_lines: Vec<ShapedLine> = Vec::with_capacity(body_lines.len());
        for i in visible_start..visible_end {
            let s: SharedString = format!("{}", i + 1).into();
            let len = s.len();
            let run = TextRun {
                len,
                font: gutter_font.clone(),
                color: gutter_color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let shaped = text_system.shape_line(s, font_size, &[run], None);
            gutter_lines.push(shaped);
        }

        let body_bounds = Bounds {
            origin: point(bounds.origin.x + px(gutter_w), bounds.origin.y),
            size: gpui::size(
                (bounds.size.width - px(gutter_w)).max(px(0.)),
                bounds.size.height,
            ),
        };

        let prepaint_ms = prepaint_start.elapsed().as_secs_f64() * 1000.;
        let shaped_count = body_lines.len();
        if std::env::var("PROTOCOL_TIMING").is_ok() {
            eprintln!(
                "editor prepaint: {:.2}ms ({} body lines, {} total lines, viewport {:.0}x{:.0})",
                prepaint_ms, shaped_count, line_count, bounds.size.width.as_f32(), bounds.size.height.as_f32(),
            );
        }
        PrepaintState {
            bounds,
            body_bounds,
            gutter_w,
            visible_start,
            body_lines,
            gutter_lines,
            text_snapshot,
            cursor,
            anchor,
            marked,
            scroll_x,
            scroll_y,
            focused,
            blink_visible,
            prepaint_ms,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _ins: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let paint_start = std::time::Instant::now();
        // No bg fill here — the parent card paints its own bg, which is the same
        // color and (unlike a square paint_quad) respects the rounded card mask.

        let line_h = px(LINE_H);
        let body = prepaint.body_bounds;
        let body_left = body.origin.x.as_f32();
        let body_top = body.origin.y.as_f32();
        let scroll_x = prepaint.scroll_x;
        let scroll_y = prepaint.scroll_y;

        // Body region paints clipped so horizontally-scrolled text never bleeds into
        // the gutter on the left or past the right edge.
        window.with_content_mask(Some(ContentMask { bounds: body }), |window| {
            // selection
            let sel = match prepaint.anchor {
                Some(a) if a != prepaint.cursor => Some(if a < prepaint.cursor {
                    a..prepaint.cursor
                } else {
                    prepaint.cursor..a
                }),
                _ => None,
            };
            if let Some(range) = sel.clone() {
                paint_selection(
                    window,
                    &prepaint.body_lines,
                    prepaint.visible_start,
                    &prepaint.text_snapshot,
                    &range,
                    body_left + LEFT_PAD - scroll_x,
                    body_top + TOP_PAD - scroll_y,
                );
            }

            // body text lines
            for (i, shaped) in prepaint.body_lines.iter().enumerate() {
                let line_idx = prepaint.visible_start + i;
                let y = body_top + TOP_PAD - scroll_y + line_idx as f32 * LINE_H;
                let x = body_left + LEFT_PAD - scroll_x;
                let _ = shaped.paint(
                    point(px(x), px(y)),
                    line_h,
                    gpui::TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
        });

        // ---- gutter line numbers (drawn AFTER body, with their own bg fill so any
        //      stray bleed from the body's neg-scroll-x is covered) ----
        let gutter_left = prepaint.bounds.origin.x.as_f32();
        // Solid gutter bg so text scrolled past the left edge never shows through.
        window.paint_quad(fill(
            Bounds::from_corners(
                point(prepaint.bounds.origin.x, prepaint.bounds.origin.y),
                point(
                    px(gutter_left + prepaint.gutter_w),
                    prepaint.bounds.origin.y + prepaint.bounds.size.height,
                ),
            ),
            highlight::theme_bg(),
        ));
        for (i, shaped) in prepaint.gutter_lines.iter().enumerate() {
            let line_idx = prepaint.visible_start + i;
            let y = body_top + TOP_PAD - scroll_y + line_idx as f32 * LINE_H;
            let line_w = shaped.width.as_f32();
            let x = gutter_left + prepaint.gutter_w - 6. - line_w;
            let _ = shaped.paint(
                point(px(x), px(y)),
                line_h,
                gpui::TextAlign::Left,
                None,
                window,
                cx,
            );
        }

        // ---- IME marked underline ----
        if let Some(range) = prepaint.marked.clone() {
            let (line, _col) = byte_to_line_col(&prepaint.text_snapshot, range.start);
            if line >= prepaint.visible_start && line < prepaint.visible_start + prepaint.body_lines.len() {
                let shaped = &prepaint.body_lines[line - prepaint.visible_start];
                let line_start = line_col_to_byte(&prepaint.text_snapshot, line, 0);
                let local_start = range.start.saturating_sub(line_start);
                let local_end = range.end.saturating_sub(line_start).min(shaped.text.len());
                let x0 = body_left + LEFT_PAD - scroll_x + shaped.x_for_index(local_start).as_f32();
                let x1 = body_left + LEFT_PAD - scroll_x + shaped.x_for_index(local_end).as_f32();
                let y = body_top + TOP_PAD - scroll_y + line as f32 * LINE_H;
                window.paint_quad(fill(
                    Bounds::from_corners(
                        point(px(x0), px(y + LINE_H - 2.)),
                        point(px(x1), px(y + LINE_H)),
                    ),
                    hsla(45. / 360., 1., 0.7, 1.),
                ));
            }
        }

        // ---- cursor caret ----
        if prepaint.focused && prepaint.blink_visible {
            let (line, _col) = byte_to_line_col(&prepaint.text_snapshot, prepaint.cursor);
            let local = prepaint
                .body_lines
                .get(line.wrapping_sub(prepaint.visible_start))
                .map(|shaped| {
                    let line_start = line_col_to_byte(&prepaint.text_snapshot, line, 0);
                    let off = prepaint.cursor.saturating_sub(line_start).min(shaped.text.len());
                    shaped.x_for_index(off).as_f32()
                })
                .unwrap_or(0.);
            let x = body_left + LEFT_PAD - scroll_x + local;
            let y = body_top + TOP_PAD - scroll_y + line as f32 * LINE_H;
            window.paint_quad(fill(
                Bounds::from_corners(
                    point(px(x), px(y)),
                    point(px(x + 2.), px(y + LINE_H)),
                ),
                theme::accent(),
            ));
        }

        // Register IME input handler over the body region.
        let focus = self.entity.update(cx, |this, _| this.focus.clone());
        window.handle_input(
            &focus,
            ElementInputHandler::new(prepaint.body_bounds, self.entity.clone()),
            cx,
        );

        let paint_ms = paint_start.elapsed().as_secs_f64() * 1000.;
        if std::env::var("PROTOCOL_TIMING").is_ok() {
            eprintln!(
                "editor paint:    {:.2}ms (prepaint was {:.2}ms, total {:.2}ms)",
                paint_ms,
                prepaint.prepaint_ms,
                paint_ms + prepaint.prepaint_ms,
            );
        }
    }
}

fn paint_selection(
    window: &mut Window,
    body_lines: &[ShapedLine],
    visible_start: usize,
    text: &str,
    range: &Range<usize>,
    origin_x: f32,
    origin_y: f32,
) {
    let (start_line, _) = byte_to_line_col(text, range.start);
    let (end_line, _) = byte_to_line_col(text, range.end);
    let color = hsla(220. / 360., 0.5, 0.55, 0.30);
    for line in start_line..=end_line {
        let line_start = line_col_to_byte(text, line, 0);
        let line_end_byte = if line == text.matches('\n').count() {
            text.len()
        } else {
            line_col_to_byte(text, line, usize::MAX)
        };
        let local_start_byte = if line == start_line {
            range.start.saturating_sub(line_start)
        } else {
            0
        };
        let local_end_byte = if line == end_line {
            range.end.saturating_sub(line_start)
        } else {
            line_end_byte - line_start + 1 // +1 to bleed into the newline area
        };

        // Find shaped line if visible to use its actual x positions.
        let shaped = body_lines.get(line.wrapping_sub(visible_start));
        let (x0, x1) = if let Some(shaped) = shaped {
            let safe_end = local_end_byte.min(shaped.text.len());
            let safe_start = local_start_byte.min(safe_end);
            let mut a = shaped.x_for_index(safe_start).as_f32();
            let mut b = shaped.x_for_index(safe_end).as_f32();
            // Selection spilling past EOL gets a small visible tail.
            if local_end_byte > shaped.text.len() {
                b += FONT_SIZE * 0.5;
            }
            if a > b { std::mem::swap(&mut a, &mut b); }
            (a, b)
        } else {
            // Off-screen — skip painting (it's clipped anyway).
            continue;
        };

        let y = origin_y + line as f32 * LINE_H;
        window.paint_quad(fill(
            Bounds::from_corners(
                point(px(origin_x + x0), px(y)),
                point(px(origin_x + x1), px(y + LINE_H)),
            ),
            color,
        ));
    }
}

// ---- Render impl ----

impl Render for CodeEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        div()
            .id("code-editor")
            .track_focus(&self.focus)
            .key_context("CodeEditor")
            .on_key_down(cx.listener(Self::handle_key))
            .flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .on_scroll_wheel(cx.listener(move |this, ev: &gpui::ScrollWheelEvent, _, cx| {
                let (dx, dy) = match ev.delta {
                    gpui::ScrollDelta::Pixels(p) => (p.x.as_f32(), p.y.as_f32()),
                    gpui::ScrollDelta::Lines(p) => (p.x * CHAR_W, p.y * LINE_H),
                };
                let max_y = this.max_scroll_y();
                let max_x = this.max_scroll_x();
                this.scroll_y = (this.scroll_y - dy).clamp(0., max_y);
                this.scroll_x = (this.scroll_x - dx).clamp(0., max_x);
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_body_mouse_down))
            .on_mouse_move(cx.listener(Self::on_body_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_body_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_body_mouse_up))
            .child(EditorElement { entity })
    }
}
