/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

/// Visibility of `p` / `c` to user `$1`, mirroring `load_*(Readable)`: no superuser bypass.
macro_rules! project_readable {
    () => {
        "(p.public OR EXISTS (SELECT 1 FROM project_user pu \
        WHERE pu.project = p.id AND pu.\"user\" = $1))"
    };
}

macro_rules! cache_readable {
    () => {
        "(c.public OR c.created_by = $1 \
        OR EXISTS (SELECT 1 FROM cache_user cu WHERE cu.cache = c.id AND cu.\"user\" = $1) \
        OR EXISTS (SELECT 1 FROM project_cache pc JOIN project_user pu ON pu.project = pc.project \
        WHERE pc.cache = c.id AND pu.\"user\" = $1))"
    };
}

const NON_PR: &str = "tt.trigger_type IS DISTINCT FROM 2";

mod rail;
mod search;
mod stars;
mod tasks;
mod totals;

pub use rail::{RailCacheRow, RailProjectRow, RailTaskRow, rail_caches, rail_projects, rail_tasks};
pub use search::{
    CommitHitRow, NameHitRow, NameKind, NarHitRow, search_commits, search_names, search_nars,
};
pub use stars::{StarKind, StarredNames, StarredTask, star, starred, starred_names, unstar};
pub use tasks::{HistoryRow, TaskFactsRow, entry_point_outcomes, histories, task_facts};
pub use totals::{
    ActivityDay, Totals, activity, activity_sql, cache_size, scoped_totals, totals_sql,
};
