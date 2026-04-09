/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::Utc;
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use entity::evaluation_message::MessageLevel;
use futures::stream::{self, StreamExt};
use gradient_core::ci_reporter::{CiReport, CiStatus, parse_owner_repo, reporter_for_project};
use gradient_core::webhooks::decrypt_webhook_secret;
use gradient_core::sources::*;
use gradient_core::status::{
    record_evaluation_message, update_build_status, update_evaluation_status,
    update_evaluation_status_with_error,
};
use gradient_core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, JoinType, QueryFilter,
    QueryOrder, QuerySelect, RelationTrait,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{error, info, instrument, warn};
use uuid::Uuid;

use crate::eval::evaluate;

pub async fn schedule_evaluation_loop(state: Arc<ServerState>) {
    let _guard = if state.cli.report_errors {
        Some(sentry::init(
            "https://5895e5a5d35f4dbebbcc47d5a722c402@reports.wavelens.io/1",
        ))
    } else {
        None
    };

    let mut current_schedules = vec![];
    let mut interval = time::interval(Duration::from_secs(5));

    loop {
        let mut added_schedule = false;
        current_schedules.retain(|schedule: &JoinHandle<()>| !schedule.is_finished());

        // TODO: look at tokio semaphore
        while current_schedules.len() < state.cli.max_concurrent_evaluations {
            let evaluation = get_next_evaluation(Arc::clone(&state)).await;
            let schedule = tokio::spawn(schedule_evaluation(Arc::clone(&state), evaluation));
            current_schedules.push(schedule);
            added_schedule = true;
        }

        if !added_schedule {
            interval.tick().await;
        }
    }
}

#[instrument(skip(state), fields(evaluation_id = %evaluation.id))]
pub async fn schedule_evaluation(state: Arc<ServerState>, evaluation: MEvaluation) {
    info!("Reviewing evaluation");

    // Report the top-level "gradient" check as Running so GitHub/Gitea shows
    // the evaluation is in progress before nix eval even starts.
    if let Some(project_id) = evaluation.project {
        report_ci_for_evaluation(
            Arc::clone(&state),
            project_id,
            evaluation.commit,
            &evaluation.repository,
            evaluation.id,
            CiStatus::Running,
        )
        .await;
    }

    let builds = evaluate(Arc::clone(&state), &evaluation).await;

    match builds {
        Ok(builds) => {
            // Re-fetch to check if aborted while evaluate() was running
            match EEvaluation::find_by_id(evaluation.id).one(&state.db).await {
                Ok(Some(current)) if current.status == EvaluationStatus::Aborted => return,
                _ => {}
            }

            let (
                builds,
                new_derivations,
                new_derivation_outputs,
                new_derivation_dependencies,
                entry_point_build_ids,
                failed_derivations,
                pending_features,
                eval_warnings,
            ) = builds;

            info!(
                build_count = builds.len(),
                new_derivation_count = new_derivations.len(),
                dependency_count = new_derivation_dependencies.len(),
                "Created builds + derivations"
            );

            const BATCH_SIZE: usize = 1000;

            // 1. Derivations first (builds + derivation_output FK into it).
            if !new_derivations.is_empty() {
                let active: Vec<ADerivation> = new_derivations
                    .iter()
                    .map(|d| d.clone().into_active_model())
                    .collect();
                for chunk in active.chunks(BATCH_SIZE) {
                    if let Err(e) = EDerivation::insert_many(chunk.to_vec())
                        .exec(&state.db)
                        .await
                    {
                        error!(error = %e, "Failed to insert derivations");
                        update_evaluation_status_with_error(
                            Arc::clone(&state),
                            evaluation,
                            EvaluationStatus::Failed,
                            format!("Failed to insert derivations: {}", e),
                            Some("db-insert".to_string()),
                        )
                        .await;
                        return;
                    }
                }
            }

            // 2. Derivation outputs.
            if !new_derivation_outputs.is_empty() {
                for chunk in new_derivation_outputs.chunks(BATCH_SIZE) {
                    if let Err(e) = EDerivationOutput::insert_many(chunk.to_vec())
                        .exec(&state.db)
                        .await
                    {
                        error!(error = %e, "Failed to insert derivation outputs");
                    }
                }
            }

            // 3. Derivation dependency edges.
            if !new_derivation_dependencies.is_empty() {
                let active: Vec<ADerivationDependency> = new_derivation_dependencies
                    .iter()
                    .map(|d| d.clone().into_active_model())
                    .collect();
                for chunk in active.chunks(BATCH_SIZE) {
                    if let Err(e) = EDerivationDependency::insert_many(chunk.to_vec())
                        .exec(&state.db)
                        .await
                    {
                        error!(error = %e, "Failed to insert derivation dependencies");
                    }
                }
            }

            // 4. Builds (FK into derivation).
            let active_builds = builds
                .iter()
                .map(|b| b.clone().into_active_model())
                .collect::<Vec<ABuild>>();
            if !active_builds.is_empty() {
                for chunk in active_builds.chunks(BATCH_SIZE) {
                    if let Err(e) = EBuild::insert_many(chunk.to_vec()).exec(&state.db).await {
                        error!(error = %e, "Failed to insert builds");
                        update_evaluation_status_with_error(
                            Arc::clone(&state),
                            evaluation,
                            EvaluationStatus::Failed,
                            format!("Failed to insert builds: {}", e),
                            Some("db-insert".to_string()),
                        )
                        .await;
                        return;
                    }
                }
            }

            // 5. Derivation features (FK satisfied now).
            for (derivation_id, features) in pending_features {
                if let Err(e) = gradient_core::database::add_features(
                    Arc::clone(&state),
                    features,
                    Some(derivation_id),
                    None,
                )
                .await
                {
                    error!(error = %e, %derivation_id, "Failed to add features for derivation");
                }
            }

            if let Some(project_id) = evaluation.project {
                let now = chrono::Utc::now().naive_utc();
                let active_entry_points = entry_point_build_ids
                    .iter()
                    .map(|(build_id, eval)| AEntryPoint {
                        id: sea_orm::ActiveValue::Set(Uuid::new_v4()),
                        project: sea_orm::ActiveValue::Set(project_id),
                        evaluation: sea_orm::ActiveValue::Set(evaluation.id),
                        build: sea_orm::ActiveValue::Set(*build_id),
                        eval: sea_orm::ActiveValue::Set(eval.clone()),
                        created_at: sea_orm::ActiveValue::Set(now),
                    })
                    .collect::<Vec<AEntryPoint>>();

                if !active_entry_points.is_empty() {
                    const BATCH_SIZE: usize = 1000;
                    for chunk in active_entry_points.chunks(BATCH_SIZE) {
                        if let Err(e) = EEntryPoint::insert_many(chunk.to_vec())
                            .exec(&state.db)
                            .await
                        {
                            error!(error = %e, "Failed to insert entry points");
                        }
                    }
                }

                // Report one CI check per entry point (Pending — builds are now queued).
                report_ci_for_entry_points(
                    Arc::clone(&state),
                    project_id,
                    evaluation.commit,
                    &evaluation.repository,
                    evaluation.id,
                    &entry_point_build_ids,
                    CiStatus::Pending,
                )
                .await;
            }

            // Transition all Created builds for this evaluation to Queued now that
            // their dependency records are fully inserted. This covers both newly
            // created builds and clones of previously-failed builds.
            let created_builds = EBuild::find()
                .filter(CBuild::Evaluation.eq(evaluation.id))
                .filter(CBuild::Status.eq(BuildStatus::Created))
                .all(&state.db)
                .await
                .unwrap_or_default();

            // Persist per-attr evaluation failures as individual evaluation_message rows.
            if !failed_derivations.is_empty() {
                warn!(count = failed_derivations.len(), "Partial evaluation failure — some derivations skipped");
                for (attr, err_msg) in &failed_derivations {
                    record_evaluation_message(
                        &state,
                        evaluation.id,
                        MessageLevel::Error,
                        err_msg.clone(),
                        Some(format!("nix-eval:{}", attr)),
                    )
                    .await;
                }
            }

            // Persist evaluation warnings (e.g. Nix "evaluation warning: …" messages).
            if !eval_warnings.is_empty() {
                for warning in &eval_warnings {
                    record_evaluation_message(
                        &state,
                        evaluation.id,
                        MessageLevel::Warning,
                        warning.clone(),
                        Some("nix-eval".to_string()),
                    )
                    .await;
                }
            }

            if created_builds.is_empty() {
                update_evaluation_status(
                    Arc::clone(&state),
                    evaluation,
                    EvaluationStatus::Completed,
                )
                .await;
                return;
            }

            for build in created_builds {
                update_build_status(Arc::clone(&state), build, BuildStatus::Queued).await;
            }

            info!("Starting evaluation build phase");
            update_evaluation_status(Arc::clone(&state), evaluation, EvaluationStatus::Building)
                .await;
        }

        Err(e) => {
            error!(error = %format!("{:#}", e), "Failed to evaluate");
            // Determine source from the error message prefix set by evaluate().
            let source = {
                let msg = format!("{}", e);
                if msg.contains("prefetch") || msg.contains("fetch") {
                    Some("flake-prefetch".to_string())
                } else {
                    Some("nix-eval".to_string())
                }
            };
            // Report the top-level "gradient" check as Failure — nix eval errored out.
            if let Some(project_id) = evaluation.project {
                report_ci_for_evaluation(
                    Arc::clone(&state),
                    project_id,
                    evaluation.commit,
                    &evaluation.repository,
                    evaluation.id,
                    CiStatus::Failure,
                )
                .await;
            }
            update_evaluation_status_with_error(
                Arc::clone(&state),
                evaluation,
                EvaluationStatus::Failed,
                format!("{}", e),
                source,
            )
            .await;
        }
    }
}

/// Polls for the next project that is due for evaluation, creates the evaluation and commit
/// records, and returns the evaluation to run. Loops until one is found.
async fn get_next_evaluation(state: Arc<ServerState>) -> MEvaluation {
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
                gradient_core::gc::gc_project_evaluations(gc_state, gc_project_id, gc_keep).await
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

/// Fetches the project and commit for the evaluation, then fires one CI status
/// report per entry point using the project's configured reporter.
///
/// Failures are logged and swallowed — CI reporting is best-effort.
pub async fn report_ci_for_entry_points(
    state: Arc<ServerState>,
    project_id: Uuid,
    commit_id: Uuid,
    repository_url: &str,
    evaluation_id: Uuid,
    entry_points: &[(Uuid, String)],
    status: CiStatus,
) {
    if entry_points.is_empty() {
        return;
    }

    let project = match EProject::find_by_id(project_id).one(&state.db).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            warn!(%project_id, "Project not found for CI reporting");
            return;
        }
        Err(e) => {
            error!(error = %e, %project_id, "Failed to query project for CI reporting");
            return;
        }
    };

    let decrypted_token = project.ci_reporter_token.as_deref().and_then(|enc| {
        match decrypt_webhook_secret(&state.cli.crypt_secret_file, enc) {
            Ok(t) => Some(t),
            Err(e) => {
                warn!(error = %e, "Failed to decrypt CI token, skipping CI reporting");
                None
            }
        }
    });

    let reporter = reporter_for_project(
        project.ci_reporter_type.as_deref(),
        project.ci_reporter_url.as_deref(),
        decrypted_token.as_deref(),
    );

    let commit = match ECommit::find_by_id(commit_id).one(&state.db).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            warn!(%commit_id, "Commit not found for CI reporting");
            return;
        }
        Err(e) => {
            error!(error = %e, %commit_id, "Failed to query commit for CI reporting");
            return;
        }
    };

    let sha = gradient_core::input::vec_to_hex(&commit.hash);

    let (owner, repo) = match parse_owner_repo(repository_url) {
        Some(pair) => pair,
        None => {
            warn!(repository_url, "Could not parse owner/repo for CI reporting");
            return;
        }
    };

    let org_name = match EOrganization::find_by_id(project.organization).one(&state.db).await {
        Ok(Some(org)) => Some(org.name),
        _ => None,
    };

    let details_url = org_name.map(|org| {
        format!(
            "{}/organization/{}/log/{}",
            state.cli.frontend_url, org, evaluation_id
        )
    });

    for (_build_id, eval) in entry_points {
        let report = CiReport {
            owner: owner.clone(),
            repo: repo.clone(),
            sha: sha.clone(),
            context: format!("gradient/{}", eval),
            status: status.clone(),
            description: None,
            details_url: details_url.clone(),
        };

        if let Err(e) = reporter.report(&report).await {
            warn!(error = %e, eval, "CI status report failed");
        }
    }
}

/// Reports a single `"gradient"` top-level CI status for the whole evaluation.
///
/// - **Running** when evaluation starts (before nix eval).
/// - **Failure** if nix eval itself fails.
/// - **Success / Failure / Error** when all builds finish (reported from builder).
///
/// Links always point to the evaluation log page.
pub async fn report_ci_for_evaluation(
    state: Arc<ServerState>,
    project_id: Uuid,
    commit_id: Uuid,
    repository_url: &str,
    evaluation_id: Uuid,
    status: CiStatus,
) {
    let project = match EProject::find_by_id(project_id).one(&state.db).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            warn!(%project_id, "Project not found for CI evaluation report");
            return;
        }
        Err(e) => {
            error!(error = %e, %project_id, "Failed to query project for CI evaluation report");
            return;
        }
    };

    let decrypted_token = project.ci_reporter_token.as_deref().and_then(|enc| {
        match decrypt_webhook_secret(&state.cli.crypt_secret_file, enc) {
            Ok(t) => Some(t),
            Err(e) => {
                warn!(error = %e, "Failed to decrypt CI token, skipping CI evaluation report");
                None
            }
        }
    });

    let reporter = reporter_for_project(
        project.ci_reporter_type.as_deref(),
        project.ci_reporter_url.as_deref(),
        decrypted_token.as_deref(),
    );

    let commit = match ECommit::find_by_id(commit_id).one(&state.db).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            warn!(%commit_id, "Commit not found for CI evaluation report");
            return;
        }
        Err(e) => {
            error!(error = %e, %commit_id, "Failed to query commit for CI evaluation report");
            return;
        }
    };

    let sha = gradient_core::input::vec_to_hex(&commit.hash);

    let (owner, repo) = match parse_owner_repo(repository_url) {
        Some(pair) => pair,
        None => {
            warn!(repository_url, "Could not parse owner/repo for CI evaluation report");
            return;
        }
    };

    let org_name = match EOrganization::find_by_id(project.organization).one(&state.db).await {
        Ok(Some(org)) => Some(org.name),
        _ => None,
    };

    let details_url = org_name.map(|org| {
        format!(
            "{}/organization/{}/log/{}",
            state.cli.frontend_url, org, evaluation_id
        )
    });

    let report = CiReport {
        owner,
        repo,
        sha,
        context: "gradient".to_string(),
        status,
        description: None,
        details_url,
    };

    if let Err(e) = reporter.report(&report).await {
        warn!(error = %e, "CI evaluation status report failed");
    }
}
