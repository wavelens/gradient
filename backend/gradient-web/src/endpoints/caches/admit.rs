/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Duration;

use gradient_core::ServerState;
use gradient_storage::admission::{Admission, HeldPermit, ObjectKey};

use crate::error::{WebError, WebResult};

pub(super) async fn admit(state: &ServerState, size: u64) -> WebResult<HeldPermit> {
    admit_within(state, size, rest_wait(state)).await
}

pub(super) async fn admit_nar(
    state: &ServerState,
    hash: &str,
    size: u64,
) -> WebResult<Option<HeldPermit>> {
    admit_nar_within(state, hash, size, rest_wait(state)).await
}

fn rest_wait(state: &ServerState) -> Duration {
    Duration::from_secs(state.config.upload.rest_wait_secs)
}

async fn admit_within(state: &ServerState, size: u64, wait: Duration) -> WebResult<HeldPermit> {
    let object = ObjectKey::Rest(uuid::Uuid::now_v7().to_string());
    match admit_object(state, object, size, wait).await? {
        Admission::Granted(permit) => Ok(permit),
        Admission::AlreadyCommitted => Err(WebError::UploadBusy { retry_after: wait }),
    }
}

async fn admit_nar_within(
    state: &ServerState,
    hash: &str,
    size: u64,
    wait: Duration,
) -> WebResult<Option<HeldPermit>> {
    match admit_object(state, ObjectKey::Nar(hash.to_owned()), size, wait).await? {
        Admission::Granted(permit) => Ok(Some(permit)),
        Admission::AlreadyCommitted => Ok(None),
    }
}

async fn admit_object(
    state: &ServerState,
    object: ObjectKey,
    size: u64,
    wait: Duration,
) -> WebResult<Admission> {
    state
        .upload_admission
        .admit("rest", object, size, wait)
        .await
        .ok_or(WebError::UploadBusy { retry_after: wait })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_storage::admission::Admitted;
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

    #[tokio::test]
    async fn a_nar_upload_waits_for_the_upload_holding_its_path() {
        let state = test_state(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let hash = "a".repeat(32);
        let (holder, mut admitted) = state.upload_admission.open_session("holder");
        holder.request(1, ObjectKey::Nar(hash.clone()), 1, false);
        let Some(Admitted::Granted { permit, .. }) = admitted.recv().await else {
            panic!("the holder leads the path");
        };

        let waiting = admit_nar_within(&state, &hash, 1, Duration::from_secs(5));
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(200), &mut waiting)
                .await
                .is_err(),
            "the upload waits while another upload holds the path"
        );
        permit.committed();
        assert!(
            matches!(waiting.await, Ok(None)),
            "the other upload stored the path"
        );
    }
}
