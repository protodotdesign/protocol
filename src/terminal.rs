use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event as AlacEvent, EventListener, Notify, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, Msg};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Point as AlacPoint;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Config;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor};
use alacritty_terminal::tty::{self, Options as PtyOptions, Shell};

use anyhow::Result;
use futures::StreamExt;
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, Hsla, IntoElement, KeyDownEvent,
    MouseButton, Rgba, SharedString, StyledText, TextRun, Window, div, font, hsla, prelude::*, px,
    rgb,
};

const DEFAULT_LINES: usize = 24;
const DEFAULT_COLS: usize = 80;
const SCROLLBACK: usize = 5_000;

static NEXT_TERMINAL_ID: AtomicUsize = AtomicUsize::new(1);

#[derive(Clone, Copy, Debug)]
pub struct GridSize {
    pub lines: usize,
    pub cols: usize,
}

impl Default for GridSize {
    fn default() -> Self {
        Self { lines: DEFAULT_LINES, cols: DEFAULT_COLS }
    }
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize { self.lines + SCROLLBACK }
    fn screen_lines(&self) -> usize { self.lines }
    fn columns(&self) -> usize { self.cols }
}

#[derive(Clone)]
struct EventProxy {
    sender: futures::channel::mpsc::UnboundedSender<AlacEvent>,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: AlacEvent) {
        let _ = self.sender.unbounded_send(event);
    }
}

pub struct Terminal {
    pub id: usize,
    pub title: String,
    pub cwd: PathBuf,
    term: Arc<FairMutex<Term<EventProxy>>>,
    notifier: Box<dyn Fn(Vec<u8>) + Send + Sync>,
    resizer: Box<dyn Fn(WindowSize) + Send + Sync>,
    size: GridSize,
    pub focus: FocusHandle,
    pub last_cell_w: f32,
    pub last_cell_h: f32,
}

impl Terminal {
    pub fn create(cwd: PathBuf, cx: &mut App) -> Result<Entity<Terminal>> {
        let id = NEXT_TERMINAL_ID.fetch_add(1, Ordering::Relaxed);
        let size = GridSize::default();
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<AlacEvent>();
        let proxy = EventProxy { sender: tx };

        let term_inner = Term::new(Config::default(), &size, proxy.clone());
        let term = Arc::new(FairMutex::new(term_inner));

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let mut env: HashMap<String, String> = std::env::vars().collect();
        env.insert("TERM".into(), "xterm-256color".into());
        env.insert("COLORTERM".into(), "truecolor".into());

        let opts = PtyOptions {
            shell: Some(Shell::new(shell, vec![])),
            working_directory: Some(cwd.clone()),
            drain_on_exit: true,
            env,
            ..Default::default()
        };
        let win_size = WindowSize {
            num_lines: size.lines as u16,
            num_cols: size.cols as u16,
            cell_width: 7,
            cell_height: 16,
        };
        let pty = tty::new(&opts, win_size, id as u64)?;

        let event_loop = EventLoop::new(term.clone(), proxy, pty, false, false)?;
        let sender = event_loop.channel();
        let _io_thread = event_loop.spawn();

        let sender_for_input = sender.clone();
        let notifier: Box<dyn Fn(Vec<u8>) + Send + Sync> = Box::new(move |bytes: Vec<u8>| {
            let _ = sender_for_input.send(Msg::Input(bytes.into()));
        });
        let sender_for_resize = sender;
        let resizer: Box<dyn Fn(WindowSize) + Send + Sync> = Box::new(move |ws: WindowSize| {
            let _ = sender_for_resize.send(Msg::Resize(ws));
        });

        let entity = cx.new(|entity_cx| {
            entity_cx
                .spawn(async move |this, cx| {
                    while let Some(event) = rx.next().await {
                        let title_update = match event {
                            AlacEvent::Title(t) => Some(t),
                            AlacEvent::ResetTitle => Some(String::new()),
                            _ => None,
                        };
                        if this
                            .update(cx, |this: &mut Terminal, cx| {
                                if let Some(t) = title_update {
                                    this.title = t;
                                }
                                cx.notify();
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                })
                .detach();

            Terminal {
                id,
                title: String::new(),
                cwd,
                term,
                notifier,
                resizer,
                size,
                focus: entity_cx.focus_handle(),
                last_cell_w: 7.51,
                last_cell_h: 16.,
            }
        });
        Ok(entity)
    }

    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        (self.notifier)(bytes.into());
    }

    pub fn resize(&mut self, lines: usize, cols: usize, cell_w: u16, cell_h: u16) {
        let lines = lines.max(1);
        let cols = cols.max(1);
        if self.size.lines == lines && self.size.cols == cols {
            return;
        }
        self.size = GridSize { lines, cols };
        // Resize alacritty term grid.
        self.term.lock().resize(self.size);
        (self.resizer)(WindowSize {
            num_lines: lines as u16,
            num_cols: cols as u16,
            cell_width: cell_w,
            cell_height: cell_h,
        });
    }

    pub fn size(&self) -> GridSize { self.size }

    /// Take a snapshot of the visible viewport for rendering.
    pub fn snapshot(&self) -> Snapshot {
        let term = self.term.lock();
        let grid = term.grid();
        let display_offset = grid.display_offset();
        let mut lines: Vec<Vec<RenderCell>> = Vec::with_capacity(self.size.lines);
        for row_idx in 0..self.size.lines {
            // Lines in alacritty are signed: 0 is topmost visible. With scrollback,
            // grid[Line(row)] indexes the visible viewport when display_offset is 0.
            let line_idx = alacritty_terminal::index::Line(row_idx as i32);
            let mut row = Vec::with_capacity(self.size.cols);
            for col in 0..self.size.cols {
                let cell = &grid[line_idx][alacritty_terminal::index::Column(col)];
                row.push(RenderCell::from(cell));
            }
            lines.push(row);
        }
        let cursor_point = term.grid().cursor.point;
        let cursor_visible = term.mode().contains(alacritty_terminal::term::TermMode::SHOW_CURSOR);
        Snapshot {
            lines,
            cursor: CursorPos {
                line: cursor_point.line.0,
                col: cursor_point.column.0,
                visible: cursor_visible,
            },
            display_offset,
        }
    }
}

pub struct Snapshot {
    pub lines: Vec<Vec<RenderCell>>,
    pub cursor: CursorPos,
    pub display_offset: usize,
}

pub struct CursorPos {
    pub line: i32,
    pub col: usize,
    pub visible: bool,
}

#[derive(Clone, Copy)]
pub struct RenderCell {
    pub ch: char,
    pub fg: Hsla,
    pub bg: Hsla,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

impl From<&Cell> for RenderCell {
    fn from(c: &Cell) -> Self {
        let flags = c.flags;
        RenderCell {
            ch: c.c,
            fg: ansi_to_hsla(c.fg, true),
            bg: ansi_to_hsla(c.bg, false),
            bold: flags.contains(Flags::BOLD),
            italic: flags.contains(Flags::ITALIC),
            underline: flags.contains(Flags::UNDERLINE),
        }
    }
}

fn rgba_to_hsla(r: Rgba) -> Hsla { r.into() }

fn ansi_to_hsla(color: AnsiColor, is_fg: bool) -> Hsla {
    match color {
        AnsiColor::Named(n) => named_color(n, is_fg),
        AnsiColor::Spec(rgb_color) => {
            let r = rgb_color.r as f32 / 255.;
            let g = rgb_color.g as f32 / 255.;
            let b = rgb_color.b as f32 / 255.;
            rgba_to_hsla(Rgba { r, g, b, a: 1. })
        }
        AnsiColor::Indexed(i) => indexed_color(i),
    }
}

fn named_color(c: NamedColor, is_fg: bool) -> Hsla {
    use NamedColor::*;
    match c {
        Foreground => rgba_to_hsla(rgb(0xc8ccd4)),
        Background => hsla(0., 0., 0., 0.),
        Cursor => rgba_to_hsla(rgb(0xffffff)),
        Black => rgba_to_hsla(rgb(0x000000)),
        Red => rgba_to_hsla(rgb(0xe06c75)),
        Green => rgba_to_hsla(rgb(0x98c379)),
        Yellow => rgba_to_hsla(rgb(0xe5c07b)),
        Blue => rgba_to_hsla(rgb(0x61afef)),
        Magenta => rgba_to_hsla(rgb(0xc678dd)),
        Cyan => rgba_to_hsla(rgb(0x56b6c2)),
        White => rgba_to_hsla(rgb(0xc8ccd4)),
        BrightBlack => rgba_to_hsla(rgb(0x5c6370)),
        BrightRed => rgba_to_hsla(rgb(0xff7b85)),
        BrightGreen => rgba_to_hsla(rgb(0xa6d189)),
        BrightYellow => rgba_to_hsla(rgb(0xf5d08b)),
        BrightBlue => rgba_to_hsla(rgb(0x71bfff)),
        BrightMagenta => rgba_to_hsla(rgb(0xd688ed)),
        BrightCyan => rgba_to_hsla(rgb(0x66c6d2)),
        BrightWhite => rgba_to_hsla(rgb(0xffffff)),
        BrightForeground | DimForeground => rgba_to_hsla(rgb(0xffffff)),
        DimBlack => rgba_to_hsla(rgb(0x000000)),
        DimRed => rgba_to_hsla(rgb(0xb04047)),
        DimGreen => rgba_to_hsla(rgb(0x749f5b)),
        DimYellow => rgba_to_hsla(rgb(0xb39c5d)),
        DimBlue => rgba_to_hsla(rgb(0x4a87ba)),
        DimMagenta => rgba_to_hsla(rgb(0x9c5fae)),
        DimCyan => rgba_to_hsla(rgb(0x428791)),
        DimWhite => rgba_to_hsla(rgb(0x9aa0a8)),
    }.alpha_for(is_fg)
}

trait AlphaFor {
    fn alpha_for(self, is_fg: bool) -> Hsla;
}
impl AlphaFor for Hsla {
    fn alpha_for(self, is_fg: bool) -> Hsla {
        // Background cells with alpha=0 inherit terminal bg in renderer.
        if !is_fg && self.a == 0. { self } else { self }
    }
}

fn indexed_color(i: u8) -> Hsla {
    if i < 16 {
        let named = match i {
            0 => NamedColor::Black,
            1 => NamedColor::Red,
            2 => NamedColor::Green,
            3 => NamedColor::Yellow,
            4 => NamedColor::Blue,
            5 => NamedColor::Magenta,
            6 => NamedColor::Cyan,
            7 => NamedColor::White,
            8 => NamedColor::BrightBlack,
            9 => NamedColor::BrightRed,
            10 => NamedColor::BrightGreen,
            11 => NamedColor::BrightYellow,
            12 => NamedColor::BrightBlue,
            13 => NamedColor::BrightMagenta,
            14 => NamedColor::BrightCyan,
            _ => NamedColor::BrightWhite,
        };
        return named_color(named, true);
    }
    if i >= 232 {
        // 24-step grayscale.
        let v = (i - 232) as f32 * 10.0 + 8.0;
        let v = v / 255.0;
        return rgba_to_hsla(Rgba { r: v, g: v, b: v, a: 1. });
    }
    // 6x6x6 color cube.
    let i = i - 16;
    let r = ((i / 36) % 6) as f32;
    let g = ((i / 6) % 6) as f32;
    let b = (i % 6) as f32;
    let to_chan = |v: f32| if v == 0. { 0. } else { (40. * v + 55.) / 255. };
    rgba_to_hsla(Rgba {
        r: to_chan(r),
        g: to_chan(g),
        b: to_chan(b),
        a: 1.,
    })
}

#[allow(dead_code)]
pub fn _suppress_unused_warning(_p: AlacPoint) {}

impl Focusable for Terminal {
    fn focus_handle(&self, _: &App) -> FocusHandle { self.focus.clone() }
}

const TERM_FONT_FAMILY: &str = "Menlo";
const TERM_FONT_SIZE: f32 = 12.5;
const TERM_LINE_H: f32 = 16.;
const TERM_CHAR_W: f32 = 7.51;

impl Terminal {
    pub fn handle_key(&mut self, ev: &KeyDownEvent, _w: &mut Window, _cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let key = ks.key.as_str();
        let mods = &ks.modifiers;
        let bytes: Option<Vec<u8>> = match key {
            "enter" => Some(b"\r".to_vec()),
            "backspace" => Some(b"\x7f".to_vec()),
            "tab" => Some(b"\t".to_vec()),
            "escape" => Some(b"\x1b".to_vec()),
            "left" => Some(b"\x1b[D".to_vec()),
            "right" => Some(b"\x1b[C".to_vec()),
            "up" => Some(b"\x1b[A".to_vec()),
            "down" => Some(b"\x1b[B".to_vec()),
            "home" => Some(b"\x1b[H".to_vec()),
            "end" => Some(b"\x1b[F".to_vec()),
            "delete" => Some(b"\x1b[3~".to_vec()),
            "pageup" => Some(b"\x1b[5~".to_vec()),
            "pagedown" => Some(b"\x1b[6~".to_vec()),
            _ => {
                if mods.control && key.len() == 1 {
                    let c = key.chars().next().unwrap();
                    if c.is_ascii_alphabetic() {
                        let byte = (c.to_ascii_lowercase() as u8) - b'a' + 1;
                        Some(vec![byte])
                    } else if c == ' ' {
                        Some(vec![0])
                    } else {
                        None
                    }
                } else if let Some(c) = ks.key_char.as_deref() {
                    if !c.is_empty() {
                        Some(c.as_bytes().to_vec())
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
        };
        if let Some(b) = bytes {
            self.write(b);
        }
    }
}

impl Render for Terminal {
    fn render(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = self.snapshot();
        let cell_w = self.last_cell_w;
        let cell_h = self.last_cell_h;
        let cursor_visible = snapshot.cursor.visible;
        let cursor_line = snapshot.cursor.line;
        let cursor_col = snapshot.cursor.col;
        let id = self.id;

        let lines = build_lines(&snapshot.lines);

        let body = div()
            .id(("term-body", id))
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .bg(rgb(0x12141a))
            .px_2()
            .py_1()
            .font_family(TERM_FONT_FAMILY)
            .text_size(px(TERM_FONT_SIZE))
            .line_height(px(cell_h))
            .text_color(rgb(0xc8ccd4))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::handle_key))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.focus, cx);
                    cx.notify();
                }),
            );

        let mut col = div().flex().flex_col();
        for (line_idx, line) in lines.into_iter().enumerate() {
            let cell_h_px = px(cell_h);
            // Row of background quads + foreground text overlay.
            let LineRender { background_runs, runs, text } = line;
            let mut row = div()
                .relative()
                .h(cell_h_px)
                .min_w_0();
            for bg in background_runs {
                row = row.child(
                    div()
                        .absolute()
                        .top_0()
                        .left(px(bg.x))
                        .w(px(bg.w))
                        .h(cell_h_px)
                        .bg(bg.color),
                );
            }
            row = row.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .h(cell_h_px)
                    .child(StyledText::new(text).with_runs(runs)),
            );
            if cursor_visible && cursor_line == line_idx as i32 {
                row = row.child(
                    div()
                        .absolute()
                        .top_0()
                        .left(px(cursor_col as f32 * cell_w))
                        .w(px(cell_w))
                        .h(cell_h_px)
                        .bg(hsla(45. / 360., 1., 0.7, 0.5)),
                );
            }
            col = col.child(row);
        }

        body.child(col)
    }
}

struct LineRender {
    text: SharedString,
    runs: Vec<TextRun>,
    background_runs: Vec<BgRun>,
}

struct BgRun {
    x: f32,
    w: f32,
    color: Hsla,
}

fn build_lines(grid: &[Vec<RenderCell>]) -> Vec<LineRender> {
    let mut out = Vec::with_capacity(grid.len());
    let code_font = font(TERM_FONT_FAMILY);
    for cells in grid {
        let mut text = String::with_capacity(cells.len());
        let mut runs: Vec<TextRun> = Vec::new();
        let mut bgs: Vec<BgRun> = Vec::new();
        let mut cur_run: Option<TextRun> = None;
        let mut cur_bg: Option<(f32, f32, Hsla)> = None;
        for (i, cell) in cells.iter().enumerate() {
            let ch = if cell.ch == '\0' { ' ' } else { cell.ch };
            text.push(ch);
            let bytes = ch.len_utf8();
            // Foreground run.
            let fg = cell.fg;
            match &mut cur_run {
                Some(run) if run.color == fg => run.len += bytes,
                _ => {
                    if let Some(r) = cur_run.take() { runs.push(r); }
                    cur_run = Some(TextRun {
                        len: bytes,
                        font: code_font.clone(),
                        color: fg,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    });
                }
            }
            // Background run (skip if alpha is 0 = inherit).
            let visible_bg = cell.bg.a > 0.05;
            let bg_x = i as f32 * TERM_CHAR_W;
            match cur_bg {
                Some((start_x, end_x, color)) if visible_bg && color == cell.bg => {
                    cur_bg = Some((start_x, end_x + TERM_CHAR_W, color));
                }
                _ => {
                    if let Some((sx, ex, color)) = cur_bg.take() {
                        bgs.push(BgRun { x: sx, w: ex - sx, color });
                    }
                    if visible_bg {
                        cur_bg = Some((bg_x, bg_x + TERM_CHAR_W, cell.bg));
                    }
                }
            }
        }
        if let Some(r) = cur_run.take() { runs.push(r); }
        if let Some((sx, ex, color)) = cur_bg.take() {
            bgs.push(BgRun { x: sx, w: ex - sx, color });
        }
        out.push(LineRender {
            text: SharedString::from(text),
            runs,
            background_runs: bgs,
        });
    }
    out
}
