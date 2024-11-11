use std::sync::Arc;
use super::types::*;

pub async fn check_project_updates(state: Arc<ServerState>, project: &MProject) -> bool {
    println!("Checking for updates on project: {}", project.id);
    // TODO: dummy
    true
}
