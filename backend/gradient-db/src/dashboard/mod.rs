/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

/// Visibility rules reflect `load_*(Readable)`. There's no superuser bypass. Placeholder arguments
/// set the public list flag. Public items don't appear with that flag false.
macro_rules! project_readable {
    ($listed:literal) => {
        concat!(
            "((",
            $listed,
            " AND p.public) OR EXISTS (SELECT 1 FROM project_access pu \
            WHERE pu.project = p.id AND pu.\"user\" = $1))"
        )
    };
}

macro_rules! cache_readable {
    ($listed:literal) => {
        concat!(
            "((",
            $listed,
            " AND c.public) OR c.created_by = $1 \
            OR EXISTS (SELECT 1 FROM cache_access cu WHERE cu.cache = c.id AND cu.\"user\" = $1) \
            OR EXISTS (SELECT 1 FROM project_cache pc JOIN project_access pu ON pu.project = pc.project \
            WHERE pc.cache = c.id AND pu.\"user\" = $1))"
        )
    };
}

use gradient_types::UserId;
use sea_orm::Value;

const NON_PR: &str = "tt.trigger_type IS DISTINCT FROM 2";

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
pub use tasks::{HistoryRow, TaskFactsRow, entry_point_outcomes, histories, task_facts};
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
