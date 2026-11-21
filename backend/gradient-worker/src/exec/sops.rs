/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use gradient_sops::{SopsKey, decrypt_file};
use gradient_types::{SecretBytes, SecretString};
use gradient_util::sync::Mutex;
use gradient_vm::SopsSecretFileEntry;
use gradient_wire::host_resource_project_access::WorkerResourceProjectAccess;
use uuid::Uuid;

pub struct WorkerSecrets {
    files: Vec<SopsSecretFileEntry>,
    key: Option<SopsKey>,
    values: Mutex<BTreeMap<String, DecryptedSecretWithProjectAccess>>,
}

#[derive(Clone)]
struct DecryptedSecretWithProjectAccess {
    value: SecretBytes,
    projects: WorkerResourceProjectAccess,
}

impl WorkerSecrets {
    pub fn open(
        files: &[SopsSecretFileEntry],
        key_file: Option<&Path>,
    ) -> Result<Arc<WorkerSecrets>> {
        let key = key_file.map(read_key).transpose()?;
        if !files.is_empty() && key.is_none() {
            bail!("exec.sops.files set without exec.sops.keyFile");
        }
        let values = match &key {
            Some(key) => load(files, key)?,
            None => BTreeMap::new(),
        };
        Ok(Arc::new(WorkerSecrets {
            files: files.to_vec(),
            key,
            values: Mutex::new(values),
        }))
    }

    pub fn reload(&self) -> Result<usize> {
        let Some(key) = &self.key else {
            return Ok(0);
        };
        let values = load(&self.files, key)?;
        let count = values.len();
        *self.values.lock() = values;
        Ok(count)
    }

    pub fn project_access_by_secret_name(&self) -> BTreeMap<String, WorkerResourceProjectAccess> {
        self.values
            .lock()
            .iter()
            .map(|(name, secret)| (name.clone(), secret.projects.clone()))
            .collect()
    }

    pub fn secret_value_if_released_to_project(
        &self,
        name: &str,
        project: &Uuid,
    ) -> Option<SecretBytes> {
        self.values
            .lock()
            .get(name)
            .filter(|secret| secret.projects.contains(project))
            .map(|secret| secret.value.clone())
    }
}

fn read_key(path: &Path) -> Result<SopsKey> {
    let openssh = std::fs::read_to_string(path)
        .map(SecretString::new)
        .with_context(|| format!("read the sops key file {}", path.display()))?;
    Ok(SopsKey::from_ssh_private_key(openssh.expose())?)
}

fn load(
    files: &[SopsSecretFileEntry],
    key: &SopsKey,
) -> Result<BTreeMap<String, DecryptedSecretWithProjectAccess>> {
    let mut loaded: BTreeMap<String, (DecryptedSecretWithProjectAccess, &Path)> = BTreeMap::new();
    for entry in files {
        let file = entry.path.as_path();
        let content =
            std::fs::read(file).with_context(|| format!("read sops file {}", file.display()))?;
        let secrets = decrypt_file(&file.to_string_lossy(), &content, key)?;
        for name in secrets.names() {
            if let Some((_, first)) = loaded.get(name) {
                bail!(
                    "secret \"{name}\" found in two sources: {}, {}",
                    first.display(),
                    file.display()
                );
            }
            let secret = DecryptedSecretWithProjectAccess {
                value: SecretBytes::from(secrets.get(name).unwrap_or_default()),
                projects: entry.projects.clone(),
            };
            loaded.insert(name.to_owned(), (secret, file));
        }
    }
    Ok(loaded
        .into_iter()
        .map(|(name, (value, _))| (name, value))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../gradient-sops/tests/fixtures")
            .join(name)
    }

    fn sops_file_entry_released_to_every_project(path: PathBuf) -> SopsSecretFileEntry {
        SopsSecretFileEntry {
            path,
            projects: WorkerResourceProjectAccess::EveryProject,
        }
    }

    #[test]
    fn worker_sops_files_load_with_the_host_key() {
        let secrets = WorkerSecrets::open(
            &[sops_file_entry_released_to_every_project(fixture(
                "worker.yaml",
            ))],
            Some(&fixture("ed25519")),
        )
        .unwrap();
        assert_eq!(
            secrets.project_access_by_secret_name(),
            BTreeMap::from([(
                "lab-key".to_owned(),
                WorkerResourceProjectAccess::EveryProject
            )])
        );
        assert_eq!(
            secrets
                .secret_value_if_released_to_project("lab-key", &Uuid::now_v7())
                .expect("loaded")
                .expose(),
            b"lab-value-123"
        );
    }

    #[test]
    fn a_failed_reload_keeps_the_previous_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join("worker.yaml");
        std::fs::copy(fixture("worker.yaml"), &copy).unwrap();
        let secrets = WorkerSecrets::open(
            &[sops_file_entry_released_to_every_project(copy.clone())],
            Some(&fixture("ed25519")),
        )
        .unwrap();

        std::fs::write(&copy, "garbage").unwrap();

        assert!(secrets.reload().is_err());
        assert_eq!(
            secrets.project_access_by_secret_name(),
            BTreeMap::from([(
                "lab-key".to_owned(),
                WorkerResourceProjectAccess::EveryProject
            )])
        );
    }

    #[test]
    fn a_name_in_two_worker_files_fails_the_load() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.yaml"), dir.path().join("b.yaml"));
        std::fs::copy(fixture("worker.yaml"), &a).unwrap();
        std::fs::copy(fixture("worker.yaml"), &b).unwrap();

        let err = WorkerSecrets::open(
            &[
                sops_file_entry_released_to_every_project(a),
                sops_file_entry_released_to_every_project(b),
            ],
            Some(&fixture("ed25519")),
        )
        .err()
        .expect("two sources");

        assert!(err.to_string().contains("found in two sources"), "{err}");
    }
}
