/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::tui::View;
use connector::caches::NarSummary;
use ratatui::crossterm::event::{KeyCode, KeyEvent};

pub struct NarBrowser {
    all: Vec<NarSummary>,
    filtered_idx: Vec<usize>,
    pub selected: usize,
    pub filter: String,
}

impl NarBrowser {
    pub fn new(all: Vec<NarSummary>) -> Self {
        let mut b = Self { all, filtered_idx: Vec::new(), selected: 0, filter: String::new() };
        b.recompute();
        b
    }

    fn recompute(&mut self) {
        let f = self.filter.to_lowercase();
        self.filtered_idx = self.all.iter().enumerate()
            .filter(|(_, n)| f.is_empty() || n.package.to_lowercase().contains(&f) || n.hash.contains(&f))
            .map(|(i, _)| i)
            .collect();
        self.selected = 0;
    }

    pub fn visible(&self) -> Vec<&NarSummary> {
        self.filtered_idx.iter().map(|&i| &self.all[i]).collect()
    }

    pub fn push_filter(&mut self, c: char) { self.filter.push(c); self.recompute(); }
    pub fn pop_filter(&mut self) { self.filter.pop(); self.recompute(); }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.filtered_idx.len() { self.selected += 1; }
    }

    pub fn move_up(&mut self) { self.selected = self.selected.saturating_sub(1); }

    #[allow(dead_code)]
    pub fn selected_item(&self) -> Option<&NarSummary> {
        self.filtered_idx.get(self.selected).map(|&i| &self.all[i])
    }
}

impl View for NarBrowser {
    fn render(&mut self, frame: &mut ratatui::Frame) {
        use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
        let items: Vec<ListItem> = self.visible().iter()
            .map(|n| ListItem::new(format!("{}  {}", &n.hash[..n.hash.len().min(12)], n.package)))
            .collect();
        let mut state = ListState::default();
        if !self.filtered_idx.is_empty() { state.select(Some(self.selected)); }
        let title = format!("NARs ({})  filter: {}", self.filtered_idx.len(), self.filter);
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_symbol("> ");
        frame.render_stateful_widget(list, frame.area(), &mut state);
    }

    fn on_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => return true,
            KeyCode::Down => self.move_down(),
            KeyCode::Up => self.move_up(),
            KeyCode::Backspace => self.pop_filter(),
            KeyCode::Char(c) => self.push_filter(c),
            _ => {}
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(hash: &str, pkg: &str) -> NarSummary {
        NarSummary {
            hash: hash.into(),
            store_path: format!("/nix/store/{hash}-{pkg}"),
            package: pkg.into(),
            nar_size: Some(1),
            file_size: Some(1),
            created_at: "2026-01-01T00:00:00".into(),
            last_fetched_at: None,
        }
    }

    #[test]
    fn filter_narrows_and_resets_selection() {
        let mut b = NarBrowser::new(vec![item("a", "hello"), item("b", "zlib"), item("c", "hella")]);
        b.selected = 2;
        b.push_filter('h');
        b.push_filter('e');
        assert_eq!(b.visible().len(), 2);
        assert_eq!(b.selected, 0);
    }

    #[test]
    fn move_down_clamps_to_last() {
        let mut b = NarBrowser::new(vec![item("a", "x"), item("b", "y")]);
        b.move_down(); b.move_down(); b.move_down();
        assert_eq!(b.selected, 1);
    }

    #[test]
    fn filter_by_hash_prefix() {
        let mut b = NarBrowser::new(vec![item("abc111", "x"), item("def222", "y")]);
        b.push_filter('a'); b.push_filter('b');
        assert_eq!(b.visible().len(), 1);
    }
}
