/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeSet;
use std::fmt;

use gradient_ci::actions::decrypt_secret_with_file;
use gradient_pool::{DeviceCount, WorkerInfo};
use gradient_sops::{SopsError, SopsKey, decrypt_file};
use gradient_types::exec::{ExecRequirement, ExecSpec};
use gradient_types::{EProject, ProjectId, RuntimeConfig};
use sea_orm::{ConnectionTrait, EntityTrait};
use zeroize::Zeroizing;

use crate::loops::caps_of;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResolvedSecretNames {
    pub server: Vec<ServerSecret>,
    pub worker: Vec<String>,
}

impl ResolvedSecretNames {
    pub fn server_names(&self) -> Vec<String> {
        self.server.iter().map(|s| s.name.clone()).collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ServerSecret {
    pub name: String,
    pub source: ServerSecretSource,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ServerSecretSource {
    Project,
    RepoSops { file: String },
}

impl fmt::Display for ServerSecretSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Project => f.write_str("project secrets"),
            Self::RepoSops { file } => write!(f, "sops file {file}"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SecretResolutionError {
    #[error("secret \"{name}\" found in two sources: {first}, {second}")]
    TwoSources {
        name: String,
        first: String,
        second: String,
    },
    #[error(
        "secret \"{name}\" not found in project secrets, repository sops files or worker sops files"
    )]
    NotFound { name: String },
    #[error(transparent)]
    Sops(#[from] SopsError),
    #[error("project key unusable for sops: {0}")]
    ProjectKey(String),
    #[error("project secret \"{name}\" unreadable: {reason}")]
    Unreadable { name: String, reason: String },
    #[error("reading project secrets failed: {0}")]
    Database(#[from] sea_orm::DbErr),
}

pub fn resolve_sources(
    listed: &[String],
    project: &BTreeSet<String>,
    repo: &[(String, BTreeSet<String>)],
) -> Result<ResolvedSecretNames, SecretResolutionError> {
    let mut resolved = ResolvedSecretNames::default();
    for name in listed {
        let mut sources = project
            .contains(name)
            .then_some(ServerSecretSource::Project)
            .into_iter()
            .chain(
                repo.iter()
                    .filter(|(_, names)| names.contains(name))
                    .map(|(file, _)| ServerSecretSource::RepoSops { file: file.clone() }),
            );
        match (sources.next(), sources.next()) {
            (Some(first), Some(second)) => {
                return Err(SecretResolutionError::TwoSources {
                    name: name.clone(),
                    first: first.to_string(),
                    second: second.to_string(),
                });
            }
            (Some(source), None) => resolved.server.push(ServerSecret {
                name: name.clone(),
                source,
            }),
            (None, _) => resolved.worker.push(name.clone()),
        }
    }

    Ok(resolved)
}

pub async fn resolve_secret_names(
    db: &impl ConnectionTrait,
    config: &RuntimeConfig,
    project: ProjectId,
    spec: &ExecSpec,
) -> Result<ResolvedSecretNames, SecretResolutionError> {
    if spec.secrets.is_empty() {
        return Ok(ResolvedSecretNames::default());
    }

    let stored = gradient_db::project_secrets::names(db, project).await?;
    let mut repo = Vec::with_capacity(spec.sops.len());
    if !spec.sops.is_empty() {
        let key = project_sops_key(db, config, project).await?;
        for file in &spec.sops {
            let secrets = decrypt_file(&file.path, &file.ciphertext, &key)?;
            repo.push((
                file.path.clone(),
                secrets.names().map(str::to_owned).collect(),
            ));
        }
    }

    resolve_sources(&spec.secrets, &stored, &repo)
}

pub async fn server_secret_values(
    db: &impl ConnectionTrait,
    config: &RuntimeConfig,
    project: ProjectId,
    spec: &ExecSpec,
    names: &[String],
) -> Result<Vec<(String, Zeroizing<Vec<u8>>)>, SecretResolutionError> {
    let mut values = Vec::with_capacity(names.len());
    let mut missing = Vec::new();
    for name in names {
        match gradient_db::project_secrets::one(db, project, name).await? {
            Some(row) => {
                let plain = decrypt_secret_with_file(&config.secrets.crypt_file, &row.value)
                    .map_err(|e| SecretResolutionError::Unreadable {
                        name: name.clone(),
                        reason: e.to_string(),
                    })?;
                values.push((
                    name.clone(),
                    Zeroizing::new(plain.expose().as_bytes().to_vec()),
                ));
            }
            None => missing.push(name.clone()),
        }
    }
    if missing.is_empty() {
        return Ok(values);
    }

    let key = project_sops_key(db, config, project).await?;
    for file in &spec.sops {
        let secrets = decrypt_file(&file.path, &file.ciphertext, &key)?;
        missing.retain(|name| match secrets.get(name) {
            Some(value) => {
                values.push((name.clone(), Zeroizing::new(value.to_vec())));
                false
            }
            None => true,
        });
    }
    if let Some(name) = missing.into_iter().next() {
        return Err(SecretResolutionError::NotFound { name });
    }

    let order = |name: &String| names.iter().position(|n| n == name);
    values.sort_by_key(|(name, _)| order(name));
    Ok(values)
}

async fn project_sops_key(
    db: &impl ConnectionTrait,
    config: &RuntimeConfig,
    project: ProjectId,
) -> Result<SopsKey, SecretResolutionError> {
    let row = EProject::find_by_id(project)
        .one(db)
        .await?
        .ok_or_else(|| SecretResolutionError::ProjectKey("the project no longer exists".into()))?;
    let (private_key, _) = gradient_sources::ssh_key::decrypt_ssh_private_key(
        &config.secrets.crypt_file,
        row,
        &config.server.serve_url,
    )
    .map_err(|e| SecretResolutionError::ProjectKey(e.to_string()))?;
    let private_key = Zeroizing::new(private_key);
    SopsKey::from_ssh_private_key(&private_key)
        .map_err(|e| SecretResolutionError::ProjectKey(e.to_string()))
}

pub fn secret_refusal(
    req: &ExecRequirement,
    names: &ResolvedSecretNames,
    workers: &[WorkerInfo],
) -> Option<SecretResolutionError> {
    let without_secrets = ExecRequirement {
        secrets: Vec::new(),
        server_secrets: Vec::new(),
        ..req.clone()
    };
    let fitting: Vec<(&str, Vec<String>)> = workers
        .iter()
        .filter(|w| caps_of(w).fits_exec(&without_secrets, DeviceCount::Total))
        .map(|w| {
            let advertised = w.exec.as_ref().map(|e| e.secrets.clone());
            (w.id.as_str(), advertised.unwrap_or_default())
        })
        .collect();
    if fitting.is_empty() {
        return None;
    }

    for secret in &names.server {
        if let Some((worker, _)) = fitting.iter().find(|(_, held)| held.contains(&secret.name)) {
            return Some(SecretResolutionError::TwoSources {
                name: secret.name.clone(),
                first: secret.source.to_string(),
                second: format!("worker sops of {worker}"),
            });
        }
    }

    let holds_all = |held: &[String]| names.worker.iter().all(|n| held.contains(n));
    if fitting.iter().any(|(_, held)| holds_all(held)) {
        return None;
    }

    names
        .worker
        .iter()
        .find(|name| !fitting.iter().any(|(_, held)| held.contains(name)))
        .or(names.worker.first())
        .map(|name| SecretResolutionError::NotFound { name: name.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_jobs::{allow_all, exec_caps, worker_info};
    use gradient_types::exec::ExecDefaults;

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn listed(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn x86(edit: impl FnOnce(&mut ExecRequirement)) -> ExecRequirement {
        let mut req = ExecRequirement {
            architecture: "x86_64-linux".into(),
            ..Default::default()
        };
        edit(&mut req);
        req
    }

    #[test]
    fn a_name_in_the_project_store_and_a_repo_file_fails() {
        let err = resolve_sources(
            &listed(&["deploy-ssh-key"]),
            &set(&["deploy-ssh-key"]),
            &[("secrets/deploy.yaml".into(), set(&["deploy-ssh-key"]))],
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            r#"secret "deploy-ssh-key" found in two sources: project secrets, sops file secrets/deploy.yaml"#
        );
    }

    #[test]
    fn a_name_in_two_repo_files_fails() {
        let repo = [
            ("a.yaml".into(), set(&["hf-token"])),
            ("b.yaml".into(), set(&["hf-token"])),
        ];
        assert!(matches!(
            resolve_sources(&listed(&["hf-token"]), &set(&[]), &repo),
            Err(SecretResolutionError::TwoSources { .. })
        ));
    }

    #[test]
    fn server_names_split_from_worker_names() {
        let got = resolve_sources(
            &listed(&["wandb-token", "hf-token", "lab-key"]),
            &set(&["wandb-token"]),
            &[("a.yaml".into(), set(&["hf-token", "unused"]))],
        )
        .unwrap();
        assert_eq!(got.worker, vec!["lab-key".to_owned()]);
        assert_eq!(
            got.server
                .iter()
                .map(|s| (s.name.as_str(), s.source.to_string()))
                .collect::<Vec<_>>(),
            vec![
                ("wandb-token", "project secrets".to_owned()),
                ("hf-token", "sops file a.yaml".to_owned())
            ]
        );
    }

    #[test]
    fn a_name_no_fitting_worker_holds_fails() {
        let names = ResolvedSecretNames {
            worker: listed(&["lab-key"]),
            ..Default::default()
        };
        let workers = [worker_info(
            "w1",
            exec_caps(allow_all(), ExecDefaults::default()),
        )];
        assert_eq!(
            secret_refusal(&x86(|_| {}), &names, &workers)
                .unwrap()
                .to_string(),
            r#"secret "lab-key" not found in project secrets, repository sops files or worker sops files"#
        );
    }

    #[test]
    fn no_fitting_worker_means_waiting_not_failing() {
        let names = ResolvedSecretNames {
            worker: listed(&["lab-key"]),
            ..Default::default()
        };
        let workers = [worker_info(
            "w1",
            exec_caps(allow_all(), ExecDefaults::default()),
        )];
        let req = x86(|r| r.labels = listed(&["3d-printer"]));
        assert!(secret_refusal(&req, &names, &workers).is_none());
    }

    #[test]
    fn a_fitting_worker_holding_a_server_name_fails_the_job() {
        let mut caps = exec_caps(allow_all(), ExecDefaults::default());
        caps.exec.as_mut().unwrap().secrets = listed(&["wandb-token"]);
        let names = ResolvedSecretNames {
            server: vec![ServerSecret {
                name: "wandb-token".into(),
                source: ServerSecretSource::Project,
            }],
            ..Default::default()
        };
        assert_eq!(
            secret_refusal(&x86(|_| {}), &names, &[worker_info("w1", caps)])
                .unwrap()
                .to_string(),
            r#"secret "wandb-token" found in two sources: project secrets, worker sops of w1"#
        );
    }
}
