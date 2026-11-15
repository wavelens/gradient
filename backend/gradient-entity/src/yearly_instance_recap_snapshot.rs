/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::{NaiveDate, NaiveDateTime};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::YearlyInstanceRecapSnapshotId;

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "yearly_instance_recap_snapshot")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: YearlyInstanceRecapSnapshotId,
    #[sea_orm(unique)]
    pub year: i32,
    pub created_at: NaiveDateTime,
    pub data_since: NaiveDate,
    pub statistics: Json,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
