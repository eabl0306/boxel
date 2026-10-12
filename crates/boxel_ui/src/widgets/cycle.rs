use crate::layout::UiRect;
use crate::memory::WidgetKind;
use crate::skin::SkinSlot;
use crate::ui::{Painter, Resp, Response};

impl<'a> Painter<'a> {
    /// A button showing `"label: value"` that steps through `options` (`(value, its text)`):
    /// a click or Enter goes to the next one, Shift+click and Left to the previous one, Right
    /// to the next one, wrapping around. A value not in `options` shows no text and steps from
    /// the first option.
    pub fn cycle<T: PartialEq + Copy>(
        &mut self,
        rect: UiRect,
        label: &str,
        value: &mut T,
        options: &[(T, &str)],
    ) -> Resp<'_, 'a> {
        let it = self.interact(rect, WidgetKind::Cycle, label);
        let mut step = i64::from(it.nudge);
        if it.pressed {
            step += if it.shift { -1 } else { 1 };
        }
        if it.activated {
            step += 1;
        }
        let at = options.iter().position(|(v, _)| v == value);
        let mut changed = false;
        if step != 0 && !options.is_empty() {
            let len = options.len() as i64;
            let start = at.map_or(if step > 0 { -1 } else { 0 }, |i| i as i64);
            let next = (start + step).rem_euclid(len) as usize;
            if options[next].0 != *value {
                *value = options[next].0;
                changed = true;
            }
        }
        let shown = options
            .iter()
            .find(|(v, _)| v == value)
            .map_or("", |(_, text)| text);
        let text = format!("{label}{}{shown}", self.style.separator);
        let slot = if !it.enabled {
            SkinSlot::ButtonDisabled
        } else if it.hovered || it.focused {
            SkinSlot::ButtonHovered
        } else {
            SkinSlot::Button
        };
        self.skin(slot, rect);
        self.centered_label(rect, &text, it.enabled);
        Resp {
            painter: self,
            response: Response {
                clicked: it.pressed || it.activated,
                changed,
                hovered: it.hovered,
                focused: it.focused,
                ..Response::default()
            },
            rect,
        }
    }

    /// A cycle of two values, on and off, shown with [`crate::UiStyle::on`] and
    /// [`crate::UiStyle::off`].
    pub fn toggle(&mut self, rect: UiRect, label: &str, value: &mut bool) -> Resp<'_, 'a> {
        let style = self.style;
        self.cycle(
            rect,
            label,
            value,
            &[(true, style.on.as_str()), (false, style.off.as_str())],
        )
    }
}
