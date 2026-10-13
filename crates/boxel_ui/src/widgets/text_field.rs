use crate::font::{BitmapFont, GLYPH_HEIGHT};
use crate::layout::UiRect;
use crate::memory::WidgetKind;
use crate::skin::SkinSlot;
use crate::text::{self, Text};
use crate::ui::{Painter, Resp, Response};
use boxel_math::{IVec2, UVec2};
use boxel_window::{ImeCursorArea, KeyCode, TextInput};
use std::time::Duration;

/// A focused text field's caret shows and hides every this much real time (Minecraft's
/// 300 ms), starting shown each time it moves.
pub const CARET_BLINK: Duration = Duration::from_millis(300);

/// Between a text field's edge and its text (Minecraft's 4).
const PAD: i32 = 4;

/// What a text field keeps between frames. Positions count characters (`char`s) of the value.
#[derive(Debug, Default)]
struct FieldData {
    /// The caret.
    caret: usize,
    /// The selection's other end (the caret's when nothing is selected).
    anchor: usize,
    /// The first character shown: a text wider than the field scrolls to keep the caret in view.
    first: usize,
    /// When the caret last moved, in real time (it blinks from there).
    moved: Duration,
    /// It had the focus last frame.
    focused: bool,
}

/// A value being edited: its characters, the caret and the selection's other end.
struct Edit {
    chars: Vec<char>,
    caret: usize,
    anchor: usize,
    max: usize,
}

impl Edit {
    /// The selection, start first.
    fn selection(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }

    fn has_selection(&self) -> bool {
        self.caret != self.anchor
    }

    fn delete(&mut self, from: usize, to: usize) {
        self.chars.drain(from..to);
        self.caret = from;
        self.anchor = from;
    }

    fn delete_selection(&mut self) {
        let (from, to) = self.selection();
        self.delete(from, to);
    }

    /// Replaces the selection with `text`, cut to the room left under the maximum.
    fn insert(&mut self, text: &str) {
        self.delete_selection();
        let room = self.max.saturating_sub(self.chars.len());
        let typed: Vec<char> = text.chars().take(room).collect();
        let n = typed.len();
        self.chars.splice(self.caret..self.caret, typed);
        self.caret += n;
        self.anchor = self.caret;
    }

    /// Moves the caret, extending the selection or dropping it.
    fn move_to(&mut self, at: usize, extend: bool) {
        self.caret = at.min(self.chars.len());
        if !extend {
            self.anchor = self.caret;
        }
    }

    /// The start of the word before `at`, past the spaces before it.
    fn word_left(&self, at: usize) -> usize {
        let mut i = at;
        while i > 0 && self.chars[i - 1] == ' ' {
            i -= 1;
        }
        while i > 0 && self.chars[i - 1] != ' ' {
            i -= 1;
        }
        i
    }

    /// The end of the word after `at` and the spaces after it.
    fn word_right(&self, at: usize) -> usize {
        let n = self.chars.len();
        let mut i = at;
        while i < n && self.chars[i] != ' ' {
            i += 1;
        }
        while i < n && self.chars[i] == ' ' {
            i += 1;
        }
        i
    }

    /// An editing key. Returns true for Enter.
    fn key(&mut self, key: KeyCode, shift: bool, ctrl: bool) -> bool {
        let (caret, n) = (self.caret, self.chars.len());
        match key {
            KeyCode::Backspace | KeyCode::Delete if self.has_selection() => {
                self.delete_selection();
            }
            KeyCode::Backspace => {
                let from = if ctrl {
                    self.word_left(caret)
                } else {
                    caret.saturating_sub(1)
                };
                self.delete(from, caret);
            }
            KeyCode::Delete => {
                let to = if ctrl {
                    self.word_right(caret)
                } else {
                    (caret + 1).min(n)
                };
                self.delete(caret, to);
            }
            KeyCode::ArrowLeft => {
                let to = if ctrl {
                    self.word_left(caret)
                } else if self.has_selection() && !shift {
                    self.selection().0
                } else {
                    caret.saturating_sub(1)
                };
                self.move_to(to, shift);
            }
            KeyCode::ArrowRight => {
                let to = if ctrl {
                    self.word_right(caret)
                } else if self.has_selection() && !shift {
                    self.selection().1
                } else {
                    caret + 1
                };
                self.move_to(to, shift);
            }
            KeyCode::Home => self.move_to(0, shift),
            KeyCode::End => self.move_to(n, shift),
            KeyCode::KeyA if ctrl => {
                self.anchor = 0;
                self.caret = n;
            }
            KeyCode::Enter | KeyCode::NumpadEnter => return true,
            _ => {}
        }
        false
    }
}

/// The width of `chars` in GUI pixels.
fn width(font: &BitmapFont, chars: &[char]) -> i32 {
    chars.iter().map(|&c| font.resolve(c).advance as i32).sum()
}

/// The caret position nearest to `x` (GUI pixels from where `chars[first]` is drawn).
fn index_at(font: &BitmapFont, chars: &[char], first: usize, x: i32) -> usize {
    let mut pen = 0;
    for (i, &c) in chars.iter().enumerate().skip(first) {
        let advance = font.resolve(c).advance as i32;
        if x < pen + advance / 2 {
            return i;
        }
        pen += advance;
    }
    chars.len()
}

impl<'a> Painter<'a> {
    /// A one-line text field editing `value`, at most `max_chars` characters. `label` names it
    /// (identity, tests), it is not drawn; `hint` shows in gray while the field is empty and
    /// does not have the focus (Minecraft's rule).
    ///
    /// It takes the focus with a press (the caret goes to the pressed character; Shift extends
    /// the selection, a drag selects) or with Tab, Up and Down. While focused it reads
    /// `TextInput`: typed text (accents, dead keys and input method commits included) replaces
    /// the selection, and the keys: Left, Right, Home and End move the caret (Shift selects,
    /// Ctrl jumps words), Backspace and Delete erase (Ctrl a word; held, they repeat), Ctrl+A
    /// selects everything, Enter reports `submitted` (once while held). Space types; Escape stays
    /// the game's. An input method's composition shows at the caret, and the UI turns the input
    /// method on (`ImeOptions`) while a field has the focus, with the caret's place for its
    /// window.
    ///
    /// The caret blinks every [`CARET_BLINK`] of real time (always shown without a `Time`
    /// resource). A text wider than the field scrolls to keep it in view.
    pub fn text_field(
        &mut self,
        rect: UiRect,
        label: &str,
        value: &mut String,
        max_chars: usize,
        hint: &str,
    ) -> Resp<'_, 'a> {
        let it = self.interact(rect, WidgetKind::TextField, label);
        let mut data = std::mem::take(self.widget_data::<FieldData>(it.id));
        let now = self.now().unwrap_or_default();
        let font = self.font;
        let mut edit = Edit {
            chars: value.chars().collect(),
            caret: 0,
            anchor: 0,
            max: max_chars,
        };
        let n = edit.chars.len();
        edit.caret = data.caret.min(n);
        edit.anchor = data.anchor.min(n);
        let first = data.first.min(edit.caret);
        let (caret, anchor) = (edit.caret, edit.anchor);
        let text_x = rect.x + PAD;
        let text_y = rect.y + (rect.h - GLYPH_HEIGHT as i32) / 2;
        let inner_w = (rect.w - 2 * PAD).max(0);

        // The mouse: a press puts the caret under the cursor, a drag extends the selection.
        if let Some(cursor) = it.cursor {
            let at = index_at(font, &edit.chars, first, cursor.x - text_x);
            if it.pressed {
                edit.move_to(at, it.shift);
            } else if it.dragging {
                edit.move_to(at, true);
            }
        }
        // Typed text and keys, in the order they came.
        let mut submitted = false;
        let mem = &self.state.memory;
        for (id, event) in &mem.text {
            if *id != it.id {
                continue;
            }
            match event {
                TextInput::Text(text) => edit.insert(text),
                // Enter submits on its press, not on the OS repeats of a held Enter.
                TextInput::Key { key, repeat } => {
                    submitted |= edit.key(*key, mem.shift, mem.ctrl) && !repeat;
                }
            }
        }
        let preedit = if it.focused {
            mem.preedit.clone()
        } else {
            String::new()
        };
        if (edit.caret, edit.anchor) != (caret, anchor)
            || edit.chars.len() != n
            || (it.focused && !data.focused)
        {
            data.moved = now;
        }
        let changed = !edit.chars.iter().copied().eq(value.chars());
        if changed {
            *value = edit.chars.iter().collect();
        }

        // Scroll the text to keep the caret in view, with the composition after it, and use the
        // room a shorter text leaves.
        let chars = &edit.chars;
        let composing_w = text::measure(font, &preedit, 1) as i32;
        let mut first = first.min(edit.caret);
        while first < edit.caret && width(font, &chars[first..edit.caret]) + composing_w > inner_w {
            first += 1;
        }
        while first > 0 && width(font, &chars[first - 1..]) + composing_w <= inner_w {
            first -= 1;
        }
        let mut last = first;
        let mut pen = 0;
        while last < chars.len() && pen + font.resolve(chars[last]).advance as i32 <= inner_w {
            pen += font.resolve(chars[last]).advance as i32;
            last += 1;
        }
        let last = last.max(edit.caret);

        // Drawing, cut to the inside of the border.
        let style = self.style;
        let color = if it.enabled {
            style.field_text
        } else {
            style.field_text_disabled
        };
        let slot = if it.focused {
            SkinSlot::TextFieldFocused
        } else {
            SkinSlot::TextField
        };
        self.skin(slot, rect);
        let caret_x = text_x + width(font, &chars[first..edit.caret]);
        let blink_on = (now.saturating_sub(data.moved).as_millis() / CARET_BLINK.as_millis())
            .is_multiple_of(2);
        self.with_clip(rect.shrink(1), |p| {
            if chars.is_empty() && preedit.is_empty() {
                if !it.focused {
                    p.text(text_x, text_y, hint, Text::new(style.field_hint));
                }
            } else {
                let (from, to) = edit.selection();
                let (from, to) = (from.clamp(first, last), to.clamp(first, last));
                if from < to {
                    let x0 = text_x + width(font, &chars[first..from]);
                    let x1 = text_x + width(font, &chars[first..to]);
                    let h = GLYPH_HEIGHT as i32 + 2;
                    p.rect(
                        UiRect::new(x0, text_y - 1, x1 - x0, h),
                        style.field_selection,
                    );
                }
                let before: String = chars[first..edit.caret].iter().collect();
                let after: String = chars[edit.caret..last].iter().collect();
                let w = p.text(text_x, text_y, &before, Text::new(color));
                let composing = p.text(caret_x, text_y, &preedit, Text::new(color));
                if composing > 0 {
                    let underline =
                        UiRect::new(caret_x, text_y + GLYPH_HEIGHT as i32, composing, 1);
                    p.rect(underline, color);
                }
                p.text(text_x + w + composing, text_y, &after, Text::new(color));
            }
            // Minecraft's caret: an underscore after the text, a bar inside it or when full.
            if it.focused && preedit.is_empty() && blink_on {
                if edit.caret < chars.len() || chars.len() >= max_chars {
                    let bar = UiRect::new(caret_x, text_y - 1, 1, GLYPH_HEIGHT as i32 + 2);
                    p.rect(bar, style.caret);
                } else {
                    p.text(caret_x, text_y, "_", Text::new(color));
                }
            }
        });
        if it.focused {
            let n = self.scale.scale();
            let position = self.scale.to_physical(IVec2::new(caret_x, rect.y));
            self.state.memory.ime_area = Some(ImeCursorArea {
                position,
                size: UVec2::new(n, rect.h.max(0) as u32 * n),
            });
        }

        data.caret = edit.caret;
        data.anchor = edit.anchor;
        data.first = first;
        data.focused = it.focused;
        *self.widget_data::<FieldData>(it.id) = data;
        Resp {
            painter: self,
            response: Response {
                clicked: it.pressed,
                changed,
                hovered: it.hovered,
                focused: it.focused,
                submitted,
                ..Response::default()
            },
            rect,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(text: &str, caret: usize) -> Edit {
        Edit {
            chars: text.chars().collect(),
            caret,
            anchor: caret,
            max: 12,
        }
    }

    fn text(edit: &Edit) -> String {
        edit.chars.iter().collect()
    }

    #[test]
    fn typing_replaces_the_selection_and_stops_at_the_maximum() {
        let mut e = edit("año", 1);
        e.insert("ñé");
        assert_eq!((text(&e), e.caret), ("añéño".to_owned(), 3));
        e.anchor = 0;
        e.insert("X");
        assert_eq!((text(&e), e.caret), ("Xño".to_owned(), 1));
        e.insert("0123456789AB");
        assert_eq!(text(&e), "X012345678ño");
        assert_eq!(e.caret, 10);
        e.insert("Z");
        assert_eq!(text(&e), "X012345678ño");
    }

    #[test]
    fn keys_move_select_and_erase() {
        let mut e = edit("uno dos  tres", 13);
        e.key(KeyCode::Backspace, false, true);
        assert_eq!(text(&e), "uno dos  ");
        e.key(KeyCode::Backspace, false, true);
        assert_eq!(text(&e), "uno ");
        e.key(KeyCode::Home, false, false);
        e.key(KeyCode::ArrowRight, true, true);
        assert_eq!(e.selection(), (0, 4));
        e.key(KeyCode::ArrowLeft, false, false);
        assert_eq!(
            (e.caret, e.anchor),
            (0, 0),
            "Left drops the selection at its start"
        );
        e.key(KeyCode::Delete, false, false);
        assert_eq!(text(&e), "no ");
        e.key(KeyCode::End, true, false);
        assert_eq!(e.selection(), (0, 3));
        e.key(KeyCode::Delete, false, false);
        assert_eq!((text(&e), e.caret), (String::new(), 0));
        // Nothing to erase: nothing happens.
        e.key(KeyCode::Backspace, false, false);
        e.key(KeyCode::Delete, false, true);
        assert_eq!(text(&e), "");

        let mut e = edit("hola mundo", 2);
        e.key(KeyCode::KeyA, false, true);
        assert_eq!(e.selection(), (0, 10));
        e.key(KeyCode::KeyA, false, false);
        assert_eq!(
            e.selection(),
            (0, 10),
            "A without Ctrl is typed text, not a key"
        );
        assert!(e.key(KeyCode::Enter, false, false));
        assert!(!e.key(KeyCode::Tab, false, false));
    }
}
