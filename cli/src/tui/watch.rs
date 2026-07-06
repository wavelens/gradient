/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::tui::View;
use connector::evals::{BuildItem, EvalMessage, EvaluationResponse};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::{Color, Modifier, Style};
use std::collections::HashSet;
use std::time::Instant;
use tokio::sync::mpsc::UnboundedReceiver;

pub enum WatchEvent {
    Eval(EvaluationResponse),
    Builds(Vec<BuildItem>),
    Message(EvalMessage),
    Log(String),
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct BuildSummary {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub building: usize,
    pub queued: usize,
}

impl BuildSummary {
    pub fn of(builds: &[BuildItem]) -> Self {
        let mut s = BuildSummary {
            total: builds.len(),
            ..Default::default()
        };
        for b in builds {
            match build_class(&b.status) {
                BuildClass::Succeeded => s.succeeded += 1,
                BuildClass::Failed => s.failed += 1,
                BuildClass::Building => s.building += 1,
                BuildClass::Queued => s.queued += 1,
            }
        }
        s
    }

    pub fn done(&self) -> usize {
        self.succeeded + self.failed
    }
}

enum BuildClass {
    Succeeded,
    Failed,
    Building,
    Queued,
}

fn build_class(status: &str) -> BuildClass {
    match status {
        "Completed" | "Substituted" => BuildClass::Succeeded,
        "Building" => BuildClass::Building,
        "Queued" | "Created" => BuildClass::Queued,
        _ => BuildClass::Failed,
    }
}

pub fn build_icon(status: &str) -> &'static str {
    match build_class(status) {
        BuildClass::Succeeded => "✔",
        BuildClass::Failed => "✗",
        BuildClass::Building => "▶",
        BuildClass::Queued => "·",
    }
}

fn build_style(status: &str) -> Style {
    let color = match build_class(status) {
        BuildClass::Succeeded => Color::Green,
        BuildClass::Failed => Color::Red,
        BuildClass::Building => Color::Yellow,
        BuildClass::Queued => Color::DarkGray,
    };
    Style::default().fg(color)
}

/// Convert a log line carrying nix's ANSI SGR sequences into a styled ratatui
/// line - ratatui does not interpret escape codes itself. Unknown sequences are
/// dropped; the colour set mirrors the web log viewer.
fn ansi_to_line(s: &str) -> ratatui::text::Line<'static> {
    use ratatui::text::{Line, Span};
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut style = Style::default();
    let mut buf = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), style));
            }
            let mut code = String::new();
            let mut final_byte = None;
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() {
                    final_byte = Some(n);
                    break;
                }
                code.push(n);
            }
            if final_byte == Some('m') {
                style = apply_sgr(style, &code);
            }
        } else {
            buf.push(c);
        }
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, style));
    }
    Line::from(spans)
}

/// Fold one SGR parameter string (e.g. `1;31`) into `style`. Empty resets.
fn apply_sgr(mut style: Style, code: &str) -> Style {
    if code.is_empty() {
        return Style::default();
    }
    for part in code.split(';') {
        style = match part {
            "0" => Style::default(),
            "1" => style.add_modifier(Modifier::BOLD),
            "2" => style.add_modifier(Modifier::DIM),
            "3" => style.add_modifier(Modifier::ITALIC),
            "4" => style.add_modifier(Modifier::UNDERLINED),
            "30" => style.fg(Color::Black),
            "31" => style.fg(Color::Red),
            "32" => style.fg(Color::Green),
            "33" => style.fg(Color::Yellow),
            "34" => style.fg(Color::Blue),
            "35" => style.fg(Color::Magenta),
            "36" => style.fg(Color::Cyan),
            "37" => style.fg(Color::Gray),
            "39" => style.fg(Color::Reset),
            "90" => style.fg(Color::DarkGray),
            "91" => style.fg(Color::LightRed),
            "92" => style.fg(Color::LightGreen),
            "93" => style.fg(Color::LightYellow),
            "94" => style.fg(Color::LightBlue),
            "95" => style.fg(Color::LightMagenta),
            "96" => style.fg(Color::LightCyan),
            "97" => style.fg(Color::White),
            _ => style,
        };
    }
    style
}

pub fn eval_is_terminal(status: &str) -> bool {
    matches!(status, "Completed" | "Failed" | "Aborted")
}

fn eval_style(status: &str) -> Style {
    let color = match status {
        "Completed" => Color::Green,
        "Failed" | "Aborted" => Color::Red,
        _ => Color::Yellow,
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

pub fn format_duration(total_secs: u64) -> String {
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

pub fn format_build_time(ms: Option<i64>) -> String {
    match ms {
        Some(ms) if ms >= 1000 => format!("{:.1}s", ms as f64 / 1000.0),
        Some(ms) => format!("{ms}ms"),
        None => "-".to_string(),
    }
}

pub struct Dashboard {
    eval_id: String,
    eval: Option<EvaluationResponse>,
    builds: Vec<BuildItem>,
    seen_messages: HashSet<String>,
    log: Vec<String>,
    pending: String,
    offset: usize,
    viewport: usize,
    follow: bool,
    started: Instant,
    rx: Option<UnboundedReceiver<WatchEvent>>,
}

impl Dashboard {
    pub fn new(eval_id: String, rx: UnboundedReceiver<WatchEvent>) -> Self {
        Self {
            rx: Some(rx),
            ..Self::bare(eval_id)
        }
    }

    fn bare(eval_id: String) -> Self {
        Self {
            eval_id,
            eval: None,
            builds: Vec::new(),
            seen_messages: HashSet::new(),
            log: Vec::new(),
            pending: String::new(),
            offset: 0,
            viewport: 1,
            follow: true,
            started: Instant::now(),
            rx: None,
        }
    }

    pub fn apply(&mut self, ev: WatchEvent) {
        match ev {
            WatchEvent::Eval(e) => self.eval = Some(e),
            WatchEvent::Builds(b) => self.builds = b,
            WatchEvent::Message(m) => {
                if self.seen_messages.insert(m.id.clone()) {
                    self.push_line(format!("[eval/{}] {}", m.level, m.message));
                }
            }
            WatchEvent::Log(chunk) => self.push_chunk(chunk),
        }
    }

    fn push_chunk(&mut self, chunk: String) {
        self.pending.push_str(&crate::logfmt::decode_escapes(&chunk));
        while let Some(nl) = self.pending.find('\n') {
            let line: String = self.pending.drain(..=nl).collect();
            self.push_line(line.trim_end_matches('\n').to_string());
        }
    }

    fn push_line(&mut self, line: String) {
        self.log.push(line);
        if self.follow {
            self.scroll_to_bottom();
        }
    }

    fn scroll_to_bottom(&mut self) {
        self.offset = self.log.len().saturating_sub(self.viewport);
    }

    fn clamp(&mut self) {
        let max = self.log.len().saturating_sub(self.viewport);
        if self.offset > max {
            self.offset = max;
        }
    }

    fn scroll_down(&mut self) {
        let max = self.log.len().saturating_sub(self.viewport);
        if self.offset < max {
            self.offset += 1;
        }
        if self.offset >= max {
            self.follow = true;
        }
    }

    fn scroll_up(&mut self) {
        self.offset = self.offset.saturating_sub(1);
        self.follow = false;
    }

    fn status(&self) -> &str {
        self.eval.as_ref().map(|e| e.status.as_str()).unwrap_or("…")
    }

    #[cfg(test)]
    fn offset(&self) -> usize {
        self.offset
    }
    #[cfg(test)]
    fn log_len(&self) -> usize {
        self.log.len()
    }
    #[cfg(test)]
    fn set_viewport(&mut self, n: usize) {
        self.viewport = n.max(1);
        self.clamp();
    }
}

impl View for Dashboard {
    fn render(&mut self, frame: &mut ratatui::Frame) {
        use ratatui::layout::{Constraint, Direction, Layout};
        use ratatui::text::{Line, Span, Text};
        use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};

        let area = frame.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5),
                Constraint::Percentage(45),
                Constraint::Min(3),
            ])
            .split(area);

        let summary = BuildSummary::of(&self.builds);
        let status = self.status().to_string();
        let elapsed = format_duration(self.started.elapsed().as_secs());
        let mut header: Vec<Line> = vec![
            Line::from(vec![
                Span::raw("Status "),
                Span::styled(status.clone(), eval_style(&status)),
                Span::raw(format!(
                    "   Elapsed {elapsed}   Builds {}/{} ",
                    summary.done(),
                    summary.total
                )),
                Span::styled(format!("✔{}", summary.succeeded), build_style("Completed")),
                Span::raw(" "),
                Span::styled(format!("✗{}", summary.failed), build_style("Failed")),
                Span::raw(" "),
                Span::styled(format!("▶{}", summary.building), build_style("Building")),
                Span::raw(" "),
                Span::styled(format!("·{}", summary.queued), build_style("Queued")),
            ]),
        ];
        if let Some(e) = &self.eval {
            header.push(Line::raw(format!(
                "Project {}   Commit {}   Target {}",
                e.project.as_deref().unwrap_or("-"),
                short_commit(&e.commit),
                e.wildcard
            )));
            if let Some(err) = e.error.as_ref().filter(|s| !s.is_empty()) {
                header.push(Line::styled(
                    format!("Error: {err}"),
                    Style::default().fg(Color::Red),
                ));
            }
        }
        let header_block = Paragraph::new(Text::from(header)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Evaluation {}", self.eval_id)),
        );
        frame.render_widget(header_block, chunks[0]);

        let rows: Vec<ListItem> = self
            .builds
            .iter()
            .map(|b| {
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{} ", build_icon(&b.status)), build_style(&b.status)),
                    Span::raw(format!("{:<40} ", truncate(&b.name, 40))),
                    Span::styled(format!("{:<12}", b.status), build_style(&b.status)),
                    Span::raw(format_build_time(b.build_time_ms)),
                ]))
            })
            .collect();
        let builds_list = List::new(rows).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Builds ({})", self.builds.len())),
        );
        frame.render_widget(builds_list, chunks[1]);

        self.viewport = (chunks[2].height.saturating_sub(2)).max(1) as usize;
        self.clamp();
        if self.follow {
            self.scroll_to_bottom();
        }
        let end = (self.offset + self.viewport).min(self.log.len());
        let body: Vec<Line> = self
            .log
            .get(self.offset..end)
            .unwrap_or(&[])
            .iter()
            .map(|l| ansi_to_line(l))
            .collect();
        let log_title = format!(
            "Logs ({} lines){}  [↑/↓ scroll, f follow, q quit]",
            self.log.len(),
            if self.follow { " FOLLOW" } else { "" }
        );
        let log_widget = Paragraph::new(Text::from(body))
            .block(Block::default().borders(Borders::ALL).title(log_title));
        frame.render_widget(log_widget, chunks[2]);
    }

    fn on_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return true,
            KeyCode::Down => self.scroll_down(),
            KeyCode::Up => self.scroll_up(),
            KeyCode::Char('f') => self.follow = !self.follow,
            _ => {}
        }
        false
    }

    fn on_tick(&mut self) {
        let mut drained = Vec::new();
        if let Some(rx) = self.rx.as_mut() {
            while let Ok(ev) = rx.try_recv() {
                drained.push(ev);
            }
        }
        for ev in drained {
            self.apply(ev);
        }
    }
}

fn short_commit(commit: &str) -> &str {
    commit.get(..8).unwrap_or(commit)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let kept: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{kept}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(name: &str, status: &str, ms: Option<i64>) -> BuildItem {
        BuildItem {
            id: name.to_string(),
            name: name.to_string(),
            status: status.to_string(),
            updated_at: String::new(),
            build_time_ms: ms,
        }
    }

    fn msg(id: &str, level: &str, text: &str) -> EvalMessage {
        EvalMessage {
            id: id.to_string(),
            level: level.to_string(),
            message: text.to_string(),
            source: None,
            created_at: String::new(),
            entry_points: Vec::new(),
        }
    }

    #[test]
    fn summary_classifies_statuses() {
        let builds = vec![
            build("a", "Completed", Some(10)),
            build("b", "Substituted", None),
            build("c", "Building", None),
            build("d", "Queued", None),
            build("e", "FailedPermanent", Some(5)),
            build("f", "DependencyFailed", None),
        ];
        let s = BuildSummary::of(&builds);
        assert_eq!(
            s,
            BuildSummary {
                total: 6,
                succeeded: 2,
                failed: 2,
                building: 1,
                queued: 1,
            }
        );
        assert_eq!(s.done(), 4);
    }

    #[test]
    fn eval_terminal_detection() {
        assert!(eval_is_terminal("Completed"));
        assert!(eval_is_terminal("Failed"));
        assert!(eval_is_terminal("Aborted"));
        assert!(!eval_is_terminal("Building"));
        assert!(!eval_is_terminal("EvaluatingFlake"));
    }

    #[test]
    fn ansi_line_splits_styled_and_plain_spans() {
        let line = ansi_to_line("plain \u{1b}[31mred\u{1b}[0m tail");
        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[1].content, "red");
        assert_eq!(line.spans[1].style.fg, Some(Color::Red));
        assert_eq!(line.spans[2].style.fg, None);
    }

    #[test]
    fn duration_formats_minutes_and_hours() {
        assert_eq!(format_duration(83), "01:23");
        assert_eq!(format_duration(3723), "1:02:03");
    }

    #[test]
    fn build_time_formats_units() {
        assert_eq!(format_build_time(Some(1500)), "1.5s");
        assert_eq!(format_build_time(Some(250)), "250ms");
        assert_eq!(format_build_time(None), "-");
    }

    #[test]
    fn chunk_splits_into_lines_and_buffers_partial() {
        let mut d = Dashboard::bare("e1".into());
        d.set_viewport(5);
        d.apply(WatchEvent::Log("a> hello\nb> wor".into()));
        assert_eq!(d.log_len(), 1);
        d.apply(WatchEvent::Log("ld\n".into()));
        assert_eq!(d.log_len(), 2);
    }

    #[test]
    fn messages_are_deduplicated() {
        let mut d = Dashboard::bare("e1".into());
        d.set_viewport(5);
        d.apply(WatchEvent::Message(msg("m1", "error", "boom")));
        d.apply(WatchEvent::Message(msg("m1", "error", "boom")));
        d.apply(WatchEvent::Message(msg("m2", "warn", "careful")));
        assert_eq!(d.log_len(), 2);
    }

    #[test]
    fn follow_keeps_bottom_then_scroll_up_detaches() {
        let mut d = Dashboard::bare("e1".into());
        d.set_viewport(5);
        for i in 0..20 {
            d.push_line(format!("line {i}"));
        }
        assert_eq!(d.offset(), 15);
        d.scroll_up();
        assert_eq!(d.offset(), 14);
        assert!(!d.follow);
    }

    #[test]
    fn icons_map_to_class() {
        assert_eq!(build_icon("Completed"), "✔");
        assert_eq!(build_icon("FailedTimeout"), "✗");
        assert_eq!(build_icon("Building"), "▶");
        assert_eq!(build_icon("Queued"), "·");
    }
}
