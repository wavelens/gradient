/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Durable build and evaluation events are emitted with ids only. The fields a receiver is routing
//! and rendering on are filled here once, before fan-out.

use anyhow::{Context, Result};
use gradient_ci::reporting::eval_kind_str;
use gradient_db::DbContext;
use gradient_types::events::{Event, build, evaluation, exec_job};
use gradient_types::*;
use sea_orm::{ConnectionTrait, EntityTrait};
use std::sync::Arc;

pub async fn enrich(ctx: &DbContext, event: Event) -> Result<Option<Event>> {
    enrich_with(&ctx.worker_db, &ctx.store_path_mapping, event).await
}

pub async fn enrich_with<C: ConnectionTrait>(
    db: &C,
    mapping: &Arc<dyn StorePathMapping>,
    event: Event,
) -> Result<Option<Event>> {
    Ok(match event {
        Event::BuildReported(b) => enrich_build(db, mapping, b).await?.map(Event::from),
        Event::EvaluationReported(e) => enrich_evaluation(db, mapping, e).await?.map(Event::from),
        Event::ExecJobReported(r) => enrich_exec_job(db, r).await?.map(Event::from),
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
        .context("look up the evaluation of an event")?
    else {
        return Ok(None);
    };
    let Some(task) = evaluation.task else {
        return Ok(None);
    };
    let Some(task_row) = ETask::find_by_id(task)
        .one(db)
        .await
        .context("look up the task of an event")?
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
    mapping: &Arc<dyn StorePathMapping>,
    mut b: build::Reported,
) -> Result<Option<build::Reported>> {
    let Some(scope) = evaluation_scope(db, b.evaluation_id).await? else {
        return Ok(None);
    };
    b.task = Some(scope.task);
    b.project = Some(scope.project);
    b.evaluation_kind = Some(eval_kind_str(scope.evaluation.kind).to_owned());
    if let Some(mapper) = project_mapper(mapping, scope.project).await {
        b.derivation_path = EDerivation::find_by_id(b.derivation)
            .one(db)
            .await
            .context("look up the derivation of a build report")?
            .map(|d| mapper.outbound(&d.as_store_path()).full());
    }
    Ok(Some(b))
}

async fn enrich_exec_job<C: ConnectionTrait>(
    db: &C,
    mut r: exec_job::Reported,
) -> Result<Option<exec_job::Reported>> {
    let Some(scope) = evaluation_scope(db, r.evaluation).await? else {
        return Ok(None);
    };
    r.task = Some(scope.task);
    r.project = Some(scope.project);
    r.entry_point_name = EEntryPoint::find_by_id(r.entry_point)
        .one(db)
        .await
        .context("look up the entry point of an exec job report")?
        .map(|entry_point| entry_point.eval);
    Ok(Some(r))
}

async fn enrich_evaluation<C: ConnectionTrait>(
    db: &C,
    mapping: &Arc<dyn StorePathMapping>,
    mut e: evaluation::Reported,
) -> Result<Option<evaluation::Reported>> {
    let Some(scope) = evaluation_scope(db, e.evaluation_id).await? else {
        return Ok(None);
    };
    e.task = Some(scope.task);
    e.project = Some(scope.project);
    e.repository = project_mapper(mapping, scope.project)
        .await
        .map(|mapper| mapper.outbound_if_store_path(&scope.evaluation.repository));
    e.evaluation_kind = Some(eval_kind_str(scope.evaluation.kind).to_owned());
    Ok(Some(e))
}

async fn project_mapper(
    mapping: &Arc<dyn StorePathMapping>,
    project: ProjectId,
) -> Option<StorePathMapper> {
    StorePathMapper::for_project(Arc::clone(mapping), project)
        .await
        .inspect_err(|error| tracing::warn!(%project, %error, "event leaves out its store paths"))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::build::BuildStatus;
    use gradient_entity::{
        EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH, RotatedHashMapping, UnresolvableProjectMapping,
    };
    use gradient_fixtures::mock::{empty_db, mock_db};

    #[tokio::test]
    async fn a_build_whose_evaluation_was_collected_reports_nothing() {
        let db = mock_db()
            .append_query_results([Vec::<MEvaluation>::new()])
            .into_connection();
        let event: Event = build::Reported {
            status: i16::from(BuildStatus::Completed),
            ..Default::default()
        }
        .into();

        let mapping: Arc<dyn StorePathMapping> = Arc::new(gradient_entity::IdentityMapping);
        assert!(enrich_with(&db, &mapping, event).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_reported_build_names_its_original_derivation_path() {
        let evaluation = MEvaluation {
            id: EvaluationId::now_v7(),
            task: Some(TaskId::now_v7()),
            ..Default::default()
        };
        let task = MTask {
            id: evaluation.task.unwrap(),
            project: ProjectId::now_v7(),
            ..Default::default()
        };
        let derivation = MDerivation {
            id: DerivationId::now_v7(),
            hash: EXAMPLE_STORED_HASH.into(),
            name: "hello".into(),
            ..Default::default()
        };
        let db = mock_db()
            .append_query_results([vec![evaluation.clone()]])
            .append_query_results([vec![task]])
            .append_query_results([vec![derivation.clone()]])
            .into_connection();
        let event: Event = build::Reported {
            evaluation_id: evaluation.id,
            derivation: derivation.id,
            ..Default::default()
        }
        .into();
        let mapping: Arc<dyn StorePathMapping> = Arc::new(RotatedHashMapping);

        let Some(Event::BuildReported(reported)) = enrich_with(&db, &mapping, event).await.unwrap()
        else {
            panic!("the build is reported");
        };
        assert_eq!(
            reported.derivation_path,
            Some(format!("/nix/store/{EXAMPLE_ORIGINAL_HASH}-hello.drv"))
        );
    }

    #[tokio::test]
    async fn a_build_of_a_project_that_does_not_resolve_names_no_derivation_path() {
        let evaluation = MEvaluation {
            id: EvaluationId::now_v7(),
            task: Some(TaskId::now_v7()),
            ..Default::default()
        };
        let task = MTask {
            id: evaluation.task.unwrap(),
            project: ProjectId::now_v7(),
            ..Default::default()
        };
        let db = mock_db()
            .append_query_results([vec![evaluation.clone()]])
            .append_query_results([vec![task]])
            .into_connection();
        let event: Event = build::Reported {
            evaluation_id: evaluation.id,
            derivation: DerivationId::now_v7(),
            ..Default::default()
        }
        .into();
        let mapping: Arc<dyn StorePathMapping> = Arc::new(UnresolvableProjectMapping);

        let Some(Event::BuildReported(reported)) = enrich_with(&db, &mapping, event).await.unwrap()
        else {
            panic!("the build is reported");
        };
        assert_eq!(reported.derivation_path, None);
    }

    #[tokio::test]
    async fn an_exec_job_event_names_its_task_project_and_entry_point() {
        let evaluation = MEvaluation {
            id: EvaluationId::now_v7(),
            task: Some(TaskId::now_v7()),
            ..Default::default()
        };
        let task = MTask {
            id: evaluation.task.unwrap(),
            project: ProjectId::now_v7(),
            ..Default::default()
        };
        let entry_point = MEntryPoint {
            id: EntryPointId::now_v7(),
            evaluation: evaluation.id,
            eval: "apps.x86_64-linux.curl".into(),
            ..Default::default()
        };
        let db = mock_db()
            .append_query_results([vec![evaluation.clone()]])
            .append_query_results([vec![task.clone()]])
            .append_query_results([vec![entry_point.clone()]])
            .into_connection();
        let event: Event = exec_job::Reported {
            job: ExecJobId::now_v7(),
            entry_point: entry_point.id,
            evaluation: evaluation.id,
            status: i16::from(gradient_entity::exec_job::ExecJobStatus::Queued),
            ..Default::default()
        }
        .into();
        let mapping: Arc<dyn StorePathMapping> = Arc::new(gradient_entity::IdentityMapping);

        let Some(Event::ExecJobReported(reported)) =
            enrich_with(&db, &mapping, event).await.unwrap()
        else {
            panic!("the exec job is reported");
        };
        assert_eq!(reported.task, Some(task.id));
        assert_eq!(reported.project, Some(task.project));
        assert_eq!(
            reported.entry_point_name.as_deref(),
            Some("apps.x86_64-linux.curl")
        );
    }

    #[tokio::test]
    async fn events_without_enrichment_pass_through_untouched() {
        let db = empty_db();
        let event: Event = gradient_types::events::gc::Swept {
            pass: gradient_types::events::gc::Pass::OrphanNars,
            removed: 2,
        }
        .into();

        let mapping: Arc<dyn StorePathMapping> = Arc::new(gradient_entity::IdentityMapping);
        assert_eq!(
            enrich_with(&db, &mapping, event.clone()).await.unwrap(),
            Some(event)
        );
        assert!(db.into_transaction_log().is_empty());
    }
}
