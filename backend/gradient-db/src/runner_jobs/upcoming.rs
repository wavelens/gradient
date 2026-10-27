/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::runner_job::RunnerJobStatus;
use gradient_types::runner::RunnerSpec;
use sea_orm::{ConnectionTrait, DbErr, QueryResult};
use std::sync::LazyLock;

use crate::sql::status::{eval_in, runner_in};

#[derive(Debug, Clone, PartialEq)]
pub struct UpcomingRunner {
    pub spec: RunnerSpec,
    pub architecture: String,
}

static WITHOUT_IMAGE_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT e.runner AS spec, d.architecture FROM entry_point e \
         JOIN evaluation ev ON ev.id = e.evaluation \
         JOIN derivation d ON d.id = e.derivation \
         WHERE e.runner IS NOT NULL AND e.runner->>'image' IS NULL \
         AND ev.status NOT IN ({terminal}) \
         AND NOT EXISTS (SELECT 1 FROM runner_job r WHERE r.entry_point = e.id) \
         UNION ALL \
         SELECT r.spec, d.architecture FROM runner_job r \
         JOIN derivation d ON d.id = r.program_derivation \
         WHERE r.status IN ({waiting}) AND r.image_derivation IS NULL",
        terminal = eval_in(&EvaluationStatus::TERMINAL),
        waiting = runner_in(&[RunnerJobStatus::Created, RunnerJobStatus::Queued]),
    )
});

crate::sql_lazy! {
    UPCOMING_RUNNERS_WITHOUT_IMAGE = || WITHOUT_IMAGE_SQL.as_str(),
        params = [];
}

pub async fn without_image<C: ConnectionTrait>(db: &C) -> Result<Vec<UpcomingRunner>, DbErr> {
    Ok(db
        .query_all_raw(UPCOMING_RUNNERS_WITHOUT_IMAGE.stmt())
        .await?
        .iter()
        .filter_map(upcoming)
        .collect())
}

fn upcoming(row: &QueryResult) -> Option<UpcomingRunner> {
    let spec: serde_json::Value = row.try_get("", "spec").ok()?;
    Some(UpcomingRunner {
        spec: serde_json::from_value(spec).ok()?,
        architecture: row.try_get("", "architecture").ok()?,
    })
}
