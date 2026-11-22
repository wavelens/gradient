/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::gateway_user::parse_login;
use crate::session::{Session, SshHandles};
use crate::store::SshBackend;
use gradient_db::lookup::by_id;
use gradient_types::permissions::{
    PermissionMask, api_key_without_cache_pin_can_read_project_caches,
};
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
        Ok(Some(session)) => Ok(Arc::new(session.with_gateway_target(login.target))),
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
            true,
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
        session.matches_user_project_permissions_and_readable_cache_ids_if_not_closed(
            login.user.id,
            login.project.id,
            login.permissions,
            &login.caches,
        )
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

/// Reject job tokens for evaluations without project credentials first. Every session opened here can use those credentials.
pub async fn open_project_store_backend_for_api_key(
    handles: SshHandles,
    user: UserId,
    project: ProjectId,
    key_mask: PermissionMask,
    cache_pin: Option<CacheId>,
) -> anyhow::Result<Option<Arc<SshBackend>>> {
    let Some(access) =
        load_project_store_backend_record_for_api_key(&handles, user, project, key_mask, cache_pin)
            .await?
    else {
        return Ok(None);
    };
    let session = Session::open(
        handles,
        access.user,
        access.project,
        access.permissions,
        access.caches,
        true,
    )
    .await?;
    Ok(Some(Arc::new(SshBackend::new(Arc::new(session)))))
}

pub async fn check_session_access_still_matches_api_key(
    backend: &SshBackend,
    key_mask: PermissionMask,
    cache_pin: Option<CacheId>,
) -> anyhow::Result<bool> {
    let session = backend.session();
    let access = load_project_store_backend_record_for_api_key(
        &session.handles,
        session.user.id,
        session.project.id,
        key_mask,
        cache_pin,
    )
    .await?;
    Ok(access.is_some_and(|access| {
        session.matches_user_project_permissions_and_readable_cache_ids_if_not_closed(
            access.user.id,
            access.project.id,
            access.permissions,
            &access.caches,
        )
    }))
}

struct ApiKeyUserProjectAccessRecord {
    user: MUser,
    project: MProject,
    permissions: PermissionMask,
    caches: Vec<CacheId>,
}

async fn load_project_store_backend_record_for_api_key(
    handles: &SshHandles,
    user: UserId,
    project: ProjectId,
    key_mask: PermissionMask,
    cache_pin: Option<CacheId>,
) -> anyhow::Result<Option<ApiKeyUserProjectAccessRecord>> {
    let Some(user) = by_id::<EUser, _>(&handles.db.web_db, user)
        .await?
        .filter(|u| u.active)
    else {
        return Ok(None);
    };

    let Some(project) = by_id::<EProject, _>(&handles.db.web_db, project).await? else {
        return Ok(None);
    };

    let Some(permissions) =
        gradient_db::access::project_permission_mask(&handles.db.web_db, project.id, user.id)
            .await?
    else {
        return Ok(None);
    };
    let permissions = permissions & key_mask;
    if !api_key_without_cache_pin_can_read_project_caches(permissions, cache_pin) {
        return Ok(None);
    }

    let caches =
        gradient_db::cache_paths::project_read_caches(&handles.db.web_db, project.id).await?;
    Ok(Some(ApiKeyUserProjectAccessRecord {
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
    use sea_orm::Value;
    use std::collections::BTreeMap;

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
            true,
        )
        .await
        .unwrap();

        let still = check_session_access_still_matches_login(&session, "SHA256:known")
            .await
            .unwrap();

        assert!(!still);
    }

    #[tokio::test]
    async fn api_key_store_access_check_returns_false_after_cache_removed_from_readable_caches() {
        let user = gradient_fixtures::rows::user();
        let project = gradient_fixtures::rows::project();
        let role = MRole {
            id: RoleId::now_v7(),
            permission: i64::MAX,
            ..Default::default()
        };
        let access = MProjectAccess {
            project: project.id,
            user: user.id,
            role: role.id,
            ..Default::default()
        };
        let cache = CacheId::now_v7();
        let readable_caches = |caches: &[CacheId]| -> Vec<BTreeMap<String, Value>> {
            caches
                .iter()
                .map(|cache| {
                    BTreeMap::from([("cache".to_owned(), Value::from(cache.into_inner()))])
                })
                .collect()
        };
        let mut db = mock_db();
        for caches in [&[cache][..], &[cache], &[]] {
            db = db
                .append_query_results([vec![user.clone()]])
                .append_query_results([vec![project.clone()]])
                .append_query_results([vec![access.clone()]])
                .append_query_results([vec![role.clone()]])
                .append_query_results([readable_caches(caches)]);
        }
        let state = TestState::new().web_db(db.into_connection()).build();
        let backend = open_project_store_backend_for_api_key(
            SshHandles::for_tests(&state),
            user.id,
            project.id,
            PermissionMask::MAX,
            None,
        )
        .await
        .unwrap()
        .expect("store backend opens");

        assert!(
            check_session_access_still_matches_api_key(&backend, PermissionMask::MAX, None)
                .await
                .unwrap()
        );
        assert!(
            !check_session_access_still_matches_api_key(&backend, PermissionMask::MAX, None)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn open_project_store_for_api_key_returns_no_store_without_project_view_permission() {
        use gradient_types::permissions::{Permission, mask_from};
        let user = gradient_fixtures::rows::user();
        let project = gradient_fixtures::rows::project();
        let role = MRole {
            id: RoleId::now_v7(),
            permission: i64::MAX,
            ..Default::default()
        };
        let access = MProjectAccess {
            project: project.id,
            user: user.id,
            role: role.id,
            ..Default::default()
        };
        let cache = CacheId::now_v7();
        for (key_mask, cache_pin) in [
            (mask_from(&[Permission::TriggerEvaluation]), None),
            (0, None),
            (PermissionMask::MAX, Some(cache)),
        ] {
            let db = mock_db()
                .append_query_results([vec![user.clone()]])
                .append_query_results([vec![project.clone()]])
                .append_query_results([vec![access.clone()]])
                .append_query_results([vec![role.clone()]])
                .append_query_results([vec![BTreeMap::from([(
                    "cache".to_owned(),
                    Value::from(cache.into_inner()),
                )])]])
                .into_connection();
            let state = TestState::new().web_db(db).build();

            let backend = open_project_store_backend_for_api_key(
                SshHandles::for_tests(&state),
                user.id,
                project.id,
                key_mask,
                cache_pin,
            )
            .await
            .unwrap();

            assert!(backend.is_none(), "{key_mask:#x} {cache_pin:?}");
        }
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
