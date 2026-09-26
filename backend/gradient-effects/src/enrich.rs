/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Durable build and evaluation events are emitted with ids only; the fields a
//! receiver routes and renders on are filled here, once, before fan-out.

use anyhow::{Context, Result};
use gradient_ci::reporting::eval_kind_str;
use gradient_db::DbContext;
use gradient_types::events::{Event, build, evaluation};
use gradient_types::*;
use sea_orm::{ConnectionTrait, EntityTrait};

/// `None` when the event's evaluation or task is gone: nothing is left to report to.
pub async fn enrich(ctx: &DbContext, event: Event) -> Result<Option<Event>> {
    enrich_with(&ctx.worker_db, event).await
}

pub async fn enrich_with<C: ConnectionTrait>(db: &C, event: Event) -> Result<Option<Event>> {
    Ok(match event {
        Event::BuildReported(b) => enrich_build(db, b).await?.map(Event::from),
        Event::EvaluationReported(e) => enrich_evaluation(db, e).await?.map(Event::from),
        other => Some(other),
    })
}

struct Scope {
    evaluation: MEvaluation,
    task: TaskId,
    project: ProjectId,
}

async fn evaluation_scope<C: ConnectionTrait>(db: &C, id: EvaluationId) -> Result<Option<Scope>> {
    let Some(evaluation) = EEvaluation::find_by_id(id)
        .one(db)
        .await
        .context("looking up the evaluation of an event")?
    else {
        return Ok(None);
    };
    let Some(task) = evaluation.task else {
        return Ok(None);
    };
    let Some(task_row) = ETask::find_by_id(task)
        .one(db)
        .await
        .context("looking up the task of an event")?
    else {
        return Ok(None);
    };
    Ok(Some(Scope {
        evaluation,
        task,
        project: task_row.project,
    }))
}

async fn enrich_build<C: ConnectionTrait>(
    db: &C,
    mut b: build::Reported,
) -> Result<Option<build::Reported>> {
    let Some(scope) = evaluation_scope(db, b.evaluation_id).await? else {
        return Ok(None);
    };
    b.task = Some(scope.task);
    b.project = Some(scope.project);
    b.evaluation_kind = Some(eval_kind_str(scope.evaluation.kind).to_owned());
    b.derivation_path = EDerivation::find_by_id(b.derivation)
        .one(db)
        .await
        .context("looking up the derivation of a build report")?
        .map(|d| d.store_path());
    Ok(Some(b))
}

async fn enrich_evaluation<C: ConnectionTrait>(
    db: &C,
    mut e: evaluation::Reported,
) -> Result<Option<evaluation::Reported>> {
    let Some(scope) = evaluation_scope(db, e.evaluation_id).await? else {
        return Ok(None);
    };
    e.task = Some(scope.task);
    e.project = Some(scope.project);
    e.repository = Some(scope.evaluation.repository);
    e.evaluation_kind = Some(eval_kind_str(scope.evaluation.kind).to_owned());
    Ok(Some(e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::build::BuildStatus;
    use sea_orm::{DatabaseBackend, MockDatabase};

    #[tokio::test]
    async fn a_build_whose_evaluation_was_collected_reports_nothing() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MEvaluation>::new()])
            .into_connection();
        let event: Event = build::Reported {
            status: i32::from(BuildStatus::Completed) as i16,
            ..Default::default()
        }
        .into();

        assert!(enrich_with(&db, event).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn events_without_enrichment_pass_through_untouched() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let event: Event = gradient_types::events::gc::Swept {
            pass: gradient_types::events::gc::Pass::OrphanNars,
            removed: 2,
        }
        .into();

        assert_eq!(enrich_with(&db, event.clone()).await.unwrap(), Some(event));
        assert!(db.into_transaction_log().is_empty());
    }
}
