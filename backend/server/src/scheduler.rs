use std::time::Duration;
use sea_orm::{IntoActiveModel, QueryOrder};
use tokio::time;
use tokio::task::JoinHandle;
use std::sync::Arc;
use chrono::Utc;
use sea_orm::ActiveValue::Set;
use sea_orm::{EntityTrait, ActiveModelTrait, ColumnTrait, QueryFilter};
use entity::build::BuildStatus;

use super::input;
use super::types::*;
use super::executer::*;

pub async fn schedule_project_loop(db: DBConn) {
    let mut current_schedules = vec![];
    let mut interval = time::interval(Duration::from_secs(5));

    loop {
        let mut added_schedule = false;
        current_schedules.retain(|schedule: &JoinHandle<()>| !schedule.is_finished());

        // TODO: look at tokio semaphore
        while current_schedules.len() < 1 {
            let project = get_next_project(Arc::clone(&db)).await;
            let schedule = tokio::spawn(schedule_project(Arc::clone(&db), project));
            current_schedules.push(schedule);
            added_schedule = true;
        }

        if !added_schedule {
            interval.tick().await;
        }
    }
}

pub async fn schedule_project(db: DBConn, project: MProject) {
    println!("Reviewing Project: {}", project.name);

    let has_update = check_project_updates(Arc::clone(&db), &project).await;
    if !has_update { return; }

    let can_evaluate = evaluate_project(Arc::clone(&db), &project).await;
    // TODO: Register the project can't be evaluated
    if !can_evaluate { return; }

    let builds = get_project_rebuilds(Arc::clone(&db), &project).await;

    for build in builds {
        register_build(Arc::clone(&db), build).await;
    }
}

pub async fn schedule_build_loop(db: DBConn) {
    let mut current_schedules = vec![];
    let mut interval = time::interval(Duration::from_secs(5));

    loop {
        let mut added_schedule = false;
        current_schedules.retain(|schedule: &JoinHandle<()>| !schedule.is_finished());

        while current_schedules.len() < 1 {
            let build = get_next_build(Arc::clone(&db)).await;

            if let Some(server) = get_available_server(Arc::clone(&db), &build).await {
                let schedule = tokio::spawn(schedule_build(Arc::clone(&db), build, server));
                current_schedules.push(schedule);
                added_schedule = true;
            } else {
                requeue_build(Arc::clone(&db), build).await;
            }
        }

        if !added_schedule {
            interval.tick().await;
        }
    }
}

pub async fn schedule_build(db: DBConn, build: MBuild, server: MServer) {
    println!("Executing Build: {}", build.id);

    let server_addr = input::url_to_addr(server.host.as_str(), server.port).unwrap();

    let mut local_daemon = get_local_store().await;
    let mut server_daemon = connect(server_addr).await.unwrap();

    println!("Connected to server: {}", server.id);

    // TODO: Change this to Uuid to build
    // https://docs.rs/nix-daemon/latest/nix_daemon/trait.Store.html#tymethod.query_missing
    let deps = get_next_build(db).await; // dummy
    let dependencies = vec![&deps];

    // TODO: somewhere else
    // let missing_dependencies = get_missing_builds(&build, &mut server_daemon).await.unwrap();

    copy_builds(dependencies, &mut local_daemon, &mut server_daemon, server_addr, false).await;

    execute_build(vec![&build], &mut server_daemon).await;

    copy_builds(vec![&build], &mut server_daemon, &mut local_daemon, server_addr, true).await;
}

pub async fn get_next_project(db: DBConn) -> MProject {
    loop {
        match EProject::find()
            .filter(CProject::LastCheckAt.lte(Utc::now().naive_utc() - chrono::Duration::seconds(5)))
            .filter(CProject::CurrentlyChecking.eq(false))
            .order_by_asc(CProject::LastCheckAt)
            .one(&*db)
            .await
        {
            Ok(Some(project)) => {
                let mut active_project: AProject = project.clone().into();

                active_project.last_check_at = Set(Utc::now().naive_utc());
                active_project.currently_checking = Set(true);

                match active_project.update(&*db).await {
                    Ok(updated_project) => {
                        println!("Getting next project: {}", updated_project.name);
                        return updated_project;
                    }

                    Err(e) => {
                        eprintln!("Failed to update project status: {:?}", e);
                        time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }

            Ok(None) => {
                time::sleep(Duration::from_secs(5)).await;
            }

            Err(e) => {
                eprintln!("Database query error: {:?}", e);
                time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

pub async fn check_project_updates(db: DBConn, project: &MProject) -> bool {
    println!("Checking for updates on project: {}", project.name);
    // TODO: dummy
    true
}

pub async fn evaluate_project(db: DBConn, project: &MProject) -> bool {
    // dummy
    println!("Evaluating project: {}", project.name);
    true
}

pub async fn get_project_rebuilds(db: DBConn, project: &MProject) -> Vec<MBuild> {
    // dummy
    EBuild::find()
        .filter(CBuild::Project.eq(project.id))
        .filter(CBuild::Status.eq(BuildStatus::Queued))
        .all(&*db).await.unwrap().into_iter().collect()
}

pub async fn register_build(db: DBConn, build: MBuild) -> MBuild {
    // dummy
    let build = build.into_active_model();
    let build = build.insert(&*db).await.unwrap();

    println!("Registering build: {}", build.id);

    build
}

pub async fn requeue_build(db: DBConn, build: MBuild) -> MBuild {
    // dummy
    let mut build: ABuild = EBuild::find_by_id(build.id).one(&*db).await.unwrap().unwrap().into();

    build.status = Set(BuildStatus::Queued);
    let build = build.update(&*db).await.unwrap();

    println!("Requeueing build: {}", build.id);
    build
}

pub async fn get_next_build(db: DBConn) -> MBuild {
    loop {
        match EBuild::find()
            .filter(CBuild::Status.eq(BuildStatus::Queued))
            .order_by_asc(CBuild::CreatedAt)
            .one(&*db)
            .await
        {
            Ok(Some(build)) => {
                let mut active_build: ABuild = build.clone().into();

                active_build.status = Set(BuildStatus::Evaluating);

                match active_build.update(&*db).await {
                    Ok(updated_build) => {
                        println!("Getting next build: {}", updated_build.id);
                        return updated_build;
                    }

                    Err(e) => {
                        eprintln!("Failed to update build status: {:?}", e);
                        time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }

            Ok(None) => {
                time::sleep(Duration::from_secs(5)).await;
            }

            Err(e) => {
                eprintln!("Database query error: {:?}", e);
                time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

pub async fn get_available_server(db: DBConn, build: &MBuild) -> Option<MServer> {
    // dummy
    EServer::find().all(&*db).await.unwrap().into_iter().next()
}
