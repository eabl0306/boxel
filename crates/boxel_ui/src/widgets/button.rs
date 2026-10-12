use crate::layout::UiRect;
use crate::memory::WidgetKind;
use crate::skin::SkinSlot;
use crate::ui::{Painter, Resp, Response};

impl<'a> Painter<'a> {
    /// A button with `label` centered on it. It activates when the left button goes down on
    /// it (as Minecraft's), or with Enter or Space while focused.
    pub fn button(&mut self, rect: UiRect, label: &str) -> Resp<'_, 'a> {
        let it = self.interact(rect, WidgetKind::Button, label);
        let slot = if !it.enabled {
            SkinSlot::ButtonDisabled
        } else if it.hovered || it.focused {
            SkinSlot::ButtonHovered
        } else {
            SkinSlot::Button
        };
        self.skin(slot, rect);
        self.centered_label(rect, label, it.enabled);
        Resp {
            painter: self,
            response: Response {
                clicked: it.pressed || it.activated,
                hovered: it.hovered,
                focused: it.focused,
                ..Response::default()
            },
            rect,
        }
    }
}
