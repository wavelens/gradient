/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod commands;
mod config;
mod error;
mod input;
mod logfmt;
mod narinfo;
mod netrc;
pub mod output;
mod tui;

use commands::base;

#[cfg(unix)]
fn reset_sigpipe_to_default() {
    unsafe extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }
    const SIGPIPE: i32 = 13;
    const SIG_DFL: usize = 0;
    // Without this, writing to a closed pipe such as `gradient account list | head -1` panics instead of exiting quietly.
    unsafe {
        signal(SIGPIPE, SIG_DFL);
    }
}

fn main() -> std::io::Result<()> {
    #[cfg(unix)]
    reset_sigpipe_to_default();
    base::run()
}
