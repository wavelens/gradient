/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::EventKind;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Client,
    Server,
}

/// The header is never carrying the payload. A NAR chunk is costing only a few bytes here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub direction: Direction,
    pub worker_id: String,
    pub message: String,
    pub job_id: Option<String>,
    pub len: Option<usize>,
}

impl EventKind for Message {
    const NAME: &'static str = "proto.*";
    const DURABLE: bool = false;
    const NAMES: &'static [&'static str] = &["proto.client.*", "proto.server.*"];

    fn name(&self) -> Cow<'static, str> {
        let side = match self.direction {
            Direction::Client => "client",
            Direction::Server => "server",
        };
        Cow::Owned(format!("proto.{side}.{}", snake(&self.message)))
    }
}

fn snake(camel: &str) -> String {
    let mut out = String::with_capacity(camel.len() + 4);
    for (i, c) in camel.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proto_names_are_snake_case() {
        let m = Message {
            direction: Direction::Client,
            worker_id: "w".into(),
            message: "NarPush".into(),
            job_id: None,
            len: None,
        };
        assert_eq!(m.name(), "proto.client.nar_push");
    }
}
