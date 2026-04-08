/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Severity level of an evaluation message, matching Nix's verbosity levels.
#[derive(Debug, Clone, PartialEq, Eq, DeriveActiveEnum, EnumIter, Deserialize, Serialize)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum MessageLevel {
    #[sea_orm(num_value = 0)]
    Error,
    #[sea_orm(num_value = 1)]
    Warning,
    #[sea_orm(num_value = 2)]
    Notice,
}

/// A single message emitted during an evaluation — error, warning, or notice.
///
/// Messages with no `entry_point_message` rows are **evaluation-scoped** (e.g.
/// flake fetch failures, global warnings). Messages joined via `entry_point_message`
/// are attributed to specific attribute paths.
///
/// `source` carries where the message originated:
/// - `"flake-prefetch"` — `nix flake prefetch` or SSH fetch failure
/// - `"nix-eval"` — wildcard listing or path resolution (not attr-specific)
/// - `"nix-eval:<attr>"` — resolution of a specific attribute path
/// - `"dep-graph"` — dependency graph walk error
/// - `"db-insert"` — internal: batch insert failure
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "evaluation_message")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub evaluation: Uuid,
    pub level: MessageLevel,
    pub message: String,
    pub source: Option<String>,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::evaluation::Entity",
        from = "Column::Evaluation",
        to = "super::evaluation::Column::Id"
    )]
    Evaluation,
}

impl ActiveModelBehavior for ActiveModel {}
