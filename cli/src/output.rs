/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use serde::Serialize;
use std::io::{self, Write};
use std::process::exit;

#[derive(Clone, Copy)]
pub enum ExitKind {
    Ok,
    Api,
    Usage,
    Unauthorized,
}

impl ExitKind {
    fn code(self) -> i32 {
        match self {
            Self::Ok => 0,
            Self::Api => 1,
            Self::Usage => 2,
            Self::Unauthorized => 3,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Output {
    json: bool,
}

impl Output {
    pub fn new(json: bool) -> Self {
        Self { json }
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

    pub fn human(&self, msg: impl std::fmt::Display) {
        if !self.json {
            println!("{}", msg);
        }
    }

    pub fn progress(&self, msg: impl std::fmt::Display) {
        if !self.json {
            eprintln!("{}", msg);
        }
    }

    pub fn err(&self, kind: ExitKind, msg: impl std::fmt::Display) -> ! {
        let text = msg.to_string();
        if self.json {
            let env = serde_json::json!({ "error": true, "message": text });
            let _ = serde_json::to_writer(io::stdout(), &env);
            println!();
        } else {
            eprintln!("{}", text);
        }
        exit(kind.code());
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
        let buf = Output::new(true).render_ok_to_string(&"hello".to_string()).unwrap();
        assert_eq!(buf.trim(), r#"{"error":false,"message":"hello"}"#);
    }

    #[test]
    fn failure_envelope_serializes() {
        let buf = Output::new(true).render_err_to_string("missing arg").unwrap();
        assert_eq!(buf.trim(), r#"{"error":true,"message":"missing arg"}"#);
    }
}
