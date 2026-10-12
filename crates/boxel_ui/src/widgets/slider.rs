use crate::layout::UiRect;
use crate::memory::WidgetKind;
use crate::skin::SkinSlot;
use crate::ui::{Painter, Resp, Response};

/// The values a slider can take.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Steps {
    /// `min`, `min + step`, ... up to `max` (`max` included only when it falls on a step).
    Range {
        /// First value.
        min: i32,
        /// Last value.
        max: i32,
        /// Distance between values (at least 1).
        step: i32,
    },
    /// These values, in this order.
    List(&'static [i32]),
}

impl Steps {
    /// `min..=max` in steps of 1.
    pub const fn range(min: i32, max: i32) -> Self {
        Self::Range { min, max, step: 1 }
    }

    /// How many values.
    pub fn count(&self) -> usize {
        match *self {
            Self::Range { min, max, step } => {
                if max < min {
                    1
                } else {
                    ((i64::from(max) - i64::from(min)) / i64::from(step.max(1))) as usize + 1
                }
            }
            Self::List(values) => values.len().max(1),
        }
    }

    /// Value number `i` (clamped to the last).
    pub fn value(&self, i: usize) -> i32 {
        let i = i.min(self.count() - 1);
        match *self {
            Self::Range { min, step, .. } => min + i as i32 * step.max(1),
            Self::List(values) => values.get(i).copied().unwrap_or(0),
        }
    }

    /// The number of the value nearest to `v` (the first on a tie).
    pub fn index_of(&self, v: i32) -> usize {
        (0..self.count())
            .min_by_key(|&i| (i64::from(self.value(i)) - i64::from(v)).abs())
            .unwrap_or(0)
    }
}

/// The handle's width in GUI pixels.
const HANDLE_W: i32 = 8;

impl<'a> Painter<'a> {
    /// A slider showing `"label: fmt(value)"` over its track. Pressing or dragging along it
    /// picks the nearest step under the cursor; Left and Right move one step while focused.
    /// A `value` between steps snaps to the nearest one when it changes.
    pub fn slider(
        &mut self,
        rect: UiRect,
        label: &str,
        value: &mut i32,
        steps: Steps,
        fmt: impl FnOnce(i32) -> String,
    ) -> Resp<'_, 'a> {
        let it = self.interact(rect, WidgetKind::Slider, label);
        let n = steps.count();
        let last = (n - 1) as i64;
        let span = i64::from((rect.w - HANDLE_W).max(1));
        let mut index = steps.index_of(*value) as i64;
        if (it.pressed || it.dragging)
            && let Some(cursor) = it.cursor
        {
            let rel = i64::from(cursor.x - rect.x - HANDLE_W / 2).clamp(0, span);
            index = (rel * last + span / 2) / span;
        }
        index = (index + i64::from(it.nudge)).clamp(0, last);
        let new = steps.value(index as usize);
        let changed = new != *value && (it.pressed || it.dragging || it.nudge != 0);
        if changed {
            *value = new;
        }

        let lit = it.enabled && (it.hovered || it.focused);
        let track = if lit {
            SkinSlot::SliderTrackHovered
        } else {
            SkinSlot::SliderTrack
        };
        self.skin(track, rect);
        let handle_x = if last > 0 {
            rect.x + ((index * span + last / 2) / last) as i32
        } else {
            rect.x
        };
        let handle = if lit {
            SkinSlot::SliderHandleHovered
        } else {
            SkinSlot::SliderHandle
        };
        self.skin(handle, UiRect::new(handle_x, rect.y, HANDLE_W, rect.h));
        let text = format!("{label}{}{}", self.style.separator, fmt(*value));
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_count_values_and_snap() {
        let fov = Steps::range(30, 110);
        assert_eq!(fov.count(), 81);
        assert_eq!(fov.value(40), 70);
        assert_eq!(fov.index_of(70), 40);
        assert_eq!(fov.index_of(500), 80);
        let by_ten = Steps::Range {
            min: 0,
            max: 100,
            step: 10,
        };
        assert_eq!(by_ten.count(), 11);
        assert_eq!(by_ten.index_of(44), 4);
        assert_eq!(by_ten.index_of(45), 4);
        assert_eq!(by_ten.index_of(46), 5);
        let fps = Steps::List(&[30, 60, 144, 0]);
        assert_eq!(fps.count(), 4);
        assert_eq!(fps.value(2), 144);
        assert_eq!(fps.index_of(0), 3);
        assert_eq!(fps.value(9), 0);
    }
}
