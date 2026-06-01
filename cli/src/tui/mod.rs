/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod nar_browser;

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyEvent};
use ratatui::crossterm::{execute, terminal};
use std::io::{self, Stdout};
use std::time::Duration;

/// A full-screen view: owns its state, renders a frame, reacts to keys.
pub trait View {
    fn render(&mut self, frame: &mut ratatui::Frame);
    /// Return `true` to request exit.
    fn on_key(&mut self, key: KeyEvent) -> bool;
    fn on_tick(&mut self) {}
}

pub fn run<V: View>(mut view: V) -> io::Result<()> {
    let mut terminal = setup()?;
    let result = run_loop(&mut terminal, &mut view);
    restore(&mut terminal)?;
    result
}

fn run_loop<V: View>(terminal: &mut Terminal<CrosstermBackend<Stdout>>, view: &mut V) -> io::Result<()> {
    loop {
        terminal.draw(|f| view.render(f))?;
        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()?
                && view.on_key(key)
            {
                return Ok(());
            }
        } else {
            view.on_tick();
        }
    }
}

fn setup() -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, terminal::EnterAlternateScreen)?;
    install_panic_hook();
    Terminal::new(CrosstermBackend::new(stdout))
}

fn restore(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    terminal::disable_raw_mode()?;
    execute!(terminal.backend_mut(), terminal::LeaveAlternateScreen)?;
    terminal.show_cursor()
}

fn install_panic_hook() {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), terminal::LeaveAlternateScreen);
        hook(info);
    }));
}
