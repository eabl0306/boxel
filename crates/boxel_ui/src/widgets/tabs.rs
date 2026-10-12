use crate::font::GLYPH_HEIGHT;
use crate::layout::UiRect;
use crate::memory::WidgetKind;
use crate::skin::SkinSlot;
use crate::text;
use crate::ui::{Painter, Resp, Response};

impl<'a> Painter<'a> {
    /// A tab bar over `rect`: one tab per label, side by side and touching, `selected` the open
    /// one (the game draws that tab's page under the bar). A press, or Enter or Space on a
    /// focused tab, opens it. Every tab takes the focus on its own (Tab goes through them);
    /// Left and Right on a focused tab open the one next to it and move the focus there.
    /// Ctrl+Tab and Ctrl+Shift+Tab open the next and the previous tab, wrapping around, on the
    /// first tab bar of the modal layer. The open tab's label is underlined.
    ///
    /// The response covers the whole bar: `clicked` when a tab was pressed or activated,
    /// `changed` when another tab opened (also when `selected` was past the last one), and
    /// `hovered` and `focused` when any tab is.
    pub fn tabs(&mut self, rect: UiRect, selected: &mut usize, labels: &[&str]) -> Resp<'_, 'a> {
        // `enabled` and `id` apply to the whole bar.
        let enabled = std::mem::replace(&mut self.next_enabled, true);
        let scope = self.next_id.take();
        self.scopes.extend(scope);
        let parts = rect.split_n(labels.len(), 0);
        let mut tabs = Vec::with_capacity(labels.len());
        for (part, label) in parts.iter().zip(labels) {
            self.next_enabled = enabled;
            tabs.push(self.interact(*part, WidgetKind::Tab, label));
        }
        if scope.is_some() {
            self.scopes.pop();
        }

        let mut response = Response::default();
        let Some(last) = labels.len().checked_sub(1) else {
            return Resp {
                painter: self,
                response,
                rect,
            };
        };
        let mut open = (*selected).min(last);
        let mut by_key = false;
        for (i, tab) in tabs.iter().enumerate() {
            response.hovered |= tab.hovered;
            response.focused |= tab.focused;
            if tab.pressed || tab.activated {
                open = i;
                response.clicked = true;
            }
            if tab.nudge != 0 {
                open = (i as i64 + i64::from(tab.nudge)).clamp(0, last as i64) as usize;
                by_key = true;
            }
        }
        let mem = &mut self.state.memory;
        if enabled && mem.tab_step != 0 && mem.modal == Some(self.layer) {
            let count = last as i64 + 1;
            open = (open as i64 + i64::from(mem.tab_step)).rem_euclid(count) as usize;
            mem.tab_step = 0;
        }
        if by_key {
            mem.focused = Some(tabs[open].id);
            mem.focus_visible = true;
        }
        if open != *selected {
            *selected = open;
            response.changed = true;
        }

        for (i, (tab, part)) in tabs.iter().zip(&parts).enumerate() {
            let lit = tab.enabled && (tab.hovered || tab.focused);
            let slot = match (i == open, lit) {
                (true, true) => SkinSlot::TabSelectedHovered,
                (true, false) => SkinSlot::TabSelected,
                (false, true) => SkinSlot::TabHovered,
                (false, false) => SkinSlot::Tab,
            };
            self.skin(slot, *part);
            self.centered_label(*part, labels[i], tab.enabled);
            if i == open {
                // Under the label as `centered_label` places it, one pixel below the glyphs.
                let max = (part.w - 6).max(0) as u32;
                let line = text::ellipsis(self.font, labels[i], max, 1);
                let w = self.measure(&line, 1) as i32;
                let y = part.y + (part.h - GLYPH_HEIGHT as i32) / 2 + GLYPH_HEIGHT as i32 + 1;
                let x = part.x + part.w / 2 - w / 2;
                let color = if tab.enabled {
                    self.style.text
                } else {
                    self.style.text_disabled
                };
                self.rect(UiRect::new(x, y, (w - 1).max(0), 1), color);
            }
        }
        Resp {
            painter: self,
            response,
            rect,
        }
    }
}
