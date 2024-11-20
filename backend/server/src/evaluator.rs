use std::sync::Arc;
use uuid::Uuid;
use tokio::process::Command;
use serde_json::Value;
use nix_daemon::{nix::DaemonStore, Progress, Store, PathInfo};
use tokio::net::UnixStream;
use entity::build::BuildStatus;
use sea_orm::{EntityTrait, ColumnTrait, QuerySelect, JoinType, Condition};
use sea_orm::entity::prelude::*;
use chrono::Utc;

use super::types::*;


pub async fn evaluate(state: Arc<ServerState>, store: &mut DaemonStore<UnixStream>, evaluation: &MEvaluation) -> Result<(Vec<MBuild>, Vec<MBuildDependency>), String> {
    println!("Evaluating Evaluation: {}", evaluation.id);

    let organization_id = EProject::find_by_id(evaluation.project)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap()
        .organization;

    let output = Command::new("nix")
        .arg("flake")
        .arg("show")
        .arg("--json")
        .arg(&evaluation.repository)
        .output()
        .await
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        return Err(format!(
            "Command failed with status: {:?}, stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let json_output = String::from_utf8_lossy(&output.stdout);
    let parsed_json: Value = serde_json::from_str(&json_output)
        .map_err(|e| format!("Failed to parse JSON: {:?}", e))?;

    if !parsed_json.is_object() {
        return Err("Expected JSON object but found another type".to_string());
    }

    let packages_with_system = parsed_json["packages"].as_object().ok_or("Expected `packages` key in JSON object")?;
    let mut all_builds: Vec<MBuild> = vec![];
    let mut all_dependencies: Vec<MBuildDependency> = vec![];

    for (system_name, packages) in packages_with_system {
        for (package_name, _package) in packages.as_object().ok_or("Expected `packages` key in JSON object")? {
            let path = format!("{}#packages.{}.{}", evaluation.repository, system_name, package_name);
            // TODO: use nix api
            let (derivation, _references) = get_derivation_cmd(&path).await?;

            let already_exsists = all_builds.iter().any(|b| b.path == derivation);

            let build = vec![derivation.clone()];

            if already_exsists || !find_builds(Arc::clone(&state), organization_id, build.clone()).await?.is_empty() {
                println!("Skipping package: {}", derivation);
                continue;
            }

            println!("Creating build {} with path {}", derivation, path);

            query_all_dependencies(Arc::clone(&state), &mut all_builds, &mut all_dependencies, evaluation, organization_id, build, store).await;

            println!("Found package: {}", derivation);
        }
    }

    Ok((all_builds, all_dependencies))
}

async fn query_all_dependencies(
    state: Arc<ServerState>,
    all_builds: &mut Vec<MBuild>,
    all_dependencies: &mut Vec<MBuildDependency>,
    evaluation: &MEvaluation,
    organization_id: Uuid,
    dependencies: Vec<String>,
    store: &mut DaemonStore<UnixStream>
) {
    let mut dependencies = dependencies.clone().into_iter()
        .map(|d| (d, None, Uuid::new_v4()))
        .collect::<Vec<(String, Option<Uuid>, Uuid)>>();

    while let Some((dependency, dependency_id, build_id)) = dependencies.pop() {
        let path_info = get_derivation(store, dependency.as_str()).await.unwrap();

        let already_exsists = find_builds(Arc::clone(&state), organization_id, path_info.references.clone()).await.unwrap();
        let mut references = path_info.references.clone().into_iter()
            .map(|d| (d, Some(build_id), Uuid::new_v4()))
            .collect::<Vec<(String, Option<Uuid>, Uuid)>>();

        references.retain(|d| {
            let d_path = d.0.clone();

            let in_builds = all_builds.iter().any(|b| b.path == d_path);
            let in_exsists = already_exsists.iter().any(|b| b.path == d_path);
            let in_dependencies = dependencies.iter().any(|(path, _, _)| *path == d_path);

            if  in_builds || in_dependencies {
                let d_id = if in_builds {
                    all_builds.iter().find(|b| b.path == d_path).unwrap().id
                } else {
                    dependencies.iter().find(|(path, _, _)| *path == d_path).unwrap().2
                };

                let dep = MBuildDependency {
                    id: Uuid::new_v4(),
                    build: d_id,
                    dependency: build_id,
                };

                all_dependencies.push(dep);

                false
            } else {
                !in_exsists
            }
        });

        let build = MBuild {
            id: build_id,
            evaluation: evaluation.id,
            path: dependency.clone(),
            status: BuildStatus::Created,
            created_at: Utc::now().naive_utc(),
        };

        if let Some(d_id) = dependency_id {
            let dep = MBuildDependency {
                id: Uuid::new_v4(),
                build: d_id,
                dependency: build_id,
            };

            all_dependencies.push(dep);
        }

        println!("Creating build {} with path {}", build.id, build.path);

        all_builds.push(build);
        dependencies.extend(references);
    }
}

async fn find_builds(state: Arc<ServerState>, organization_id: Uuid, build_paths: Vec<String>) -> Result<Vec<MBuild>, String> {
    let mut condition = Condition::any();
    for path in build_paths {
        condition = condition.add(CBuild::Path.eq(path.as_str()));
    }

    let builds = EBuild::find()
        .join(JoinType::InnerJoin, RBuild::Evaluation.def())
        .join(JoinType::InnerJoin, REvaluation::Project.def())
        .filter(CProject::Organization.eq(organization_id))
        .filter(condition)
        .all(&state.db)
        .await
        .map_err(|e| e.to_string())?;

    Ok(builds)
}

pub async fn get_derivation_cmd(path: &str) -> Result<(String, Vec<String>), String> {
    let output = Command::new("nix")
        .arg("path-info")
        .arg("--json")
        .arg("--derivation")
        .arg(path)
        .output()
        .await
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        return Err(format!(
            "Command failed with status: {:?}, stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let json_output = String::from_utf8_lossy(&output.stdout);
    let parsed_json: Value = serde_json::from_str(&json_output)
        .map_err(|e| format!("Failed to parse JSON: {:?}", e))?;

    if !parsed_json.is_object() {
        return Err("Expected JSON object but found another type".to_string());
    }

    let path = parsed_json.as_object().ok_or("Expected JSON object")?.keys().next().ok_or("Expected JSON object with Derivation Path")?.to_string();
    let input_paths = parsed_json[path.clone()].as_object().ok_or("Expected JSON object with Derivation Path")?.keys().map(|k| k.to_string()).collect();

    Ok((path, input_paths))
}

pub async fn get_derivation(store: &mut DaemonStore<UnixStream>, path: &str) -> Result<PathInfo, String> {
    Ok(store.query_pathinfo(path).result().await.map_err(|e| e.to_string())?.unwrap())
}
