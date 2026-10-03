/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::session::Session;
use gradient_core::ServerState;
use gradient_types::*;
use russh::keys::{HashAlg, PublicKey};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set};
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
#[error("unknown key or project")]
pub struct Rejected;

pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

pub async fn authorize(
    state: &Arc<ServerState>,
    project_name: &str,
    fingerprint: &str,
) -> Result<Arc<Session>, Rejected> {
    match load_session(state, project_name, fingerprint).await {
        Ok(Some(session)) => Ok(Arc::new(session)),
        Ok(None) => Err(Rejected),
        Err(error) => {
            tracing::warn!(%error, "ssh login lookup failed");
            Err(Rejected)
        }
    }
}

async fn load_session(
    state: &Arc<ServerState>,
    project_name: &str,
    fingerprint: &str,
) -> anyhow::Result<Option<Session>> {
    let Some(key) = EUserSshKey::find()
        .filter(CUserSshKey::Fingerprint.eq(fingerprint))
        .one(&state.web_db)
        .await?
    else {
        return Ok(None);
    };

    let Some(user) = EUser::find_by_id(key.user)
        .one(&state.web_db)
        .await?
        .filter(|u| u.active)
    else {
        return Ok(None);
    };

    let Some(project) =
        gradient_db::lookup::get_project_by_name(&state.db(), user.id, project_name.to_string())
            .await?
    else {
        return Ok(None);
    };

    let Some((_, permissions)) =
        gradient_db::access::project_permission_mask(&state.web_db, project.id, user.id).await?
    else {
        return Ok(None);
    };

    let caches = gradient_db::cache_paths::project_read_caches(&state.web_db, project.id).await?;
    let mut used = key.into_active_model();
    used.last_used_at = Set(Some(now()));
    used.update(&state.web_db).await?;

    Ok(Some(Session {
        state: state.clone(),
        user,
        project,
        permissions,
        caches,
        evaluation: Default::default(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    const KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHfwJ61+Nu5yJhfB3PfAyywWMtpJcwybHAVPzGWnPQ6v test";

    #[test]
    fn fingerprints_match_the_web_format() {
        let key = PublicKey::from_openssh(KEY).expect("key");
        assert_eq!(
            fingerprint(&key),
            "SHA256:oriknbp5Jg0jqvSVYJ2XG6wWsyBelapZTV+AMQTOowo"
        );
    }

    #[tokio::test]
    async fn an_unknown_key_is_rejected() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MUserSshKey>::new()])
            .into_connection();
        let state = gradient_test_support::state::test_state_web(db);

        let rejected = authorize(&state, "project", "SHA256:unknown").await;
        assert!(rejected.is_err());
    }

    #[tokio::test]
    async fn a_project_the_user_is_not_in_is_rejected_like_an_unknown_key() {
        let user = gradient_test_support::fixtures::user();
        let key = MUserSshKey {
            user: user.id,
            ..Default::default()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![key]])
            .append_query_results([vec![user]])
            .append_query_results([Vec::<MProject>::new()])
            .into_connection();
        let state = gradient_test_support::state::test_state_web(db);

        let rejected = authorize(&state, "someone-elses", "SHA256:known").await;
        assert_eq!(
            rejected.err().map(|r| r.to_string()),
            Some(Rejected.to_string())
        );
    }

    #[tokio::test]
    async fn a_deactivated_user_is_rejected() {
        let user = MUser {
            active: false,
            ..gradient_test_support::fixtures::user()
        };
        let key = MUserSshKey {
            user: user.id,
            ..Default::default()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![key]])
            .append_query_results([vec![user]])
            .into_connection();
        let state = gradient_test_support::state::test_state_web(db.clone());

        assert!(authorize(&state, "project", "SHA256:known").await.is_err());
        let log = format!("{:?}", db.into_transaction_log());
        assert!(!log.contains("project_user"), "{log}");
    }
}
