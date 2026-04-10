/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::Utc;
use entity::evaluation::EvaluationStatus;
use futures::stream::{self, StreamExt};
use gradient_core::sources::{check_project_updates, get_commit_info};
use gradient_core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, JoinType, QueryFilter,
    QueryOrder, QuerySelect, RelationTrait,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::time;
use tracing::{error, info, warn};
use uuid::Uuid;

/// Polls for the next project that is due for evaluation, creates the evaluation and commit
/// records, and returns the evaluation to run. Loops until one is found.
pub(super) async fn get_next_evaluation(state: Arc<ServerState>) -> MEvaluation {
    loop {
        let threshold_time =
            Utc::now().naive_utc() - chrono::Duration::seconds(state.cli.evaluation_timeout);

        let mut projects = match EProject::find()
            .join(JoinType::InnerJoin, RProject::LastEvaluation.def())
            .filter(
                Condition::all()
                    .add(CProject::Active.eq(true))
                    .add(CProject::LastCheckAt.lte(threshold_time))
                    .add(
                        Condition::any()
                            .add(CEvaluation::Status.eq(EvaluationStatus::Completed))
                            .add(CEvaluation::Status.eq(EvaluationStatus::Failed))
                            .add(CProject::ForceEvaluation.eq(true)),
                    ),
            )
            .order_by_asc(CProject::LastCheckAt)
            .all(&state.db)
            .await
        {
            Ok(projects) => projects,
            Err(e) => {
                error!(error = %e, "Failed to query projects for evaluation");
                time::sleep(Duration::from_secs(60)).await;
                continue;
            }
        };

        match EProject::find()
            .filter(
                Condition::all()
                    .add(CProject::Active.eq(true))
                    .add(CProject::LastCheckAt.lte(threshold_time))
                    .add(CProject::LastEvaluation.is_null()),
            )
            .order_by_asc(CProject::LastCheckAt)
            .all(&state.db)
            .await
        {
            Ok(additional_projects) => projects.extend(additional_projects),
            Err(e) => {
                error!(error = %e, "Failed to query projects without evaluations");
                time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        let mut i_offset = 0;
        for (i, project) in projects.clone().iter().enumerate() {
            let has_no_servers = match EServer::find()
                .filter(
                    Condition::all()
                        .add(CServer::Active.eq(true))
                        .add(CServer::Organization.eq(project.organization)),
                )
                .one(&state.db)
                .await
            {
                Ok(server_opt) => server_opt.is_none(),
                Err(e) => {
                    error!(error = %e, "Failed to query servers for project organization");
                    true // Assume no servers on error
                }
            };

            if has_no_servers {
                projects.remove(i - i_offset);
                i_offset += 1;
            }
        }

        let evaluations = stream::iter(projects.clone().into_iter())
            .filter_map(|p| {
                let intern_state = Arc::clone(&state);
                async move {
                    //TODO: query last evaluation early and pass it to check_project_updates
                    let (has_update, commit_hash) =
                        match check_project_updates(Arc::clone(&intern_state), &p).await {
                            Ok((update, hash)) => (update, hash),
                            Err(e) => {
                                error!(error = %e, "Failed to check project updates");
                                return None;
                            }
                        };

                    if !has_update {
                        return None;
                    }

                    if let Some(evaluation) = p.last_evaluation {
                        match EEvaluation::find_by_id(evaluation)
                            .filter(
                                Condition::any()
                                    .add(CEvaluation::Status.eq(EvaluationStatus::Completed))
                                    .add(CEvaluation::Status.eq(EvaluationStatus::Failed))
                                    .add(CEvaluation::Status.eq(EvaluationStatus::Aborted))
                                    .add(CEvaluation::Status.eq(EvaluationStatus::Queued)),
                            )
                            .one(&intern_state.db)
                            .await
                        {
                            Ok(Some(eval)) => Some((eval, commit_hash)),
                            Ok(None) => None,
                            Err(_) => None,
                        }
                    } else {
                        Some((
                            MEvaluation {
                                id: Uuid::nil(),
                                project: Some(p.id),
                                repository: p.repository,
                                commit: Uuid::nil(),
                                wildcard: p.evaluation_wildcard,
                                status: EvaluationStatus::Queued,
                                previous: None,
                                next: None,
                                created_at: Utc::now().naive_utc(),
                                updated_at: Utc::now().naive_utc(),
                            },
                            commit_hash,
                        ))
                    }
                }
            })
            .collect::<Vec<(MEvaluation, Vec<u8>)>>()
            .await;

        if evaluations.is_empty() {
            time::sleep(Duration::from_secs(5)).await;
            continue;
        }

        let (evaluation, commit_hash) = match evaluations.first() {
            Some(eval) => eval,
            None => {
                error!("No evaluations found despite non-empty evaluations list");
                time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        let project = if let Some(project_id) = evaluation.project {
            projects
                .into_iter()
                .find(|p| p.id == project_id)
                .unwrap_or_else(|| {
                    error!(
                        project_id = %project_id,
                        evaluation_id = %evaluation.id,
                        "Failed to find project for evaluation - critical error"
                    );
                    std::process::exit(1);
                })
        } else {
            // For direct builds, we don't have a project
            error!(
                evaluation_id = %evaluation.id,
                "Direct build evaluation scheduled as regular project evaluation - critical error"
            );
            std::process::exit(1);
        };

        // If the evaluation was pre-created as Queued (e.g. via the API endpoint), use it
        // directly without creating a new one.
        if evaluation.status == EvaluationStatus::Queued && evaluation.id != Uuid::nil() {
            let mut active_project: AProject = project.clone().into();
            active_project.last_check_at = Set(Utc::now().naive_utc());
            active_project.force_evaluation = Set(false);
            if let Err(e) = active_project.update(&state.db).await {
                error!(error = %e, "Failed to update project");
            }
            return evaluation.clone();
        }

        let evaluation_id = if evaluation.id == Uuid::nil() {
            None
        } else {
            Some(evaluation.id)
        };

        // Guard against the race condition where the API endpoint (or a
        // concurrent fetcher tick) already created a Queued/Evaluating/Building
        // evaluation while we were checking for updates with stale project data.
        match EEvaluation::find()
            .filter(CEvaluation::Project.eq(project.id))
            .filter(
                Condition::any()
                    .add(CEvaluation::Status.eq(EvaluationStatus::Queued))
                    .add(CEvaluation::Status.eq(EvaluationStatus::EvaluatingFlake))
                    .add(CEvaluation::Status.eq(EvaluationStatus::EvaluatingDerivation))
                    .add(CEvaluation::Status.eq(EvaluationStatus::Building))
                    .add(CEvaluation::Status.eq(EvaluationStatus::Waiting)),
            )
            .one(&state.db)
            .await
        {
            Ok(Some(existing)) => {
                // An evaluation is already in progress — reuse it and bail out.
                let mut active_project: AProject = project.clone().into();
                active_project.last_check_at = Set(Utc::now().naive_utc());
                active_project.force_evaluation = Set(false);
                if let Err(e) = active_project.update(&state.db).await {
                    error!(error = %e, "Failed to update project after in-progress guard");
                }
                return existing;
            }
            Ok(None) => {}
            Err(e) => {
                error!(error = %e, "Failed to check for in-progress evaluations");
                time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        }

        let (commit_message, author_email, author_name) =
            match get_commit_info(Arc::clone(&state), &project, commit_hash).await {
                Ok((msg, email, name)) => (msg, email, name),
                Err(e) => {
                    warn!(
                        error = %e,
                        "Failed to fetch commit info, using defaults"
                    );
                    ("".to_string(), None, "".to_string())
                }
            };

        let author_display = if let Some(email) = author_email {
            if !author_name.is_empty() {
                format!("{} <{}>", author_name, email)
            } else {
                email
            }
        } else {
            author_name
        };

        let acommit = ACommit {
            id: Set(Uuid::new_v4()),
            message: Set(commit_message),
            hash: Set(commit_hash.clone()),
            author: Set(None),
            author_name: Set(author_display),
        };

        let commit = match acommit.insert(&state.db).await {
            Ok(c) => c,
            Err(e) => {
                error!(error = %e, "Failed to insert commit");
                continue;
            }
        };
        let now = Utc::now().naive_utc();
        let new_evaluation = AEvaluation {
            id: Set(Uuid::new_v4()),
            project: Set(Some(project.id)),
            repository: Set(project.repository.clone()),
            commit: Set(commit.id),
            wildcard: Set(project.evaluation_wildcard.clone()),
            status: Set(EvaluationStatus::Queued),
            previous: Set(evaluation_id),
            next: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        };

        let new_evaluation = match new_evaluation.insert(&state.db).await {
            Ok(e) => e,
            Err(e) => {
                error!(error = %e, "Failed to insert evaluation");
                continue;
            }
        };
        info!(evaluation_id = %new_evaluation.id, "Created new evaluation");

        // GC: remove evaluations beyond keep_evaluations for this project
        let gc_state = Arc::clone(&state);
        let gc_project_id = project.id;
        let gc_keep = project.keep_evaluations as usize;
        tokio::spawn(async move {
            if let Err(e) =
                gradient_core::db::gc_project_evaluations(gc_state, gc_project_id, gc_keep).await
            {
                tracing::error!(error = %e, project_id = %gc_project_id, "GC: per-project evaluation GC failed");
            }
        });

        let mut active_project: AProject = project.clone().into();

        active_project.last_check_at = Set(Utc::now().naive_utc());
        active_project.last_evaluation = Set(Some(new_evaluation.id));
        active_project.force_evaluation = Set(false);

        if let Err(e) = active_project.update(&state.db).await {
            error!(error = %e, "Failed to update project");
        }

        if evaluation_id.is_some() {
            let mut active_evaluation: AEvaluation = evaluation.clone().into();
            active_evaluation.next = Set(Some(new_evaluation.id));

            if let Err(e) = active_evaluation.update(&state.db).await {
                error!(error = %e, "Failed to update evaluation");
            }
        };

        return new_evaluation;
    }
}
