/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::run::{has_finished, job_exit_code};
use crate::commands::run_forward::{self, PortForward};
use crate::output::{ExitKind, Output};
use connector::api::ExecJobStatus;
use connector::exec_jobs::{GuestEvent, GuestSocket};
use crossterm::terminal;
use std::io::Write;
use std::process::exit;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::signal::unix::{Signal, SignalKind, signal};

const ABORTED_EXIT_CODE: i32 = 130;
const POLL_INTERVAL: Duration = Duration::from_secs(1);

enum TerminalEnd {
    Closed {
        exit_code: Option<i32>,
        message: Option<String>,
    },
    EscapeRequested,
    ConnectionLost,
}

pub(crate) async fn reattach(
    client: &connector::Client,
    job: &str,
    forwards: &[PortForward],
    out: Output,
) -> ! {
    let mut announced = false;
    loop {
        let current = client
            .exec_jobs()
            .get(job)
            .await
            .unwrap_or_else(|e| out.err(ExitKind::Api, format!("Failed to read jobs: {e}")));
        if has_finished(current.status) {
            out.progress(format!("The job exited with status {}.", current.status));
            exit(job_exit_code(current.status, current.program_exit_code));
        }
        if current.status == ExecJobStatus::Running {
            run_forward::start(client, job, forwards, out).await;
            attach(client, job, out).await;
        }
        if !announced {
            out.progress(format!("Waiting for job {job} to start..."));
            announced = true;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

pub(crate) async fn attach(client: &connector::Client, job: &str, out: Output) -> ! {
    let program = client
        .exec_jobs()
        .attach_program_terminal(job)
        .await
        .unwrap_or_else(|e| {
            out.err(
                ExitKind::Api,
                format!("Can't attach to program terminal: {e}"),
            )
        });
    let raw = RawTerminal::enable().unwrap_or_else(|e| {
        out.err(
            ExitKind::Api,
            format!("Can't switch terminal to raw mode: {e}"),
        )
    });
    let end = forward(program).await;
    drop(raw);
    finish(client, job, end, out).await
}

pub(crate) async fn attach_terminal_to_job_serial_console(
    client: &connector::Client,
    job: &str,
    node: Option<u32>,
    out: Output,
) -> ! {
    let console = client
        .exec_jobs()
        .open_job_serial_console_socket(job, node)
        .await
        .unwrap_or_else(|e| out.err(ExitKind::Api, format!("Can't open the console: {e}")));
    let raw = RawTerminal::enable().unwrap_or_else(|e| {
        out.err(
            ExitKind::Api,
            format!("Can't switch terminal to raw mode: {e}"),
        )
    });
    let end = forward(console).await;
    drop(raw);
    match end {
        TerminalEnd::Closed {
            exit_code: Some(code),
            ..
        } => exit(code),
        TerminalEnd::Closed {
            exit_code: None,
            message: Some(message),
        } => out.err(ExitKind::Api, message),
        TerminalEnd::Closed {
            exit_code: None,
            message: None,
        } => {
            out.progress("Console closed.");
            exit(0)
        }
        TerminalEnd::EscapeRequested => {
            out.progress("The console session ended.");
            exit(0)
        }
        TerminalEnd::ConnectionLost => out.err(
            ExitKind::Api,
            "Connection to the console ended unexpectedly.",
        ),
    }
}

async fn finish(client: &connector::Client, job: &str, end: TerminalEnd, out: Output) -> ! {
    match end {
        TerminalEnd::Closed {
            exit_code: Some(code),
            ..
        } => exit(code),
        TerminalEnd::Closed {
            exit_code: None,
            message,
        } => out.err(
            ExitKind::Api,
            message.unwrap_or_else(|| "Program exited without an exit code.".to_owned()),
        ),
        TerminalEnd::EscapeRequested => {
            match client.exec_jobs().abort(job).await {
                Ok(_) => out.progress("Job aborted."),
                Err(e) => out.progress(format!("Failed to abort job: {e}")),
            }
            exit(ABORTED_EXIT_CODE)
        }
        TerminalEnd::ConnectionLost => out.err(
            ExitKind::Api,
            format!("Connection closed unexpectedly. Use gradient run --attach {job}"),
        ),
    }
}

async fn forward(mut program: GuestSocket) -> TerminalEnd {
    let mut stdin = tokio::io::stdin();
    let mut stdout = std::io::stdout();
    let mut window_changes = signal(SignalKind::window_change()).ok();
    let mut escape = EscapeSequence::default();
    let mut typed = [0u8; 4096];
    let mut stdin_open = true;
    if send_size(&mut program).await.is_err() {
        return TerminalEnd::ConnectionLost;
    }

    loop {
        tokio::select! {
            event = program.next_event() => match event {
                Some(GuestEvent::Output(bytes)) => {
                    let _ = stdout.write_all(&bytes);
                    let _ = stdout.flush();
                }
                Some(GuestEvent::Closed { exit_code, message }) => {
                    return TerminalEnd::Closed { exit_code, message };
                }
                None => return TerminalEnd::ConnectionLost,
            },
            read = stdin.read(&mut typed), if stdin_open => match read {
                Ok(0) | Err(_) => stdin_open = false,
                Ok(n) => match escape.filter(&typed[..n]) {
                    Typed::Abort => return TerminalEnd::EscapeRequested,
                    Typed::Forward(bytes) if bytes.is_empty() => {}
                    Typed::Forward(bytes) => {
                        if program.send_bytes(bytes).await.is_err() {
                            return TerminalEnd::ConnectionLost;
                        }
                    }
                },
            },
            () = window_changed(&mut window_changes) => {
                if send_size(&mut program).await.is_err() {
                    return TerminalEnd::ConnectionLost;
                }
            }
        }
    }
}

async fn send_size(program: &mut GuestSocket) -> Result<(), connector::ConnectorError> {
    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    program.resize(cols, rows).await
}

async fn window_changed(changes: &mut Option<Signal>) {
    match changes {
        Some(changes) => {
            changes.recv().await;
        }
        None => std::future::pending().await,
    }
}

struct RawTerminal;

impl RawTerminal {
    fn enable() -> std::io::Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Typed {
    Forward(Vec<u8>),
    Abort,
}

#[derive(Default)]
struct EscapeSequence {
    mid_line: bool,
    after_tilde: bool,
}

impl EscapeSequence {
    fn filter(&mut self, typed: &[u8]) -> Typed {
        let mut forward = Vec::with_capacity(typed.len());
        for &byte in typed {
            if self.after_tilde {
                self.after_tilde = false;
                match byte {
                    b'.' => return Typed::Abort,
                    b'~' => {
                        forward.push(b'~');
                        self.mid_line = true;
                        continue;
                    }
                    _ => forward.push(b'~'),
                }
            } else if !self.mid_line && byte == b'~' {
                self.after_tilde = true;
                continue;
            }
            forward.push(byte);
            self.mid_line = !matches!(byte, b'\r' | b'\n');
        }
        Typed::Forward(forward)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(chunks: &[&[u8]]) -> Vec<Typed> {
        let mut escape = EscapeSequence::default();
        chunks.iter().map(|chunk| escape.filter(chunk)).collect()
    }

    #[test]
    fn tilde_dot_at_the_start_of_a_line_aborts() {
        assert_eq!(typed(&[b"~."]), [Typed::Abort]);
        assert_eq!(
            typed(&[b"ls\r", b"~", b"."]),
            [
                Typed::Forward(b"ls\r".to_vec()),
                Typed::Forward(Vec::new()),
                Typed::Abort
            ]
        );
    }

    #[test]
    fn a_tilde_inside_a_line_or_before_other_keys_reaches_the_program() {
        assert_eq!(typed(&[b"a~."]), [Typed::Forward(b"a~.".to_vec())]);
        assert_eq!(typed(&[b"~x"]), [Typed::Forward(b"~x".to_vec())]);
        assert_eq!(typed(&[b"~~."]), [Typed::Forward(b"~.".to_vec())]);
    }
}
