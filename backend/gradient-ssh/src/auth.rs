/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::gateway_user::parse_login;
use crate::session::{Session, SshHandles};
use gradient_db::lookup::by_id;
use gradient_types::permissions::PermissionMask;
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
    handles: &SshHandles,
    login: &str,
    fingerprint: &str,
) -> Result<Arc<Session>, Rejected> {
    let login = parse_login(login).map_err(|_| Rejected)?;
    match load_session(handles, &login.project, fingerprint).await {
        Ok(Some(session)) => Ok(Arc::new(Session {
            target: login.target,
            ..session
        })),
        Ok(None) => Err(Rejected),
        Err(error) => {
            tracing::warn!(%error, "ssh login lookup failed");
            Err(Rejected)
        }
    }
}

struct SshKeyProjectLoginRecord {
    key: MUserSshKey,
    user: MUser,
    project: MProject,
    permissions: PermissionMask,
    caches: Vec<CacheId>,
}

async fn load_session(
    handles: &SshHandles,
    project_name: &str,
    fingerprint: &str,
) -> anyhow::Result<Option<Session>> {
    let Some(login) =
        find_user_project_permission_mask_and_caches_by_ssh_key_fingerprint_and_project(
            handles,
            project_name,
            fingerprint,
        )
        .await?
    else {
        return Ok(None);
    };
    let mut used = login.key.into_active_model();
    used.last_used_at = Set(Some(now()));
    used.update(&handles.db.web_db).await?;

    Ok(Some(
        Session::open(
            handles.clone(),
            login.user,
            login.project,
            login.permissions,
            login.caches,
        )
        .await?,
    ))
}

pub async fn check_session_access_still_matches_login(
    session: &Session,
    fingerprint: &str,
) -> anyhow::Result<bool> {
    let login = find_user_project_permission_mask_and_caches_by_ssh_key_fingerprint_and_project(
        &session.handles,
        &session.project.name,
        fingerprint,
    )
    .await?;
    Ok(login.is_some_and(|login| {
        login.user.id == session.user.id
            && login.project.id == session.project.id
            && login.permissions == session.permissions
            && login.caches == session.caches
    }))
}

async fn find_user_project_permission_mask_and_caches_by_ssh_key_fingerprint_and_project(
    handles: &SshHandles,
    project_name: &str,
    fingerprint: &str,
) -> anyhow::Result<Option<SshKeyProjectLoginRecord>> {
    let Some(key) = EUserSshKey::find()
        .filter(CUserSshKey::Fingerprint.eq(fingerprint))
        .one(&handles.db.web_db)
        .await?
    else {
        return Ok(None);
    };

    let Some(user) = by_id::<EUser, _>(&handles.db.web_db, key.user)
        .await?
        .filter(|u| u.active)
    else {
        return Ok(None);
    };

    let Some(project) =
        gradient_db::lookup::get_project_by_name(&handles.db, user.id, project_name.to_string())
            .await?
    else {
        return Ok(None);
    };

    let Some(permissions) =
        gradient_db::access::project_permission_mask(&handles.db.web_db, project.id, user.id)
            .await?
    else {
        return Ok(None);
    };

    let caches =
        gradient_db::cache_paths::project_read_caches(&handles.db.web_db, project.id).await?;
    Ok(Some(SshKeyProjectLoginRecord {
        key,
        user,
        project,
        permissions,
        caches,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::mock_db;
    use gradient_test_support::state::TestState;

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
        let db = mock_db()
            .append_query_results([Vec::<MUserSshKey>::new()])
            .into_connection();
        let state = TestState::new().web_db(db).build();

        let rejected = authorize(&SshHandles::for_tests(&state), "project", "SHA256:unknown").await;
        assert!(rejected.is_err());
    }

    #[tokio::test]
    async fn a_project_the_user_is_not_in_is_rejected_like_an_unknown_key() {
        let user = gradient_fixtures::rows::user();
        let key = MUserSshKey {
            user: user.id,
            ..Default::default()
        };
        let db = mock_db()
            .append_query_results([vec![key]])
            .append_query_results([vec![user]])
            .append_query_results([Vec::<MProject>::new()])
            .into_connection();
        let state = TestState::new().web_db(db).build();

        let rejected = authorize(
            &SshHandles::for_tests(&state),
            "someone-elses",
            "SHA256:known",
        )
        .await;
        assert_eq!(
            rejected.err().map(|r| r.to_string()),
            Some(Rejected.to_string())
        );
    }

    #[tokio::test]
    async fn open_session_loses_access_when_user_is_deactivated() {
        let user = gradient_fixtures::rows::user();
        let key = MUserSshKey {
            user: user.id,
            ..Default::default()
        };
        let deactivated = MUser {
            active: false,
            ..user.clone()
        };
        let db = mock_db()
            .append_query_results([vec![key]])
            .append_query_results([vec![deactivated]])
            .into_connection();
        let state = TestState::new().web_db(db).build();
        let session = Session::open(
            SshHandles::for_tests(&state),
            user,
            gradient_fixtures::rows::project(),
            0,
            Vec::new(),
        )
        .await
        .unwrap();

        let still = check_session_access_still_matches_login(&session, "SHA256:known")
            .await
            .unwrap();

        assert!(!still);
    }

    #[tokio::test]
    async fn a_deactivated_user_is_rejected() {
        let user = MUser {
            active: false,
            ..gradient_fixtures::rows::user()
        };
        let key = MUserSshKey {
            user: user.id,
            ..Default::default()
        };
        let db = mock_db()
            .append_query_results([vec![key]])
            .append_query_results([vec![user]])
            .into_connection();
        let state = TestState::new().web_db(db.clone()).build();

        assert!(
            authorize(&SshHandles::for_tests(&state), "project", "SHA256:known")
                .await
                .is_err()
        );
        let log = format!("{:?}", db.into_transaction_log());
        assert!(!log.contains("project_user"), "{log}");
    }
}
