/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::{WebError, WebResult};
use crate::resource::Resource;
use gradient_types::{
    EApi, EBuildJob, ECacheRole, ECommit, EEvaluation, ERole, ESession, EUploadSession,
};
use sea_orm::{ConnectionTrait, EntityTrait, PrimaryKeyTrait};

pub trait HasResource: EntityTrait {
    const RESOURCE: Resource;
}

macro_rules! has_resource {
    ($($entity:ty => $resource:ident),* $(,)?) => {
        $(impl HasResource for $entity {
            const RESOURCE: Resource = Resource::$resource;
        })*
    };
}

has_resource! {
    EApi => ApiKey,
    EBuildJob => Build,
    ECacheRole => Role,
    ECommit => Commit,
    EEvaluation => Evaluation,
    ERole => Role,
    ESession => Session,
    EUploadSession => UploadSession,
    gradient_entity::dispatched_job::Entity => Job,
}

pub async fn require<E>(
    db: &impl ConnectionTrait,
    id: <E::PrimaryKey as PrimaryKeyTrait>::ValueType,
) -> WebResult<E::Model>
where
    E: HasResource,
{
    gradient_db::lookup::by_id::<E, _>(db, id)
        .await?
        .ok_or_else(|| WebError::not_found(E::RESOURCE))
}
