/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Duration;

use gradient_core::ServerState;
use gradient_storage::admission::{AdmissionSession, Admitted, ObjectKey, UploadPermit};

use crate::error::{WebError, WebResult};

/// The session is living as long as the permit.
pub(super) struct RestPermit {
    permit: UploadPermit,
    _session: AdmissionSession,
}

impl RestPermit {
    pub(super) fn committed(self) {
        self.permit.committed();
    }
}

pub(super) async fn admit(state: &ServerState, size: u64) -> WebResult<RestPermit> {
    admit_within(
        state,
        size,
        Duration::from_secs(state.config.upload.rest_wait_secs),
    )
    .await
}

async fn admit_within(state: &ServerState, size: u64, wait: Duration) -> WebResult<RestPermit> {
    let (session, mut admitted) = state.upload_admission.open_session("rest");
    session.request(
        0,
        ObjectKey::Rest(uuid::Uuid::now_v7().to_string()),
        size,
        false,
    );
    match tokio::time::timeout(wait, admitted.recv()).await {
        Ok(Some(Admitted::Granted { permit, .. })) => Ok(RestPermit {
            permit,
            _session: session,
        }),
        _ => Err(WebError::UploadBusy { retry_after: wait }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_test_support::state::test_state;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::time::Duration;

    #[tokio::test]
    async fn a_rest_upload_waits_for_a_permit_and_gives_up_busy() {
        let state = test_state(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let mut held = Vec::new();
        while let Ok(permit) = admit_within(&state, 1, Duration::from_millis(50)).await {
            held.push(permit);
        }
        assert_eq!(held.len(), 16, "the test state admits 16 uploads");
        held.pop();
        assert!(
            admit_within(&state, 1, Duration::from_secs(1))
                .await
                .is_ok()
        );
    }
}
