/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::io;
use std::os::fd::OwnedFd;
use std::process::Stdio;
use std::sync::Arc;

use rustix::termios::{Winsize, tcsetwinsize};
use tokio::fs::File;
use tokio::process::{Child, Command};

pub const TERMINAL_TYPE: &str = "xterm-256color";
pub const DEFAULT_COLS: u16 = 80;
pub const DEFAULT_ROWS: u16 = 24;

pub struct PtyMaster {
    fd: OwnedFd,
}

impl PtyMaster {
    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let size = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        tcsetwinsize(&self.fd, size)?;
        Ok(())
    }

    pub fn stream(&self) -> io::Result<File> {
        Ok(File::from_std(self.fd.try_clone()?.into()))
    }
}

pub fn spawn_on_pty(
    mut command: Command,
    cols: u16,
    rows: u16,
) -> io::Result<(Child, Arc<PtyMaster>)> {
    let pty = nix::pty::openpty(None, None)?;
    let master = Arc::new(PtyMaster { fd: pty.master });
    master.resize(cols, rows)?;
    command
        .stdin(Stdio::from(pty.slave.try_clone()?))
        .stdout(Stdio::from(pty.slave.try_clone()?))
        .stderr(Stdio::from(pty.slave));
    let child = command.spawn()?;
    Ok((child, master))
}
