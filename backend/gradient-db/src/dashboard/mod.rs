/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

/// Visibility rules match `load_*(Readable)`. Superusers can't bypass them. Placeholder arguments
/// keep the public list flag. Stars from users keep public items visible regardless of that flag.
macro_rules! project_readable {
    ($listed:literal) => {
        concat!(
            "(((",
            $listed,
            " OR EXISTS (SELECT 1 FROM user_project_star ps WHERE ps.\"user\" = $1 AND ps.project = p.id)) \
            AND p.public) OR EXISTS (SELECT 1 FROM project_access pu \
            WHERE pu.project = p.id AND pu.\"user\" = $1))"
        )
    };
}

macro_rules! cache_readable {
    ($listed:literal) => {
        concat!(
            "(((",
            $listed,
            " OR EXISTS (SELECT 1 FROM user_cache_star cs WHERE cs.\"user\" = $1 AND cs.cache = c.id)) \
            AND c.public) OR c.created_by = $1 \
            OR EXISTS (SELECT 1 FROM cache_access cu WHERE cu.cache = c.id AND cu.\"user\" = $1) \
            OR EXISTS (SELECT 1 FROM project_cache pc JOIN project_access pu ON pu.project = pc.project \
            WHERE pc.cache = c.id AND pu.\"user\" = $1))"
        )
    };
}

use gradient_types::UserId;
use sea_orm::Value;

use crate::evaluations::current_evaluation::NON_PR;

mod rail;
mod search;
mod stars;
mod tasks;
mod totals;

pub use rail::{RailCacheRow, RailProjectRow, RailTaskRow, rail_caches, rail_projects, rail_tasks};
pub use search::{
    CommitHitRow, NameHitRow, NameKind, NarHitRow, ilike_contains, readable_cache_ids,
    search_commits, search_names, search_nars,
};
pub use stars::{StarKind, StarredNames, StarredTask, star, starred, starred_names, unstar};
pub use tasks::{
    HistoryRow, LatestTaskEvaluationRow, TaskFactsRow, entry_point_outcomes, histories, task_facts,
};
pub use totals::{
    ActivityDay, Totals, activity, activity_sql, cache_size, scoped_totals, totals_sql,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Viewer {
    pub user: UserId,
    pub public_listing: bool,
}

impl Viewer {
    fn user_value(self) -> Value {
        stars::user_value(self.user)
    }

    fn listing_value(self) -> Value {
        Value::Bool(Some(self.public_listing))
    }
}
