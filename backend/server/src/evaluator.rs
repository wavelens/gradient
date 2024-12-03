/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only OR WL-1.0
 */

use chrono::Utc;
use entity::build::BuildStatus;
use nix_daemon::nix::DaemonStore;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait, JoinType, QuerySelect};
use serde_json::Value;
use std::sync::Arc;
use tokio::net::UnixStream;
use tokio::process::Command;
use uuid::Uuid;

use super::executer::*;
use super::types::*;
use super::input::{repository_url_to_nix, vec_to_hex};

pub async fn evaluate(
    state: Arc<ServerState>,
    store: &mut DaemonStore<UnixStream>,
    evaluation: &MEvaluation,
) -> Result<(Vec<MBuild>, Vec<MBuildDependency>), String> {
    println!("Evaluating Evaluation: {}", evaluation.id);

    let organization_id = EProject::find_by_id(evaluation.project)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap()
        .organization;

    let commit = ECommit::find_by_id(evaluation.commit)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();

    let repository = repository_url_to_nix(&evaluation.repository, vec_to_hex(&commit.hash).as_str()).unwrap();
    let output = Command::new("nix")
        .arg("flake")
        .arg("show")
        .arg("--json")
        .arg(repository.clone())
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
    let parsed_json: Value =
        serde_json::from_str(&json_output).map_err(|e| format!("Failed to parse JSON: {:?}", e))?;

    if !parsed_json.is_object() {
        return Err("Expected JSON object but found another type".to_string());
    }

    let packages_with_system = parsed_json["packages"]
        .as_object()
        .ok_or("Expected `packages` key in JSON object")?;
    let mut all_builds: Vec<MBuild> = vec![];
    let mut all_dependencies: Vec<MBuildDependency> = vec![];

    for (system_name, packages) in packages_with_system {
        for (package_name, _package) in packages
            .as_object()
            .ok_or("Expected `packages` key in JSON object")?
        {
            let path = format!(
                "{}#packages.{}.{}",
                repository, system_name, package_name
            );

            // TODO: use nix api
            let (derivation, _references) = match get_derivation_cmd(&path).await {
                Ok((d, r)) => (d, r),
                Err(e) => {
                    println!("Error: {}", e);
                    continue;
                }
            };

            let missing = get_missing_builds(vec![derivation.clone()], store).await?;

            if missing.is_empty() {
                println!("Skipping package: {}", derivation);
                continue;
            }

            let already_exsists = all_builds.iter().any(|b| b.derivation_path == derivation);

            let build = vec![derivation.clone()];

            if already_exsists
                || !find_builds(Arc::clone(&state), organization_id, build.clone())
                    .await?
                    .is_empty()
            {
                println!("Skipping package: {}", derivation);
                continue;
            }

            println!("Creating build {} with path {}", derivation, path);

            query_all_dependencies(
                Arc::clone(&state),
                &mut all_builds,
                &mut all_dependencies,
                evaluation,
                organization_id,
                build,
                store,
            )
            .await;

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
    store: &mut DaemonStore<UnixStream>,
) {
    let mut dependencies = dependencies
        .clone()
        .into_iter()
        .map(|d| (d, None, Uuid::new_v4()))
        .collect::<Vec<(String, Option<Uuid>, Uuid)>>();

    while let Some((dependency, dependency_id, build_id)) = dependencies.pop() {
        let path_info = get_derivation(dependency.clone(), store).await.unwrap();
        let references = get_missing_builds(path_info.references.clone(), store)
            .await
            .unwrap();

        let already_exsists = find_builds(Arc::clone(&state), organization_id, references.clone())
            .await
            .unwrap();
        let mut references = references
            .clone()
            .into_iter()
            .map(|d| (d, Some(build_id), Uuid::new_v4()))
            .collect::<Vec<(String, Option<Uuid>, Uuid)>>();

        references.retain(|d| {
            let d_path = d.0.clone();

            let in_builds = all_builds.iter().any(|b| b.derivation_path == d_path);
            let in_exsists = already_exsists.iter().any(|b| b.derivation_path == d_path);
            let in_dependencies = dependencies.iter().any(|(path, _, _)| *path == d_path);

            if in_builds || in_dependencies {
                let d_id = if in_builds {
                    all_builds
                        .iter()
                        .find(|b| b.derivation_path == d_path)
                        .unwrap()
                        .id
                } else {
                    dependencies
                        .iter()
                        .find(|(path, _, _)| *path == d_path)
                        .unwrap()
                        .2
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

        let (system, features) = get_features_cmd(dependency.as_str()).await.unwrap();

        let build = MBuild {
            id: build_id,
            evaluation: evaluation.id,
            derivation_path: dependency.clone(),
            architecture: system,
            status: BuildStatus::Created,
            server: None,
            created_at: Utc::now().naive_utc(),
            updated_at: Utc::now().naive_utc(),
        };

        add_features(Arc::clone(&state), features, Some(build_id), None).await;

        if let Some(d_id) = dependency_id {
            let dep = MBuildDependency {
                id: Uuid::new_v4(),
                build: d_id,
                dependency: build_id,
            };

            all_dependencies.push(dep);
        }

        println!(
            "Creating build {} with path {}",
            build.id, build.derivation_path
        );

        all_builds.push(build);
        dependencies.extend(references);
    }
}

pub async fn add_features(
    state: Arc<ServerState>,
    features: Vec<String>,
    build_id: Option<Uuid>,
    server_id: Option<Uuid>,
) {
    for f in features {
        let feature = EFeature::find()
            .filter(CFeature::Name.eq(f.clone()))
            .one(&state.db)
            .await
            .unwrap();

        let feature = if let Some(f) = feature {
            f
        } else {
            let afeature = AFeature {
                id: Set(Uuid::new_v4()),
                name: Set(f),
            };

            afeature.insert(&state.db).await.unwrap()
        };

        if let Some(b_id) = build_id {
            let abuild_feature = ABuildFeature {
                id: Set(Uuid::new_v4()),
                build: Set(b_id),
                feature: Set(feature.id),
            };

            abuild_feature.insert(&state.db).await.unwrap();
        }

        if let Some(s_id) = server_id {
            let aserver_feature = AServerFeature {
                id: Set(Uuid::new_v4()),
                server: Set(s_id),
                feature: Set(feature.id),
            };

            aserver_feature.insert(&state.db).await.unwrap();
        }
    }
}

async fn find_builds(
    state: Arc<ServerState>,
    organization_id: Uuid,
    build_paths: Vec<String>,
) -> Result<Vec<MBuild>, String> {
    let mut condition = Condition::any();
    for path in build_paths {
        condition = condition.add(CBuild::DerivationPath.eq(path.as_str()));
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
    let parsed_json: Value =
        serde_json::from_str(&json_output).map_err(|e| format!("Failed to parse JSON: {:?}", e))?;

    if !parsed_json.is_object() {
        return Err("Expected JSON object but found another type".to_string());
    }

    let path = parsed_json
        .as_object()
        .ok_or("Expected JSON object")?
        .keys()
        .next()
        .ok_or("Expected JSON object with Derivation Path")?
        .to_string();

    let input_paths = parsed_json[path.clone()]
        .as_object()
        .ok_or("Expected JSON object with Derivation Path")?
        .get("references")
        .ok_or("Expected JSON object with Derivation Path")?
        .as_array()
        .ok_or("Expected JSON object with Derivation Path")?
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();

    Ok((path, input_paths))
}

pub async fn get_features_cmd(
    path: &str,
) -> Result<(entity::server::Architecture, Vec<String>), String> {
    let output = Command::new("nix")
        .arg("derivation")
        .arg("show")
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
    let parsed_json: Value =
        serde_json::from_str(&json_output).map_err(|e| format!("Failed to parse JSON: {:?}", e))?;

    if !parsed_json.is_object() {
        return Err("Expected JSON object but found another type".to_string());
    }

    let parsed_json = parsed_json
        .as_object()
        .ok_or("Expected JSON object")?
        .get(path)
        .ok_or("Expected JSON object with path")?
        .as_object()
        .ok_or("Expected JSON object with path")?
        .get("env")
        .ok_or("Expected JSON object with env")?
        .as_object()
        .ok_or("Expected JSON object with env")?;

    let parsed_json = if let Some(new_json) = parsed_json.get("__json") {
        let new_json = new_json.as_str().unwrap();
        serde_json::from_str(new_json)
            .map_err(|e| format!("Failed to parse JSON: {:?}", e))?
    } else {
        parsed_json.clone()
    };

    let features: Vec<String> = if let Some(pjson) = parsed_json
        .get("requiredSystemFeatures")
    {
        pjson
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    } else {
        vec![]
    };

    let system: entity::server::Architecture = parsed_json
        .get("system")
        .ok_or("Expected JSON object with system")?
        .as_str()
        .unwrap()
        .try_into()
        .unwrap();

    Ok((system, features))
}
