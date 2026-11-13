use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use finder_fs::privacy;
use gpui::{Context, ScrollStrategy, SharedString, Window};

use crate::app::FinderDemo;
use crate::data::Line;
use crate::listing::{Edit, folds_to};
use crate::menus::SelectAll;
use crate::view::{COLUMN_VIEW, LIST_VIEW};

pub(crate) fn outermost_by<T>(items: Vec<T>, path: impl Fn(&T) -> &Path) -> Vec<T> {
    let all: HashSet<&Path> = items.iter().map(&path).collect();
    let keep: Vec<bool> = items
        .iter()
        .map(|item| !path(item).ancestors().skip(1).any(|up| all.contains(up)))
        .collect();
    items
        .into_iter()
        .zip(keep)
        .filter_map(|(item, keep)| keep.then_some(item))
        .collect()
}

pub(crate) fn outermost(keys: Vec<SharedString>) -> Vec<SharedString> {
    outermost_by(keys, |key| Path::new(key.as_ref()))
}

/// `place` is an index in the view. Keys the view is not showing keep their relative order, last.
pub(crate) fn order_for_drag<T>(keys: Vec<T>, place: impl Fn(&T) -> Option<usize>) -> Vec<T> {
    let mut ranked: Vec<(usize, usize, T)> = keys
        .into_iter()
        .enumerate()
        .map(|(seq, key)| (place(&key).unwrap_or(usize::MAX), seq, key))
        .collect();
    ranked.sort_by_key(|(rank, seq, _)| (*rank, *seq));
    ranked.into_iter().map(|(_, _, key)| key).collect()
}

const TYPING: Duration = Duration::from_secs(1);

const DEEP_LEVELS: usize = 16;
const DEEP_FOLDERS: usize = 5_000;

impl FinderDemo {
    pub(crate) fn reveal(&mut self, key: &SharedString) {
        if self.view == COLUMN_VIEW {
            if self.column_reveal_row(key) {
                self.reveal_next.take_if(|next| *next == *key);
            }
            return;
        }
        let lines = self.lines(self.view == LIST_VIEW);
        if let Some(index) = self.line_at(&lines, key) {
            self.reveal_next.take_if(|next| *next == *key);
            self.scroll.scroll_to_item(
                self.slot_of(&lines, index) + self.list_inset().0,
                ScrollStrategy::Center,
            );
        }
    }

    pub(crate) fn disclose_selected(&mut self, open: bool, deep: bool) {
        if self.flat_results() {
            return;
        }
        let lines = self.lines(true);
        let chosen = self.selected.snapshot();
        let one = match chosen.len() {
            0 => self.cursor.clone(),
            1 => chosen.iter().next().cloned(),
            _ => None,
        };
        let only = one.as_ref().and_then(|key| self.line_at(&lines, key));
        let mut folders: Vec<(SharedString, &'static str)> = Vec::new();
        let mut edits: Vec<Edit> = Vec::new();
        let mut take = |at: usize, line: &Line| {
            if let Some(into) = line
                .row
                .opens
                .filter(|_| line.row.discloses && line.row.readable)
            {
                folders.push((line.key.clone(), into));
                edits.push((at, line.key.clone(), open.then_some(into)));
            }
        };
        if let Some(at) = only {
            take(at, &lines[at]);
        } else if one.is_none() {
            for (at, line) in lines.iter().enumerate() {
                if line.row.opens.is_some() && chosen.contains(&line.key) {
                    take(at, line);
                }
            }
        }
        let expandable = folders
            .iter()
            .filter(|(key, _)| self.expanded.contains(key) != open)
            .count();
        if expandable > 0 || (deep && open) {
            let was = self.expanded.rev();
            self.disclose_all(&folders, open, deep);
            self.disclosed(was, edits);
            return;
        }
        let Some(index) = only.filter(|_| !open) else {
            return;
        };
        let depth = lines[index].depth;
        if let Some(parent) = lines[..index].iter().rev().find(|line| line.depth < depth) {
            self.pick(parent, false, false);
            self.reveal(&parent.key);
        }
    }

    pub(crate) fn disclose_at(
        &mut self,
        at: usize,
        key: &SharedString,
        into: Option<&'static str>,
        open: bool,
        deep: bool,
    ) {
        let was = self.expanded.rev();
        match into.filter(|_| deep) {
            Some(into) => self.disclose_all(&[(key.clone(), into)], open, true),
            None => self.disclose(key, open),
        }
        if let Some(into) = into {
            self.disclosed(was, vec![(at, key.clone(), open.then_some(into))]);
        }
    }

    fn disclose_all(&mut self, folders: &[(SharedString, &'static str)], open: bool, deep: bool) {
        if open {
            let mut opened: Vec<SharedString> = Vec::with_capacity(folders.len());
            for (key, into) in folders {
                opened.push(key.clone());
                if deep {
                    self.expand_under(into, 0, &mut opened);
                }
            }
            self.expanded.extend(opened);
            return;
        }
        let shut: HashSet<&SharedString> = folders.iter().map(|(key, _)| key).collect();
        let under: HashSet<&str> = if deep {
            folders
                .iter()
                .map(|(_, into)| into.trim_end_matches('/'))
                .collect()
        } else {
            HashSet::new()
        };
        let inside = |path: &str| {
            let mut rest = path;
            while let Some(cut) = rest.rfind('/') {
                rest = &rest[..cut];
                if under.contains(rest) {
                    return true;
                }
            }
            false
        };
        self.expanded
            .retain(|key| !shut.contains(key) && !inside(key));
        if deep {
            self.expanding.retain(|id, _| !inside(id));
        }
    }

    pub(crate) fn chosen_lines(&self) -> Vec<Line> {
        self.chosen_in(&self.focused_lines(), usize::MAX)
    }

    pub(crate) fn chosen_in(&self, lines: &Arc<Vec<Line>>, most: usize) -> Vec<Line> {
        if self.selected.len() * 16 >= lines.len() {
            return lines
                .iter()
                .filter(|line| self.selected.contains(&line.key))
                .take(most)
                .cloned()
                .collect();
        }
        let mut at: Vec<usize> = self
            .selected
            .iter()
            .filter_map(|key| self.line_at(lines, key))
            .collect();
        at.sort_unstable();
        at.into_iter()
            .take(most)
            .map(|at| lines[at].clone())
            .collect()
    }

    /// What a context menu on `line` acts on: the selection when the line is in it.
    pub(crate) fn subject(&self, line: &Line) -> Vec<PathBuf> {
        if self.selected.contains(&line.key) {
            self.paths()
        } else if crate::remote::is_remote(&line.key) {
            Vec::new()
        } else {
            vec![PathBuf::from(line.key.as_ref())]
        }
    }

    pub(crate) fn chosen_keys(&self) -> Vec<SharedString> {
        self.chosen_lines()
            .into_iter()
            .map(|line| line.key)
            .collect()
    }

    pub(crate) fn chosen_folders(&self) -> Vec<&'static str> {
        self.chosen_lines()
            .iter()
            .filter_map(|line| line.row.opens)
            .collect()
    }

    fn expand_under(&mut self, id: &'static str, depth: usize, opened: &mut Vec<SharedString>) {
        let home = crate::home();
        let mut walk = vec![(id, depth)];
        while let Some((id, depth)) = walk.pop() {
            if depth >= DEEP_LEVELS || self.expanded.len() + opened.len() >= DEEP_FOLDERS {
                continue;
            }
            let Some(folder) = self.folders.get(id) else {
                self.expanding.insert(id, depth);
                continue;
            };
            if folder.loading {
                self.expanding.insert(id, depth);
            } else {
                self.expanding.remove(id);
            }
            for row in folder.rows.iter().filter(|row| row.discloses) {
                let Some(deeper) = row.opens else { continue };
                if crate::remote::is_remote(deeper)
                    || !privacy::off_limits(Path::new(deeper), &home)
                {
                    opened.push(row.path.clone());
                    walk.push((deeper, depth + 1));
                }
            }
        }
    }

    pub(crate) fn expand_landed(&mut self, id: &'static str) {
        if let Some(depth) = self.expanding.remove(id) {
            let mut opened: Vec<SharedString> = Vec::new();
            self.expand_under(id, depth, &mut opened);
            self.expanded.extend(opened);
        }
    }

    pub(crate) fn tab(&mut self, backwards: bool) {
        let lines = self.lines(self.view == LIST_VIEW);
        if lines.is_empty() {
            return;
        }
        let by_name = self.name_order(&lines);
        let at = self
            .cursor
            .as_ref()
            .filter(|key| self.selected.contains(*key))
            .and_then(|key| self.line_at(&lines, key))
            .and_then(|index| by_name.iter().position(|&i| i == index));
        let next = match (at, backwards) {
            (Some(at), false) => (at + 1) % by_name.len(),
            (Some(at), true) => (at + by_name.len() - 1) % by_name.len(),
            (None, false) => 0,
            (None, true) => by_name.len() - 1,
        };
        let line = &lines[by_name[next]];
        self.pick(line, false, false);
        self.reveal(&line.key);
    }

    pub(crate) fn type_select(&mut self, typed: &str, cx: &mut Context<Self>) {
        let now = Instant::now();
        if self
            .typed_at
            .is_some_and(|at| now.duration_since(at) > TYPING)
        {
            self.clear_type_filter();
        }
        self.typed.push_str(&typed.to_lowercase());
        self.typed_at = Some(now);
        let lines = self.focused_lines();
        let typed = self.typed.as_str();
        let prefix = lines.iter().find(|line| {
            if typed.is_ascii() && line.row.name.is_ascii() {
                line.row
                    .name
                    .as_bytes()
                    .get(..typed.len())
                    .is_some_and(|head| head.eq_ignore_ascii_case(typed.as_bytes()))
            } else {
                finder_fs::sort::starts_with(&line.row.name, typed)
            }
        });
        match prefix {
            Some(line) => {
                self.type_filter = None;
                self.type_filter_seen_match = true;
                self.pick_in(&lines, line, false, false);
                self.reveal(&line.key);
            }
            None => {
                let wanted = finder_fs::sort::fold(&self.typed);
                if !lines.iter().any(|line| folds_to(&line.row.name, &wanted)) {
                    return;
                }
                self.type_filter = Some(self.typed.clone());
                if self.type_filter_seen_match {
                    self.schedule_filter_clear(cx);
                }
            }
        }
    }

    pub(crate) fn typing(&self) -> bool {
        self.typed_at.is_some_and(|at| at.elapsed() <= TYPING)
    }

    pub(crate) fn clear_type_filter(&mut self) {
        self.typed.clear();
        self.typed_at = None;
        self.type_filter = None;
        self.type_filter_seen_match = false;
    }

    fn schedule_filter_clear(&mut self, cx: &mut Context<Self>) {
        let since = self.typed_at;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1500))
                .await;
            this.update(cx, |this, cx| {
                if this.typed_at == since {
                    this.clear_type_filter();
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn pick(&mut self, line: &Line, extend: bool, toggle: bool) {
        let lines = self.lines(self.view == LIST_VIEW);
        self.pick_in(&lines, line, extend, toggle);
    }

    pub(crate) fn pick_in(
        &mut self,
        lines: &Arc<Vec<Line>>,
        line: &Line,
        extend: bool,
        toggle: bool,
    ) {
        if toggle {
            if !self.selected.remove(&line.key) {
                self.selected.insert(line.key.clone());
            }
            self.anchor = Some(line.key.clone());
        } else if extend {
            let anchor = self
                .anchor
                .as_ref()
                .or(self.cursor.as_ref())
                .and_then(|key| self.line_at(lines, key))
                .unwrap_or(0);
            if let Some(target) = self.line_at(lines, &line.key) {
                let (lo, hi) = (anchor.min(target), anchor.max(target));
                self.selected = lines[lo..=hi].iter().map(|line| line.key.clone()).collect();
            }
        } else {
            self.selected.clear();
            self.selected.insert(line.key.clone());
            self.anchor = Some(line.key.clone());
        }
        self.cursor = Some(line.key.clone());
    }

    pub(crate) fn deselect(&mut self) {
        self.selected.clear();
        self.cursor = None;
        self.anchor = None;
    }

    pub(crate) fn disclose(&mut self, key: &SharedString, open: bool) {
        if open {
            self.expanded.insert(key.clone());
        } else {
            self.expanded.remove(key);
        }
    }

    pub(crate) fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = self
            .focused_lines()
            .iter()
            .map(|line| line.key.clone())
            .collect();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    fn outermost(items: Vec<PathBuf>) -> Vec<PathBuf> {
        super::outermost_by(items, |item| Path::new(item))
    }

    fn paths(list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn drops_descendants_and_keeps_order() {
        let got = outermost(paths(&["/a/b/c", "/z", "/a", "/a/b", "/z/y"]));
        assert_eq!(got, paths(&["/z", "/a"]));
    }

    #[test]
    fn sibling_with_shared_prefix_stays() {
        let got = outermost(paths(&["/a/b", "/a/bc", "/a/b/c"]));
        assert_eq!(got, paths(&["/a/b", "/a/bc"]));
    }

    #[test]
    fn remote_keys_and_duplicates() {
        let got = outermost(paths(&["sftp://h/a/b", "sftp://h/a", "/x", "/x"]));
        assert_eq!(got, paths(&["sftp://h/a", "/x", "/x"]));
    }

    fn shots(from: u32, to: u32) -> Vec<String> {
        (from..=to).map(|n| format!("/Shots/IMG_{n}.jpg")).collect()
    }

    #[test]
    fn dragged_files_follow_the_view_not_the_order_they_were_grabbed() {
        let photos = shots(4522, 4525);
        let at = |key: &String| photos.iter().position(|name| name == key);
        // The permutation a browser stored after a drop, then an arbitrary click order.
        let uploaded = vec![
            photos[2].clone(),
            photos[3].clone(),
            photos[0].clone(),
            photos[1].clone(),
        ];
        assert_eq!(super::order_for_drag(uploaded, at), photos);
        let clicked = vec![
            photos[3].clone(),
            photos[0].clone(),
            photos[2].clone(),
            photos[1].clone(),
        ];
        assert_eq!(super::order_for_drag(clicked, at), photos);

        let batch = shots(4526, 4542);
        let at = |key: &String| batch.iter().position(|name| name == key);
        let mut grabbed = batch.clone();
        grabbed.rotate_left(7);
        assert_eq!(super::order_for_drag(grabbed, at), batch);
    }

    #[test]
    fn dragged_files_follow_the_view_when_that_is_not_filename_order() {
        let dated = ["c.jpg", "a.jpg", "b.jpg"].map(String::from);
        let at = |key: &String| dated.iter().position(|name| name == key);
        let clicked = vec![dated[2].clone(), dated[0].clone(), dated[1].clone()];
        assert_eq!(super::order_for_drag(clicked, at), dated.to_vec());
        let with_strays = vec![
            String::from("c.jpg"),
            String::from("gone.jpg"),
            String::from("a.jpg"),
        ];
        assert_eq!(super::order_for_drag(with_strays, at), vec![
            String::from("c.jpg"),
            String::from("a.jpg"),
            String::from("gone.jpg"),
        ]);
    }

    #[test]
    fn an_outgoing_drag_keeps_view_order_after_nested_items_are_dropped() {
        let view = paths(&["/a", "/a/b", "/m", "/z"]);
        let grabbed = outermost(paths(&["/z", "/a/b", "/a", "/m"]));
        let at = |key: &PathBuf| view.iter().position(|name| name == key);
        assert_eq!(
            super::order_for_drag(grabbed, at),
            paths(&["/a", "/m", "/z"])
        );
    }
}
