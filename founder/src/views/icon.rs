use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Weak};
use std::time::Duration;

use finder_fs::ds_store::{self, Background, IconOptions, IconView};
use finder_fs::sort::name_cmp;
use gpui::{
    AnyElement, App, Div, ExternalPaths, Hsla, MouseButton, MouseDownEvent, Pixels, Point, Rgba,
    ScrollHandle, SharedString, Size, TextAlign, Window, div, img, point, prelude::*, px, size,
};
use native_gpui::native_tooltip;
use native_gpui::tokens::{SystemColor, system_color};
use objc2_app_kit::NSFont;

use crate::app::FinderDemo;
use crate::art::{Anchor, art_at, hidden_name, hidden_pill, item_art, marked};
use crate::data::{Column, Line, Order, Row};
use crate::dragdrop::{GHOST_ALPHA, Grabbed, KeyDrag, Lit, RowDrag, drop_hover, drop_target};
use crate::elastic::stretching;
use crate::item_info::item_info;
use crate::menus::tiles_menu;
use crate::palette::Palette;
use crate::prefs;
use crate::tags::{RowTag, row_tags, tag_dots, tag_dots_w};
use crate::ui::{pick_on_click, pick_on_press, rename_field};
use crate::view::ICON_VIEW;

const MARGIN: f32 = 10.;
const FLOW_INSET: f32 = 4.;
const FLOW_GAP: f32 = 2.;
const DEFAULT_ICON: f32 = 64.;
const DEFAULT_SPACING: f32 = 54.;
const DEFAULT_TEXT: f32 = 12.;
const FRAME_PER_ICON: f32 = 9. / 8.;
const PAD_PER_ICON: f32 = 1. / 16.;
const LABEL_LINE_PAD: f32 = 3.4;
const LABEL_SPACING_OFFSET: f32 = 33.;
const LABEL_SPACING_UNIT: f32 = 27.2;
const SECOND_LINE_PER_SPACING: f32 = 0.73;
const SIDE_FRAME_PAD: f32 = 6.;
const SIDE_TEXT_PAD: f32 = 8.;
const LABEL_LINES: usize = 2;
const LABEL_INSET: f32 = 0.5;
const LABEL_GAP: f32 = 2.;
const LABEL_PAD: f32 = 4.;
const LABEL_RADIUS: f32 = 5.;
const PLATE_RADIUS: f32 = 8.;
const CHAR_PER_TEXT: f32 = 0.58;
const EDITOR_PAD: f32 = 24.;
const ELIDE_MIN: usize = 12;
const ELIDE_TAIL: usize = 8;
const OFF_AXIS_WEIGHT: f32 = 2.;

const SMALLEST_ICON: f32 = 16.;
const FINE_STEP: f32 = 2.;
const FINE_STEPS: f32 = (SIZE_STOPS[0] - SMALLEST_ICON) / FINE_STEP;
const STEPS_PER_STOP: f32 = 8.;
const SIZE_GRAIN: f32 = 4.;
const SIZE_STOPS: [f32; 8] = [128., 164., 204., 244., 296., 356., 428., 512.];
pub(crate) const SIZE_STEPS: f64 = 112.;

const SOURCE_ALPHA: f32 = 0.35;
const FINDER_WAIT: Duration = Duration::from_millis(30);

pub(crate) fn slider_size(step: f64) -> f32 {
    let step = step.clamp(0., SIZE_STEPS) as f32;
    let size = if step <= FINE_STEPS {
        SMALLEST_ICON + FINE_STEP * step
    } else {
        let at = (step - FINE_STEPS) / STEPS_PER_STOP;
        let low = at.floor() as usize;
        let high = (at.ceil() as usize).min(SIZE_STOPS.len() - 1);
        SIZE_STOPS[low] + (SIZE_STOPS[high] - SIZE_STOPS[low]) * at.fract()
    };
    (size / SIZE_GRAIN).round() * SIZE_GRAIN
}

pub(crate) fn size_slider(size: f32) -> f64 {
    (0..=SIZE_STEPS as u32)
        .min_by(|a, b| {
            let off = |step: &u32| (slider_size(f64::from(*step)) - size).abs();
            off(a).total_cmp(&off(b))
        })
        .map_or(0., f64::from)
}

#[derive(Clone, Copy)]
struct Look {
    label: Hsla,
    selection: Hsla,
    selection_text: Hsla,
    backdrop: Hsla,
    info: Hsla,
}

impl Look {
    fn new(palette: &Palette) -> Self {
        Self {
            label: palette.label,
            selection: palette.selection,
            selection_text: palette.selection_text,
            backdrop: system_color(SystemColor::QuaternaryLabel),
            info: system_color(SystemColor::Link),
        }
    }
}

type Tile = (SharedString, Point<Pixels>);
type Slot = (i32, i32);

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Arrangement {
    Free,
    Grid,
    By(Column),
}

impl Arrangement {
    fn of(id: &str) -> Option<Self> {
        match id {
            "none" => Some(Self::Free),
            "grid" => Some(Self::Grid),
            id => Column::from_id(id).map(Self::By),
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Free => "none",
            Self::Grid => "grid",
            Self::By(column) => column.id(),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Options {
    pub(crate) size: f32,
    pub(crate) spacing: f32,
    pub(crate) text: f32,
    pub(crate) bottom: bool,
    pub(crate) info: bool,
    line: f32,
    cell: Size<f32>,
}

impl Options {
    pub(crate) fn new(chosen: &IconOptions) -> Self {
        let standard = &standard_options();
        let icon = chosen
            .icon_size
            .or(standard.icon_size)
            .unwrap_or(DEFAULT_ICON);
        let spacing = chosen
            .grid_spacing
            .or(standard.grid_spacing)
            .unwrap_or(DEFAULT_SPACING);
        let text = chosen
            .text_size
            .or(standard.text_size)
            .unwrap_or(DEFAULT_TEXT);
        let bottom = chosen
            .label_on_bottom
            .or(standard.label_on_bottom)
            .unwrap_or(true);
        let info = chosen
            .show_item_info
            .or(standard.show_item_info)
            .unwrap_or(false);
        let font = NSFont::systemFontOfSize(text.into());
        let line = (font.ascender().round() - font.descender().round()) as f32;
        let info_line = if info { line } else { 0. };
        let frame = icon * FRAME_PER_ICON;
        let even_line = 2. * (line / 2.).floor();
        let label = 2.
            * ((even_line + LABEL_LINE_PAD) * (spacing + LABEL_SPACING_OFFSET)
                / LABEL_SPACING_UNIT)
                .round();
        let cell = if bottom {
            let under = line + line.min(SECOND_LINE_PER_SPACING * (spacing - 1.)) + info_line;
            size(
                (frame + MARGIN).max(label),
                2. * ((frame + MARGIN + under) / 2.).floor(),
            )
        } else {
            size(
                frame + label + LABEL_GAP,
                (frame + SIDE_FRAME_PAD).max(2. * line + info_line + SIDE_TEXT_PAD),
            )
        };
        Self {
            size: icon,
            spacing,
            text,
            bottom,
            info,
            line,
            cell,
        }
    }

    fn frame(self) -> f32 {
        self.size * FRAME_PER_ICON
    }

    fn pad(self) -> f32 {
        self.size * PAD_PER_ICON
    }

    pub(crate) fn cell(self) -> Size<f32> {
        self.cell
    }

    fn image_centre(self) -> Point<f32> {
        if self.bottom {
            point(self.cell.width / 2., self.frame() / 2.)
        } else {
            point(self.frame() / 2., self.cell.height / 2.)
        }
    }

    fn slot_centre(self, (column, row): Slot) -> Point<Pixels> {
        let at = self.image_centre();
        point(
            px(MARGIN + column as f32 * self.cell.width + at.x),
            px(MARGIN + row as f32 * self.cell.height + at.y),
        )
    }

    fn nearest_slot(self, at: Point<Pixels>) -> Slot {
        let icon = self.image_centre();
        let column = (f32::from(at.x) - MARGIN - icon.x) / self.cell.width;
        let row = (f32::from(at.y) - MARGIN - icon.y) / self.cell.height;
        (column.round().max(0.) as i32, row.round().max(0.) as i32)
    }

    fn origin(self, centre: Point<Pixels>) -> Point<Pixels> {
        let at = self.image_centre();
        point(centre.x - px(at.x), centre.y - px(at.y))
    }

    pub(crate) fn lanes(self, room: Size<f32>) -> usize {
        let (room, cell) = if self.bottom {
            (room.width, self.cell.width)
        } else {
            (room.height, self.cell.height)
        };
        (((room - 2. * MARGIN) / cell).floor() as usize).max(1)
    }

    fn slot_at(self, index: usize, lanes: usize) -> Slot {
        let (along, across) = ((index % lanes) as i32, (index / lanes) as i32);
        if self.bottom {
            (along, across)
        } else {
            (across, along)
        }
    }

    fn moved_from(self, from: Options, at: Point<Pixels>) -> Point<Pixels> {
        let (was, now) = (from.image_centre(), self.image_centre());
        let axis = |at: Pixels, was: f32, now: f32, from: f32, to: f32| {
            px(MARGIN + now + (f32::from(at) - MARGIN - was) / from * to)
        };
        point(
            axis(at.x, was.x, now.x, from.cell.width, self.cell.width),
            axis(at.y, was.y, now.y, from.cell.height, self.cell.height),
        )
    }

    fn flowed(self, index: usize, count: usize, room: Size<f32>) -> Point<Pixels> {
        let (cell, at, gap) = (self.cell, self.image_centre(), self.pad());
        let (x, y) = if self.bottom {
            let across = (((room.width - 2. * FLOW_INSET + FLOW_GAP) / (cell.width + FLOW_GAP))
                .floor() as usize)
                .max(1);
            let (spread, step) = if count > across {
                let spread = ((room.width - 2. * FLOW_INSET - across as f32 * cell.width)
                    / (across + 1) as f32)
                    .max(0.);
                (spread, cell.width + spread)
            } else {
                (0., cell.width + FLOW_GAP)
            };
            (
                FLOW_INSET + spread + at.x + (index % across) as f32 * step,
                (index / across) as f32 * (cell.height + gap),
            )
        } else {
            let down =
                (((room.height - 2. * MARGIN) / (cell.height + gap)).floor() as usize).max(1);
            (
                FLOW_INSET + at.x + (index / down) as f32 * (cell.width + gap),
                (index % down) as f32 * (cell.height + gap),
            )
        };
        point(px(x.floor()), px((2. * MARGIN + at.y + y).floor()))
    }
}

fn standard_options() -> IconOptions {
    prefs::icon_defaults().or(prefs::finder().icon_options.clone())
}

#[derive(Clone, Default)]
pub(crate) struct Paint {
    pub(crate) previews: bool,
    pub(crate) background: Background,
}

impl Paint {
    fn new(chosen: &IconOptions) -> Self {
        let standard = standard_options();
        Self {
            previews: chosen
                .show_icon_preview
                .or(standard.show_icon_preview)
                .unwrap_or(true),
            background: chosen
                .background
                .clone()
                .or(standard.background)
                .unwrap_or_default(),
        }
    }
}

fn claim_nearest(at: Point<Pixels>, taken: &mut HashSet<Slot>, options: Options) -> Point<Pixels> {
    let (column, row) = options.nearest_slot(at);
    let reach = options.cell.width.min(options.cell.height);
    let mut best: Option<(f32, Slot)> = None;
    for ring in 0i32.. {
        let near = (ring as f32 - 0.5).max(0.) * reach;
        if best.is_some_and(|(far, _)| near * near > far) {
            break;
        }
        for dy in -ring..=ring {
            for dx in -ring..=ring {
                let slot = (column + dx, row + dy);
                if dx.abs().max(dy.abs()) != ring
                    || slot.0 < 0
                    || slot.1 < 0
                    || taken.contains(&slot)
                {
                    continue;
                }
                let centre = options.slot_centre(slot);
                let (x, y) = (f32::from(centre.x - at.x), f32::from(centre.y - at.y));
                let far = x * x + y * y;
                if best.is_none_or(|(best, _)| far < best) {
                    best = Some((far, slot));
                }
            }
        }
    }
    let slot = best.map_or((column, row), |(_, slot)| slot);
    taken.insert(slot);
    options.slot_centre(slot)
}

fn sort_order(lines: &[Line], column: Column) -> Vec<usize> {
    let mut rows: Vec<Arc<Row>> = lines.iter().map(|line| line.row.clone()).collect();
    let by = Order {
        group: None,
        sort: column,
        ascending: column.first_ascending(),
    };
    crate::read::sort_rows(&mut rows, by, true, true);
    // Keyed by path, not pointer: sort_rows reads extras into fresh Arcs, so pointers no longer match.
    let order: HashMap<&str, usize> = rows
        .iter()
        .enumerate()
        .map(|(at, row)| (row.path.as_ref(), at))
        .collect();
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| order.get(line.row.path.as_ref()).copied().unwrap_or(index))
        .collect()
}

fn keys_in_order(lines: &[Line], column: Column) -> Vec<SharedString> {
    let rank = sort_order(lines, column);
    let mut index: Vec<usize> = (0..lines.len()).collect();
    index.sort_by(|&left, &right| rank[left].cmp(&rank[right]).then(left.cmp(&right)));
    index.into_iter().map(|at| lines[at].key.clone()).collect()
}

fn reading_index(at: impl Fn(usize) -> (f32, f32), count: usize) -> Vec<usize> {
    let mut index: Vec<usize> = (0..count).collect();
    index.sort_by(|&left, &right| {
        let (a, b) = (at(left), at(right));
        a.1.total_cmp(&b.1)
            .then(a.0.total_cmp(&b.0))
            .then(left.cmp(&right))
    });
    index
}

fn named(key: &SharedString) -> Option<SharedString> {
    let name = Path::new(key.as_ref()).file_name()?.to_str()?;
    Some(SharedString::from(name.to_owned()))
}

struct Layout {
    lines: Weak<Vec<Line>>,
    lanes: usize,
    room: Option<Size<f32>>,
    options: Options,
    moved: u64,
    tiles: Vec<Tile>,
    width: f32,
    height: f32,
    by_top: Vec<usize>,
}

impl Layout {
    fn visible(&self, top: f32, room: f32) -> Vec<usize> {
        let tall = self.options.cell.height;
        let y = |index: &usize| f32::from(self.tiles[*index].1.y);
        let first = self
            .by_top
            .partition_point(|index| y(index) + 2. * tall <= top);
        let mut shown: Vec<usize> = self.by_top[first..]
            .iter()
            .take_while(|index| y(index) < top + room + tall)
            .copied()
            .collect();
        shown.sort_unstable();
        shown
    }

    fn centre(&self, index: usize) -> Point<Pixels> {
        let (tile, at) = (self.tiles[index].1, self.options.image_centre());
        point(tile.x + px(at.x), tile.y + px(at.y))
    }
}

struct Places {
    folder: &'static str,
    recorded: IconOptions,
    arrangement: Arrangement,
    own: HashMap<SharedString, Point<Pixels>>,
    finder: HashMap<SharedString, Point<Pixels>>,
    incoming: HashMap<SharedString, Point<Pixels>>,
    /// Kept for the visit only: Finder records a folder's places once one of them moves.
    placed: HashMap<SharedString, Point<Pixels>>,
}

impl Places {
    fn new(folder: &'static str, view: IconView, chosen: &IconOptions, options: Options) -> Self {
        let arrangement = chosen
            .arrange_by
            .as_deref()
            .or(prefs::finder().icon_options.arrange_by.as_deref())
            .and_then(Arrangement::of)
            .unwrap_or(Arrangement::Free);
        let recorded = Options::new(&view.options);
        let points = |spots: HashMap<String, (f32, f32)>, from: Options| {
            spots
                .into_iter()
                .map(|(name, (x, y))| {
                    let at = options.moved_from(from, point(px(x), px(y)));
                    (SharedString::from(name), at)
                })
                .collect()
        };
        Self {
            folder,
            arrangement,
            own: points(prefs::icon_spots(folder), options),
            finder: points(view.locations, recorded),
            incoming: HashMap::new(),
            placed: HashMap::new(),
            recorded: view.options,
        }
    }

    fn spot(&self, name: &str) -> Option<Point<Pixels>> {
        self.incoming
            .get(name)
            .or_else(|| self.own.get(name))
            .or_else(|| self.finder.get(name))
            .or_else(|| self.placed.get(name))
            .copied()
    }

    fn place(
        &mut self,
        lines: &[Line],
        lanes: usize,
        options: Options,
        settled: bool,
    ) -> Vec<Point<Pixels>> {
        let mut centres: Vec<Option<Point<Pixels>>> =
            lines.iter().map(|line| self.spot(&line.row.name)).collect();
        let taken: HashSet<Slot> = centres
            .iter()
            .flatten()
            .map(|at| options.nearest_slot(*at))
            .collect();
        let mut waiting: Vec<usize> = (0..lines.len())
            .filter(|&index| centres[index].is_none())
            .collect();
        waiting.sort_by(|&a, &b| name_cmp(&lines[a].row.name, &lines[b].row.name));
        let mut free = (0..)
            .map(|index| options.slot_at(index, lanes))
            .filter(|slot| !taken.contains(slot));
        for &index in &waiting {
            let centre = options.slot_centre(free.next().unwrap_or_default());
            centres[index] = Some(centre);
            if settled {
                self.placed.insert(lines[index].row.name.clone(), centre);
            }
        }
        if settled {
            let arrived: Vec<_> = lines
                .iter()
                .filter_map(|line| self.incoming.remove_entry(&line.row.name))
                .collect();
            if !arrived.is_empty() {
                self.own.extend(arrived);
                self.keep(lines);
            }
        }
        centres.into_iter().flatten().collect()
    }

    fn keep(&mut self, lines: &[Line]) {
        for (name, at) in self.placed.drain() {
            self.own.entry(name).or_insert(at);
        }
        let listed: HashSet<&SharedString> = lines.iter().map(|line| &line.row.name).collect();
        self.own.retain(|name, _| listed.contains(name));
        self.save();
    }

    fn save(&self) {
        prefs::set_icon_spots(
            self.folder,
            self.own
                .iter()
                .map(|(name, at)| (name.as_ref(), (f32::from(at.x), f32::from(at.y)))),
        );
    }
}

#[derive(Default)]
pub(crate) struct Grid {
    places: Option<Places>,
    options: Option<(&'static str, Options)>,
    paint: Paint,
    reading: Option<&'static str>,
    moved: u64,
    /// Finder leaves icons where it first put them, whatever the window does next.
    kept: Cell<Option<(usize, Options)>>,
    /// Finder lays a prepared window's first icons out for its default width.
    pub(crate) sized: bool,
    layout: Option<Rc<Layout>>,
    dragging: Option<SharedString>,
    pub(crate) scroll: ScrollHandle,
}

impl Grid {
    pub(crate) fn hold(&self, lanes: usize, options: Options) -> usize {
        match self.kept.get() {
            Some((held, at)) if at == options => held,
            _ => {
                self.kept.set(Some((lanes, options)));
                lanes
            }
        }
    }

    pub(crate) fn first_width(&self, now: f32) -> f32 {
        if self.sized { now } else { crate::WINDOW_W }
    }

    pub(crate) fn forget(&mut self) {
        self.moved += 1;
        self.places = None;
        self.options = None;
        self.reading = None;
        self.kept.set(None);
        self.dragging = None;
    }

    fn read(&mut self, folder: &'static str, view: IconView) {
        let chosen = prefs::icon_options(folder).or(view.options.clone());
        let options = Options::new(&chosen);
        self.places = Some(Places::new(folder, view, &chosen, options));
        self.options = Some((folder, options));
        self.paint = Paint::new(&chosen);
        self.reading = None;
        self.moved += 1;
    }

    fn layout(
        &mut self,
        lines: &Arc<Vec<Line>>,
        lanes: usize,
        room: Size<f32>,
        options: Options,
        settled: bool,
    ) -> Rc<Layout> {
        let sorted = self
            .places
            .as_ref()
            .is_some_and(|places| matches!(places.arrangement, Arrangement::By(_)));
        let room = sorted.then_some(room);
        let held = self.layout.clone().filter(|built| {
            std::ptr::eq(built.lines.as_ptr(), Arc::as_ptr(lines))
                && built.lanes == lanes
                && built.room == room
                && built.options == options
                && built.moved == self.moved
        });
        if let Some(built) = held {
            return built;
        }
        let centres = match self.places.as_mut() {
            Some(places) => match (places.arrangement, room) {
                (Arrangement::By(column), Some(room)) => sort_order(lines, column)
                    .into_iter()
                    .map(|at| options.flowed(at, lines.len(), room))
                    .collect(),
                _ => places.place(lines, lanes, options, settled),
            },
            None => (0..lines.len())
                .map(|index| options.slot_centre(options.slot_at(index, lanes)))
                .collect(),
        };
        self.sized |= settled && !lines.is_empty();
        let cell = options.cell;
        let tiles: Vec<Tile> = lines
            .iter()
            .zip(centres)
            .map(|(line, centre)| (line.key.clone(), options.origin(centre)))
            .collect();
        let across = if options.bottom {
            lanes as f32 * cell.width
        } else {
            cell.width
        };
        let width = tiles
            .iter()
            .map(|(_, at)| f32::from(at.x) + cell.width)
            .fold(MARGIN + across, f32::max)
            + MARGIN;
        let height = tiles
            .iter()
            .map(|(_, at)| f32::from(at.y) + cell.height)
            .fold(MARGIN + cell.height, f32::max)
            + MARGIN;
        let mut by_top: Vec<usize> = (0..tiles.len()).collect();
        by_top.sort_by(|&a, &b| f32::from(tiles[a].1.y).total_cmp(&f32::from(tiles[b].1.y)));
        let built = Rc::new(Layout {
            lines: Arc::downgrade(lines),
            lanes,
            room,
            options,
            moved: self.moved,
            tiles,
            width,
            height,
            by_top,
        });
        self.layout = Some(built.clone());
        built
    }

    fn standing(&self) -> Option<(Rc<Layout>, Arc<Vec<Line>>)> {
        let layout = self.layout.clone()?;
        let lines = layout.lines.upgrade()?;
        Some((layout, lines))
    }

    fn reading_order(&self) -> Option<Vec<SharedString>> {
        let (layout, _) = self.standing()?;
        let index = reading_index(
            |at| {
                let point = layout.tiles[at].1;
                (f32::from(point.x), f32::from(point.y))
            },
            layout.tiles.len(),
        );
        Some(
            index
                .into_iter()
                .map(|at| layout.tiles[at].0.clone())
                .collect(),
        )
    }

    fn put(&mut self, moves: impl IntoIterator<Item = (usize, Point<Pixels>)>) {
        let (Some((_, lines)), Some(places)) = (self.standing(), self.places.as_mut()) else {
            return;
        };
        for (index, centre) in moves {
            if let Some(line) = lines.get(index) {
                places.own.insert(line.row.name.clone(), centre);
            }
        }
        places.keep(&lines);
        self.moved += 1;
    }

    fn arrange(&mut self, arrangement: Arrangement) {
        let Some(places) = self.places.as_mut() else {
            return;
        };
        let was = std::mem::replace(&mut places.arrangement, arrangement);
        if was == arrangement {
            return;
        }
        prefs::set_sort_text(ICON_VIEW, places.folder, "arrangeBy", arrangement.id());
        self.moved += 1;
        let Some((layout, _)) = self.standing() else {
            return;
        };
        if let Arrangement::By(_) = was {
            self.put((0..layout.tiles.len()).map(|index| (index, layout.centre(index))));
        }
    }

    fn tidy(&mut self, chosen: impl Fn(&SharedString) -> bool) {
        let Some((layout, _)) = self.standing() else {
            return;
        };
        let options = layout.options;
        let (mut moving, staying): (Vec<usize>, Vec<usize>) =
            (0..layout.tiles.len()).partition(|&index| chosen(&layout.tiles[index].0));
        let mut taken: HashSet<Slot> = staying
            .iter()
            .map(|&index| options.nearest_slot(layout.centre(index)))
            .collect();
        let off = |index: usize| {
            let at = layout.centre(index);
            let on = options.slot_centre(options.nearest_slot(at));
            f32::from(at.x - on.x).hypot(f32::from(at.y - on.y))
        };
        moving.sort_by(|&a, &b| {
            let (at, to) = (layout.centre(a), layout.centre(b));
            off(a)
                .total_cmp(&off(b))
                .then(f32::from(at.y).total_cmp(&f32::from(to.y)))
                .then(f32::from(at.x).total_cmp(&f32::from(to.x)))
        });
        self.put(moving.into_iter().map(|index| {
            (
                index,
                claim_nearest(layout.centre(index), &mut taken, options),
            )
        }));
    }

    fn sort_into(&mut self, column: Column, lanes: usize) {
        let Some((layout, lines)) = self.standing() else {
            return;
        };
        let options = layout.options;
        let order = sort_order(&lines, column);
        self.put(
            order
                .into_iter()
                .enumerate()
                .map(|(index, at)| (index, options.slot_centre(options.slot_at(at, lanes)))),
        );
    }

    fn shift(&mut self, keys: &[SharedString], delta: Point<Pixels>) {
        let Some(arrangement) = self.places.as_ref().map(|places| places.arrangement) else {
            return;
        };
        let Some((layout, _)) = self.standing() else {
            return;
        };
        let dragged: Vec<usize> = (0..layout.tiles.len())
            .filter(|&index| keys.contains(&layout.tiles[index].0))
            .collect();
        let options = layout.options;
        let landed = |index: usize| {
            let (at, icon) = (layout.centre(index) + delta, options.image_centre());
            point(at.x.max(px(icon.x)), at.y.max(px(icon.y)))
        };
        let moves: Vec<(usize, Point<Pixels>)> = match arrangement {
            Arrangement::By(_) => return,
            Arrangement::Free => dragged
                .iter()
                .map(|&index| (index, landed(index)))
                .collect(),
            Arrangement::Grid => {
                let mut taken: HashSet<Slot> = (0..layout.tiles.len())
                    .filter(|index| !dragged.contains(index))
                    .map(|index| options.nearest_slot(layout.centre(index)))
                    .collect();
                dragged
                    .iter()
                    .map(|&index| (index, claim_nearest(landed(index), &mut taken, options)))
                    .collect()
            }
        };
        self.put(moves);
    }

    fn land(&mut self, names: Vec<SharedString>, at: Point<Pixels>) {
        let (Some((layout, lines)), Some(places)) = (self.standing(), self.places.as_mut()) else {
            return;
        };
        let options = layout.options;
        let icon = options.image_centre();
        let at = point(at.x.max(px(icon.x)), at.y.max(px(icon.y)));
        let spots: Vec<Point<Pixels>> = match places.arrangement {
            Arrangement::By(_) => return,
            Arrangement::Free => names.iter().map(|_| at).collect(),
            Arrangement::Grid => {
                let mut taken: HashSet<Slot> = (0..layout.tiles.len())
                    .filter(|&index| !names.contains(&lines[index].row.name))
                    .map(|index| options.nearest_slot(layout.centre(index)))
                    .chain(places.incoming.values().map(|at| options.nearest_slot(*at)))
                    .collect();
                names
                    .iter()
                    .map(|_| claim_nearest(at, &mut taken, options))
                    .collect()
            }
        };
        for (name, spot) in names.into_iter().zip(spots) {
            if lines.iter().any(|line| line.row.name == name) {
                places.own.insert(name, spot)
            } else {
                places.incoming.insert(name, spot)
            };
        }
        places.keep(&lines);
        self.moved += 1;
    }

    pub(crate) fn renamed(&mut self, from: &str, to: &str) {
        let Some(places) = self.places.as_mut() else {
            return;
        };
        if let Some(at) = places.spot(from) {
            places.own.insert(SharedString::from(to.to_owned()), at);
            places.save();
        }
    }

    pub(crate) fn reveal(&self, key: &SharedString, visible: Size<f32>) {
        let Some(layout) = &self.layout else {
            return;
        };
        let Some(at) = layout
            .tiles
            .iter()
            .find(|(tile, _)| tile == key)
            .map(|(_, at)| *at)
        else {
            return;
        };
        let cell = layout.options.cell;
        let offset = self.scroll.offset();
        let into_view = |start: f32, extent: f32, span: f32, seen: f32| {
            if start < seen {
                start
            } else if start + span > seen + extent {
                start + span - extent
            } else {
                seen
            }
        };
        let x = into_view(
            f32::from(at.x),
            visible.width,
            cell.width,
            -f32::from(offset.x),
        );
        let y = into_view(
            f32::from(at.y),
            visible.height,
            cell.height,
            -f32::from(offset.y),
        );
        self.scroll
            .set_offset(point(px(-x.max(0.)), px(-y.max(0.))));
    }

    pub(crate) fn scroll_to_end(&self, end: bool) {
        let x = self.scroll.offset().x;
        let y = if end {
            self.scroll.max_offset().y
        } else {
            px(0.)
        };
        self.scroll.set_offset(point(x, -y));
    }
}

pub(crate) struct TileDrag {
    key: SharedString,
}

impl KeyDrag for TileDrag {
    fn key(&self) -> &SharedString {
        &self.key
    }
}

struct Ghost {
    line: Line,
    options: Options,
    scale: f32,
    previews: bool,
    look: Look,
    tags: Vec<RowTag>,
    info: Option<SharedString>,
}

impl Render for Ghost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let art = item_art(&self.line.row, self.options.size, self.scale, self.previews);
        tile(
            &self.line,
            self.options,
            art,
            self.look,
            true,
            &self.tags,
            None,
            None,
            None,
            self.info.clone(),
        )
        .opacity(GHOST_ALPHA)
    }
}

#[allow(clippy::too_many_arguments)]
fn tile(
    line: &Line,
    options: Options,
    art: AnyElement,
    look: Look,
    selected: bool,
    tags: &[RowTag],
    editor: Option<AnyElement>,
    anchor: Option<Anchor>,
    drop: Option<Hsla>,
    info: Option<SharedString>,
) -> Div {
    let (cell, frame, size) = (options.cell, options.frame(), options.size);
    let pad = options.pad();
    let lit = selected || drop.is_some();
    let pill = drop.unwrap_or(look.selection);
    let ink = if lit { look.selection_text } else { look.label };
    let (ink, fill) = if line.row.hidden {
        (hidden_name(ink), hidden_pill(pill))
    } else {
        (ink, pill)
    };
    let mut icon = div()
        .w(px(size))
        .h(px(size))
        .flex_none()
        .relative()
        .flex()
        .items_center()
        .justify_center();
    if lit {
        icon = icon.child(
            div()
                .absolute()
                .top(px(-pad))
                .left(px(-pad))
                .right(px(-pad))
                .bottom(px(-pad))
                .rounded(px(PLATE_RADIUS))
                .bg(look.backdrop),
        );
    }
    let icon = icon.child(art_at(marked(&line.row, size, art), size, anchor));

    let dots = tag_dots_w(tags);
    let room = if options.bottom {
        cell.width - 2. * LABEL_INSET
    } else {
        cell.width - frame - LABEL_GAP
    } - if dots > 0. { dots + 2. } else { 0. };
    let caption = match editor {
        Some(editor) => {
            let fit =
                line.row.display.chars().count() as f32 * options.text * CHAR_PER_TEXT + EDITOR_PAD;
            div().w(px(fit.min(room))).child(editor).into_any_element()
        }
        None => {
            let shown = elide(&line.row.display, room - 2. * LABEL_PAD, options.text);
            let clipped = shown != line.row.display;
            let caption = div()
                .px(px(LABEL_PAD))
                .max_w(px(room))
                .rounded(px(LABEL_RADIUS))
                .text_size(px(options.text))
                .line_height(px(options.line))
                .line_clamp(((cell.height / options.line) as usize).clamp(1, LABEL_LINES))
                .text_align(if options.bottom {
                    TextAlign::Center
                } else {
                    TextAlign::Left
                })
                .text_color(ink)
                .when(lit, |label| label.bg(fill))
                .child(shown);
            if clipped {
                native_tooltip("ng.finder.name.tip", line.row.display.clone(), caption)
                    .into_any_element()
            } else {
                caption.into_any_element()
            }
        }
    };
    let label = div()
        .flex()
        .items_start()
        .gap(px(4.))
        .child(caption)
        .children(tag_dots(tags));
    let label = match info {
        Some(info) => div()
            .flex()
            .flex_col()
            .when(options.bottom, |text| text.items_center())
            .child(label)
            .child(
                div()
                    .px(px(LABEL_PAD))
                    .max_w(px(room))
                    .text_size(px(options.text))
                    .line_height(px(options.line))
                    .text_color(look.info)
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(info),
            ),
        None => label,
    };

    let tile = div()
        .w(px(cell.width))
        .h(px(cell.height))
        .flex()
        .gap(px(pad + LABEL_GAP));
    if options.bottom {
        tile.flex_col().items_center().pt(px(pad))
    } else {
        tile.items_center().pl(px(pad))
    }
    .child(icon)
    .child(label)
}

fn tile_at(layout: &[Tile], at: Point<Pixels>, cell: Size<f32>) -> Option<usize> {
    layout.iter().rposition(|(_, tile)| {
        at.x >= tile.x
            && at.x < tile.x + px(cell.width)
            && at.y >= tile.y
            && at.y < tile.y + px(cell.height)
    })
}

fn elide(name: &SharedString, room: f32, text: f32) -> SharedString {
    let per_line = (room / (text * CHAR_PER_TEXT)).floor() as usize;
    let budget = per_line * LABEL_LINES;
    let count = name.chars().count();
    if count <= budget || budget < ELIDE_MIN {
        return name.clone();
    }
    let head = budget - 1 - ELIDE_TAIL;
    let at = |nth: usize| {
        name.char_indices()
            .nth(nth)
            .map_or(name.len(), |(at, _)| at)
    };
    SharedString::from(format!(
        "{}…{}",
        &name[..at(head)],
        &name[at(count - ELIDE_TAIL)..]
    ))
}

impl FinderDemo {
    pub(crate) fn icon_body(
        &mut self,
        palette: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !cx.has_active_drag() {
            self.icons.dragging = None;
        }

        let lines = if self.know_places(cx) {
            self.lines(false)
        } else {
            Arc::default()
        };
        let lanes = self.columns(window);
        let room = self.icon_room(f32::from(window.viewport_size().width));
        let options = self.icon_options();
        let paint = self.icon_paint();
        let scale = self.scale;
        let cell = options.cell;
        let look = Look::new(palette);
        let this = cx.entity().downgrade();
        let scroll = self.icons.scroll.clone();
        let dragging = self.icons.dragging.clone();

        let settled = self
            .folders
            .get(self.here())
            .is_some_and(|folder| !folder.loading);
        let layout = self.icons.layout(&lines, lanes, room, options, settled);
        let (width, height) = (layout.width, layout.height);
        let inset = self.under_bars();
        let shift = self.icon_shift();
        let mut canvas = div()
            .id("ng.finder.icons")
            .flex_1()
            .relative()
            // Runs on beneath the pane, but scrolls only as far as the pane's edge.
            .pr(self.metrics().right)
            .overflow_x_scroll()
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .on_scroll_wheel(stretching(
                |this| (&mut this.bands.icons, this.icons.scroll.clone()),
                cx,
            ))
            .map(|canvas| match paint.background {
                Background::Color([r, g, b]) => canvas.bg(Rgba { r, g, b, a: 1. }),
                Background::Picture(path) if !path.is_empty() => {
                    canvas.child(img(Path::new(&path)).absolute().left(shift.x).top(shift.y))
                }
                _ => canvas,
            })
            // Tiles are absolute and add no scroll extent; this spacer does.
            .child(
                div()
                    .w(px(width))
                    .h(px(height) + inset)
                    .flex_none(),
            );

        let top = -f32::from(self.icons.scroll.offset().y + inset);
        let tall = f32::from(window.viewport_size().height);
        let visible = layout.visible(top, tall);
        let seen = visible.iter().copied();
        self.report_scene(cx, self.here(), &lines, seen, options.size, paint.previews);
        for index in visible {
            let line = &lines[index];
            let at = layout.tiles[index].1;
            let is_selected = self.selected.contains(&line.key);
            let carried = row_tags(&line.key);
            let held = dragging.as_ref() == Some(&line.key);
            let editor = (self.renaming.as_ref() == Some(&line.key)).then(|| {
                rename_field(
                    line,
                    index,
                    self.typed_ahead.as_deref(),
                    self.rename_draft.as_ref(),
                    this.clone(),
                )
                .into_any_element()
            });
            let info = options.info.then(|| item_info(&line.row)).flatten();

            let tile = div()
                .id(("ng.finder.tile", index))
                .absolute()
                .left(at.x + shift.x)
                .top(at.y + shift.y)
                .w(px(cell.width))
                .h(px(cell.height))
                .child(tile(
                    line,
                    options,
                    item_art(&line.row, options.size, scale, paint.previews),
                    look,
                    is_selected,
                    &carried,
                    editor,
                    (self.cursor.as_ref() == Some(&line.key)).then(|| self.ql_icon.clone()),
                    self.drop_fill(&line.key, look.selection),
                    info.clone(),
                ))
                .when(held, |tile| tile.opacity(SOURCE_ALPHA))
                .on_drag(
                    TileDrag {
                        key: line.key.clone(),
                    },
                    {
                        let this = this.clone();
                        let line = line.clone();
                        move |drag: &TileDrag, _, _window, cx: &mut App| {
                            let key = drag.key.clone();
                            this.update(cx, |this: &mut FinderDemo, _| {
                                this.icons.dragging = Some(key);
                            })
                            .ok();
                            cx.new(|_| Ghost {
                                line: line.clone(),
                                options,
                                scale,
                                previews: paint.previews,
                                look,
                                tags: carried.clone(),
                                info: info.clone(),
                            })
                        }
                    },
                )
                .on_mouse_down(MouseButton::Left, pick_on_press(line, this.clone()))
                .on_click(pick_on_click(line, this.clone()));
            let tile = match line.row.opens {
                Some(into) => drop_target::<ExternalPaths>(
                    drop_target::<RowDrag>(
                        drop_hover::<TileDrag>(tile, line.key.clone(), into, None, this.clone()),
                        line.key.clone(),
                        into,
                        None,
                        Lit::Drawn,
                        this.clone(),
                    ),
                    line.key.clone(),
                    into,
                    None,
                    Lit::Drawn,
                    this.clone(),
                ),
                None => tile,
            };
            canvas = canvas.child(tile);
        }

        let dropped = layout.clone();
        let drop_scroll = scroll.clone();
        let this_drop = this.clone();
        canvas = canvas.on_drop(move |drag: &TileDrag, window, cx| {
            if !dropped.tiles.iter().any(|(key, _)| key == &drag.key) {
                return;
            }
            let landed = window.mouse_position()
                - drop_scroll.bounds().origin
                - drop_scroll.offset()
                - shift;
            let under = tile_at(&dropped.tiles, landed, cell).and_then(|index| lines.get(index));
            this_drop
                .update(cx, |this: &mut FinderDemo, cx| {
                    let selected = this.selected.snapshot();
                    let into = under
                        .filter(|line| !drag.carries(&selected, &line.key))
                        .and_then(|line| line.row.opens);
                    if let Some(into) = into {
                        if !drag.wandered(window, this.pressed_at) {
                            return;
                        }
                        this.drop_into(into, &drag.keys(&selected), window, cx);
                        this.icons.dragging = None;
                        cx.notify();
                        return;
                    }
                    let group: Vec<SharedString> = if selected.contains(&drag.key) {
                        dropped
                            .tiles
                            .iter()
                            .map(|(key, _)| key)
                            .filter(|key| selected.contains(*key))
                            .cloned()
                            .collect()
                    } else {
                        vec![drag.key.clone()]
                    };
                    this.icons
                        .shift(&group, window.mouse_position() - this.pressed_at);
                    this.icons.dragging = None;
                    cx.notify();
                })
                .ok();
        });

        let hit_layout = layout.clone();
        let hit_scroll = scroll.clone();
        let hit = move |demo: &FinderDemo, at: Point<Pixels>| {
            let at = at - hit_scroll.offset() - shift;
            let index = tile_at(&hit_layout.tiles, at, cell)?;
            demo.lines(false).get(index).cloned()
        };

        // Hit-tested by hand: GPUI's drag is a bubble listener and misses a press a tile swallows.
        let canvas =
            canvas.on_mouse_down(MouseButton::Left, move |event: &MouseDownEvent, _, cx| {
                let at = event.position - scroll.bounds().origin - scroll.offset() - shift;
                if tile_at(&layout.tiles, at, cell).is_some() {
                    return;
                }
                this.update(cx, |this: &mut FinderDemo, cx| {
                    this.deselect();
                    cx.notify();
                })
                .ok();
            });
        tiles_menu("ng.finder.icons.menu", cx.entity().downgrade(), hit, canvas).into_any_element()
    }

    fn keeps_places(&self) -> bool {
        self.here().starts_with('/')
            && !self.results()
            && self.filtered.is_empty()
            && self.type_filter.is_none()
    }

    fn know_places(&mut self, cx: &mut Context<Self>) -> bool {
        let here = self.here();
        if !self.keeps_places() {
            self.icons.places = None;
            if self.icons.options.is_none_or(|(at, _)| at != here) {
                let chosen = prefs::icon_options(here);
                self.icons.options = Some((here, Options::new(&chosen)));
                self.icons.paint = Paint::new(&chosen);
            }
            return true;
        }
        if self
            .icons
            .places
            .as_ref()
            .is_some_and(|places| places.folder == here)
        {
            return true;
        }
        if self.icons.reading == Some(here) {
            return false;
        }
        let (sent, answer) = mpsc::channel();
        std::thread::spawn(move || sent.send(ds_store::icon_view(Path::new(here))).ok());
        if let Ok(view) = answer.recv_timeout(FINDER_WAIT) {
            self.icons.read(here, view);
            return true;
        }
        self.icons.reading = Some(here);
        cx.spawn(async move |this, cx| {
            let view = loop {
                cx.background_executor().timer(FINDER_WAIT).await;
                match answer.try_recv() {
                    Ok(view) => break view,
                    Err(TryRecvError::Empty) => continue,
                    Err(TryRecvError::Disconnected) => return,
                }
            };
            this.update(cx, |this, cx| {
                if this.icons.reading == Some(here) {
                    this.icons.read(here, view);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        false
    }

    pub(crate) fn icon_options(&self) -> Options {
        match self.icons.options {
            Some((at, options)) if at == self.here() => options,
            _ => Options::new(&prefs::icon_options(self.here())),
        }
    }

    pub(crate) fn icon_paint(&self) -> Paint {
        match self.icons.options {
            Some((at, _)) if at == self.here() => self.icons.paint.clone(),
            _ => Paint::new(&prefs::icon_options(self.here())),
        }
    }

    pub(crate) fn change_icon_options(
        &mut self,
        change: impl FnOnce(&mut IconOptions),
        cx: &mut Context<Self>,
    ) {
        let here = self.here();
        let mut chosen = prefs::icon_options(here);
        change(&mut chosen);
        let was = self.icon_options();
        let places = self
            .icons
            .places
            .as_mut()
            .filter(|places| places.folder == here);
        let recorded = places
            .as_ref()
            .map(|places| places.recorded.clone())
            .unwrap_or_default();
        let recorded_look = IconOptions {
            arrange_by: None,
            ..recorded.clone()
        } != IconOptions::default();
        let home = recorded_look
            .then_some(here)
            .or_else(|| prefs::look_home(ICON_VIEW, here));
        prefs::set_icon_options(home, &chosen);
        let chosen = chosen.or(recorded);
        let options = Options::new(&chosen);
        if let Some(places) = places {
            for spots in [
                &mut places.own,
                &mut places.finder,
                &mut places.incoming,
                &mut places.placed,
            ] {
                for at in spots.values_mut() {
                    *at = options.moved_from(was, *at);
                }
            }
            places.save();
        }
        self.icons.options = Some((here, options));
        self.icons.paint = Paint::new(&chosen);
        self.icons.moved += 1;
        cx.notify();
    }

    pub(crate) fn land_icons(&mut self, keys: &[SharedString], at: Point<Pixels>) {
        let scroll = &self.icons.scroll;
        let at = at - scroll.bounds().origin - scroll.offset() - self.icon_shift();
        self.icons.land(keys.iter().filter_map(named).collect(), at);
    }

    fn icon_shift(&self) -> Point<Pixels> {
        self.bands.icons.shown() + point(px(0.), self.under_bars())
    }

    pub(crate) fn arrangement(&self) -> Option<Arrangement> {
        let here = self.here();
        self.icons
            .places
            .as_ref()
            .filter(|places| places.folder == here)
            .map(|places| places.arrangement)
    }

    // Arrange By follows that column. Free and grid icons are read left to right, then down.
    pub(crate) fn icon_drag_order(&self) -> Option<Vec<SharedString>> {
        if self.view != ICON_VIEW {
            return None;
        }
        match self.arrangement() {
            Some(Arrangement::By(column)) => Some(keys_in_order(&self.lines(false), column)),
            _ => self.icons.reading_order(),
        }
    }

    pub(crate) fn cleans_up(&self) -> bool {
        self.view == ICON_VIEW
            && matches!(
                self.arrangement(),
                Some(Arrangement::Free | Arrangement::Grid)
            )
    }

    pub(crate) fn arrange(&mut self, arrangement: Arrangement, cx: &mut Context<Self>) {
        self.icons.arrange(arrangement);
        cx.notify();
    }

    pub(crate) fn clean_up(&mut self, selection: bool, cx: &mut Context<Self>) {
        if !self.cleans_up() {
            return;
        }
        let chosen = self.selected.snapshot();
        self.icons.tidy(|key| !selection || chosen.contains(key));
        cx.notify();
    }

    pub(crate) fn clean_up_by(&mut self, column: Column, window: &Window, cx: &mut Context<Self>) {
        if !self.cleans_up() {
            return;
        }
        let lanes = self.fits(window);
        self.icons.sort_into(column, lanes);
        cx.notify();
    }

    pub(crate) fn icon_step(&self, key: &str, from: Option<&SharedString>) -> Option<SharedString> {
        let layout = self.icons.layout.as_ref()?;
        let tiles = &layout.tiles;
        let reading = |a: &&Tile, b: &&Tile| {
            f32::from(a.1.y)
                .total_cmp(&f32::from(b.1.y))
                .then(f32::from(a.1.x).total_cmp(&f32::from(b.1.x)))
        };
        let Some(here) = from.and_then(|from| tiles.iter().find(|(tile, _)| tile == from)) else {
            let pick = match key {
                "down" | "right" => tiles.iter().min_by(reading),
                _ => tiles.iter().max_by(reading),
            };
            return pick.map(|(tile, _)| tile.clone());
        };
        let reach = |(_, at): &&Tile| {
            let (x, y) = (f32::from(at.x - here.1.x), f32::from(at.y - here.1.y));
            let (ahead, aside) = match key {
                "left" => (-x, y),
                "right" => (x, y),
                "up" => (-y, x),
                _ => (y, x),
            };
            (ahead > 0.5).then_some(ahead + OFF_AXIS_WEIGHT * aside.abs())
        };
        let next = tiles
            .iter()
            .filter_map(|tile| Some((reach(&tile)?, tile)))
            .min_by(|(a, _), (b, _)| a.total_cmp(b))
            .map_or(here, |(_, tile)| tile);
        Some(next.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use finder_fs::entry::{Entry, EntryKind};

    use super::*;

    fn line(name: &str, size: u64, age: u64) -> Line {
        let entry = Entry {
            name: name.into(),
            kind: EntryKind::File,
            size,
            modified: UNIX_EPOCH + Duration::from_secs(age),
            created: UNIX_EPOCH,
            added: None,
            hidden: false,
            package: false,
            ext_hidden: false,
            alias: false,
            custom_icon: false,
            locked: false,
            dataless: false,
            readable: true,
            executable: false,
            file_id: 0,
            children: None,
        };
        let row = crate::data::hit_row(&Path::new("/tmp/found").join(name), &entry);
        Line {
            key: row.path.clone(),
            row,
            depth: 0,
        }
    }

    #[test]
    fn tiles_rank_by_the_chosen_column() {
        let lines = [line("a", 1, 30), line("b", 3, 10), line("c", 2, 20)];
        assert_eq!(sort_order(&lines, Column::Name), [0, 1, 2]);
        assert_eq!(sort_order(&lines, Column::Size), [2, 0, 1]);
        assert_eq!(sort_order(&lines, Column::Modified), [0, 2, 1]);
        let names: Vec<&str> = keys_in_order(&lines, Column::Size)
            .iter()
            .map(|key| {
                lines
                    .iter()
                    .find(|line| &line.key == key)
                    .unwrap()
                    .row
                    .name
                    .as_ref()
            })
            .collect();
        assert_eq!(names, ["b", "c", "a"]);
    }

    #[test]
    fn icons_are_read_across_each_row() {
        // 17 icons, 5 across: the IMG_4526…IMG_4542 grid, stored back to front.
        let count = 17;
        let across = 5;
        let stored: Vec<usize> = (0..count).rev().collect();
        let read = reading_index(
            |at| {
                let slot = stored[at];
                ((slot % across) as f32, (slot / across) as f32)
            },
            count,
        );
        let names: Vec<usize> = read.into_iter().map(|at| stored[at]).collect();
        assert_eq!(names, (0..count).collect::<Vec<_>>());
    }
}
