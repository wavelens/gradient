/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;

use chrono::NaiveDateTime;
use gradient_util::sync::Mutex;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionDirection {
    Outbound,
    Inbound,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ConnectionFailure {
    pub reason: String,
    pub at: NaiveDateTime,
    pub direction: ConnectionDirection,
    pub before_auth: bool,
}

#[derive(Default)]
pub struct ConnectionFailures(Mutex<HashMap<String, ConnectionFailure>>);

impl ConnectionFailures {
    pub fn record(
        &self,
        worker_id: &str,
        direction: ConnectionDirection,
        before_auth: bool,
        reason: impl Into<String>,
    ) {
        let failure = ConnectionFailure {
            reason: reason.into(),
            at: gradient_types::now(),
            direction,
            before_auth,
        };
        self.0.lock().insert(worker_id.to_owned(), failure);
    }

    pub fn clear(&self, worker_id: &str) {
        self.0.lock().remove(worker_id);
    }

    pub fn last(&self, worker_id: &str) -> Option<ConnectionFailure> {
        self.0.lock().get(worker_id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_later_failure_replaces_the_earlier_one_and_a_session_clears_it() {
        let failures = ConnectionFailures::default();
        failures.record(
            "w1",
            ConnectionDirection::Outbound,
            false,
            "dial timed out after 10 s",
        );
        failures.record(
            "w1",
            ConnectionDirection::Inbound,
            true,
            "401 no valid peer tokens provided",
        );

        let last = failures.last("w1").expect("recorded");
        assert_eq!(last.reason, "401 no valid peer tokens provided");
        assert!(last.before_auth);

        failures.clear("w1");
        assert_eq!(failures.last("w1"), None);
    }
}
