# Protocol — Architecture & Agent Guide

What this app is, how the code is organized, and the gotchas a future AI agent
needs to know before touching anything. Read GPUI_MANUAL.md first if you've
never used gpui — this doc assumes you know what `Entity`, `Render`, `Element`,
and `Context` are.

## What it is

A multi-repo workspace shell. Concepts:

- **Workspace** — a `~/.protocol/workspaces/<name>.toml` declaring N git repos.
- **Instance** — a directory containing fresh clones of every repo in a
  workspace. Lives at `~/.protocol/instances/<workspace>/<instance>/`. Each
  repo inside is a normal git checkout on its workspace-default branch.
- **Tab** — an open file inside the editor. Pool is shared across instance
  switches; tabs are filtered to the active instance.
- **Terminal** — an alacritty-backed PTY scoped to an instance. Multiple
  terminals per instance, kept alive across instance switches.

Everything below the workspace layer is just the filesystem. There is no
"instance metadata" file. State that has to persist (last selection, panel
sizes, sidebar collapse, expanded tree paths) lives in `~/.protocol/state.toml`.

## Layout

```
TitleBar  [◀ collapse-left]    Protocol › ws › inst              [▶ collapse-right]
─────────────────────────────────────────────────────────────────────────────────
| LEFT SIDEBAR  | ┌── center card (rounded, panel_bg surround) ──┐ | RIGHT SIDEBAR
|               | │ tab-bar  README.md ×                          │ |  FILES
| [ workspace ▾]│ ├──────────── editor (CodeEditor) ─────────────┤│ |    repo-a/
|   instance    │ │   1  # Repo B                                 ││ |     src/
|   instance    │ │   2  …                                        ││ |       lib.rs
|               │ ├─── resize-seam ─────────────────────────────-─┤│ |    repo-b/
|               │ │ tab-bar  term 1 ×          + ↧ dock bottom    ││ |
|               │ │   terminal grid                              ││ |
|               │ └───────────────────────────────────────────────┘│ |
─────────────────────────────────────────────────────────────────────────────────
StatusBar                                                              ready
```

- Both sidebars are independently resizable (drag the seam) and collapsible
  (title-bar toggles).
- Center is a single rounded card (`card_frame`) holding the editor + terminal.
  Terminal can dock `bottom` (flex_col inside the card) or `right` (flex_row
  inside the card). In either layout it's *one* card with an internal seam,
  not two cards.
- Resize handles are zero-width layout slots with a 7px wide absolute hit
  zone — surfaces abut directly, no body color shows through the seam.

## Crates / modules

```
src/
  main.rs       # Protocol entity (root). Layout, sidebars, drag state, persistence.
  config.rs     # ~/.protocol paths, Workspace + AppState (TOML serde).
  instance.rs   # On-disk instance ops: create (git clone), delete, dir listing.
  editor.rs     # CodeEditor entity + custom EditorElement. Per-line shape_line.
  terminal.rs   # Terminal entity wrapping alacritty_terminal Term + Pty + EventLoop.
  highlight.rs  # syntect SyntaxSet/Theme cache + run-builder.
  theme.rs      # Hsla/Rgba color constants and px sizing constants.
```

Dependencies that matter:
- `gpui` + `gpui_platform` (with `font-kit` feature — DO NOT FORGET)
  pulled from the zed git repo.
- `syntect` for syntax highlighting (per-line cached in CodeEditor).
- `alacritty_terminal` for the PTY + VT parser.
- `git2` is *not* used — clones shell out to `git`.

## Key data structures

### `Protocol` (main.rs)
The root entity. Owns:

- `workspaces: Vec<(Workspace, Option<error>)>` — loaded TOML.
- `instances: HashMap<String, Vec<Instance>>` — scanned per workspace.
- `selected_workspace`, `selected_instance: Option<String>` — current selection.
- `editors: Vec<OpenEditor>` — open files. Each `OpenEditor` has the path and
  `Entity<CodeEditor>`. Pool is global; tabs render only those whose path is
  inside the active instance.
- `terminals: HashMap<(workspace, instance), Vec<Entity<Terminal>>>` — kept
  alive across instance switches.
- `active_terminal_idx: HashMap<(workspace, instance), usize>`.
- `layouts: HashMap<(workspace, instance), Layout>` — per-instance panel sizes,
  collapse flags, terminal dock position. Persisted to `state.toml`.
- `expanded: HashSet<PathBuf>` — which folders are expanded in the right
  sidebar's file tree.
- `dir_cache: RefCell<HashMap<PathBuf, Vec<DirEntry>>>` — avoids `fs::read_dir`
  on every render. Cleared on workspace/instance switch.
- `creating_instance: Option<String>` — buffer for the new-instance modal.
- `cloning: Option<String>` — name of the instance currently being cloned in
  the background.
- `pending_delete: Option<(String, String)>` — modal state.
- `drag: Option<DragKind>` — what the user is currently dragging
  (LeftPanel, RightPanel, TerminalPanel, TerminalPanelMove).
- `collapsed_workspaces: HashSet<String>` — UI-only collapse state for the
  left sidebar.

### `CodeEditor` (editor.rs)

One per open file. Holds:
- `text: String`, `cursor: usize` (byte offset), `anchor: Option<usize>` (sel),
  `marked: Option<Range<usize>>` (IME), `goal_col` for vertical cursor moves.
- `scroll_x`, `scroll_y` (f32). `viewport_h`, `viewport_w` are read in the
  custom Element's `prepaint` from its bounds.
- `body_bounds: Option<Bounds<Pixels>>` — set in `prepaint`, used by mouse and
  IME math to translate window→text coords.
- `dirty: bool`, `text_version: u64`.
- `cached_highlight: Vec<TextRun>` — full-file syntect runs, cache key
  `cached_highlight_version`.
- `cached_line_runs: Vec<LineRuns>` — per-line slices of the runs, cache key
  `cached_line_runs_version`. Built once per text change; **never** in the
  scroll path.
- `history: Vec<Snapshot>`, `history_idx`, `last_edit_kind`, `last_edit_at` —
  undo/redo with 500ms-coalesce-by-kind.

### `Terminal` (terminal.rs)

Wraps `alacritty_terminal::Term<EventProxy>` behind a `FairMutex`. Owns:
- `term: Arc<FairMutex<Term<EventProxy>>>` — alacritty's grid.
- `notifier`, `resizer` — closures wrapping `EventLoopSender::send(Msg::...)`.
- `size: GridSize` (lines × cols at scrollback 5000).
- `last_cell_w`, `last_cell_h` — pixel dims of one cell. Used by both the
  size probe and the per-cell render.
- `focus: FocusHandle`.

`Terminal::create(cwd, &mut App) -> Result<Entity<Terminal>>` does the
fallible PTY/EventLoop setup outside `cx.new`, then constructs the entity.
Listener events (title changes etc.) land via a `futures` channel polled by an
async task spawned during creation.

## Rendering

### Editor (Zed-style)

The editor uses a custom `EditorElement`, not StyledText for the whole file.
Rationale: shaping a 1000-line file every frame is what made it laggy.

- `request_layout`: relative(1.0) size.
- `prepaint`:
  1. Snapshot state from the entity (text, cursor, anchor, scroll, focus,
     `line_runs()`).
  2. Compute `visible_start..visible_end` from `scroll_y` and viewport height.
  3. Call `window.text_system().shape_line(...)` per visible line with the
     pre-split `TextRun`s for that line, plus per-visible-line gutter numbers.
  4. Update `body_bounds`, `viewport_h`, `viewport_w` on the entity.
  5. Stuff everything in `PrepaintState`.
- `paint`:
  1. `paint_quad` for the panel-bg rectangle.
  2. `with_content_mask(body_bounds)` then paint selection quads, then call
     `shaped.paint(origin, line_h, ...)` for each visible line.
  3. Solid gutter bg + gutter line numbers on top (so horizontally scrolled
     text can never bleed into the gutter).
  4. IME marker underline (a 2px quad), cursor caret (a 2px-wide quad),
     `window.handle_input(&focus, ElementInputHandler::new(body_bounds, entity))`.

Scroll wheel and key handling are on the wrapping `div`. They mutate the
entity and call `cx.notify()`. The custom Element re-runs prepaint+paint.

### Terminal

Per-cell rendering — every grid cell is its own absolutely-positioned div at
`(col*cell_w, line*cell_h)`. Avoids text-shaping kerning that would let cells
drift away from alacritty's grid (this was the prompt-overlapping-input bug).
A `TermSizeProbe` custom Element captures the body's bounds in `prepaint` and
calls `Terminal::resize(lines, cols, cell_w, cell_h)`, which:
1. Resizes the alacritty Term grid (mutex lock).
2. Sends `Msg::Resize` to the EventLoop, which calls
   `pty.on_resize(WindowSize)` → SIGWINCH to the child shell.
3. On the *first* size probe (transitioning off the default 80×24), sends
   `\x0c` (Ctrl+L) so any prompt drawn at the wrong width gets cleared.

### Layout

- `card_frame()` is a helper that returns a div with 1px inset, 6px corner
  radius, 1px `theme::divider` border, `bg(highlight::theme_bg())`, and
  `overflow_hidden`. Both editor and terminal sit inside this single card.
- The body row is `bg(theme::panel_bg())` so the rounded corners cut to the
  sidebar surface.
- Resize handles use `resize_handle(kind, vertical, current_drag, cx)`. The
  outer is a 0-width flex slot. The hit zone is an absolute 7px overlay with
  the resize cursor + click handler. While dragging, an absolute 1px accent
  stroke is painted along the seam.

## Gotchas / battle scars

These are real bugs we hit and what to do about them.

### gpui won't render text without `font-kit`

`gpui_platform` defaults to `default = []`. The `font-kit` feature is the only
thing that wires up system font loading on macOS. **Without it, the app
silently shapes every glyph to nothing** — windows open, backgrounds paint,
zero text. `Cargo.toml` already enables it; never drop the feature.

### gpui's `rounded()` doesn't clip child `paint_quad` calls

`overflow_hidden` + `rounded(N)` only clips the parent's own bg/border draws.
Children calling `window.paint_quad` (like our terminal cells, or any custom
Element) ignore the rounded mask and paint with a square edge.

The fix in `card_frame`: paint the card's bg in the same color as the
children's bg. Then any pixel that leaks past the rounded clip blends with
the card's own fill, invisibly. The editor's outer div *does not* paint a bg
for this reason — only the card paints, and only the card's bg is rounded.

If you ever add a new color inside the card, make sure it doesn't differ from
the card bg, or its square corners will leak past the rounded mask. The
terminal body is allowed to be different (`#12141a`) because its corners are
the *outer* corners of the card on the right-dock side — covered by the
card's own rounded bg.

### gpui `hsla(h, s, l, a)` takes hue 0-1, not 0-360

`hsla(220., ...)` is `hue = 220 mod 1 = 0` = red. Always divide degrees by
360 first. `hsla(220. / 360., 0.6, 0.6, 0.25)` is the right pattern.

### `git status --porcelain` will tank scroll FPS

Runs in time linear with the working tree size. We *had* a "branch + dirty"
status line; on tinygrad/ripgrep instances it ate hundreds of ms per Protocol
render. The whole feature was deleted (`bfe2c9b`). If you re-introduce
anything that touches the filesystem on the render path, gate it behind a
cache *and* an explicit refresh trigger, never the render itself.

### `instance::DirEntry` is cached on Protocol

`fs::read_dir` was being called on every render for every visible expanded
folder. `Protocol.dir_cache` caches the listing per absolute path. Cleared on
workspace/instance switch. If you add file-create/delete UI inside an instance,
remember to invalidate the entry for the parent dir.

### Per-line `LineRuns` + per-visible-line `shape_line`

The editor's render-path cost is now bounded by the visible line count
(~30-60 per frame). `runs_for` (syntect) only re-runs when `text_version`
changes. `split_runs_per_line` only re-runs on the same trigger. Inside
`prepaint`, we shape only the lines in `visible_start..visible_end`. **Don't
move text shaping outside of this loop**, and don't switch back to `StyledText`
on the whole buffer — that was the original lag source.

Run with `PROTOCOL_TIMING=1` to see prepaint/paint timings in stderr; the
steady-state numbers should be ~0.2-0.5ms each. If you see it spike on
keystroke, you've broken the cache invalidation; if you see it spike on
scroll, you've put work back into the render path.

### Terminal cells use `0x12141a`, but editor uses syntect bg

We tried unifying these and the user objected. The terminal panel reads as
visually distinct from the editor on purpose — keep it that way. Don't unify
the bg colors when "fixing" something else.

### Async clone isn't optional

`instance::create_instance` shells out to `git clone` per repo, which is
seconds-to-minutes blocking. The clone runs on `cx.background_executor()`;
the UI thread shows a "Cloning workspace/instance" overlay. If you "simplify"
this back to synchronous, macOS will mark the window unresponsive — that's
what the user originally reported as "the app crashed".

### Text input is `EntityInputHandler`, not key listener

The editor implements `EntityInputHandler` (`text_for_range`, `selected_text_
range`, `marked_text_range`, `unmark_text`, `replace_text_in_range`,
`replace_and_mark_text_in_range`, `bounds_for_range`, `character_index_for_
point`). Registered via `window.handle_input` inside the custom Element's
paint. **This is what makes IME work** (composition, dead keys, character
palette). Don't replace it with a plain `on_key_down` handler.

`bounds_for_range` returns *window-coordinate* bounds (relative to the
window), but the input is `bounds: Bounds<Pixels>` already in window coords —
just add LEFT_PAD + col*CHAR_W − scroll_x and TOP_PAD + line*LINE_H −
scroll_y. We track `body_bounds` for `character_index_for_point`.

### Windows get an off-screen sibling

When alacritty / gpui_macos creates a window, you may see two windows for the
same PID in `CGWindowListCopyWindowInfo`: the real titled window plus an
off-screen helper buffer. `capture.sh` filters on `kCGWindowIsOnscreen` *and*
non-empty `kCGWindowName` — keep that filter if you touch the script.

### Layout-shift-free focus ring

Active editor + active terminal get a 1px accent ring rendered as an
*absolutely positioned overlay child* (`focus_overlay`), not as a
`border_1()` on the wrapper. Borders reserve layout space; the overlay
doesn't, so focus changes don't shift surrounding content. Don't "simplify"
this back to a border.

## Persistence

`~/.protocol/state.toml` is the single persistent state file. Owned by
`config::AppState`. Fields:

- `last_workspace`, `last_instance` — restore on launch.
- `expanded: Vec<PathBuf>` — file-tree expansion state.
- `selected_file: Option<PathBuf>` — last-opened file.
- `layouts: Vec<InstanceLayout>` — per-(workspace, instance) layout state
  (left/right widths, terminal panel size + position, collapse flags).

Persisted by `Protocol::persist()`, called from any handler that mutates
selection or layout. **Never call `persist()` from inside `Render::render`** or
the layout fields will be stomped before they're used (we hit this once and
reverted to call-site persistence).

## Performance budget

Steady-state under `PROTOCOL_TIMING=1` on a release build:
- `editor prepaint` 0.2-0.5 ms
- `editor paint` 0.1-0.3 ms
- `protocol render_body` not logged (under the 0.5ms threshold) after the
  first paint.

Cold first frame is ~20-100ms (syntect + font cache populate). Don't be
alarmed by that.

If you add a feature and the steady-state numbers climb, your feature is
doing something on every render that should be cached or moved off the
render path.

## How to extend

- **New global state on Protocol** — add the field, default it in `Protocol::
  new`, persist (only) what should survive restarts via `AppState`.
- **New per-instance state** — add it to `Layout` *and* `config::Instance
  Layout` (with a `serde` default), wire up read in `Protocol::new` and write
  in `persist`, drive UI through `current_layout()` and `mutate_layout()`.
- **New panel** — add a render fn returning `impl IntoElement`, slot it into
  `render_body` between resize handles, add a `DragKind` variant if it
  resizes. Mirror an existing panel's structure (don't add `border_1` on the
  wrapper — use `card_frame` if you want the card aesthetic, or none).
- **New action on a row** — wrap the row in `cx.listener(|this, ev, win, cx|
  { ... })` patterns; remember to `cx.notify()` after state changes.
- **New editor feature** — touch state on `CodeEditor`, bump
  `text_version` if it changes the buffer (this invalidates the highlight
  cache), call `record(EditKind::...)` if it should be a separate undo step.

## What not to do

- Don't put filesystem reads on the render path.
- Don't replace the per-line shaping with a whole-buffer `StyledText`.
- Don't reintroduce the git-status feature.
- Don't put `border_1()` on a panel wrapper to indicate focus — use
  `focus_overlay()`.
- Don't add `border_b_1` or `border_t_1` between adjoining panel surfaces.
  Surfaces just abut. Use the divider color only on intentional separators
  (between editor tabs, between terminal tabs).
- Don't change the terminal cell color (`#12141a`) trying to "fix" rounded
  corners — fix the rounded corners by matching colors elsewhere.
- Don't drop the `font-kit` feature on `gpui_platform`.

## Useful commands

```sh
# Debug build (slow but fast to iterate)
cargo run

# Release build with timing instrumentation
cargo build --release
PROTOCOL_TIMING=1 ./target/release/protocol 2>&1 | grep editor

# Capture the running window into /tmp/protocol.png for visual review
./capture.sh

# Run the upstream gpui hello_world example as a reference
cd ~/.cargo/git/checkouts/zed-*/8068aeb && cargo run -p gpui --example hello_world
```

## Files an agent should read before writing code

In order:

1. This file.
2. `GPUI_MANUAL.md` — gpui basics + the `font-kit` gotcha.
3. `SPEC.md` — original v1 product spec. Some details have drifted; treat as
   intent, not as truth about the current code.
4. `src/main.rs::Protocol::render_body` — single source of truth for the
   layout structure.
5. The example you're closest to in `references/zed/crates/gpui/examples/`
   (e.g. `input.rs` for IME, `uniform_list.rs` for big lists, `text.rs` for
   text styling).
