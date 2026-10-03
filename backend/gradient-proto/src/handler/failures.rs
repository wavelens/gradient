/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_core::ServerState;
use gradient_scheduler::connection_failures::{ConnectionDirection, ConnectionFailures};
use gradient_wire::session::handshake::Rejected;

use super::auth::has_any_registrations;

fn rejection_reason(code: u16, reason: &str) -> String {
    format!("{code} {reason}")
}

pub(super) async fn record_inbound(
    state: &ServerState,
    failures: &ConnectionFailures,
    rejected: &Rejected,
    before_auth: bool,
) {
    let Some(claimed) = rejected.claimed.as_deref() else {
        return;
    };
    if !is_registered(state, claimed).await {
        return;
    }

    failures.record(
        claimed,
        ConnectionDirection::Inbound,
        before_auth,
        rejection_reason(rejected.code, &rejected.reason),
    );
}

pub(super) fn record_dialed(failures: &ConnectionFailures, worker_id: &str, error: &anyhow::Error) {
    let reason = match error.downcast_ref::<Rejected>() {
        Some(rejected) => rejection_reason(rejected.code, &rejected.reason),
        None => format!("handshake failed: {error:#}"),
    };
    failures.record(worker_id, ConnectionDirection::Outbound, false, reason);
}

async fn is_registered(state: &ServerState, worker_id: &str) -> bool {
    has_any_registrations(state, worker_id).await
        || gradient_db::teams::workers::is_team_worker(&state.worker_db, worker_id)
            .await
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::{team_worker, worker_registration};
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn rejected(claimed: &str) -> Rejected {
        Rejected {
            code: 401,
            reason: "no valid peer tokens provided".into(),
            claimed: Some(claimed.into()),
        }
    }

    fn registered_worker() -> MockDatabase {
        MockDatabase::new(DatabaseBackend::Postgres).append_query_results([vec![
            worker_registration::Model {
                worker_id: "w1".into(),
                ..Default::default()
            },
        ]])
    }

    #[tokio::test]
    async fn an_unregistered_claim_leaves_no_trace() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<worker_registration::Model>::new()])
            .append_query_results([Vec::<team_worker::Model>::new()])
            .into_connection();
        let state = gradient_test_support::prelude::test_state(db);
        let failures = ConnectionFailures::default();

        record_inbound(&state, &failures, &rejected("intruder"), true).await;

        assert_eq!(failures.last("intruder"), None);
    }

    #[tokio::test]
    async fn a_registered_claim_records_a_failure_before_auth() {
        let state =
            gradient_test_support::prelude::test_state(registered_worker().into_connection());
        let failures = ConnectionFailures::default();

        record_inbound(&state, &failures, &rejected("w1"), true).await;

        let failure = failures.last("w1").expect("recorded");
        assert_eq!(failure.reason, "401 no valid peer tokens provided");
        assert_eq!(failure.direction, ConnectionDirection::Inbound);
        assert!(failure.before_auth);
    }
}
