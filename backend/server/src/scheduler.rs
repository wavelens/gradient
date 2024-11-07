use std::time::Duration;
use uuid::Uuid;
use tokio::time;
use tokio::task::JoinHandle;
use std::sync::Arc;

use super::input;
use super::tables::*;
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

pub async fn schedule_project(db: DBConn, project: Project) {
    println!("Reviewing Project: {}", project.name);

    let has_update = check_project_updates(Arc::clone(&db), &project);
    if !has_update { return; }

    let can_evaluate = evaluate_project(Arc::clone(&db), &project);
    // TODO: Register the project can't be evaluated
    if !can_evaluate { return; }

    let builds = get_project_rebuilds(Arc::clone(&db), &project);

    for build in builds {
        register_build(Arc::clone(&db), &build);
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

            if let Some(server) = get_available_server(Arc::clone(&db), &build) {
                let schedule = tokio::spawn(schedule_build(Arc::clone(&db), build, server));
                current_schedules.push(schedule);
                added_schedule = true;
            } else {
                requeue_build(Arc::clone(&db), &build);
            }
        }

        if !added_schedule {
            interval.tick().await;
        }
    }
}

pub async fn schedule_build(db: DBConn, build: Build, server: Server) {
    println!("Executing Build: {}", build.id);

    let server_addr = input::url_to_addr(server.url.as_str()).unwrap();

    let mut local_daemon = get_local_store().await;
    let mut server_daemon = connect(server_addr).await.unwrap();

    println!("Connected to server: {}", server.url);

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

pub async fn get_next_project(db: DBConn) -> Project {
    // TODO: sort after most dependencies
    // dummy
    Project {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        name: "Project Title".to_string(),
        description: "Project Description".to_string(),
        last_check_at: 0,
        created_by: Uuid::nil(),
        created_at: 0,
    }
}

pub fn check_project_updates(db: DBConn, project: &Project) -> bool {
    println!("Checking for updates on project: {}", project.name);
    // TODO: dummy
    true
}

pub fn evaluate_project(db: DBConn, project: &Project) -> bool {
    // dummy
    println!("Evaluating project: {}", project.name);
    true
}

pub fn get_project_rebuilds(db: DBConn, project: &Project) -> Vec<Build> {
    // dummy
    vec![
        Build {
            id: Uuid::new_v4(),
            project_id: project.id,
            path: "/nix/store/l2kvr9vx55cc469r3ncg04jf425alh3p-tig-2.5.10".to_string(),
            dependencies: vec![],
            created_at: 0,
        }
    ]
}

pub fn register_build(db: DBConn, build: &Build) {
    // dummy
    println!("Registering build: {}", build.id);
}

pub fn requeue_build(db: DBConn, build: &Build) {
    // dummy
    println!("Requeueing build: {}", build.id);
}

pub async fn get_next_build(db: DBConn) -> Build {
    // dummy
    Build {
        id: Uuid::new_v4(),
        project_id: Uuid::nil(),
        path: "/nix/store/l2kvr9vx55cc469r3ncg04jf425alh3p-tig-2.5.10".to_string(),
        dependencies: vec![],
        created_at: 0,
    }
}

pub fn get_available_server(db: DBConn, build: &Build) -> Option<Server> {
    // dummy
    Some(Server {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        url: "127.0.0.1:22".to_string(),
        connected: true,
        last_connection_at: 0,
        created_at: 0,
    })
}
