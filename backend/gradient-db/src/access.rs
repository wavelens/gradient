/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::permissions::PermissionMask;
use gradient_types::*;
use sea_orm::{ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

pub async fn project_permission_mask<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    user: UserId,
) -> Result<Option<(MProjectUser, PermissionMask)>, DbErr> {
    let Some(membership) = EProjectUser::find()
        .filter(
            Condition::all()
                .add(CProjectUser::Project.eq(project))
                .add(CProjectUser::User.eq(user)),
        )
        .one(db)
        .await?
    else {
        return Ok(None);
    };

    let mask = ERole::find_by_id(membership.role)
        .one(db)
        .await?
        .map(|r| r.permission)
        .unwrap_or(0);

    Ok(Some((membership, mask)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::{Permission, mask_from, mask_grants};
    use sea_orm::{DatabaseBackend, MockDatabase};

    #[tokio::test]
    async fn a_non_member_has_no_mask() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MProjectUser>::new()])
            .into_connection();

        let mask = project_permission_mask(&db, ProjectId::now_v7(), UserId::now_v7())
            .await
            .expect("query");
        assert!(mask.is_none());
    }

    #[tokio::test]
    async fn a_member_gets_the_role_mask() {
        let role = MRole {
            permission: mask_from(&[Permission::TriggerEvaluation]),
            ..Default::default()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![MProjectUser::default()]])
            .append_query_results([vec![role]])
            .into_connection();

        let (_, mask) = project_permission_mask(&db, ProjectId::now_v7(), UserId::now_v7())
            .await
            .expect("query")
            .expect("member");
        assert!(mask_grants(mask, Permission::TriggerEvaluation));
        assert!(!mask_grants(mask, Permission::ManageSshKey));
    }

    #[tokio::test]
    async fn a_missing_role_grants_nothing() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![MProjectUser::default()]])
            .append_query_results([Vec::<MRole>::new()])
            .into_connection();

        let (_, mask) = project_permission_mask(&db, ProjectId::now_v7(), UserId::now_v7())
            .await
            .expect("query")
            .expect("member");
        assert_eq!(mask, 0);
    }
}
