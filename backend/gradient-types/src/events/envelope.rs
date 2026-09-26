/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::Event;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq)]
pub struct Envelope {
    pub at: DateTime<Utc>,
    pub event: Event,
}

impl Envelope {
    pub fn now(event: Event) -> Self {
        Self {
            at: Utc::now(),
            event,
        }
    }

    fn at_rfc3339(&self) -> String {
        self.at.to_rfc3339_opts(SecondsFormat::AutoSi, true)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "event": self.event.name(),
            "at": self.at_rfc3339(),
            "content": self.event.content(),
        })
    }

    /// Key order is fixed (`event`, `at`, `content`) so a line reads the same on every consumer.
    pub fn to_line(&self) -> String {
        format!(
            "{{\"event\":{},\"at\":{},\"content\":{}}}",
            Value::from(self.event.name().as_ref()),
            Value::from(self.at_rfc3339()),
            self.event.content(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::cache;

    #[test]
    fn envelope_is_event_at_content() {
        let at = DateTime::parse_from_rfc3339("2026-09-26T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let env = Envelope {
            at,
            event: cache::Changed {}.into(),
        };
        assert_eq!(
            env.to_line(),
            r#"{"event":"cache.changed","at":"2026-09-26T12:00:00Z","content":{}}"#
        );
    }
}
