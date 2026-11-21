/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::CliError;
use crate::logfmt::{
    StreamingTerminalEscapeSequenceFilter, TerminalColorOrPlainText,
    filter_complete_text_for_terminal_output, stdout_filter_mode,
};
use connector::api::EvaluationStatus;
use serde::Serialize;
use std::io::{self, IsTerminal, Write};
use std::process::exit;

#[derive(Clone, Copy)]
pub enum ExitKind {
    Ok,
    Api,
    Usage,
    Unauthorized,
    EvaluationAborted,
    EvaluationTimeout,
}

impl ExitKind {
    fn code(self) -> i32 {
        match self {
            Self::Ok => 0,
            Self::Api => 1,
            Self::Usage => 2,
            Self::Unauthorized => 3,
            Self::EvaluationAborted => 4,
            Self::EvaluationTimeout => 5,
        }
    }

    pub fn exit_kind_from_evaluation_status(status: EvaluationStatus) -> Option<Self> {
        match status {
            EvaluationStatus::Completed => Some(Self::Ok),
            EvaluationStatus::Failed => Some(Self::Api),
            EvaluationStatus::Aborted => Some(Self::EvaluationAborted),
            _ => None,
        }
    }

    pub fn exit_with_code_without_printing(self) -> ! {
        exit(self.code())
    }
}

#[derive(Clone, Copy)]
pub enum Color {
    Green,
    Red,
    White,
    Yellow,
    Plain,
}

impl Color {
    fn escape(self) -> &'static str {
        match self {
            Self::Green => "\x1b[32m",
            Self::Red => "\x1b[31m",
            Self::White => "\x1b[37m",
            Self::Yellow => "\x1b[33m",
            Self::Plain => "",
        }
    }
}

fn filter_mode_for_terminal_output(terminal: bool) -> TerminalColorOrPlainText {
    if terminal {
        TerminalColorOrPlainText::KeepSgrColorSequences
    } else {
        TerminalColorOrPlainText::StripEscapeSequences
    }
}

#[derive(Clone, Copy)]
pub struct Output {
    json: bool,
    terminal: bool,
}

#[expect(
    clippy::disallowed_macros,
    clippy::disallowed_methods,
    reason = "The output sink is the only place for writes to stdout and stderr."
)]
impl Output {
    pub fn new(json: bool) -> Self {
        Self {
            json,
            terminal: io::stdout().is_terminal(),
        }
    }

    pub fn is_json(&self) -> bool {
        self.json
    }

    pub fn ok<T: Serialize>(&self, data: &T) {
        if self.json {
            let env = serde_json::json!({ "error": false, "message": data });
            let stdout = io::stdout();
            let mut lock = stdout.lock();
            serde_json::to_writer(&mut lock, &env).expect("serialize");
            let _ = writeln!(lock);
        }
    }

    pub fn paint(&self, text: &str, color: Color) -> String {
        if self.json || matches!(color, Color::Plain) || !self.terminal {
            return text.to_string();
        }

        format!("{}{}\x1b[0m", color.escape(), text)
    }

    pub fn human(&self, msg: impl std::fmt::Display) {
        if !self.json {
            println!(
                "{}",
                filter_complete_text_for_terminal_output(
                    &msg.to_string(),
                    filter_mode_for_terminal_output(self.terminal)
                )
            );
        }
    }

    pub fn progress(&self, msg: impl std::fmt::Display) {
        if !self.json {
            eprintln!(
                "{}",
                filter_complete_text_for_terminal_output(
                    &msg.to_string(),
                    filter_mode_for_terminal_output(io::stderr().is_terminal())
                )
            );
        }
    }

    pub fn write_filtered_log_chunk_to_stdout_without_newline_and_flush(
        &self,
        filter: &mut StreamingTerminalEscapeSequenceFilter,
        chunk: &str,
    ) {
        let mut stdout = io::stdout();
        let _ = stdout.write_all(
            filter
                .filter_text_chunk_holding_incomplete_escape(chunk)
                .as_bytes(),
        );
        let _ = stdout.flush();
    }

    pub fn write_unfinished_last_line_and_flush(
        &self,
        filter: &mut StreamingTerminalEscapeSequenceFilter,
    ) {
        let mut stdout = io::stdout();
        let _ = stdout.write_all(filter.take_unfinished_line().as_bytes());
        let _ = stdout.flush();
    }

    pub fn write_filtered_log_text_to_stdout_and_flush(&self, log: &str) {
        let mut filter = StreamingTerminalEscapeSequenceFilter::new(stdout_filter_mode());
        self.write_filtered_log_chunk_to_stdout_without_newline_and_flush(&mut filter, log);
        self.write_unfinished_last_line_and_flush(&mut filter);
    }

    pub fn print_serializable_value_as_json_line<T: Serialize>(&self, value: &T) {
        let stdout = io::stdout();
        let mut lock = stdout.lock();
        if serde_json::to_writer(&mut lock, value).is_ok() {
            let _ = writeln!(lock);
        }
    }

    pub fn step_start(&self, msg: impl std::fmt::Display) {
        if self.json {
            return;
        }
        let mut err = io::stderr();
        if err.is_terminal() {
            let msg = filter_complete_text_for_terminal_output(
                &msg.to_string(),
                TerminalColorOrPlainText::KeepSgrColorSequences,
            );
            let _ = write!(err, "\r\x1b[K{msg}");
            let _ = err.flush();
        }
    }

    pub fn step_done(&self, msg: impl std::fmt::Display) {
        if self.json {
            return;
        }
        let mut err = io::stderr();
        let msg = filter_complete_text_for_terminal_output(
            &msg.to_string(),
            filter_mode_for_terminal_output(err.is_terminal()),
        );
        if err.is_terminal() {
            let _ = write!(err, "\r\x1b[K{msg}\n");
        } else {
            let _ = writeln!(err, "{msg}");
        }
        let _ = err.flush();
    }

    pub fn err(&self, kind: ExitKind, msg: impl std::fmt::Display) -> ! {
        let text = msg.to_string();
        if self.json {
            let env = serde_json::json!({ "error": true, "message": text });
            let _ = serde_json::to_writer(io::stdout(), &env);
            println!();
        } else {
            eprintln!(
                "{}",
                filter_complete_text_for_terminal_output(
                    &text,
                    filter_mode_for_terminal_output(io::stderr().is_terminal())
                )
            );
        }
        exit(kind.code());
    }

    pub fn fail(&self, error: impl Into<CliError>) -> ! {
        let error = error.into();
        self.err(error.exit_kind(), error)
    }

    #[cfg(test)]
    pub fn render_ok_to_string<T: Serialize>(&self, data: &T) -> Result<String, serde_json::Error> {
        let env = serde_json::json!({ "error": false, "message": data });
        serde_json::to_string(&env)
    }

    #[cfg(test)]
    pub fn render_err_to_string(&self, msg: &str) -> Result<String, serde_json::Error> {
        let env = serde_json::json!({ "error": true, "message": msg });
        serde_json::to_string(&env)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_envelope_serializes() {
        let buf = Output::new(true)
            .render_ok_to_string(&"hello".to_string())
            .unwrap();
        assert_eq!(buf.trim(), r#"{"error":false,"message":"hello"}"#);
    }

    #[test]
    fn failure_envelope_serializes() {
        let buf = Output::new(true)
            .render_err_to_string("missing arg")
            .unwrap();
        assert_eq!(buf.trim(), r#"{"error":true,"message":"missing arg"}"#);
    }

    #[test]
    fn a_piped_run_stays_plain() {
        let out = Output {
            json: false,
            terminal: false,
        };
        assert_eq!(out.paint("build-a", Color::Green), "build-a");
    }

    #[test]
    fn json_output_is_never_painted() {
        let out = Output {
            json: true,
            terminal: true,
        };
        assert_eq!(out.paint("build-a", Color::Red), "build-a");
    }
}
