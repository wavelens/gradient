/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::entity::prelude::*;
use uuid::Uuid;

/// Join table: attaches an `evaluation_message` to one or more `entry_point` rows.
///
/// An `evaluation_message` with zero `entry_point_message` rows is evaluation-scoped
/// (pipeline-level error or global warning). With one or more rows the message is
/// attributed to those specific entry points (attribute paths).
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "entry_point_message")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub entry_point: Uuid,
    pub message: Uuid,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::entry_point::Entity",
        from = "Column::EntryPoint",
        to = "super::entry_point::Column::Id"
    )]
    EntryPoint,
    #[sea_orm(
        belongs_to = "super::evaluation_message::Entity",
        from = "Column::Message",
        to = "super::evaluation_message::Column::Id"
    )]
    EvaluationMessage,
}

impl ActiveModelBehavior for ActiveModel {}
