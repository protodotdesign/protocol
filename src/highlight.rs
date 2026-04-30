use std::sync::OnceLock;

use gpui::{Hsla, TextRun, font};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Style as SynStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

struct Engine {
    syntax_set: SyntaxSet,
    theme: Theme,
}

fn engine() -> &'static Engine {
    static E: OnceLock<Engine> = OnceLock::new();
    E.get_or_init(|| {
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let mut themes = ThemeSet::load_defaults();
        let theme = themes
            .themes
            .remove("base16-ocean.dark")
            .unwrap_or_else(|| themes.themes.remove("base16-eighties.dark").unwrap());
        Engine { syntax_set, theme }
    })
}

fn syn_color_to_hsla(c: syntect::highlighting::Color) -> Hsla {
    let r = c.r as f32 / 255.;
    let g = c.g as f32 / 255.;
    let b = c.b as f32 / 255.;
    let a = c.a as f32 / 255.;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.;
    let (h, s) = if (max - min).abs() < f32::EPSILON {
        (0., 0.)
    } else {
        let d = max - min;
        let s = if l > 0.5 { d / (2. - max - min) } else { d / (max + min) };
        let h = if max == r {
            (g - b) / d + if g < b { 6. } else { 0. }
        } else if max == g {
            (b - r) / d + 2.
        } else {
            (r - g) / d + 4.
        } / 6.;
        (h, s)
    };
    Hsla { h, s, l, a }
}

/// Highlight `text`, returning one TextRun per styled token.
/// `font_family` and `font_size` should match the font used to render the result.
pub fn runs_for(text: &str, ext: Option<&str>, font_family: &str) -> Vec<TextRun> {
    let eng = engine();
    let syntax = ext
        .and_then(|e| eng.syntax_set.find_syntax_by_extension(e))
        .or_else(|| eng.syntax_set.find_syntax_by_first_line(text))
        .unwrap_or_else(|| eng.syntax_set.find_syntax_plain_text());

    let mut highlighter = HighlightLines::new(syntax, &eng.theme);
    let code_font = font(font_family);
    let mut runs: Vec<TextRun> = Vec::new();
    for line in LinesWithEndings::from(text) {
        let Ok(spans) = highlighter.highlight_line(line, &eng.syntax_set) else {
            runs.push(TextRun {
                len: line.len(),
                font: code_font.clone(),
                color: Hsla { h: 0., s: 0., l: 0.85, a: 1. },
                background_color: None,
                underline: None,
                strikethrough: None,
            });
            continue;
        };
        for (style, slice) in spans {
            if slice.is_empty() {
                continue;
            }
            runs.push(TextRun {
                len: slice.len(),
                font: code_font.clone(),
                color: convert_color(style),
                background_color: None,
                underline: None,
                strikethrough: None,
            });
        }
    }
    runs
}

fn convert_color(style: SynStyle) -> Hsla {
    syn_color_to_hsla(style.foreground)
}

/// Background color of the syntect theme — useful as the editor background.
pub fn theme_bg() -> Hsla {
    let bg = engine()
        .theme
        .settings
        .background
        .unwrap_or(syntect::highlighting::Color { r: 0x1f, g: 0x21, b: 0x28, a: 0xff });
    syn_color_to_hsla(bg)
}
