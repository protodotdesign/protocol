//! Modal/overlay rendering. Pure render functions that take borrowed state from
//! `Protocol` and dispatch back via `cx.listener`. Keep this file purely
//! presentational — modal *state machine* logic stays on `Protocol`.

use gpui::{Context, IntoElement, MouseButton, SharedString, div, prelude::*, px};

use crate::{Protocol, theme};

pub fn render_cloning_overlay(workspace: &str, instance: &str) -> impl IntoElement {
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

pub fn render_new_instance_modal(
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

pub fn render_delete_modal(
    workspace: &str,
    instance: &str,
    cx: &mut Context<Protocol>,
) -> impl IntoElement {
    let msg = SharedString::from(format!("Delete instance \"{}/{}\"?", workspace, instance));
    let hint = SharedString::from(
        "This removes the entire directory. Press Enter to confirm, Esc to cancel.",
    );
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
