//! Shared, Protocol-agnostic UI helpers. Anything that takes a generic handler
//! or returns a plain `Div` lives here. Helpers that touch `Protocol` state
//! (drag, modals) stay in their owning module so we don't form a cycle.

use gpui::{App, IntoElement, MouseButton, SharedString, Window, div, prelude::*, px};

use crate::{highlight, theme};

/// Rounded center card. The body of the editor/terminal area paints into this.
pub fn card_frame() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .my(px(1.))
        .mx(px(1.))
        .rounded(px(6.))
        .border_1()
        .border_color(theme::divider())
        .bg(highlight::theme_bg())
        .overflow_hidden()
}

/// 1px accent ring drawn as an absolutely-positioned overlay so it doesn't
/// reserve any layout space when not present.
pub fn focus_overlay() -> gpui::Div {
    div()
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .bottom_0()
        .border_1()
        .border_color(gpui::hsla(220. / 360., 0.6, 0.6, 0.25))
}

pub fn placeholder(text: &'static str) -> gpui::Div {
    div()
        .flex()
        .flex_1()
        .items_center()
        .justify_center()
        .text_color(theme::text_muted())
        .child(text)
}

pub fn section_header(label: &'static str) -> impl IntoElement {
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

pub fn row_base() -> gpui::Div {
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

pub fn sidebar_toggle_button(
    id: &'static str,
    glyph: &'static str,
    handler: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .w(px(24.))
        .h(px(22.))
        .rounded(px(4.))
        .text_size(px(11.))
        .text_color(theme::text_muted())
        .cursor_pointer()
        .hover(|s| s.bg(theme::row_hover()).text_color(theme::text()))
        .on_mouse_down(MouseButton::Left, handler)
        .child(glyph)
}
