use crate::layout::UiRect;
use crate::memory::{Hit, WidgetKind};
use crate::skin::SkinSlot;
use crate::ui::{Painter, Resp, Response};
use std::ops::Range;
use std::time::Duration;

/// Two presses on the same row of a list within this much real time are a double click
/// (Minecraft's 250 ms).
pub const DOUBLE_CLICK: Duration = Duration::from_millis(250);

/// The width of a list's scrollbar in GUI pixels (Minecraft's 6). A list keeps it, plus a gap of
/// [`SCROLLBAR_GAP`], at its right edge even while everything fits, so its rows never move.
pub const SCROLLBAR_WIDTH: i32 = 6;

/// The gap between a list's rows and its scrollbar.
pub const SCROLLBAR_GAP: i32 = 2;

/// The shortest scrollbar handle (Minecraft's 32), unless the list itself is shorter.
const MIN_HANDLE: i32 = 32;

/// A row as [`Painter::list`] draws it, for the function that fills it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ListRow {
    /// Which row.
    pub index: usize,
    /// Where it is now in GUI pixels, scrolled. Drawing is cut to the list.
    pub rect: UiRect,
    /// It is the list's selection.
    pub selected: bool,
    /// The cursor is over the part of it that shows (also over a widget it declared).
    pub hovered: bool,
}

/// What a list keeps between frames.
#[derive(Debug, Default)]
struct ListData {
    /// How far down the rows are scrolled, in GUI pixels.
    scroll: i32,
    /// The part of the wheel's turns not scrolled yet (under a pixel, from a touchpad).
    wheel_rest: f32,
    /// While the scrollbar's handle is dragged: where the cursor holds it, from its top.
    grab: Option<i32>,
    /// The latest press on a row: which and when (real time).
    last_press: Option<(usize, Duration)>,
    /// Where the first row's top was last frame (`rect.y - scroll`), to place the rectangles
    /// its rows' widgets declared then and the row a press lands on.
    content_y: i32,
}

/// The scroll that shows `h` pixels from `top` (the top when they do not fit), moving as little
/// as possible from `scroll` in a view `view` pixels high.
fn reveal(scroll: i32, top: i32, h: i32, view: i32) -> i32 {
    if top < scroll {
        top
    } else if top + h > scroll + view {
        (top + h - view).min(top)
    } else {
        scroll
    }
}

/// The scrollbar: its track, the handle's height and how far the handle travels.
#[derive(Clone, Copy, Debug)]
struct Scrollbar {
    track: UiRect,
    handle: i32,
    travel: i32,
}

impl Scrollbar {
    /// Minecraft's handle: as long as the view is of the content, at least [`MIN_HANDLE`], and
    /// 8 pixels short of the track.
    fn new(rect: UiRect, content: i64) -> Self {
        let track = UiRect::new(
            rect.right() - SCROLLBAR_WIDTH,
            rect.y,
            SCROLLBAR_WIDTH,
            rect.h,
        );
        let h = i64::from(track.h.max(0));
        let low = MIN_HANDLE.min(track.h).max(0);
        let high = (track.h - 8).max(low);
        let handle = (h * h / content.max(1)).clamp(i64::from(low), i64::from(high)) as i32;
        Self {
            track,
            handle,
            travel: track.h - handle,
        }
    }

    /// The handle's top at `scroll`.
    fn top(&self, scroll: i32, max_scroll: i32) -> i32 {
        if max_scroll <= 0 {
            return self.track.y;
        }
        let along = i64::from(scroll) * i64::from(self.travel) / i64::from(max_scroll);
        self.track.y + along as i32
    }

    /// The scroll that puts the handle's top at `top` (rounded to the nearest pixel).
    fn scroll_at(&self, top: i32, max_scroll: i32) -> i32 {
        if self.travel <= 0 {
            return 0;
        }
        let along = i64::from((top - self.track.y).clamp(0, self.travel));
        let travel = i64::from(self.travel);
        ((along * i64::from(max_scroll) + travel / 2) / travel) as i32
    }
}

/// Declares a list's rows through `declare`, which fills one row and says whether it declared a
/// widget the keyboard can focus. Each row once, but not in row order (see [`sort_rows`]):
///
/// - the rows `first..end` that show, and one more on each side, so Tab and Shift+Tab walk in
///   from outside the list;
/// - while the keyboard focus is on the widget of row `focused`: that row, even out of view
///   (the wheel can scroll it away), and the rows from it to the nearest one on each side with a
///   widget to focus, past rows with nothing to focus (a header, a disabled setting). So Tab and
///   Shift+Tab go on from the focused widget one row at a time. The cost grows with the rows
///   skipped, not with the list.
fn declare_rows(
    first: usize,
    end: usize,
    rows: usize,
    focused: Option<usize>,
    mut declare: impl FnMut(usize) -> bool,
) {
    let (from, to) = (first.saturating_sub(1), (end + 1).min(rows));
    let shown: Vec<bool> = (from..to).map(&mut declare).collect();
    let Some(focused) = focused.filter(|&r| r < rows) else {
        return;
    };
    let mut visit = |r: usize| {
        if (from..to).contains(&r) {
            shown[r - from]
        } else {
            declare(r)
        }
    };
    visit(focused);
    for r in focused + 1..rows {
        if visit(r) {
            break;
        }
    }
    for r in (0..focused).rev() {
        if visit(r) {
            break;
        }
    }
}

/// Puts the hits of a list's rows back in row order, the order the keyboard walks them in.
/// `spans` holds each row and its first hit in the order [`declare_rows`] declared them; each
/// row's hits run to the next one's, the last row's to the end.
fn sort_rows(hits: &mut Vec<Hit>, spans: &[(usize, usize)]) {
    if spans.is_sorted_by_key(|&(row, _)| row) {
        return;
    }
    let start = spans[0].1;
    let end = hits.len();
    let mut runs: Vec<(usize, Range<usize>)> = spans
        .iter()
        .enumerate()
        .map(|(i, &(row, at))| {
            let next = spans.get(i + 1).map_or(end, |&(_, at)| at);
            (row, at - start..next - start)
        })
        .collect();
    runs.sort_unstable_by_key(|&(row, _)| row);
    let mut moved: Vec<Option<Hit>> = hits.drain(start..).map(Some).collect();
    for (_, run) in runs {
        hits.extend(moved[run].iter_mut().filter_map(Option::take));
    }
}

impl<'a> Painter<'a> {
    /// A scrolling list of `rows` rows `row_h` GUI pixels high over `rect`, with `selected` as
    /// its selection (Minecraft's world list). `label` names it (identity, tests), it is not
    /// drawn.
    ///
    /// `row` fills one row: it gets the row's rectangle, scrolled, and draws cut to the list.
    /// It is called for the rows that show and the one just past each side of the view and,
    /// while a row's widget has the keyboard focus, for that row and the rows from it to the
    /// nearest one on each side with a widget to focus (out of view, cut away entirely, they
    /// draw nothing); not in row order. The widgets it declares are scoped by the row, so
    /// every row can have a button with the same label. Those widgets take the pointer before
    /// the list; a press elsewhere on a row selects it. Tab and Shift+Tab walk into the rows
    /// out of view one by one, past rows with nothing to focus (a header, a disabled setting),
    /// and the list scrolls to show the widget the keyboard focuses; a focused widget keeps the
    /// focus while the wheel scrolls it away, and Tab goes on from it.
    ///
    /// The wheel scrolls half a row per line, wherever the cursor is over the list. The
    /// scrollbar ([`SCROLLBAR_WIDTH`] at the right edge, shown when the rows do not fit) follows
    /// a drag of its handle; a press on its track brings the handle's middle there. While the
    /// list has the focus, Up and Down move the selection (scrolling to it), and Enter or Space
    /// open it.
    ///
    /// The response: `clicked` when a row was pressed, `changed` when the selection changed
    /// (also when a selection past the last row was cleared), `double_clicked` when the same
    /// row was pressed twice within [`DOUBLE_CLICK`] of real time (needs a `Time` resource) or
    /// Enter or Space opened the selected row.
    pub fn list(
        &mut self,
        rect: UiRect,
        label: &str,
        row_h: i32,
        rows: usize,
        selected: &mut Option<usize>,
        row: impl FnMut(&mut Self, ListRow),
    ) -> Resp<'_, 'a> {
        self.rows_widget(rect, label, row_h, rows, Some(selected), row)
    }

    /// A scrolling list without a selection, for rows that hold widgets (an options page): as
    /// [`Painter::list`] but the list itself takes no focus and draws no selection, and a press
    /// on a row does nothing. The keyboard goes through the rows' widgets.
    pub fn scroll_list(
        &mut self,
        rect: UiRect,
        label: &str,
        row_h: i32,
        rows: usize,
        row: impl FnMut(&mut Self, ListRow),
    ) -> Resp<'_, 'a> {
        self.rows_widget(rect, label, row_h, rows, None, row)
    }

    fn rows_widget(
        &mut self,
        rect: UiRect,
        label: &str,
        row_h: i32,
        rows: usize,
        mut selected: Option<&mut Option<usize>>,
        mut row: impl FnMut(&mut Self, ListRow),
    ) -> Resp<'_, 'a> {
        let kind = if selected.is_some() {
            WidgetKind::List
        } else {
            WidgetKind::ScrollList
        };
        let it = self.interact(rect, kind, label);
        let row_h = row_h.max(1);
        let content = rows as i64 * i64::from(row_h);
        let max_scroll = (content - i64::from(rect.h)).clamp(0, i64::from(i32::MAX / 2)) as i32;
        let rows_w = (rect.w - SCROLLBAR_WIDTH - SCROLLBAR_GAP).max(0);
        let bar = Scrollbar::new(rect, content);
        let mut data = std::mem::take(self.widget_data::<ListData>(it.id));
        let mut response = Response {
            hovered: it.hovered,
            focused: it.focused,
            ..Response::default()
        };
        if let Some(selected) = selected.as_deref_mut()
            && selected.is_some_and(|s| s >= rows)
        {
            *selected = None;
            response.changed = true;
        }
        let current = selected.as_deref().copied().flatten();
        // The scroll the last frame drew, which a press goes by (as every widget's, against the
        // last frame's rectangles): the wheel, the keyboard and a reveal move it below, and
        // unclamped yet.
        let shown = data.scroll;

        // The wheel, and a row widget the keyboard just focused.
        let mem = &self.state.memory;
        if let Some((id, lines)) = mem.wheel
            && id == it.id
        {
            let pixels = data.wheel_rest - lines * row_h as f32 / 2.0;
            let whole = pixels.trunc();
            data.wheel_rest = pixels - whole;
            data.scroll = data.scroll.saturating_add(whole as i32);
        }
        if let Some((list, full)) = mem.reveal
            && list == it.id
        {
            data.scroll = reveal(data.scroll, full.y - data.content_y, full.h, rect.h);
        }
        let focused_row = mem
            .focused_row
            .filter(|&(list, _)| list == it.id)
            .map(|(_, row)| row);

        // Up and Down move the selection, Enter and Space open it.
        let mut pick = None;
        if it.nudge_y != 0 && rows > 0 {
            let from = current.map_or(if it.nudge_y > 0 { -1 } else { rows as i64 }, |s| s as i64);
            let index = (from + i64::from(it.nudge_y)).clamp(0, rows as i64 - 1) as usize;
            let top = (index as i64 * i64::from(row_h)).min(i64::from(i32::MAX / 2)) as i32;
            data.scroll = reveal(data.scroll, top, row_h, rect.h);
            pick = Some(index);
        }
        response.double_clicked = it.activated && current.is_some();

        // A press on the scrollbar grabs its handle, one on a row selects it.
        if it.pressed
            && let Some(cursor) = it.cursor
        {
            if cursor.x >= bar.track.x {
                if max_scroll > 0 {
                    let top = bar.top(shown.min(max_scroll), max_scroll);
                    let on_handle = (top..top + bar.handle).contains(&cursor.y);
                    data.grab = Some(if on_handle {
                        cursor.y - top
                    } else {
                        bar.handle / 2
                    });
                }
            } else if cursor.x < rect.x + rows_w && selected.is_some() {
                // The row drawn under the cursor, from where the first row's top was.
                let at =
                    (i64::from(cursor.y) - i64::from(data.content_y)).div_euclid(i64::from(row_h));
                if (0..rows as i64).contains(&at) {
                    let at = at as usize;
                    let now = self.now();
                    let double = match (data.last_press, now) {
                        (Some((last, then)), Some(now)) => {
                            last == at && now.saturating_sub(then) <= DOUBLE_CLICK
                        }
                        _ => false,
                    };
                    // A third press starts over, so it is not a second double click.
                    data.last_press = if double {
                        None
                    } else {
                        now.map(|now| (at, now))
                    };
                    response.clicked = true;
                    response.double_clicked |= double;
                    pick = Some(at);
                }
            }
        }
        // The press's own frame counts too: a press and a release within one frame (a slow
        // frame, a synthetic click) leave `dragging` already off.
        if let Some(grab) = data.grab
            && (it.pressed || it.dragging)
            && let Some(cursor) = it.cursor
        {
            data.scroll = bar.scroll_at(cursor.y - grab, max_scroll);
        }
        if !it.dragging {
            data.grab = None;
        }
        if let Some(selected) = selected
            && let Some(pick) = pick
            && *selected != Some(pick)
        {
            *selected = Some(pick);
            response.changed = true;
        }
        let chosen = pick.or(current);
        data.scroll = data.scroll.clamp(0, max_scroll);

        // The background, the rows (cut to the rows' area) and the scrollbar.
        self.skin(SkinSlot::ListBackground, rect);
        let area = UiRect::new(rect.x, rect.y, rows_w, rect.h);
        let scroll = i64::from(data.scroll);
        let first = (scroll / i64::from(row_h)) as usize;
        let end = (scroll + i64::from(rect.h) + i64::from(row_h) - 1) / i64::from(row_h);
        let end = end.clamp(0, rows as i64) as usize;
        let focused = it.focused;
        let layer = self.layer.index();
        // Each declared row and where its hits start, to put them back in row order.
        let mut spans = Vec::new();
        self.with_clip(area, |p| {
            declare_rows(first, end, rows, focused_row, |index| {
                // Far rows (the focused one, those past it) saturate instead of wrapping in.
                let top = (index as i64).saturating_mul(i64::from(row_h)) - scroll;
                let far = i64::from(i32::MAX / 4);
                let row_rect =
                    UiRect::new(rect.x, rect.y + top.clamp(-far, far) as i32, rows_w, row_h);
                let is_selected = chosen == Some(index);
                if is_selected {
                    p.selection_box(row_rect, focused);
                }
                let hovered = p.pointer_over(row_rect);
                let start = p.state.layers[layer].hits.len();
                spans.push((index, start));
                p.scopes.push(it.id.mix(&(index as u64).to_le_bytes()));
                p.rows.push((it.id, index));
                row(
                    p,
                    ListRow {
                        index,
                        rect: row_rect,
                        selected: is_selected,
                        hovered,
                    },
                );
                p.rows.pop();
                p.scopes.pop();
                p.state.layers[layer].hits[start..]
                    .iter()
                    .any(|h| h.enabled && h.kind.focusable() && h.row == Some((it.id, index)))
            });
        });
        sort_rows(&mut self.state.layers[layer].hits, &spans);
        if max_scroll > 0 {
            self.skin(SkinSlot::ScrollerBackground, bar.track);
            let top = bar.top(data.scroll, max_scroll);
            let handle = UiRect::new(bar.track.x, top, bar.track.w, bar.handle);
            self.skin(SkinSlot::Scroller, handle);
        }
        data.content_y = rect.y - data.scroll;
        *self.widget_data::<ListData>(it.id) = data;
        Resp {
            painter: self,
            response,
            rect,
        }
    }

    /// A selected row: an outline, white while the list has the focus, filled black (as
    /// Minecraft's).
    fn selection_box(&mut self, rect: UiRect, focused: bool) {
        let style = self.style;
        let outline = if focused {
            style.selection
        } else {
            style.selection_unfocused
        };
        self.rect(rect, outline);
        self.rect(rect.shrink(1), style.selection_fill);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::WidgetId;

    #[test]
    fn rows_declared_around_the_view_and_on_from_the_focused_one() {
        // The rows declared, in order, when the rows in `quiet` have nothing to focus.
        let rows = |first, end, count, focused, quiet: &[usize]| {
            let mut declared = Vec::new();
            declare_rows(first, end, count, focused, |r| {
                declared.push(r);
                !quiet.contains(&r)
            });
            declared
        };
        assert_eq!(rows(0, 5, 10, None, &[]), [0, 1, 2, 3, 4, 5]);
        assert_eq!(rows(3, 8, 10, None, &[]), [2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(rows(5, 10, 10, None, &[]), [4, 5, 6, 7, 8, 9]);
        assert_eq!(rows(0, 0, 0, None, &[]), [0usize; 0]);
        // Nothing focused: rows with nothing to focus change nothing.
        assert_eq!(rows(0, 5, 10, None, &[5, 6]), [0, 1, 2, 3, 4, 5]);
        // The focused row out of view comes with the rows next to it.
        assert_eq!(rows(5, 10, 10, Some(1), &[]), [4, 5, 6, 7, 8, 9, 1, 2, 0]);
        assert_eq!(rows(0, 3, 10, Some(8), &[]), [0, 1, 2, 3, 8, 9, 7]);
        // In view, they are there already, but for the one past the extra row.
        assert_eq!(rows(0, 3, 10, Some(2), &[]), [0, 1, 2, 3]);
        assert_eq!(rows(0, 3, 10, Some(3), &[]), [0, 1, 2, 3, 4]);
        assert_eq!(rows(0, 3, 10, Some(10), &[]), [0, 1, 2, 3]);
        // On past rows with nothing to focus, to the nearest one with something on each side.
        assert_eq!(rows(0, 5, 12, Some(4), &[5, 6]), [0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(
            rows(7, 12, 12, Some(7), &[5, 6]),
            [6, 7, 8, 9, 10, 11, 5, 4]
        );
        assert_eq!(
            rows(9, 12, 20, Some(1), &[2, 3]),
            [8, 9, 10, 11, 12, 1, 2, 3, 4, 0]
        );
        // ... or to the end.
        assert_eq!(rows(0, 2, 6, Some(1), &[2, 3, 4, 5]), [0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn row_hits_go_back_to_row_order() {
        let hit = |label: &str| Hit {
            id: WidgetId(label.len() as u64),
            rect: UiRect::default(),
            full: UiRect::default(),
            row: None,
            enabled: true,
            kind: WidgetKind::Button,
            label: label.to_owned(),
        };
        let mut hits = ["before", "4a", "4b", "5", "1", "2a", "2b", "0"]
            .map(hit)
            .to_vec();
        // Row 3 declared nothing.
        let spans = [(4, 1), (5, 3), (3, 4), (1, 4), (2, 5), (0, 7)];
        sort_rows(&mut hits, &spans);
        let labels: Vec<&str> = hits.iter().map(|h| h.label.as_str()).collect();
        assert_eq!(labels, ["before", "0", "1", "2a", "2b", "4a", "4b", "5"]);
        // In order already: untouched.
        sort_rows(&mut hits, &[(0, 1), (1, 2), (2, 3)]);
        let labels: Vec<&str> = hits.iter().map(|h| h.label.as_str()).collect();
        assert_eq!(labels, ["before", "0", "1", "2a", "2b", "4a", "4b", "5"]);
        sort_rows(&mut hits, &[]);
        assert_eq!(hits.len(), 8);
    }

    #[test]
    fn reveal_moves_as_little_as_possible() {
        // A 100-pixel view at 40: rows from 40 to 140 show.
        assert_eq!(reveal(40, 60, 20, 100), 40);
        assert_eq!(reveal(40, 20, 20, 100), 20);
        assert_eq!(reveal(40, 130, 20, 100), 50);
        // Taller than the view: its top.
        assert_eq!(reveal(40, 200, 150, 100), 200);
    }

    #[test]
    fn the_scrollbar_handle_follows_minecraft() {
        // 100 high over 400 of content: 25 by the ratio, raised to 32.
        let bar = Scrollbar::new(UiRect::new(0, 10, 200, 100), 400);
        assert_eq!((bar.track.x, bar.track.w), (194, 6));
        assert_eq!((bar.handle, bar.travel), (32, 68));
        assert_eq!(bar.top(0, 300), 10);
        assert_eq!(bar.top(300, 300), 78);
        assert_eq!(bar.top(150, 300), 44);
        // The handle's top back to the scroll, at the ends and the middle.
        assert_eq!(bar.scroll_at(10, 300), 0);
        assert_eq!(bar.scroll_at(78, 300), 300);
        assert_eq!(bar.scroll_at(44, 300), 150);
        assert_eq!(bar.scroll_at(-50, 300), 0);
        assert_eq!(bar.scroll_at(500, 300), 300);
        // Little more content than view: 8 short of the track.
        assert_eq!(Scrollbar::new(UiRect::new(0, 0, 50, 100), 101).handle, 92);
        // A list shorter than the minimum handle.
        let short = Scrollbar::new(UiRect::new(0, 0, 50, 20), 400);
        assert_eq!((short.handle, short.travel), (20, 0));
        assert_eq!(short.scroll_at(5, 380), 0);
    }
}
