use std::time::Duration;
use uuid::Uuid;
use tokio::time;
use tokio::task::JoinHandle;

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
            let project = get_next_project().await;
            let schedule = tokio::spawn(schedule_project(project));
            current_schedules.push(schedule);
            added_schedule = true;
        }

        if !added_schedule {
            interval.tick().await;
        }
    }
}

pub async fn schedule_project(project: Project) {
    println!("Reviewing Project: {}", project.name);

    let has_update = check_project_updates(&project);
    if !has_update { return; }

    let can_evaluate = evaluate_project(&project);
    // TODO: Register the project can't be evaluated
    if !can_evaluate { return; }

    let builds = get_project_rebuilds(&project);

    for build in builds {
        register_build(&build);
    }
}

pub async fn schedule_build_loop(db: DBConn) {
    let mut current_schedules = vec![];
    let mut interval = time::interval(Duration::from_secs(5));

    loop {
        let mut added_schedule = false;
        current_schedules.retain(|schedule: &JoinHandle<()>| !schedule.is_finished());

        while current_schedules.len() < 1 {
            let build = get_next_build().await;

            if let Some(server) = get_available_server(&build) {
                let schedule = tokio::spawn(schedule_build(build, server));
                current_schedules.push(schedule);
                added_schedule = true;
            } else {
                // TODO: More elegant way to handle this
                register_build(&build);
            }
        }

        if !added_schedule {
            interval.tick().await;
        }
    }
}

pub async fn schedule_build(build: Build, server: Server) {
    println!("Executing Build: {}", build.id);

    let server_addr = input::url_to_addr(server.url.as_str()).unwrap();

    let mut local_daemon = get_local_store().await;
    let mut server_daemon = connect(server_addr).await.unwrap();

    println!("Connected to server: {}", server.url);

    // TODO: Change this to Uuid to build
    // https://docs.rs/nix-daemon/latest/nix_daemon/trait.Store.html#tymethod.query_missing
    let deps = get_next_build().await; // dummy
    let dependencies = vec![&deps];

    // TODO: somewhere else
    // let missing_dependencies = get_missing_builds(&build, &mut server_daemon).await.unwrap();

    copy_builds(dependencies, &mut local_daemon, &mut server_daemon, server_addr, false).await;

    execute_build(vec![&build], &mut server_daemon).await;

    copy_builds(vec![&build], &mut server_daemon, &mut local_daemon, server_addr, true).await;
}

pub async fn get_next_project() -> Project {
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

pub fn check_project_updates(project: &Project) -> bool {
    // dummy
    println!("Checking for updates on project: {}", project.name);
    true
}

pub fn evaluate_project(project: &Project) -> bool {
    // dummy
    println!("Evaluating project: {}", project.name);
    true
}

pub fn get_project_rebuilds(project: &Project) -> Vec<Build> {
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

pub fn register_build(build: &Build) {
    // dummy
    println!("Registering build: {}", build.id);
}

pub async fn get_next_build() -> Build {
    // dummy
    Build {
        id: Uuid::new_v4(),
        project_id: Uuid::nil(),
        path: "/nix/store/l2kvr9vx55cc469r3ncg04jf425alh3p-tig-2.5.10".to_string(),
        dependencies: vec![],
        created_at: 0,
    }
}

pub fn get_available_server(build: &Build) -> Option<Server> {
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
