# GPUI Reference Manual

A working guide for building UIs with Zed's `gpui` crate. Source of truth: `references/zed/crates/gpui` (examples, docs, README).

## What GPUI is

GPU-accelerated, hybrid immediate/retained mode UI framework for Rust. Pre-1.0, breaking changes between versions. macOS uses Metal, Linux uses Wayland/X11, Windows is supported, and there's a wasm/web backend (`gpui_web`).

Three layers, pick what you need:
1. **Entities** — owned state with reactive notify/observe (like `Rc<RefCell<T>>` but managed by the app).
2. **Views** — entities that implement `Render`, return a tree of elements with a tailwind-style API (`div().flex().bg(...)`).
3. **Elements** — low-level imperative primitives with full control over layout/paint. Use when `div` isn't enough (custom text editors, big lists, charts).

## Crate layout (workspace)

GPUI is **not one crate**. The Zed repo splits it across 11 sibling crates that all reference each other via `.workspace = true`. This matters because you can't just `path = "..."` to `gpui` from outside — cargo will fail to resolve `gpui_shared_string.workspace = true` inside gpui's own Cargo.toml.

**You declare in your `Cargo.toml`:**
- `gpui` — the framework itself. Re-exports types, traits, `div`, `px`, `rgb`, etc. This is what you `use gpui::*` from.
- `gpui_platform` — platform glue. `gpui_platform::application()` returns the `Application` instance.

**Pulled in transitively (don't declare unless you need them directly):**
- `gpui_macros` — `#[derive(IntoElement)]`, `#[gpui::test]`, `actions!`.
- `gpui_shared_string` — `SharedString` (Arc-backed string, used everywhere).
- `gpui_util` — internal helpers.
- `gpui_macos` / `gpui_linux` / `gpui_windows` — per-OS rendering + windowing backends. Activated by target cfg.
- `gpui_web` — wasm backend (use when `target_family = "wasm"`).
- `gpui_wgpu` — shared wgpu rendering used by Linux/Windows/web.
- `gpui_tokio` — optional tokio integration; pull in if you need to bridge tokio futures.

**How to depend on it from outside the Zed monorepo:**

The cleanest path is git deps, because cargo will resolve transitive `.workspace = true` references against Zed's own workspace:

```toml
gpui = { git = "https://github.com/zed-industries/zed", package = "gpui" }
gpui_platform = { git = "https://github.com/zed-industries/zed", package = "gpui_platform" }
```

Pin a `rev = "..."` once it builds — gpui is pre-1.0 and breaking changes land weekly (recent commits in the repo show `gpui_shared_string: Implement SharedString via smol_str`, `Extract gpui_platform out of gpui`, etc.).

Pure path deps to `references/zed/crates/gpui` won't work without also vendoring the workspace setup. If you want to use the local checkout, the trick is to make your project a workspace member of (or sibling to) the Zed workspace, or use `[patch]` to redirect each sibling crate.

## The minimum viable app

```rust
use gpui::{
    App, Bounds, Context, Window, WindowBounds, WindowOptions,
    div, prelude::*, px, rgb, size,
};
use gpui_platform::application;

struct Hello { text: gpui::SharedString }

impl gpui::Render for Hello {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().bg(rgb(0x202020)).text_color(rgb(0xffffff)).child(format!("hi {}", self.text))
    }
}

fn main() {
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(500.), px(500.)), cx);
        cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            |_, cx| cx.new(|_| Hello { text: "world".into() }),
        ).unwrap();
        cx.activate(true);
    });
}
```

That's the whole pattern. Everything else is layering.

## Contexts (the `cx` everywhere)

Every callback gets a context. They deref into each other, so most things "just work":

- `App` — root. Owns all entity data. Use to open windows, register globals, bind keys, quit, read clipboard.
- `Context<T>` — provided when you're inside an entity. Derefs to `App`. Adds `notify()`, `emit(event)`, `listener(...)`, `entity()` (handle to self).
- `Window` — per-window state. Not a context; passed alongside `App`/`Context<T>`. Owns focus, text system, layout requests, paint. Methods like `request_layout`, `paint_quad`, `text_system`.
- `AsyncApp` / `AsyncWindowContext` — `'static` versions for async tasks; calls return `Result` because the window may be gone.
- `TestAppContext` — tests only.

Rule of thumb: if you see a method on `App`, you can call it from `Context<T>` too.

## Entities and rendering

```rust
let entity: Entity<MyState> = cx.new(|cx| MyState { ... });
entity.update(cx, |state, cx| { state.count += 1; cx.notify(); });
let value = entity.read(cx);
```

- `cx.notify()` — re-render. Call after mutating state you want reflected in UI.
- `cx.emit(SomeEvent)` — fire a typed event (impl `EventEmitter<SomeEvent>`).
- `cx.observe(&other, |this, other, cx| { ... })` — react to another entity's notify.
- `cx.subscribe(&other, |this, other, ev, cx| { ... })` — react to events.

Views are just entities that `impl Render`. Root view is created inside `cx.open_window(opts, |window, cx| cx.new(|cx| MyView::new(...)))`.

## The `div` API (tailwind-ish)

Chainable builder. Common bits seen across examples:

- Layout: `.flex()`, `.flex_col()`, `.flex_row()`, `.gap_2()`, `.justify_center()`, `.items_center()`, `.size_full()`, `.w_full()`, `.h(px(30.))`, `.p(px(4.))`, `.px_2()`.
- Sizing helpers: `.size_8()`, `.size(px(500.))`.
- Visuals: `.bg(rgb(0xeeeeee))`, `.border_1()`, `.border_color(black())`, `.rounded_md()`, `.shadow_lg()`, `.text_color(...)`, `.text_xl()`, `.text_size(px(24.))`.
- Children: `.child(thing)`, `.children(iter)`. `thing` is anything `IntoElement` (other divs, strings, entities that impl Render via `cx.entity()`).
- Interaction: `.hover(|s| s.bg(...))`, `.cursor(CursorStyle::IBeam)`, `.on_mouse_down(MouseButton::Left, cx.listener(Self::handler))`, `.on_mouse_move(...)`, `.on_action(cx.listener(Self::handle_paste))`.
- Focus: `.track_focus(&self.focus_handle(cx))`, `.key_context("MyContext")`.

`cx.listener(Self::method)` is the standard way to wire a callback that takes `&mut Self, &Event, &mut Window, &mut Context<Self>`.

## Actions & key bindings

```rust
gpui::actions!(my_app, [Quit, Paste, Copy]);

cx.bind_keys([
    KeyBinding::new("cmd-q", Quit, None),
    KeyBinding::new("cmd-c", Copy, Some("MyContext")),
]);
cx.on_action(|_: &Quit, cx| cx.quit());
```

In a view, listen with `.on_action(cx.listener(Self::handle_paste))` where the method takes `&Paste` as the event arg. The third arg of `KeyBinding::new` is the key context predicate (matches `.key_context("...")`).

## Focus

```rust
struct MyView { focus_handle: FocusHandle }
impl Focusable for MyView {
    fn focus_handle(&self, _: &App) -> FocusHandle { self.focus_handle.clone() }
}
// in constructor: focus_handle: cx.focus_handle()
// in render: .track_focus(&self.focus_handle(cx))
// to focus: window.focus(&handle, cx);
// query: handle.is_focused(window)
```

## Custom Elements (low-level)

When `div` isn't enough (text editor, custom paint), implement `Element`:

```rust
impl Element for MyEl {
    type RequestLayoutState = ();
    type PrepaintState = MyPrepaint;
    fn id(&self) -> Option<ElementId> { None }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> { None }
    fn request_layout(&mut self, _id, _ins, window, cx) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }
    fn prepaint(&mut self, _id, _ins, bounds, _layout, window, cx) -> MyPrepaint { ... }
    fn paint(&mut self, _id, _ins, bounds, _layout, prepaint, window, cx) {
        window.paint_quad(fill(bounds, gpui::blue()));
    }
}
impl IntoElement for MyEl { type Element = Self; fn into_element(self) -> Self { self } }
```

Three phases: `request_layout` (taffy style), `prepaint` (resolve geometry, shape text), `paint` (push quads/lines to GPU). `window.text_system().shape_line(...)` shapes text; `ShapedLine::paint` draws it.

## Text input (IME-aware)

`impl EntityInputHandler for YourState` then in `paint`: `window.handle_input(&focus_handle, ElementInputHandler::new(bounds, entity), cx)`. Required methods cover utf16 conversions, marked range (composition), and bounds-for-range (for IME windows). See `examples/input.rs` — copy that wholesale, it's the canonical pattern.

## Async

`cx.spawn(|cx: AsyncApp| async move { ... })` returns a `Task<T>`. `.detach()` to fire-and-forget. Inside, use `entity.update(&mut cx, |this, cx| ...)?` (returns `Result`).

## Common imports cheat sheet

```rust
use gpui::{
    // app + windowing
    App, Application, Bounds, Context, Window, WindowBounds, WindowOptions,
    // entities + rendering
    Entity, Render, IntoElement, ParentElement, Styled, InteractiveElement, StatefulInteractiveElement,
    // helpers
    div, px, rgb, rgba, hsla, size, point, relative, fill,
    // colors
    black, white, red, green, blue, yellow, opaque_grey,
    // input + focus
    FocusHandle, Focusable, KeyBinding, Keystroke, MouseButton,
    MouseDownEvent, MouseUpEvent, MouseMoveEvent,
    // text/elements (when going low-level)
    Element, ElementId, GlobalElementId, LayoutId, Pixels, Point, Style,
    TextRun, ShapedLine, PaintQuad, UnderlineStyle,
    SharedString, ClipboardItem, CursorStyle,
    // macros
    actions,
    // glob trait imports
    prelude::*,
};
use gpui_platform::application;
```

`prelude::*` brings in `Styled`, `ParentElement`, `InteractiveElement`, `IntoElement`, `FluentBuilder`, etc. Always include it.

## Examples worth reading (in order)

1. `hello_world.rs` — minimum app, div styling.
2. `window.rs` — window options, multiple windows.
3. `input.rs` — full text input with IME, custom Element, actions, focus. The big one.
4. `uniform_list.rs` — efficient long lists.
5. `animation.rs` — animations.
6. `popover.rs`, `scrollable.rs`, `drag_drop.rs` — interaction patterns.
7. `painting.rs`, `gradient.rs`, `shadow.rs` — custom paint.

## Gotchas

- Always call `cx.notify()` after mutating state you want re-rendered. GPUI does not observe field writes.
- Don't hold `&App` across awaits; convert to `AsyncApp` via `cx.to_async()` (or `cx.spawn`).
- `Entity<T>` is cheap to clone — it's a handle, not the data.
- `cx.entity()` inside `Context<T>` returns the handle to self; pass it to children that need to call back.
- On macOS, you need real Xcode (not just CLT) for Metal headers when building.
- The example files use `gpui_platform::application()` — that's the entry point, not a method on `gpui` directly.
- `text_system` is on `Window`, not `App`: `window.text_system().shape_line(...)`.
- Layout uses Taffy (`taffy = "=0.10.1"` is a hard pin in gpui).

## Dependencies to add

```toml
[dependencies]
gpui = { git = "https://github.com/zed-industries/zed", package = "gpui" }
gpui_platform = { git = "https://github.com/zed-industries/zed", package = "gpui_platform", features = ["font-kit"] }
anyhow = "1"
```

**Critical**: `gpui_platform` ships with `default = []` and the `font-kit` feature is what wires up system font loading. Forget it and your app builds, opens a window, paints backgrounds correctly — and renders zero glyphs with no warning. Every upstream example enables it via dev-deps; outside the workspace you have to add it yourself.

Everything else (`gpui_macros`, `gpui_shared_string`, `gpui_macos`/`gpui_linux`/`gpui_windows`, `gpui_wgpu`, `gpui_util`) is pulled in transitively. Pin a `rev` once it builds.

On macOS: install Xcode (not just Command Line Tools) and run `sudo xcode-select --switch /Applications/Xcode.app/Contents/Developer` so the Metal headers are visible.

On Linux: you'll need wayland/x11 dev libraries; default features cover both.
