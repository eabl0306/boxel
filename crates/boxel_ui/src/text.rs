//! Text: style, measuring, word wrap, ellipsis and the glyph layout the painter draws.

use crate::color::{Rgba, WHITE};
use crate::font::{BitmapFont, Glyph, LINE_HEIGHT};

/// How a text is drawn: an sRGB color, a shadow one GUI pixel down and right (per size step) in
/// the color's shadow ([`crate::color::shadow_of`]), and an integer size multiplier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Text {
    /// Color (sRGB, straight alpha).
    pub color: Rgba,
    /// Draw the shadow first.
    pub shadow: bool,
    /// Every glyph pixel becomes `size` by `size` GUI pixels.
    pub size: u32,
}

impl Default for Text {
    fn default() -> Self {
        Self {
            color: WHITE,
            shadow: true,
            size: 1,
        }
    }
}

impl Text {
    /// Shadowed text of `color` at size 1.
    pub const fn new(color: Rgba) -> Self {
        Self {
            color,
            shadow: true,
            size: 1,
        }
    }

    /// White shadowed text at `size` (titles).
    pub const fn title(size: u32) -> Self {
        Self {
            color: WHITE,
            shadow: true,
            size,
        }
    }

    /// The same style in another color.
    pub const fn color(mut self, color: Rgba) -> Self {
        self.color = color;
        self
    }

    /// The same style with or without the shadow.
    pub const fn shadow(mut self, on: bool) -> Self {
        self.shadow = on;
        self
    }

    /// The same style at another size.
    pub const fn size(mut self, size: u32) -> Self {
        self.size = size;
        self
    }
}

/// The width of a line in GUI pixels: the sum of its advances (the last one included, as
/// Minecraft measures), times `size`. Stops at the first newline.
pub fn measure(font: &BitmapFont, text: &str, size: u32) -> u32 {
    text.chars()
        .take_while(|&c| c != '\n')
        .map(|c| font.resolve(c).advance)
        .sum::<u32>()
        * size.max(1)
}

/// Splits `text` into lines no wider than `max_width` GUI pixels, breaking at spaces and at
/// newlines. A word wider than the limit is cut between characters.
pub fn wrap(font: &BitmapFont, text: &str, max_width: u32, size: u32) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split(' ') {
            let candidate = if line.is_empty() {
                word.to_owned()
            } else {
                format!("{line} {word}")
            };
            if measure(font, &candidate, size) <= max_width {
                line = candidate;
                continue;
            }
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            // The word alone: cut it while it is too wide.
            for c in word.chars() {
                let mut next = line.clone();
                next.push(c);
                if measure(font, &next, size) > max_width && !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                    next = c.to_string();
                }
                line = next;
            }
        }
        lines.push(line);
    }
    lines
}

/// `text` cut to fit `max_width` GUI pixels with `"..."` at the end, or unchanged when it fits.
pub fn ellipsis(font: &BitmapFont, text: &str, max_width: u32, size: u32) -> String {
    if measure(font, text, size) <= max_width {
        return text.to_owned();
    }
    let dots = "...";
    let mut out = String::new();
    for c in text.chars() {
        let mut next = out.clone();
        next.push(c);
        next.push_str(dots);
        if measure(font, &next, size) > max_width {
            break;
        }
        out.push(c);
    }
    out.push_str(dots);
    out
}

/// A glyph placed in GUI pixels (top-left of its drawn box), before the size multiplier is
/// applied to the glyph's own size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlacedGlyph {
    /// The glyph.
    pub glyph: Glyph,
    /// Left edge in GUI pixels.
    pub x: i32,
    /// Top edge in GUI pixels.
    pub y: i32,
}

/// Places every glyph of `text` from `x`, `y` (the top of the first line); newlines start a new
/// line [`LINE_HEIGHT`] times `size` lower.
pub fn layout(font: &BitmapFont, text: &str, x: i32, y: i32, size: u32) -> Vec<PlacedGlyph> {
    let size = size.max(1) as i32;
    let mut out = Vec::with_capacity(text.len());
    let (mut pen_x, mut pen_y) = (x, y);
    for c in text.chars() {
        if c == '\n' {
            pen_x = x;
            pen_y += LINE_HEIGHT as i32 * size;
            continue;
        }
        let glyph = font.resolve(c);
        if glyph.src[2] > 0 && glyph.src[3] > 0 {
            out.push(PlacedGlyph {
                glyph,
                x: pen_x,
                y: pen_y + glyph.top * size,
            });
        }
        pen_x += glyph.advance as i32 * size;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UiAtlas;
    use crate::font::default_font;

    fn font() -> BitmapFont {
        default_font(&mut UiAtlas::new(256)).unwrap()
    }

    #[test]
    fn measure_sums_advances() {
        let font = font();
        // 'A' 7, space 4, 'M' 8.
        assert_eq!(measure(&font, "A M", 1), 19);
        assert_eq!(measure(&font, "A M", 2), 38);
        assert_eq!(measure(&font, "", 1), 0);
        assert_eq!(measure(&font, "A\nMMMM", 1), 7);
    }

    #[test]
    fn wrap_breaks_at_spaces_and_cuts_long_words() {
        let font = font();
        let w = measure(&font, "AA AA", 1);
        assert_eq!(wrap(&font, "AA AA AA", w, 1), ["AA AA", "AA"]);
        assert_eq!(wrap(&font, "AAAA", 14, 1), ["AA", "AA"]);
        assert_eq!(wrap(&font, "A\nA", 100, 1), ["A", "A"]);
        assert_eq!(wrap(&font, "", 100, 1), [""]);
    }

    #[test]
    fn ellipsis_fits_the_width() {
        let font = font();
        assert_eq!(ellipsis(&font, "AAA", 100, 1), "AAA");
        let cut = ellipsis(&font, "AAAAAAAAAA", 40, 1);
        assert!(cut.ends_with("..."), "{cut}");
        assert!(measure(&font, &cut, 1) <= 40, "{cut}");
        assert!(cut.len() > 3);
    }

    #[test]
    fn layout_places_glyphs_and_lines() {
        let font = font();
        let placed = layout(&font, "A A\nM", 10, 20, 2);
        // Spaces draw nothing.
        assert_eq!(placed.len(), 3);
        assert_eq!((placed[0].x, placed[0].y), (10, 20));
        assert_eq!(placed[1].x, 10 + (7 + 4) * 2);
        assert_eq!((placed[2].x, placed[2].y), (10, 20 + 18));
    }
}
