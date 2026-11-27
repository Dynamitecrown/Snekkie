//! Which tabs show together: up to four columns, each with one or two tabs.
//! Only geometry and rearranging live here; the app draws what this says.

use egui::{Pos2, Rect, pos2};

pub const MAX_COLUMNS: usize = 4;
const MIN_PANE_WIDTH: f32 = 160.0;
const MIN_PANE_HEIGHT: f32 = 100.0;
const DIVIDER: f32 = 7.0;

#[derive(Debug, Clone, PartialEq)]
struct Column<T> {
    /// Its share of the width, relative to the other columns.
    weight: f32,
    top: T,
    bottom: Option<T>,
    /// The top tab's share of the height, when there are two.
    top_share: f32,
}

impl<T> Column<T> {
    fn new(top: T, weight: f32) -> Self {
        Column { weight, top, bottom: None, top_share: 0.5 }
    }
}

/// Where on a pane a dragged tab lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
    /// Takes the pane over.
    Center,
}

impl Edge {
    /// The nearest edge. The middle of a pane counts as its center, if allowed.
    pub fn at(pane: Rect, position: Pos2, allow_center: bool) -> Edge {
        let x = (position.x - pane.left()) / pane.width();
        let y = (position.y - pane.top()) / pane.height();
        let (distance, edge) = [(x, Edge::Left), (1.0 - x, Edge::Right), (y, Edge::Top), (1.0 - y, Edge::Bottom)]
            .into_iter()
            .fold((f32::MAX, Edge::Center), |best, next| if next.0 < best.0 { next } else { best });
        if allow_center && distance > 0.25 { Edge::Center } else { edge }
    }

    /// The part of a pane this edge marks.
    pub fn zone(self, pane: Rect) -> Rect {
        let center = pane.center();
        match self {
            Edge::Left => Rect::from_min_max(pane.min, pos2(center.x, pane.bottom())),
            Edge::Right => Rect::from_min_max(pos2(center.x, pane.top()), pane.max),
            Edge::Top => Rect::from_min_max(pane.min, pos2(pane.right(), center.y)),
            Edge::Bottom => Rect::from_min_max(pos2(pane.left(), center.y), pane.max),
            Edge::Center => pane,
        }
    }
}

/// A divider that can be dragged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Divider {
    /// Between this column and the next.
    Column(usize),
    /// Between the two tabs of this column.
    Row(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TabLayout<T> {
    columns: Vec<Column<T>>,
}

impl<T: Copy + PartialEq> TabLayout<T> {
    pub fn single(tab: T) -> Self {
        TabLayout { columns: vec![Column::new(tab, 1.0)] }
    }

    pub fn pair(left: T, right: T) -> Self {
        TabLayout { columns: vec![Column::new(left, 1.0), Column::new(right, 1.0)] }
    }

    /// Even columns of one or two tabs, as `rows` gives them back. None
    /// unless it holds two tabs or more.
    pub fn from_rows(rows: &[Vec<T>]) -> Option<Self> {
        let columns: Vec<_> = rows
            .iter()
            .filter_map(|tabs| Some(Column { bottom: tabs.get(1).copied(), ..Column::new(*tabs.first()?, 1.0) }))
            .take(MAX_COLUMNS)
            .collect();
        let layout = TabLayout { columns };
        (layout.tabs().len() >= 2).then_some(layout)
    }

    /// Every tab, by column and then from the top.
    pub fn tabs(&self) -> Vec<T> {
        self.columns.iter().flat_map(|column| std::iter::once(column.top).chain(column.bottom)).collect()
    }

    /// The tabs of each column, from the top.
    pub fn rows(&self) -> Vec<Vec<T>> {
        self.columns.iter().map(|column| std::iter::once(column.top).chain(column.bottom).collect()).collect()
    }

    pub fn contains(&self, tab: T) -> bool {
        self.tabs().contains(&tab)
    }

    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    /// Whether every pane gets a usable size in this area.
    pub fn fits(&self, area: Rect) -> bool {
        let columns = self.columns.len() as f32;
        let stacked = self.columns.iter().any(|column| column.bottom.is_some());
        area.width() >= columns * MIN_PANE_WIDTH + (columns - 1.0) * DIVIDER
            && area.height() >= if stacked { 2.0 * MIN_PANE_HEIGHT + DIVIDER } else { MIN_PANE_HEIGHT }
    }

    pub fn panes(&self, area: Rect) -> Vec<(T, Rect)> {
        let mut panes = Vec::new();
        let mut x = area.left();
        for (column, width) in self.columns.iter().zip(self.widths(area)) {
            let right = x + width;
            match column.bottom {
                None => panes.push((column.top, Rect::from_min_max(pos2(x, area.top()), pos2(right, area.bottom())))),
                Some(bottom) => {
                    let split = area.top() + column_top_height(column, area);
                    panes.push((column.top, Rect::from_min_max(pos2(x, area.top()), pos2(right, split))));
                    panes.push((bottom, Rect::from_min_max(pos2(x, split + DIVIDER), pos2(right, area.bottom()))));
                }
            }
            x = right + DIVIDER;
        }
        panes
    }

    pub fn dividers(&self, area: Rect) -> Vec<(Divider, Rect)> {
        let mut dividers = Vec::new();
        let mut x = area.left();
        let count = self.columns.len();
        for (index, (column, width)) in self.columns.iter().zip(self.widths(area)).enumerate() {
            let right = x + width;
            if column.bottom.is_some() {
                let split = area.top() + column_top_height(column, area);
                let rect = Rect::from_min_max(pos2(x, split), pos2(right, split + DIVIDER));
                dividers.push((Divider::Row(index), rect));
            }
            if index + 1 < count {
                let rect = Rect::from_min_max(pos2(right, area.top()), pos2(right + DIVIDER, area.bottom()));
                dividers.push((Divider::Column(index), rect));
            }
            x = right + DIVIDER;
        }
        dividers
    }

    fn widths(&self, area: Rect) -> Vec<f32> {
        let count = self.columns.len() as f32;
        let room = area.width() - (count - 1.0) * DIVIDER;
        let total: f32 = self.columns.iter().map(|column| column.weight).sum();
        let mut widths: Vec<f32> = self.columns.iter().map(|column| room * column.weight / total).collect();
        // A window shrunk since the last drag can leave a pane too small.
        let short: f32 = widths.iter().map(|width| (MIN_PANE_WIDTH - width).max(0.0)).sum();
        let spare: f32 = widths.iter().map(|width| (width - MIN_PANE_WIDTH).max(0.0)).sum();
        if short > 0.0 && spare > 0.0 {
            let taken = (short / spare).min(1.0);
            for width in &mut widths {
                *width =
                    if *width < MIN_PANE_WIDTH { MIN_PANE_WIDTH } else { *width - (*width - MIN_PANE_WIDTH) * taken };
            }
        }
        widths
    }

    pub fn drag_divider(&mut self, area: Rect, divider: Divider, pointer: Pos2) {
        match divider {
            Divider::Column(index) => {
                let widths = self.widths(area);
                let (Some(&left), Some(&right)) = (widths.get(index), widths.get(index + 1)) else { return };
                let before: f32 = widths[..index].iter().map(|width| width + DIVIDER).sum();
                let wanted = pointer.x - DIVIDER / 2.0 - area.left() - before;
                let new = wanted.clamp(MIN_PANE_WIDTH, (left + right - MIN_PANE_WIDTH).max(MIN_PANE_WIDTH));
                for (column, width) in self.columns.iter_mut().zip(&widths) {
                    column.weight = *width;
                }
                self.columns[index].weight = new;
                self.columns[index + 1].weight = left + right - new;
            }
            Divider::Row(index) => {
                let Some(column) = self.columns.get_mut(index) else { return };
                let room = area.height() - DIVIDER;
                let limit = (MIN_PANE_HEIGHT / room).min(0.5);
                column.top_share = ((pointer.y - DIVIDER / 2.0 - area.top()) / room).clamp(limit, 1.0 - limit);
            }
        }
    }

    pub fn mirror(&mut self) {
        self.columns.reverse();
    }

    /// Take a tab out. What's beside it fills the room.
    pub fn remove(&mut self, tab: T) {
        for column in &mut self.columns {
            if column.bottom == Some(tab) {
                column.bottom = None;
            } else if column.top == tab
                && let Some(bottom) = column.bottom.take()
            {
                column.top = bottom;
            }
        }
        // Still on top with nothing below: that was the column's only tab.
        self.columns.retain(|column| column.top != tab);
    }

    fn replace(&mut self, tab: T, with: T) {
        for column in &mut self.columns {
            if column.top == tab {
                column.top = with;
            }
            if column.bottom == Some(tab) {
                column.bottom = Some(with);
            }
        }
    }

    fn has_room(&self, target: T, edge: Edge) -> bool {
        match edge {
            Edge::Left | Edge::Right => self.columns.len() < MAX_COLUMNS,
            Edge::Top | Edge::Bottom => {
                self.columns.iter().any(|column| column.top == target && column.bottom.is_none())
            }
            Edge::Center => true,
        }
    }

    /// `entry` dropped on `target` at this edge, and where it really went: a
    /// full column or row makes it take the target's pane instead.
    pub fn dropped(&self, entry: T, target: T, edge: Edge) -> Option<(Edge, Self)> {
        if entry == target || !self.contains(target) {
            return None;
        }
        let mut next = self.clone();
        // A tab moving within the layout leaves room behind before it lands.
        next.remove(entry);
        let edge = if next.has_room(target, edge) { edge } else { Edge::Center };
        if edge == Edge::Center {
            let mut next = self.clone();
            // A tab already here trades places instead of appearing twice.
            if self.contains(entry) {
                let swap = |tab: T| {
                    if tab == entry {
                        target
                    } else if tab == target {
                        entry
                    } else {
                        tab
                    }
                };
                for column in &mut next.columns {
                    column.top = swap(column.top);
                    column.bottom = column.bottom.map(swap);
                }
            } else {
                next.replace(target, entry);
            }
            return Some((edge, next));
        }
        let index = next.columns.iter().position(|column| column.top == target || column.bottom == Some(target))?;
        let weight = next.columns.iter().map(|column| column.weight).sum::<f32>() / next.columns.len() as f32;
        match edge {
            Edge::Left => next.columns.insert(index, Column::new(entry, weight)),
            Edge::Right => next.columns.insert(index + 1, Column::new(entry, weight)),
            Edge::Top => {
                let column = &mut next.columns[index];
                column.bottom = Some(column.top);
                column.top = entry;
            }
            Edge::Bottom => next.columns[index].bottom = Some(entry),
            Edge::Center => {}
        }
        Some((edge, next))
    }
}

fn column_top_height<T>(column: &Column<T>, area: Rect) -> f32 {
    (area.height() - DIVIDER) * column.top_share
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::from_min_size(Pos2::ZERO, egui::vec2(1600.0, 800.0))
    }

    fn add(layout: &TabLayout<u32>, tab: u32, target: u32, edge: Edge) -> TabLayout<u32> {
        let (landed, next) = layout.dropped(tab, target, edge).unwrap();
        assert_eq!(landed, edge);
        next
    }

    /// 1-4 across the top, 5-8 below them.
    fn full() -> TabLayout<u32> {
        let mut layout = TabLayout::pair(1, 2);
        layout = add(&layout, 3, 2, Edge::Right);
        layout = add(&layout, 4, 3, Edge::Right);
        for (tab, above) in [(5, 1), (6, 2), (7, 3), (8, 4)] {
            layout = add(&layout, tab, above, Edge::Bottom);
        }
        layout
    }

    #[test]
    fn rebuilds_from_rows() {
        let layout = full();
        assert_eq!(TabLayout::from_rows(&layout.rows()).unwrap().rows(), layout.rows());
        assert_eq!(TabLayout::from_rows(&[vec![1], vec![], vec![2, 3, 4]]).unwrap().rows(), [vec![1], vec![2, 3]]);
        assert!(TabLayout::<u32>::from_rows(&[vec![1]]).is_none());
    }

    #[test]
    fn grows_to_four_across_and_two_down() {
        let layout = full();
        assert_eq!(layout.rows(), [[1, 5], [2, 6], [3, 7], [4, 8]]);
        let panes = layout.panes(area());
        assert_eq!(panes.len(), 8);
        for (index, (_, pane)) in panes.iter().enumerate() {
            assert!(area().contains_rect(*pane));
            assert!(pane.width() > 380.0 && pane.height() > 380.0, "{pane:?}");
            for (_, other) in &panes[index + 1..] {
                assert!(!pane.intersects(*other), "{pane:?} and {other:?} overlap");
            }
        }
    }

    #[test]
    fn a_full_row_or_column_takes_the_pane_instead() {
        let layout = full();
        for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
            let (landed, next) = layout.dropped(9, 2, edge).unwrap();
            assert_eq!(landed, Edge::Center);
            assert_eq!(next.rows(), [[1, 5], [9, 6], [3, 7], [4, 8]]);
        }
    }

    #[test]
    fn a_tab_moving_within_the_layout_never_appears_twice() {
        let layout = full();
        // Trading places, even though its own column would have room after.
        let (_, swapped) = layout.dropped(1, 8, Edge::Center).unwrap();
        assert_eq!(swapped.rows(), [[8, 5], [2, 6], [3, 7], [4, 1]]);

        // Moving out of a stack frees that stack and fills the next one.
        let three = add(&TabLayout::pair(1, 2), 3, 2, Edge::Bottom);
        assert_eq!(three.rows(), [vec![1], vec![2, 3]]);
        assert_eq!(add(&three, 3, 1, Edge::Top).rows(), [vec![3, 1], vec![2]]);

        // Its column emptying makes room for it elsewhere in the row.
        let four = add(&add(&TabLayout::pair(1, 2), 3, 2, Edge::Right), 4, 3, Edge::Right);
        assert_eq!(add(&four, 1, 4, Edge::Right).rows(), [[2], [3], [4], [1]]);
    }

    #[test]
    fn dropping_a_tab_on_itself_does_nothing() {
        assert!(TabLayout::pair(1, 2).dropped(1, 1, Edge::Left).is_none());
        assert!(TabLayout::pair(1, 2).dropped(3, 9, Edge::Left).is_none());
    }

    #[test]
    fn removing_lets_the_neighbors_fill_in() {
        let mut layout = full();
        layout.remove(1);
        assert_eq!(layout.rows()[0], [5]);
        layout.remove(5);
        assert_eq!(layout.rows(), [[2, 6], [3, 7], [4, 8]]);
        layout.remove(7);
        assert_eq!(layout.rows(), [vec![2, 6], vec![3], vec![4, 8]]);
    }

    #[test]
    fn dragging_dividers_moves_only_their_neighbors_and_respects_the_minimum() {
        let mut layout = full();
        let before = layout.widths(area());
        layout.drag_divider(area(), Divider::Column(1), pos2(before[0] + before[1] + 100.0, 0.0));
        let after = layout.widths(area());
        assert!((after[0] - before[0]).abs() < 0.5);
        assert!((after[3] - before[3]).abs() < 0.5);
        assert!(after[1] > before[1] && after[2] < before[2]);
        layout.drag_divider(area(), Divider::Column(1), pos2(0.0, 0.0));
        assert!((layout.widths(area())[1] - MIN_PANE_WIDTH).abs() < 0.5);

        layout.drag_divider(area(), Divider::Row(0), pos2(0.0, 5000.0));
        let bottom = layout.panes(area())[1].1;
        assert!((bottom.height() - MIN_PANE_HEIGHT).abs() < 0.5, "{bottom:?}");
        assert!(layout.panes(area())[3].1.height() > 300.0, "other columns keep their own split");
    }

    #[test]
    fn says_when_the_room_is_too_small() {
        let tight = Rect::from_min_size(Pos2::ZERO, egui::vec2(600.0, 800.0));
        assert!(TabLayout::pair(1, 2).fits(tight));
        assert!(!full().fits(tight));
        let short = Rect::from_min_size(Pos2::ZERO, egui::vec2(1600.0, 150.0));
        assert!(TabLayout::pair(1, 2).fits(short));
        assert!(!full().fits(short));
    }

    #[test]
    fn panes_stay_at_least_the_minimum_after_the_window_shrinks() {
        let mut layout = full();
        layout.drag_divider(area(), Divider::Column(0), pos2(1000.0, 0.0));
        let tight = Rect::from_min_size(Pos2::ZERO, egui::vec2(700.0, 800.0));
        assert!(layout.fits(tight));
        for (_, pane) in layout.panes(tight) {
            assert!(pane.width() >= MIN_PANE_WIDTH - 0.5, "{pane:?}");
            assert!(tight.expand(0.5).contains_rect(pane), "{pane:?}");
        }
    }

    #[test]
    fn edges_are_the_nearest_side_with_a_center_when_allowed() {
        let pane = Rect::from_min_size(Pos2::ZERO, egui::vec2(100.0, 100.0));
        assert_eq!(Edge::at(pane, pos2(5.0, 50.0), true), Edge::Left);
        assert_eq!(Edge::at(pane, pos2(95.0, 50.0), true), Edge::Right);
        assert_eq!(Edge::at(pane, pos2(50.0, 5.0), true), Edge::Top);
        assert_eq!(Edge::at(pane, pos2(50.0, 95.0), true), Edge::Bottom);
        assert_eq!(Edge::at(pane, pos2(50.0, 50.0), true), Edge::Center);
        assert_ne!(Edge::at(pane, pos2(50.0, 50.0), false), Edge::Center);
    }
}
