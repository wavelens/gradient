/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::exec_job::ExecJobStatus;
use gradient_types::ProjectId;
use gradient_types::exec::ExecSpec;
use sea_orm::{ConnectionTrait, DbErr, QueryResult};
use std::sync::LazyLock;

use crate::sql::status::{eval_in, exec_in};

#[derive(Debug, Clone, PartialEq)]
pub struct UpcomingExecJob {
    pub spec: ExecSpec,
    pub architecture: String,
    pub project: ProjectId,
}

static WITHOUT_IMAGE_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT e.exec AS spec, d.architecture, t.project FROM entry_point e \
         JOIN evaluation ev ON ev.id = e.evaluation \
         JOIN task t ON t.id = e.task \
         JOIN derivation d ON d.id = e.derivation \
         WHERE e.exec IS NOT NULL AND e.exec->>'image' IS NULL \
         AND ev.status NOT IN ({terminal}) \
         AND NOT EXISTS (SELECT 1 FROM exec_job r WHERE r.entry_point = e.id) \
         UNION ALL \
         SELECT r.spec, d.architecture, t.project FROM exec_job r \
         JOIN entry_point e ON e.id = r.entry_point \
         JOIN task t ON t.id = e.task \
         JOIN derivation d ON d.id = r.program_derivation \
         WHERE r.status IN ({waiting}) AND r.image_derivation IS NULL",
        terminal = eval_in(&EvaluationStatus::TERMINAL),
        waiting = exec_in(&[ExecJobStatus::Created, ExecJobStatus::Queued]),
    )
});

crate::sql_lazy! {
    UPCOMING_EXEC_JOBS_WITHOUT_IMAGE = || WITHOUT_IMAGE_SQL.as_str(),
        params = [];
}

pub async fn without_image<C: ConnectionTrait>(db: &C) -> Result<Vec<UpcomingExecJob>, DbErr> {
    Ok(db
        .query_all_raw(UPCOMING_EXEC_JOBS_WITHOUT_IMAGE.stmt())
        .await?
        .iter()
        .filter_map(upcoming)
        .collect())
}

fn upcoming(row: &QueryResult) -> Option<UpcomingExecJob> {
    let spec: serde_json::Value = row.try_get("", "spec").ok()?;
    Some(UpcomingExecJob {
        spec: serde_json::from_value(spec).ok()?,
        architecture: row.try_get("", "architecture").ok()?,
        project: row.try_get("", "project").ok()?,
    })
}
