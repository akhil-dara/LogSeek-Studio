//! Grouped view: the grid as one list of group headers and rows. Only open
//! groups are held; a level of millions of groups or an open group of
//! millions of rows costs nothing until it is on screen (no limits).

use std::collections::{BTreeMap, HashMap, HashSet};

/// One level of groups: how many there are (in the chosen sort) and which
/// of them are open, by their position in that sort.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Level {
    pub groups: u64,
    pub open: BTreeMap<u64, Open>,
}

/// What an open group shows beneath its header.
#[derive(Clone, Debug, PartialEq)]
pub enum Open {
    /// The next grouping column's level.
    Level(Level),
    /// The group's rows (the last grouping column).
    Rows(u64),
}

/// A line of the grouped view. `path` = the group's position at each level.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Header(Vec<u64>),
    Row(Vec<u64>, u64),
}

impl Open {
    fn len(&self) -> u64 {
        match self {
            Open::Level(l) => l.len(),
            Open::Rows(n) => *n,
        }
    }
}

impl Level {
    pub fn new(groups: u64) -> Level {
        Level {
            groups,
            open: BTreeMap::new(),
        }
    }

    /// Lines this level takes: its headers plus everything open beneath them.
    pub fn len(&self) -> u64 {
        self.groups + self.open.values().map(Open::len).sum::<u64>()
    }

    /// The line at `idx` (`None` past the end).
    pub fn item_at(&self, idx: u64) -> Option<Item> {
        let mut path = Vec::new();
        self.find(idx, &mut path)
    }

    fn find(&self, mut idx: u64, path: &mut Vec<u64>) -> Option<Item> {
        // headers before the first open group, then each open group's block
        let mut next_header = 0u64;
        for (&g, open) in &self.open {
            let before = g - next_header; // closed headers before this open one
            if idx < before {
                path.push(next_header + idx);
                return Some(Item::Header(path.clone()));
            }
            idx -= before;
            if idx == 0 {
                path.push(g);
                return Some(Item::Header(path.clone()));
            }
            idx -= 1;
            let n = open.len();
            if idx < n {
                path.push(g);
                return match open {
                    Open::Level(l) => l.find(idx, path),
                    Open::Rows(_) => Some(Item::Row(path.clone(), idx)),
                };
            }
            idx -= n;
            next_header = g + 1;
        }
        let g = next_header + idx;
        (g < self.groups).then(|| {
            path.push(g);
            Item::Header(path.clone())
        })
    }

    /// The line where the header at `path` sits.
    pub fn header_line(&self, path: &[u64]) -> Option<u64> {
        let (&g, rest) = path.split_first()?;
        if g >= self.groups {
            return None;
        }
        let mut line = g;
        for (&o, open) in self.open.range(..g) {
            let _ = o;
            line += open.len();
        }
        if rest.is_empty() {
            return Some(line);
        }
        match self.open.get(&g)? {
            Open::Level(l) => Some(line + 1 + l.header_line(rest)?),
            Open::Rows(_) => None,
        }
    }

    /// Open or close the group at `path` (`child` is what opening shows).
    pub fn toggle(&mut self, path: &[u64], child: impl FnOnce() -> Open) {
        let Some((&g, rest)) = path.split_first() else {
            return;
        };
        if rest.is_empty() {
            if self.open.remove(&g).is_none() && g < self.groups {
                self.open.insert(g, child());
            }
            return;
        }
        if let Some(Open::Level(l)) = self.open.get_mut(&g) {
            l.toggle(rest, child);
        }
    }

    pub fn is_open(&self, path: &[u64]) -> bool {
        match path.split_first() {
            None => false,
            Some((&g, rest)) if rest.is_empty() => self.open.contains_key(&g),
            Some((&g, rest)) => {
                matches!(self.open.get(&g), Some(Open::Level(l)) if l.is_open(rest))
            }
        }
    }
}

/// The grouped view's live state: the grouping columns, the open groups,
/// and what the engine has sent so far (only for what has been on screen).
/// Where a dragged group chip lands: the chip rectangles in order, the
/// pointer's x, and the chip being dragged. The place it would take (0 =
/// first), by the middle of each chip.
pub fn drop_place(chips: &[(f32, f32)], x: f32, dragged: usize) -> usize {
    let mut place = 0;
    for (i, &(a, b)) in chips.iter().enumerate() {
        if i != dragged && x > (a + b) * 0.5 {
            place += 1;
        }
    }
    place
}

/// `by` with the column at `from` moved to place `to` among the others.
pub fn move_to(by: &mut Vec<String>, from: usize, to: usize) {
    if from < by.len() {
        let c = by.remove(from);
        by.insert(to.min(by.len()), c);
    }
}

/// `by` with `name` added as the last level; says whether it was added
/// (a column already grouped by is not added twice).
pub fn add_level(by: &mut Vec<String>, name: &str) -> bool {
    if by.iter().any(|b| b == name) {
        return false;
    }
    by.push(name.to_string());
    true
}

pub struct Grouping {
    pub by: Vec<String>,
    pub sort: crate::bridge::GroupSort,
    pub tree: Level,
    generation: u64,
    /// Group rows seen, by (level key, position in that level).
    seen: HashMap<(u64, u64), crate::bridge::GroupRow>,
    /// Requests on their way: (key, start); `u64::MAX` = build the level.
    sent: HashSet<(u64, u64)>,
}

const PAGE: u64 = 200;

fn hash_of(parts: &[(String, Option<String>)], tail: &str, sort: crate::bridge::GroupSort) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    parts.hash(&mut h);
    tail.hash(&mut h);
    (sort as u8).hash(&mut h);
    h.finish()
}

impl Grouping {
    pub fn new(by: Vec<String>, sort: crate::bridge::GroupSort) -> Grouping {
        Grouping {
            by,
            sort,
            tree: Level::new(0),
            generation: u64::MAX,
            seen: HashMap::new(),
            sent: HashSet::new(),
        }
    }

    /// Lines in the view.
    pub fn lines(&self) -> u64 {
        self.tree.len()
    }

    /// The key of the level under `vals` (grouping by the next column).
    pub fn level_key(&self, vals: &[(String, Option<String>)]) -> u64 {
        let by = self.by.get(vals.len()).map(String::as_str).unwrap_or("");
        hash_of(vals, by, self.sort)
    }

    /// The key of an open last-level group's rows.
    pub fn rows_key(&self, vals: &[(String, Option<String>)]) -> u64 {
        hash_of(vals, "\u{1}rows", self.sort)
    }

    /// (column, value) at each level down to the group at `path`.
    pub fn path_values(&self, path: &[u64]) -> Option<crate::bridge::GroupPath> {
        let mut vals = Vec::with_capacity(path.len());
        for (d, &i) in path.iter().enumerate() {
            let key = self.level_key(&vals);
            let row = self.seen.get(&(key, i))?;
            vals.push((self.by.get(d)?.clone(), row.value.clone()));
        }
        Some(vals)
    }

    /// The group at `path` as last seen.
    pub fn group(&self, path: &[u64]) -> Option<&crate::bridge::GroupRow> {
        let (&last, above) = path.split_last()?;
        let vals = self.path_values(above)?;
        self.seen.get(&(self.level_key(&vals), last))
    }

    /// Take in what the engine sent; a new result starts over.
    pub fn sync(&mut self, snap: &crate::bridge::Snapshot) {
        if snap.generation != self.generation {
            self.generation = snap.generation;
            self.tree = Level::new(0);
            self.seen.clear();
            self.sent.clear();
        }
        for (&key, page) in &snap.group_pages {
            if page.generation != self.generation {
                continue;
            }
            for (k, r) in page.rows.iter().enumerate() {
                self.seen
                    .entry((key, page.start + k as u64))
                    .or_insert_with(|| r.clone());
            }
        }
        let top = self.level_key(&[]);
        if let Some(info) = snap
            .group_levels
            .get(&top)
            .filter(|i| i.generation == self.generation)
        {
            self.tree.groups = info.groups;
        }
        // open nested levels learn their size when it arrives
        let mut stack: Vec<Vec<u64>> = self.tree.open.keys().map(|&g| vec![g]).collect();
        while let Some(path) = stack.pop() {
            let Some(vals) = self.path_values(&path) else {
                continue;
            };
            let key = self.level_key(&vals);
            let groups = snap
                .group_levels
                .get(&key)
                .filter(|i| i.generation == self.generation)
                .map(|i| i.groups);
            if let Some(Open::Level(l)) = self.node_mut(&path) {
                if let Some(n) = groups {
                    l.groups = n;
                }
                stack.extend(l.open.keys().map(|&g| {
                    let mut p = path.clone();
                    p.push(g);
                    p
                }));
            }
        }
    }

    fn node_mut(&mut self, path: &[u64]) -> Option<&mut Open> {
        let (&first, rest) = path.split_first()?;
        let mut node = self.tree.open.get_mut(&first)?;
        for &g in rest {
            node = match node {
                Open::Level(l) => l.open.get_mut(&g)?,
                Open::Rows(_) => return None,
            };
        }
        Some(node)
    }

    /// Ask the engine for whatever the lines `top..top+visible` need.
    pub fn request(
        &mut self,
        snap: &crate::bridge::Snapshot,
        bridge: &crate::bridge::Bridge,
        top: u64,
        visible: u64,
    ) {
        use crate::bridge::Cmd;
        let key0 = self.level_key(&[]);
        if !snap.group_levels.contains_key(&key0) && self.sent.insert((key0, u64::MAX)) {
            bridge.send(Cmd::GroupLevel {
                key: key0,
                path: Vec::new(),
                by: self.by[0].clone(),
                sort: self.sort,
            });
        }
        for line in top..(top + visible).min(self.lines()) {
            match self.tree.item_at(line) {
                Some(Item::Header(path)) => {
                    let Some((&i, above)) = path.split_last() else {
                        continue;
                    };
                    let Some(vals) = self.path_values(above) else {
                        continue;
                    };
                    let key = self.level_key(&vals);
                    if snap.group_levels.contains_key(&key) && !self.seen.contains_key(&(key, i)) {
                        let start = i / PAGE * PAGE;
                        if self.sent.insert((key, start)) {
                            bridge.send(Cmd::GroupPage {
                                key,
                                start,
                                len: (PAGE * 2) as usize,
                            });
                        }
                    }
                }
                Some(Item::Row(path, r)) => {
                    let Some(vals) = self.path_values(&path) else {
                        continue;
                    };
                    let key = self.rows_key(&vals);
                    let covered = snap.group_rows.get(&key).is_some_and(|p| {
                        p.generation == self.generation
                            && p.start <= r
                            && r < p.start + p.rows.len() as u64
                    });
                    if !covered {
                        let start = r.saturating_sub(PAGE / 2) / PAGE * PAGE;
                        if self.sent.insert((key, start)) {
                            bridge.send(Cmd::GroupRows {
                                key,
                                path: vals,
                                start,
                                len: (PAGE * 2) as usize,
                            });
                        }
                    }
                }
                None => {}
            }
        }
    }

    /// Open or close the group at `path`.
    pub fn toggle(&mut self, path: &[u64], bridge: &crate::bridge::Bridge) {
        use crate::bridge::Cmd;
        if self.tree.is_open(path) {
            self.tree.toggle(path, || Open::Rows(0));
            return;
        }
        let (Some(vals), Some(rows)) = (self.path_values(path), self.group(path).map(|g| g.rows))
        else {
            return;
        };
        if vals.len() < self.by.len() {
            let key = self.level_key(&vals);
            if self.sent.insert((key, u64::MAX)) {
                bridge.send(Cmd::GroupLevel {
                    key,
                    path: vals.clone(),
                    by: self.by[vals.len()].clone(),
                    sort: self.sort,
                });
            }
            self.tree.toggle(path, || Open::Level(Level::new(0)));
        } else {
            self.tree.toggle(path, || Open::Rows(rows));
        }
    }

    /// The file row shown on line `line` (rows inside open groups only).
    pub fn row_id(&self, snap: &crate::bridge::Snapshot, line: u64) -> Option<u64> {
        let Item::Row(path, r) = self.tree.item_at(line)? else {
            return None;
        };
        let vals = self.path_values(&path)?;
        let p = snap.group_rows.get(&self.rows_key(&vals))?;
        p.rows.get(r.checked_sub(p.start)? as usize).copied()
    }

    /// The cells of row `r` of the open group at `path`.
    pub fn row_cells<'a>(
        &self,
        snap: &'a crate::bridge::Snapshot,
        path: &[u64],
        r: u64,
    ) -> Option<&'a Vec<Option<String>>> {
        let vals = self.path_values(path)?;
        let p = snap.group_rows.get(&self.rows_key(&vals))?;
        if p.generation != self.generation {
            return None;
        }
        p.cells.get(r.checked_sub(p.start)? as usize)
    }
}

/// What a "Grouped by" chip was asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChipAct {
    None,
    /// Group by this level one place earlier.
    Earlier,
    /// One place later.
    Later,
    /// Stop grouping by it.
    Remove,
}

/// One drawn chip: what was asked, its drag handle, where it and its cross are.
pub struct ChipOut {
    pub act: ChipAct,
    pub handle: eframe::egui::Response,
    pub rect: eframe::egui::Rect,
    /// Read only by the tests, which click it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub cross: eframe::egui::Rect,
}

/// A "Grouped by" chip for level `i` of `n`: earlier, the name, later, and a
/// cross to stop grouping by it.
pub fn chip(
    ui: &mut eframe::egui::Ui,
    pal: &crate::theme::Palette,
    name: &str,
    i: usize,
    n: usize,
) -> ChipOut {
    use eframe::egui::{self, CornerRadius, Frame, Margin, RichText, Sense};
    use egui_phosphor::regular as ph;
    let mut act = ChipAct::None;
    let mut cross = egui::Rect::NOTHING;
    let mut handle: Option<egui::Response> = None;
    let chip = Frame::new()
        .fill(pal.selection)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(i > 0, egui::Button::new(ph::CARET_LEFT).frame(false))
                    .on_hover_text("Group by this one first")
                    .clicked()
                {
                    act = ChipAct::Earlier;
                }
                // the name is the drag handle: the arrows and the cross stay
                // clickable (an area laid over the whole chip took their clicks)
                handle = Some(
                    ui.add(
                        egui::Label::new(RichText::new(name).color(pal.text).strong())
                            .sense(Sense::drag()),
                    ),
                );
                if ui
                    .add_enabled(i + 1 < n, egui::Button::new(ph::CARET_RIGHT).frame(false))
                    .on_hover_text("Group by this one later")
                    .clicked()
                {
                    act = ChipAct::Later;
                }
                let x = ui
                    .add(egui::Button::new(ph::X).frame(false))
                    .on_hover_text("Stop grouping by this column");
                cross = x.rect;
                if x.clicked() {
                    act = ChipAct::Remove;
                }
            });
        });
    let rect = chip.response.rect;
    let handle = handle.unwrap_or(chip.response);
    ChipOut {
        act,
        handle,
        rect,
        cross,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use Item::*;

    /// Feed `ctx` one frame with these events, drawing one chip; what it says.
    fn chip_frame(ctx: &eframe::egui::Context, events: Vec<eframe::egui::Event>) -> ChipOut {
        use eframe::egui;
        let mut out = None;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 200.0),
            )),
            events,
            ..Default::default()
        };
        let mut full = ctx.run_ui(input, |ui| {
            out = Some(chip(
                ui,
                &crate::theme::DARK,
                "protoPayload.requestMetadata.callerSuppliedUserAgent",
                0,
                1,
            ));
        });
        full.textures_delta.clear();
        out.unwrap()
    }

    #[test]
    fn the_cross_on_a_group_chip_closes_it_when_clicked() {
        use eframe::egui::{Event, Modifiers, PointerButton};
        let ctx = eframe::egui::Context::default();
        let first = chip_frame(&ctx, vec![]);
        let at = first.cross.center();
        let mut seen = ChipAct::None;
        for events in [
            vec![Event::PointerMoved(at)],
            vec![Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            }],
            vec![Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }],
            vec![],
        ] {
            let o = chip_frame(&ctx, events);
            // the chip keeps its place and size while the pointer is on it: no shaking
            assert_eq!(o.rect, first.rect, "the chip moved under the pointer");
            if o.act != ChipAct::None {
                seen = o.act;
            }
        }
        assert_eq!(seen, ChipAct::Remove);
    }

    #[test]
    fn closed_groups_are_just_headers() {
        let l = Level::new(3);
        assert_eq!(l.len(), 3);
        assert_eq!(l.item_at(1), Some(Header(vec![1])));
        assert_eq!(l.item_at(3), None);
    }

    #[test]
    fn an_open_group_shows_all_its_rows_after_its_header() {
        let mut l = Level::new(3);
        l.toggle(&[1], || Open::Rows(5));
        assert_eq!(l.len(), 8);
        assert_eq!(l.item_at(1), Some(Header(vec![1])));
        assert_eq!(l.item_at(2), Some(Row(vec![1], 0)));
        assert_eq!(l.item_at(6), Some(Row(vec![1], 4)));
        assert_eq!(l.item_at(7), Some(Header(vec![2])));
        assert_eq!(l.header_line(&[2]), Some(7));
    }

    #[test]
    fn nested_levels_splice_in_order() {
        let mut l = Level::new(2);
        l.toggle(&[0], || Open::Level(Level::new(2)));
        l.toggle(&[0, 1], || Open::Rows(3));
        let got: Vec<Item> = (0..l.len()).filter_map(|i| l.item_at(i)).collect();
        assert_eq!(
            got,
            vec![
                Header(vec![0]),
                Header(vec![0, 0]),
                Header(vec![0, 1]),
                Row(vec![0, 1], 0),
                Row(vec![0, 1], 1),
                Row(vec![0, 1], 2),
                Header(vec![1]),
            ]
        );
        assert_eq!(l.header_line(&[0, 1]), Some(2));
        assert_eq!(l.header_line(&[1]), Some(6));
        assert!(l.is_open(&[0, 1]));
        l.toggle(&[0], || Open::Rows(0));
        assert_eq!(l.len(), 2);
    }

    #[test]
    fn millions_of_groups_and_rows_cost_nothing() {
        let mut l = Level::new(5_000_000);
        l.toggle(&[4_000_000], || Open::Rows(90_000_000));
        assert_eq!(l.len(), 95_000_000);
        assert_eq!(
            l.item_at(4_000_000 + 1 + 89_999_999),
            Some(Row(vec![4_000_000], 89_999_999))
        );
        assert_eq!(l.item_at(94_999_999), Some(Header(vec![4_999_999])));
    }

    #[test]
    fn a_dragged_group_chip_lands_where_it_is_dropped() {
        // three chips: 0..100, 110..210, 220..320
        let chips = [(0.0, 100.0), (110.0, 210.0), (220.0, 320.0)];
        // the first dragged past the middle of the second: second place
        assert_eq!(drop_place(&chips, 170.0, 0), 1);
        // past the last: last place; not far enough: stays first
        assert_eq!(drop_place(&chips, 400.0, 0), 2);
        assert_eq!(drop_place(&chips, 150.0, 0), 0);
        // the last dragged to the very start
        assert_eq!(drop_place(&chips, -5.0, 2), 0);
        let mut by: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        move_to(&mut by, 0, 2);
        assert_eq!(by, ["b", "c", "a"]);
        move_to(&mut by, 2, 0);
        assert_eq!(by, ["a", "b", "c"]);
        // a column joins as the last level, once
        assert!(add_level(&mut by, "d"));
        assert!(!add_level(&mut by, "b"));
        assert_eq!(by, ["a", "b", "c", "d"]);
    }
}
