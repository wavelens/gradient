/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::dispatched_job::{DispatchedJobKind, DispatchedJobOutcome};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::EvaluationId;
use sea_orm::{ConnectionTrait, DbErr};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LostCompletion {
    pub evaluation: EvaluationId,
    pub outcome: DispatchedJobOutcome,
}

/// The grace is measured on `evaluation.updated_at`, which every status write is refreshing.
/// It must stay above the graph writer's RPC timeout.
/// A transition still in flight would look lost otherwise.
fn lost_eval_completions_sql(grace_secs: i64) -> String {
    format!(
        "SELECT ev.id AS evaluation, dj.outcome AS outcome \
         FROM evaluation ev \
         JOIN LATERAL ( \
           SELECT outcome, finished_at FROM dispatched_job \
           WHERE evaluation_id = ev.id AND kind = {eval_kind} \
           ORDER BY dispatched_at DESC LIMIT 1 \
         ) dj ON TRUE \
         WHERE ev.status IN ({evaluating}) \
           AND dj.finished_at IS NOT NULL \
           AND dj.outcome IS NOT NULL \
           AND ev.updated_at < (now() AT TIME ZONE 'UTC') - make_interval(secs => {grace_secs})",
        eval_kind = i16::from(DispatchedJobKind::Eval),
        evaluating = crate::sql::status::eval_in(&EvaluationStatus::EVALUATING),
    )
}

crate::sql_fn! {
    LOST_EVAL_COMPLETIONS = || lost_eval_completions_sql(900),
        params = [],
        tier = Sweep;
}

pub async fn lost_eval_completions<C: ConnectionTrait>(
    db: &C,
    grace_secs: i64,
) -> Result<Vec<LostCompletion>, DbErr> {
    // `grace_secs` is baked into the text instead of bound.
    // The exemplar above is then the shape the plan gate is checking.
    let rows = db
        .query_all_raw(LOST_EVAL_COMPLETIONS.bind_built(lost_eval_completions_sql(grace_secs), []))
        .await?;

    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let evaluation = r.try_get::<uuid::Uuid>("", "evaluation").ok()?;
            let outcome =
                DispatchedJobOutcome::try_from(r.try_get::<i16>("", "outcome").ok()?).ok()?;
            Some(LostCompletion {
                evaluation: EvaluationId::new(evaluation),
                outcome,
            })
        })
        .collect())
}
