/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{
    ApiId, CacheId, CachedPathId, DispatchedJobId, ProjectId, StorePathContentProducerId,
    TeamWorkerId, UserId, WorkerRegistrationId,
};

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, DeriveActiveEnum, EnumIter, Deserialize, Serialize,
)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum StoredContentVariantProducerKind {
    #[sea_orm(num_value = 0)]
    BuiltInDispatchedBuildJob,
    #[sea_orm(num_value = 1)]
    UploadedByUserOrApiKey,
    #[sea_orm(num_value = 2)]
    UploadedByProjectMember,
    #[default]
    #[sea_orm(num_value = 3)]
    SignedCacheBackfill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, DeriveActiveEnum, EnumIter, Deserialize, Serialize)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum CacheUploadChannel {
    #[sea_orm(num_value = 0)]
    RestHttpApiUpload,
    #[sea_orm(num_value = 1)]
    SshNixStoreUpload,
}

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "store_path_content_producer")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: StorePathContentProducerId,
    pub cached_path: CachedPathId,
    pub kind: StoredContentVariantProducerKind,
    pub worker_registration: Option<WorkerRegistrationId>,
    pub team_worker: Option<TeamWorkerId>,
    pub dispatched_job: Option<DispatchedJobId>,
    pub cache: Option<CacheId>,
    pub uploaded_by_user: Option<UserId>,
    pub uploaded_by_api_key: Option<ApiId>,
    pub upload_channel: Option<CacheUploadChannel>,
    pub uploading_member_project_id: Option<ProjectId>,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::cached_path::Entity",
        from = "Column::CachedPath",
        to = "super::cached_path::Column::Id"
    )]
    CachedPath,
}

impl ActiveModelBehavior for ActiveModel {}
