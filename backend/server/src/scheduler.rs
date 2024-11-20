use uuid::Uuid;
use std::time::Duration;
use tokio::time;
use tokio::task::JoinHandle;
use std::sync::Arc;
use chrono::Utc;
use sea_orm::ActiveValue::Set;
use sea_orm::{EntityTrait, IntoActiveModel, ActiveModelTrait, QuerySelect, QueryFilter, ColumnTrait, RelationTrait, QueryOrder, JoinType, Condition};
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use futures::stream::{self, StreamExt};

use super::input;
use super::types::*;
use super::executer::*;
use super::sources::*;
use super::evaluator::*;

pub async fn schedule_evaluation_loop(state: Arc<ServerState>) {
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

pub async fn schedule_evaluation(state: Arc<ServerState>, evaluation: MEvaluation) {
    println!("Reviewing Evaluation: {}", evaluation.id);

    let mut store = get_local_store().await;
    let builds = evaluate(Arc::clone(&state), &mut store, &evaluation).await;

    match builds {
        Ok(builds) => {
            let (builds, dependencies) = builds;
            let active_builds = builds
                .iter()
                .map(|b| b.clone().into_active_model())
                .collect::<Vec<ABuild>>();
            let active_dependencies = dependencies
                .iter()
                .map(|d| d.clone().into_active_model())
                .collect::<Vec<ABuildDependency>>();

            EBuild::insert_many(active_builds)
                .exec(&state.db)
                .await
                .unwrap();

            EBuildDependency::insert_many(active_dependencies)
                .exec(&state.db)
                .await
                .unwrap();

            for build in builds {
                update_build_status(Arc::clone(&state), build, BuildStatus::Queued).await;
            }

            update_evaluation_status(Arc::clone(&state), evaluation, EvaluationStatus::Building).await;
        }

        Err(e) => {
            update_evaluation_status(Arc::clone(&state), evaluation, EvaluationStatus::Failed).await;
            eprintln!("Failed to evaluate: {}", e);
        }
    }

}

pub async fn schedule_build_loop(state: Arc<ServerState>) {
    let mut current_schedules = vec![];
    let mut interval = time::interval(Duration::from_secs(5));

    loop {
        let mut added_schedule = false;
        current_schedules.retain(|schedule: &JoinHandle<()>| !schedule.is_finished());

        while current_schedules.len() < state.cli.max_concurrent_builds {
            let build = get_next_build(Arc::clone(&state)).await;

            if let Some(server) = get_available_server(Arc::clone(&state), &build).await {
                let schedule = tokio::spawn(schedule_build(Arc::clone(&state), build, server));
                current_schedules.push(schedule);
                added_schedule = true;
            } else {
                requeue_build(Arc::clone(&state), build).await;
            }
        }

        if !added_schedule {
            interval.tick().await;
        }
    }
}

pub async fn schedule_build(state: Arc<ServerState>, build: MBuild, server: MServer) {
    println!("Executing Build: {}", build.id);

    let server_addr = input::url_to_addr(server.host.as_str(), server.port).unwrap();

    let mut local_daemon = get_local_store().await;

    let mut server_deamon = connect(server_addr, None).await;
    for _ in 1..3 {
        if server_deamon.is_ok() {
            break;
        };

        time::sleep(Duration::from_secs(5)).await;
        server_deamon = connect(server_addr, None).await;
    };

    let mut server_daemon = if let Ok(daemon) = server_deamon {
        daemon
    } else {
        eprintln!("Failed to connect to server: {}", server.id);
        requeue_build(state, build.clone()).await;
        return;
    };

    println!("Connected to server: {}", server.id);

    // TODO: somewhere else
    // let missing_dependencies = get_missing_builds(&build, &mut server_daemon).await.unwrap();

    // copy_builds(dependencies, &mut local_daemon, &mut server_daemon, server_addr, false).await;

    let result = execute_build(vec![&build], &mut server_daemon).await;

    match result {
        Ok(results) => {
            let status = if results.values().all(|r| r.error_msg.is_empty()) {
                BuildStatus::Completed
            } else {
                BuildStatus::Failed
            };

            update_build_status(Arc::clone(&state), build.clone(), status).await;
            check_evaluation_status(Arc::clone(&state), build.evaluation).await;
        }

        Err(e) => {
            eprintln!("Failed to execute build: {}", e);
            update_build_status_recursivly(state, build.clone(), BuildStatus::Failed).await;
        }
    };

    copy_builds(vec![&build], &mut server_daemon, &mut local_daemon, server_addr, true).await;
}

async fn get_next_evaluation(state: Arc<ServerState>) -> MEvaluation {
    loop {
        let threshold_time = Utc::now().naive_utc() - chrono::Duration::seconds(state.cli.evaluation_timeout);

        let mut projects = EProject::find()
            .join(JoinType::InnerJoin, RProject::LastEvaluation.def())
            .filter(Condition::all()
                .add(CProject::LastCheckAt.lte(threshold_time))
                .add(CEvaluation::Status.eq(EvaluationStatus::Completed))
            )
            .order_by_asc(CProject::LastCheckAt)
            .all(&state.db)
            .await
            .unwrap();

        projects.extend(EProject::find()
            .filter(Condition::all()
                .add(CProject::LastCheckAt.lte(threshold_time))
                .add(CProject::LastEvaluation.is_null())
            )
            .order_by_asc(CProject::LastCheckAt)
            .all(&state.db)
            .await
            .unwrap());

        let evaluations = stream::iter(projects.clone().into_iter())
            .filter_map(|p| {
                let intern_state = Arc::clone(&state);
                async move {
                    if !check_project_updates(Arc::clone(&intern_state), &p).await {
                        return None;
                    }

                    if let Some(evaluation) = p.last_evaluation {
                        EEvaluation::find_by_id(evaluation)
                            .filter(
                                Condition::any()
                                    .add(CEvaluation::Status.eq(EvaluationStatus::Completed))
                                    .add(CEvaluation::Status.eq(EvaluationStatus::Failed))
                                    .add(CEvaluation::Status.eq(EvaluationStatus::Aborted))
                            )
                            .one(&intern_state.db).await.unwrap_or(None)
                    } else {
                        Some(MEvaluation {
                            id: Uuid::nil(),
                            project: p.id,
                            repository: p.repository,
                            commit: "HEAD".to_string(),
                            status: EvaluationStatus::Queued,
                            previous: None,
                            next: None,
                            created_at: Utc::now().naive_utc(),
                        })
                    }
                }
            })
            .collect::<Vec<MEvaluation>>()
            .await;

        if evaluations.is_empty() {
            time::sleep(Duration::from_secs(5)).await;
            continue;
        }

        let evaluation = evaluations.first().unwrap();


        let project = projects.into_iter().find(|p| p.id == evaluation.project).unwrap_or_else(|| {
            eprintln!("Failed to find project {} for evaluation {}", evaluation.project, evaluation.id);
            std::process::exit(1);
        });

        let evaluation_id = if evaluation.id == Uuid::nil() {
            None
        } else {
            Some(evaluation.id)
        };

        let new_evaluation = AEvaluation {
            id: Set(Uuid::new_v4()),
            project: Set(project.id),
            repository: Set(project.repository.clone()),
            commit: Set("HEAD".to_string()),
            status: Set(EvaluationStatus::Queued),
            previous: Set(evaluation_id),
            next: Set(None),
            created_at: Set(Utc::now().naive_utc()),
        };

        let new_evaluation = new_evaluation.insert(&state.db).await.unwrap();
        println!("Created evaluation: {}", new_evaluation.id);

        let mut active_project: AProject = project.clone().into();

        active_project.last_check_at = Set(Utc::now().naive_utc());
        active_project.last_evaluation = Set(Some(evaluation.id));

        active_project.update(&state.db).await.unwrap();

        if evaluation.id != Uuid::nil() {
            let mut active_evaluation: AEvaluation = evaluation.clone().into();
            active_evaluation.next = Set(Some(new_evaluation.id));

            active_evaluation.update(&state.db).await.unwrap();
        };

        return new_evaluation;
    }
}

async fn requeue_build(state: Arc<ServerState>, build: MBuild) -> MBuild {
    // dummy
    let mut build: ABuild = EBuild::find_by_id(build.id).one(&state.db).await.unwrap().unwrap().into();

    build.status = Set(BuildStatus::Queued);
    let build = build.update(&state.db).await.unwrap();

    println!("Requeueing build: {}", build.id);
    build
}

async fn get_next_build(state: Arc<ServerState>) -> MBuild {
    loop {
        let builds_sql = sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            r#"
                SELECT * FROM public.build b
                WHERE NOT EXISTS (
                    SELECT 1
                    FROM public.build_dependency d
                    JOIN public.build dep ON d.dependency = dep.id
                    WHERE d.build = b.id AND dep.status = 3
                )
                AND b.status = 1;
            "#,
        );

        let build = EBuild::find()
            .from_raw_sql(builds_sql)
            .one(&state.db)
            .await
            .unwrap();

        if let Some(build) = build {
            let mut active_build: ABuild = build.clone().into();

            active_build.status = Set(BuildStatus::Building);

            match active_build.update(&state.db).await {
                Ok(updated_build) => {
                    println!("Getting next build: {}", updated_build.id);
                    return updated_build;
                }

                Err(e) => {
                    eprintln!("Failed to update build status: {:?}", e);
                }
            }
        } else {
            time::sleep(Duration::from_secs(5)).await;
        }
    }
}

async fn get_available_server(state: Arc<ServerState>, build: &MBuild) -> Option<MServer> {
    EServer::find().all(&state.db).await.unwrap().into_iter().next()
}

async fn update_build_status(state: Arc<ServerState>, build: MBuild, status: BuildStatus) -> MBuild {
    let mut active_build: ABuild = build.into_active_model();
    active_build.status = Set(status);

    let build = active_build.update(&state.db).await.unwrap();

    println!("Updated build status: {}", build.id);

    build
}

async fn update_build_status_recursivly(state: Arc<ServerState>, build: MBuild, status: BuildStatus) -> MBuild {
    // TODO: more efficient, recursive till all dependencies are updated
    let dependencies = EBuildDependency::find()
        .filter(CBuildDependency::Dependency.eq(build.id))
        .all(&state.db)
        .await
        .unwrap()
        .into_iter()
        .map(|d| d.build)
        .collect::<Vec<Uuid>>();

    let mut condition = Condition::any();

    for dependency in dependencies {
        condition = condition.add(CBuild::Id.eq(dependency));
    }

    let dependent_builds = EBuild::find()
        .filter(condition)
        .filter(CBuild::Status.ne(status.clone()))
        .all(&state.db)
        .await
        .unwrap();

    for dependent_build in dependent_builds {
        update_build_status(Arc::clone(&state), dependent_build, status.clone()).await;
    }

    update_build_status(Arc::clone(&state), build, status.clone()).await
}

async fn update_evaluation_status(state: Arc<ServerState>, evaluation: MEvaluation, status: EvaluationStatus) -> MEvaluation {
    let mut active_evaluation: AEvaluation = evaluation.into_active_model();
    active_evaluation.status = Set(status);

    active_evaluation.update(&state.db).await.unwrap()
}

async fn check_evaluation_status(state: Arc<ServerState>, evaluation_id: Uuid) {
    let evaluation = EEvaluation::find_by_id(evaluation_id)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();

    let builds = EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation_id))
        .all(&state.db)
        .await
        .unwrap();

    let statuses = builds.into_iter().map(|b| b.status).collect::<Vec<BuildStatus>>();

    let status = if statuses.iter().all(|s| *s == BuildStatus::Completed) {
        EvaluationStatus::Completed
    } else if statuses.iter().any(|s| *s == BuildStatus::Failed) {
        EvaluationStatus::Failed
    } else if statuses.iter().any(|s| *s == BuildStatus::Aborted) {
        EvaluationStatus::Aborted
    } else {
        return;
    };

    update_evaluation_status(state, evaluation, status).await;
}
