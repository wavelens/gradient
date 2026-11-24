/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Durable build and evaluation events are emitted with ids only. The fields a receiver is routing
//! and rendering on are filled here once, before fan-out.

use anyhow::{Context, Result};
use gradient_ci::reporting::eval_kind_str;
use gradient_types::events::Event;
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

struct Scope {
    evaluation: MEvaluation,
    task: TaskId,
    project: ProjectId,
}

pub async fn fill_event_fields_in_batch<C: ConnectionTrait>(
    db: &C,
    mapping: &Arc<dyn StorePathMapping>,
    events: Vec<Event>,
) -> Result<Vec<Option<Event>>> {
    let scopes =
        load_evaluation_scopes_for_ids(db, events.iter().filter_map(evaluation_id_from_event))
            .await?;

    let mapped_projects: HashSet<ProjectId> = events
        .iter()
        .filter(|event| !matches!(event, Event::ExecJobReported(_)))
        .filter_map(|event| scopes.get(&evaluation_id_from_event(event)?))
        .map(|scope| scope.project)
        .collect();
    let mut mappers = HashMap::new();
    for project in mapped_projects {
        if let Some(mapper) = project_mapper(mapping, project).await {
            mappers.insert(project, mapper);
        }
    }

    let derivation_ids: Vec<DerivationId> = events
        .iter()
        .filter_map(|event| match event {
            Event::BuildReported(b) => scopes
                .get(&b.evaluation_id)
                .filter(|scope| mappers.contains_key(&scope.project))
                .map(|_| b.derivation),
            _ => None,
        })
        .collect();
    let derivations: HashMap<DerivationId, MDerivation> = if derivation_ids.is_empty() {
        HashMap::new()
    } else {
        EDerivation::find()
            .filter(CDerivation::Id.is_in(derivation_ids))
            .all(db)
            .await
            .context("look up the derivations of build reports")?
            .into_iter()
            .map(|d| (d.id, d))
            .collect()
    };

    let entry_point_ids: Vec<EntryPointId> = events
        .iter()
        .filter_map(|event| match event {
            Event::ExecJobReported(r) => Some(r.entry_point),
            _ => None,
        })
        .collect();
    let entry_point_names: HashMap<EntryPointId, String> = if entry_point_ids.is_empty() {
        HashMap::new()
    } else {
        EEntryPoint::find()
            .filter(CEntryPoint::Id.is_in(entry_point_ids))
            .all(db)
            .await
            .context("look up the entry points of exec job reports")?
            .into_iter()
            .map(|entry_point| (entry_point.id, entry_point.eval))
            .collect()
    };

    Ok(events
        .into_iter()
        .map(|event| match event {
            Event::BuildReported(mut b) => {
                let scope = scopes.get(&b.evaluation_id)?;
                b.task = Some(scope.task);
                b.project = Some(scope.project);
                b.evaluation_kind = Some(eval_kind_str(scope.evaluation.kind).to_owned());
                b.allows_project_credentials_and_privileged_actions = scope
                    .evaluation
                    .allows_project_credentials_and_privileged_actions;
                if let Some(mapper) = mappers.get(&scope.project) {
                    b.derivation_path = derivations
                        .get(&b.derivation)
                        .map(|d| mapper.outbound(&d.as_store_path()).full());
                }
                Some(Event::from(b))
            }
            Event::EvaluationReported(mut e) => {
                let scope = scopes.get(&e.evaluation_id)?;
                e.task = Some(scope.task);
                e.project = Some(scope.project);
                e.repository = mappers
                    .get(&scope.project)
                    .map(|mapper| mapper.outbound_if_store_path(&scope.evaluation.repository));
                e.evaluation_kind = Some(eval_kind_str(scope.evaluation.kind).to_owned());
                e.allows_project_credentials_and_privileged_actions = scope
                    .evaluation
                    .allows_project_credentials_and_privileged_actions;
                Some(Event::from(e))
            }
            Event::ExecJobReported(mut r) => {
                let scope = scopes.get(&r.evaluation)?;
                r.task = Some(scope.task);
                r.project = Some(scope.project);
                r.allows_project_credentials_and_privileged_actions = scope
                    .evaluation
                    .allows_project_credentials_and_privileged_actions;
                r.entry_point_name = entry_point_names.get(&r.entry_point).cloned();
                Some(Event::from(r))
            }
            other => Some(other),
        })
        .collect())
}

fn evaluation_id_from_event(event: &Event) -> Option<EvaluationId> {
    match event {
        Event::BuildReported(b) => Some(b.evaluation_id),
        Event::EvaluationReported(e) => Some(e.evaluation_id),
        Event::ExecJobReported(r) => Some(r.evaluation),
        _ => None,
    }
}

async fn load_evaluation_scopes_for_ids<C: ConnectionTrait>(
    db: &C,
    ids: impl Iterator<Item = EvaluationId>,
) -> Result<HashMap<EvaluationId, Scope>> {
    let ids: HashSet<EvaluationId> = ids.collect();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let evaluations = EEvaluation::find()
        .filter(CEvaluation::Id.is_in(ids))
        .all(db)
        .await
        .context("look up the evaluations of events")?;
    let task_ids: HashSet<TaskId> = evaluations.iter().filter_map(|e| e.task).collect();
    if task_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let projects: HashMap<TaskId, ProjectId> = ETask::find()
        .filter(CTask::Id.is_in(task_ids))
        .all(db)
        .await
        .context("look up the tasks of events")?
        .into_iter()
        .map(|task| (task.id, task.project))
        .collect();
    Ok(evaluations
        .into_iter()
        .filter_map(|evaluation| {
            let task = evaluation.task?;
            let project = *projects.get(&task)?;
            Some((
                evaluation.id,
                Scope {
                    evaluation,
                    task,
                    project,
                },
            ))
        })
        .collect())
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
    use gradient_types::events::{build, exec_job};

    fn first_event_from_enriched_batch(mut events: Vec<Option<Event>>) -> Option<Event> {
        events.remove(0)
    }

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
        assert!(
            first_event_from_enriched_batch(
                fill_event_fields_in_batch(&db, &mapping, vec![event])
                    .await
                    .unwrap()
            )
            .is_none()
        );
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

        let Some(Event::BuildReported(reported)) = first_event_from_enriched_batch(
            fill_event_fields_in_batch(&db, &mapping, vec![event])
                .await
                .unwrap(),
        ) else {
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

        let Some(Event::BuildReported(reported)) = first_event_from_enriched_batch(
            fill_event_fields_in_batch(&db, &mapping, vec![event])
                .await
                .unwrap(),
        ) else {
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

        let Some(Event::ExecJobReported(reported)) = first_event_from_enriched_batch(
            fill_event_fields_in_batch(&db, &mapping, vec![event])
                .await
                .unwrap(),
        ) else {
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
            first_event_from_enriched_batch(
                fill_event_fields_in_batch(&db, &mapping, vec![event.clone()])
                    .await
                    .unwrap()
            ),
            Some(event)
        );
        assert!(db.into_transaction_log().is_empty());
    }
}
