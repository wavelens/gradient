/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::github_installation;
use gradient_entity::integration::IntegrationKind;
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

pub async fn of_project<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<MIntegration>, DbErr> {
    EIntegration::find()
        .filter(CIntegration::Project.eq(project))
        .all(db)
        .await
}

pub async fn outbound_of_installation<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    installation: GithubInstallationId,
) -> Result<Option<MIntegration>, DbErr> {
    EIntegration::find()
        .filter(CIntegration::Project.eq(project))
        .filter(CIntegration::Kind.eq(IntegrationKind::Outbound))
        .filter(CIntegration::GithubInstallation.eq(installation))
        .one(db)
        .await
}

pub async fn by_name<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    kind: IntegrationKind,
    name: &str,
) -> Result<Option<MIntegration>, DbErr> {
    EIntegration::find()
        .filter(CIntegration::Project.eq(project))
        .filter(CIntegration::Kind.eq(kind))
        .filter(CIntegration::Name.eq(name))
        .one(db)
        .await
}

pub async fn other_by_name<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    kind: IntegrationKind,
    name: &str,
    except: IntegrationId,
) -> Result<Option<MIntegration>, DbErr> {
    EIntegration::find()
        .filter(CIntegration::Project.eq(project))
        .filter(CIntegration::Kind.eq(kind))
        .filter(CIntegration::Name.eq(name))
        .filter(CIntegration::Id.ne(except))
        .one(db)
        .await
}

pub async fn delete_installation_with_integrations<C: ConnectionTrait>(
    db: &C,
    installation: GithubInstallationId,
) -> Result<(), DbErr> {
    EIntegration::delete_many()
        .filter(CIntegration::GithubInstallation.eq(installation))
        .exec(db)
        .await?;
    github_installation::Entity::delete_by_id(installation)
        .exec(db)
        .await?;
    Ok(())
}

pub async fn by_id_in_project<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    id: IntegrationId,
) -> Result<Option<MIntegration>, DbErr> {
    EIntegration::find()
        .filter(CIntegration::Id.eq(id))
        .filter(CIntegration::Project.eq(project))
        .one(db)
        .await
}
