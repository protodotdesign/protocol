use gpui::{Hsla, Rgba, hsla, rgb};

// Surfaces. Zed's One Dark / default-ish — pretty close to #1f2128 / #181a1f.
pub fn bg() -> Rgba { rgb(0x1f2128) }
pub fn panel_bg() -> Rgba { rgb(0x181a1f) }
// Title bar matches the side panel surface so the window chrome reads as one
// continuous canvas with no contrast band at the top.
pub fn titlebar_bg() -> Rgba { rgb(0x181a1f) }
// Subtle separator that stays close to panel_bg; just enough to imply a seam
// without leaving a visible gap-looking line between sections.
pub fn divider() -> Rgba { rgb(0x202229) }

// Selection lives on the surface, not as a tint. Slight lift, no hue.
pub fn row_hover() -> Hsla { hsla(0., 0., 1., 0.035) }
pub fn row_selected() -> Hsla { hsla(0., 0., 1., 0.07) }

// Text.
pub fn text() -> Rgba { rgb(0xc8ccd4) }
pub fn text_strong() -> Rgba { rgb(0xe6e8ee) }
pub fn text_muted() -> Rgba { rgb(0x787b85) }
pub fn text_dim() -> Rgba { rgb(0x52545c) }
pub fn accent() -> Rgba { rgb(0x73ade9) }
pub fn danger() -> Rgba { rgb(0xe06c75) }

// Sizing rhythm.
pub const TITLEBAR_H: f32 = 30.;
pub const ROW_H: f32 = 24.;
pub const SECTION_HEADER_H: f32 = 26.;
pub const PANEL_W: f32 = 240.;
pub const STATUSBAR_H: f32 = 22.;
pub const INDENT_PX: f32 = 16.;
pub const ROW_PAD_X: f32 = 10.;
