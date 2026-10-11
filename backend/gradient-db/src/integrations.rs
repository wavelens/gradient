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

pub async fn installations_by_github_id<C: ConnectionTrait>(
    db: &C,
    installation_id: i64,
) -> Result<Vec<github_installation::Model>, DbErr> {
    github_installation::Entity::find()
        .filter(github_installation::Column::InstallationId.eq(installation_id))
        .all(db)
        .await
}

pub async fn delete_installations_by_github_id<C: ConnectionTrait>(
    db: &C,
    installation_id: i64,
) -> Result<(), DbErr> {
    github_installation::Entity::delete_many()
        .filter(github_installation::Column::InstallationId.eq(installation_id))
        .exec(db)
        .await?;
    Ok(())
}

pub async fn inbound_github_of_installations<C: ConnectionTrait>(
    db: &C,
    projects: Vec<ProjectId>,
    installations: Vec<GithubInstallationId>,
) -> Result<Vec<MIntegration>, DbErr> {
    EIntegration::find()
        .filter(CIntegration::Project.is_in(projects))
        .filter(CIntegration::Kind.eq(i16::from(IntegrationKind::Inbound)))
        .filter(CIntegration::GitHostType.eq(i16::from(GitHostType::GitHub)))
        .filter(CIntegration::GithubInstallation.is_in(installations))
        .all(db)
        .await
}
