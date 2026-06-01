/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::tui::View;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use tokio::sync::mpsc::UnboundedReceiver;

pub struct LogView {
    lines: Vec<String>,
    offset: usize,
    viewport: usize,
    follow: bool,
    rx: Option<UnboundedReceiver<String>>,
    searching: bool,
    query: String,
}

impl Default for LogView {
    fn default() -> Self {
        Self {
            lines: Vec::new(),
            offset: 0,
            viewport: 1,
            follow: true,
            rx: None,
            searching: false,
            query: String::new(),
        }
    }
}

impl LogView {
    pub fn streaming(rx: UnboundedReceiver<String>) -> Self {
        Self { rx: Some(rx), ..Self::default() }
    }

    #[cfg(test)]
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub fn set_viewport(&mut self, n: usize) {
        self.viewport = n.max(1);
        self.clamp();
    }

    pub fn push_line(&mut self, line: String) {
        self.lines.push(line);
        if self.follow {
            self.scroll_to_bottom();
        }
    }

    fn scroll_to_bottom(&mut self) {
        self.offset = self.lines.len().saturating_sub(self.viewport);
    }

    fn clamp(&mut self) {
        let max = self.lines.len().saturating_sub(self.viewport);
        if self.offset > max {
            self.offset = max;
        }
    }

    #[cfg(test)]
    pub fn offset(&self) -> usize { self.offset }
    #[cfg(test)]
    pub fn follow(&self) -> bool { self.follow }

    pub fn scroll_down(&mut self) {
        let max = self.lines.len().saturating_sub(self.viewport);
        if self.offset < max {
            self.offset += 1;
        }
        if self.offset >= max {
            self.follow = true;
        }
    }

    pub fn scroll_up(&mut self) {
        self.offset = self.offset.saturating_sub(1);
        self.follow = false;
    }

    pub fn search_to(&mut self, needle: &str) {
        if let Some(idx) = self.lines.iter().position(|l| l.contains(needle)) {
            self.offset = idx.min(self.lines.len().saturating_sub(1));
            self.follow = false;
        }
    }
}

impl View for LogView {
    fn render(&mut self, frame: &mut ratatui::Frame) {
        use ratatui::text::Text;
        use ratatui::widgets::{Block, Borders, Paragraph};
        let area = frame.area();
        self.viewport = (area.height.saturating_sub(2)).max(1) as usize;
        self.clamp();
        if self.follow {
            self.scroll_to_bottom();
        }
        let end = (self.offset + self.viewport).min(self.lines.len());
        let slice = self.lines.get(self.offset..end).unwrap_or(&[]);
        let body = slice.join("\n");
        let search_indicator = if self.searching { format!(" /{}", self.query) } else { String::new() };
        let title = format!(
            "Log ({} lines){}{}  [f follow, / search, Esc quit]",
            self.lines.len(),
            if self.follow { " FOLLOW" } else { "" },
            search_indicator,
        );
        let p = Paragraph::new(Text::raw(body))
            .block(Block::default().borders(Borders::ALL).title(title));
        frame.render_widget(p, area);
    }

    fn on_key(&mut self, key: KeyEvent) -> bool {
        if self.searching {
            match key.code {
                KeyCode::Enter => {
                    self.search_to(&self.query.clone());
                    self.searching = false;
                }
                KeyCode::Esc => {
                    self.searching = false;
                    self.query.clear();
                }
                KeyCode::Backspace => { self.query.pop(); }
                KeyCode::Char(c) => self.query.push(c),
                _ => {}
            }
            return false;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return true,
            KeyCode::Down => self.scroll_down(),
            KeyCode::Up => self.scroll_up(),
            KeyCode::Char('f') => self.follow = !self.follow,
            KeyCode::Char('/') => {
                self.searching = true;
                self.query.clear();
            }
            _ => {}
        }
        false
    }

    fn on_tick(&mut self) {
        let mut drained = Vec::new();
        if let Some(rx) = self.rx.as_mut() {
            while let Ok(line) = rx.try_recv() {
                drained.push(line);
            }
        }
        for line in drained {
            self.push_line(line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lv(n: usize) -> LogView {
        let mut v = LogView::new();
        v.set_viewport(5);
        for i in 0..n {
            v.push_line(format!("line {i}"));
        }
        v
    }

    #[test]
    fn follow_keeps_bottom() {
        let v = lv(20);
        assert_eq!(v.offset(), 15);
    }

    #[test]
    fn scroll_up_disables_follow() {
        let mut v = lv(20);
        v.scroll_up();
        assert!(!v.follow());
        assert_eq!(v.offset(), 14);
    }

    #[test]
    fn search_jumps_to_match() {
        let mut v = lv(20);
        v.search_to("line 7");
        assert_eq!(v.offset(), 7);
    }

    #[test]
    fn typing_search_then_enter_jumps() {
        use ratatui::crossterm::event::{KeyModifiers};
        let mut v = lv(20);
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        v.on_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        for c in "line 7".chars() { v.on_key(key(c)); }
        v.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(v.offset(), 7);
    }
}
